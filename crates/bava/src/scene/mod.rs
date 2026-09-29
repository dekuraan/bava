// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scenes: data-driven visualizers assembled from a `scene.toml` and the
//! assets next to it — meshes and glTF models, textures, WGSL shaders, sounds,
//! lights, a 3D camera, spectrum-driven voxel terrain — layered with (or
//! replacing) bava's own 2D visualizer.
//!
//! Nothing a scene draws needs Rust. Motion (`orbit`, `spin`) and audio
//! reactions (`react = [{ property, band, amount }]`) are declarative; custom
//! looks are WGSL files that import `bava::fx_material` and read the live audio
//! uniform. The two built-in scenes (`assets/scenes/minecraft`,
//! `assets/scenes/solar_system`) are written purely in that format, and a user
//! scene is the same thing in `~/.config/bava/scenes/<name>/` — or anywhere,
//! with `--scene path/to/dir`, where it hot-reloads on save.
//!
//! Lifecycle: [`SceneSettings::name`] says what *should* be loaded;
//! [`apply_scene`] (an exclusive system) notices a change, tears the old scene
//! down — despawning its entities and restoring every setting it overrode — and
//! builds the new one. See `docs/SCENES.md` for the format.

pub mod animate;
pub mod def;
pub mod files;
pub mod material3d;
pub mod ready;
pub mod sound;
mod spawn;
pub mod terrain;

use std::collections::HashMap;

use bevy::asset::io::embedded::EmbeddedAssetRegistry;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;

use crate::cava::{CavaRebuild, CavaSettings};
use crate::config::Config;
use crate::vis::fx::FxSettings;
use crate::vis::physics::PhysicsSettings;
use crate::vis::{DrawingMode, VisSettings};
use def::SceneDef;
use files::{SceneEntry, SceneFiles};

/// Render layer the 2D visualizer is moved to while a 3D scene hides it. No
/// camera renders this layer, so the 2D content simply disappears while the
/// vis camera keeps compositing the 3D view and drawing the HUD text.
pub const HIDDEN_2D_LAYER: usize = 29;

/// How often a user scene directory is checked for edits, in seconds.
const HOT_RELOAD_POLL: f32 = 1.0;

/// Which scene should be active. Changing [`name`](Self::name) swaps scenes.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct SceneSettings {
    /// A scene id or path (see [`files::resolve`]); empty for none.
    pub name: String,
    /// Bumped to force a reload of the current scene (the editor's button).
    pub reload: u32,
    /// Set when the *base* settings were just replaced wholesale (the editor
    /// loaded a config or profile): the live values become the new base the
    /// scene's overrides sit on, instead of being rolled back to the old one.
    pub rebase: bool,
}

/// Runtime switches for the scene system.
#[derive(Resource, Clone, Debug)]
pub struct SceneRuntime {
    /// Offline rendering: no sounds, no hot reload.
    pub offline: bool,
}

/// Status of the scene system, shown in the editor.
#[derive(Resource, Clone, Debug, Default)]
pub struct SceneStatus {
    /// The loaded scene's display name, if any.
    pub active: Option<String>,
    pub description: String,
    /// Last load result or error.
    pub message: String,
    /// What the active scene's `[config]` overrode — the editor saves through
    /// it while a scene is loaded, so a scene's look is never baked into
    /// `config.toml`.
    pub base: Option<SceneBase>,
}

/// Every entity a scene spawned; despawned when the scene unloads.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SceneEntity;

/// Marks the scene's 3D camera.
#[derive(Component, Clone, Copy, Debug)]
pub struct SceneCamera3d;

/// The settings underneath an active scene, and which of them it owns.
///
/// A scene owns the keys its `[config]` sets, and only while they still hold
/// the value it gave them. Everything else is the user's: a key the scene never
/// touched, and one the user has edited since (in the editor, or with Space).
/// Switching the scene off puts the owned keys back to their pre-scene values
/// and leaves the rest live; the editor's Save writes that same view.
#[derive(Clone, Debug)]
pub struct SceneBase {
    /// Every setting before the overrides, as a `config.toml` table.
    before: toml::Table,
    /// The same right after them (sanitized, as the resources hold them).
    after: toml::Table,
    /// The scene's `[config]` table: the keys it owns.
    owned: toml::Table,
    /// The background before the scene's `clear_color`.
    clear: ClearColor,
}

impl SceneBase {
    /// `live` with every key the scene still owns put back to its pre-scene
    /// value.
    pub fn user_table(&self, live: &toml::Table) -> toml::Table {
        let mut out = live.clone();
        restore_owned(&mut out, &self.before, &self.after, &self.owned);
        out
    }

    /// The config to save while the scene is active: the live settings, minus
    /// the scene's own overrides.
    pub fn user_config(&self, live: &Config) -> Config {
        toml::Table::try_from(live)
            .ok()
            .and_then(|t| self.user_table(&t).try_into().ok())
            .unwrap_or_else(|| live.clone())
    }
}

/// Put each `owned` key of `out` back to its `before` value, unless it no
/// longer matches `after` (it was edited while the scene was active).
fn restore_owned(
    out: &mut toml::Table,
    before: &toml::Table,
    after: &toml::Table,
    owned: &toml::Table,
) {
    use toml::Value::Table;
    for (k, o) in owned {
        match (o, out.get_mut(k), before.get(k), after.get(k)) {
            (Table(o), Some(Table(out)), Some(Table(b)), Some(Table(a))) => {
                restore_owned(out, b, a, o)
            }
            (_, Some(live), Some(b), Some(a)) if live == a => *live = b.clone(),
            _ => {}
        }
    }
}

/// The scene currently built into the world.
struct LoadedScene {
    entry: SceneEntry,
    base: SceneBase,
    stamp: Option<u64>,
    requested: SceneSettings,
    /// The embedded-registry paths its files were served under, removed on
    /// unload so reloads don't accumulate copies.
    registered: Vec<std::path::PathBuf>,
}

/// Internal lifecycle state.
#[derive(Resource, Default)]
struct SceneState {
    loaded: Option<LoadedScene>,
    /// What was last attempted (so a failing scene isn't retried every frame).
    attempted: Option<SceneSettings>,
    /// A scene directory that failed to load, and its stamp then: watched so
    /// that saving a fix retries it.
    failed: Option<(files::SceneSource, Option<u64>)>,
    since_poll: f32,
}

/// Scene plugin.
pub struct ScenePlugin {
    /// Offline rendering (`--input`): no sounds, no hot reload.
    pub offline: bool,
}

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SceneSettings>()
            .init_resource::<SceneStatus>()
            .init_resource::<SceneState>()
            .init_resource::<sound::SceneSounds>()
            .init_resource::<animate::SceneLayout>()
            .insert_resource(SceneRuntime {
                offline: self.offline,
            })
            .add_plugins(material3d::FxMaterial3dPlugin)
            .add_plugins(ready::SceneReadyPlugin)
            .add_observer(spawn::start_model_animation)
            .add_systems(
                Update,
                (
                    apply_scene.in_set(SceneApplySet),
                    cycle_scene,
                    (
                        animate::animate_objects,
                        animate::animate_lights,
                        animate::animate_camera,
                        terrain::update_terrain,
                        sound::play_scene_sounds,
                        spawn::drive_model_animation,
                    )
                        .after(apply_scene)
                        .after(crate::vis::features::FeaturesSet)
                        .after(crate::vis::fx::material::FxSyncSet),
                ),
            );
    }
}

/// [`apply_scene`]: the frame's scene load / unload. Systems that must see a
/// scene's `[config]` overrides on the frame it loads (the launch balls) run
/// after it.
#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneApplySet;

/// **N** steps to the next scene (off → each built-in / user scene → off),
/// unless the editor has the keyboard or is bound to N itself.
fn cycle_scene(
    keys: Res<ButtonInput<KeyCode>>,
    editor: Res<crate::gui::EditorState>,
    mut settings: ResMut<SceneSettings>,
) {
    if editor.capture_keyboard || editor.toggle_key == KeyCode::KeyN {
        return;
    }
    if !keys.just_pressed(KeyCode::KeyN) {
        return;
    }
    let ids: Vec<String> = files::discover().into_iter().map(|e| e.id).collect();
    settings.name = next_scene(&ids, &settings.name);
    info!(
        "bava: scene → {}",
        if settings.name.is_empty() {
            "none"
        } else {
            &settings.name
        }
    );
}

/// The scene after `current` in `ids`, wrapping through "none" (empty).
fn next_scene(ids: &[String], current: &str) -> String {
    match ids.iter().position(|id| id == current) {
        None if current.is_empty() => ids.first().cloned().unwrap_or_default(),
        None => String::new(),
        Some(i) => ids.get(i + 1).cloned().unwrap_or_default(),
    }
}

/// Load, unload and hot-reload scenes to match [`SceneSettings`].
fn apply_scene(world: &mut World) {
    // A wholesale settings change under a loaded scene: adopt the live values
    // as the new base and rebuild the scene on top of them.
    let rebase = world.resource::<SceneSettings>().rebase;
    if rebase {
        world.resource_mut::<SceneSettings>().rebase = false;
        // Nothing to put back: the new settings replace the old base
        // outright. The background is the scene's own, so it keeps the one
        // from before the scene.
        if let Ok(live) = settings_table(world)
            && let Some(loaded) = world.resource_mut::<SceneState>().loaded.as_mut()
        {
            loaded.base.before = live.clone();
            loaded.base.after = live;
        }
    }
    let wanted = world.resource::<SceneSettings>().clone();
    let offline = world.resource::<SceneRuntime>().offline;
    let dt = world.resource::<Time>().delta_secs();

    // Hot reload: re-stamp the loaded user scene every so often.
    let mut stale = rebase && world.resource::<SceneState>().loaded.is_some();
    {
        let mut state = world.resource_mut::<SceneState>();
        state.since_poll += dt;
        if !offline && state.since_poll >= HOT_RELOAD_POLL {
            state.since_poll = 0.0;
            if let Some(loaded) = &state.loaded
                && loaded.stamp.is_some()
                && files::stamp(&loaded.entry.source) != loaded.stamp
            {
                stale = true;
            }
            if let Some((source, stamp)) = &state.failed
                && stamp.is_some()
                && files::stamp(source) != *stamp
            {
                // The broken scene was edited: forget the failed attempt so it
                // is tried again below.
                state.failed = None;
                state.attempted = None;
            }
        }
    }

    let state = world.resource::<SceneState>();
    let current = state.loaded.as_ref().map(|l| l.requested.clone());
    let attempted = state.attempted.clone();
    if !stale && (current.as_ref() == Some(&wanted) || attempted.as_ref() == Some(&wanted)) {
        return;
    }
    if !stale && current.is_none() && wanted.name.trim().is_empty() {
        world.resource_mut::<SceneState>().attempted = Some(wanted);
        return;
    }

    unload(world);
    {
        let mut state = world.resource_mut::<SceneState>();
        state.attempted = Some(wanted.clone());
        state.failed = None;
    }
    if wanted.name.trim().is_empty() {
        let mut status = world.resource_mut::<SceneStatus>();
        status.active = None;
        status.description.clear();
        status.message = "No scene".into();
        return;
    }
    let Some(entry) = files::resolve(&wanted.name) else {
        let ids: Vec<String> = files::discover().into_iter().map(|e| e.id).collect();
        let e = format!("not found (available: {})", ids.join(", "));
        error!("bava: scene '{}': {e}", wanted.name);
        let mut status = world.resource_mut::<SceneStatus>();
        status.active = None;
        status.message = format!("Scene '{}' failed: {e}", wanted.name);
        return;
    };
    // Stamped before reading, so an edit that lands mid-load is still seen
    // as a change by the next poll.
    let stamp = files::stamp(&entry.source);
    match load(world, &wanted, entry.clone(), stamp) {
        Ok((name, warnings)) => {
            info!("bava: scene '{name}' loaded");
            let mut message = format!(
                "Loaded '{name}'{}",
                if stale { " (reloaded after edit)" } else { "" }
            );
            if !warnings.is_empty() {
                message.push_str(" — ");
                message.push_str(&warnings.join("; "));
            }
            world.resource_mut::<SceneStatus>().message = message;
        }
        Err(e) => {
            world.resource_mut::<SceneState>().failed = Some((entry.source, stamp));
            error!("bava: scene '{}': {e}", wanted.name);
            let mut status = world.resource_mut::<SceneStatus>();
            status.active = None;
            status.message = format!("Scene '{}' failed: {e}", wanted.name);
        }
    }
}

/// Despawn the loaded scene and restore everything it changed.
fn unload(world: &mut World) {
    let Some(loaded) = world.resource_mut::<SceneState>().loaded.take() else {
        return;
    };
    let entities: Vec<Entity> = world
        .query_filtered::<Entity, With<SceneEntity>>()
        .iter(world)
        .collect();
    for e in entities {
        if let Ok(entity) = world.get_entity_mut(e) {
            entity.despawn();
        }
    }
    world.resource_mut::<sound::SceneSounds>().sounds.clear();
    *world.resource_mut::<animate::SceneLayout>() = animate::SceneLayout::default();
    spawn::restore_builtin_layers(world);

    if let Some(registry) = world.get_resource::<EmbeddedAssetRegistry>() {
        for path in &loaded.registered {
            registry.remove_asset(path);
        }
    }

    world.resource_mut::<SceneStatus>().base = None;
    *world.resource_mut::<ClearColor>() = loaded.base.clear.clone();
    match settings_table(world) {
        Ok(live) => {
            let user = loaded.base.user_table(&live);
            if let Err(e) = write_settings(world, &live, &user) {
                error!("bava: restoring settings after the scene: {e}");
            }
        }
        Err(e) => error!("bava: restoring settings after the scene: {e}"),
    }
}

/// Build the scene `wanted` names from `entry`, whose files were stamped
/// `stamp` before being read. Returns its display name and any warnings.
fn load(
    world: &mut World,
    wanted: &SceneSettings,
    entry: SceneEntry,
    stamp: Option<u64>,
) -> Result<(String, Vec<String>), String> {
    let scene_files = files::read(&entry.source)?;
    let def = SceneDef::parse(scene_files.scene_toml()?)?;
    for path in referenced_files(&def) {
        if !scene_files.contains(&path) {
            return Err(format!("missing file {path:?}"));
        }
    }
    // Compile shaders before touching the world, so a broken reference
    // leaves the previous state intact.
    let mut shader_sources = HashMap::new();
    for (name, path) in &def.shaders {
        shader_sources.insert(
            name.clone(),
            (path.clone(), scene_files.text(path)?.to_string()),
        );
    }

    world.resource_mut::<SceneState>().since_poll = 0.0;
    let (base, warnings) = apply_overrides(world, def.config.as_ref())?;
    // A unique slot per load: re-registering the same paths would hand back
    // the asset server's cached copies after a hot reload.
    let slot = format!("{}-{}", sanitize(&entry.id), next_slot());
    let registered = world
        .get_resource::<EmbeddedAssetRegistry>()
        .map(|registry| files::register(registry, &slot, &scene_files))
        .unwrap_or_default();

    let name = if def.scene.name.is_empty() {
        entry.id.clone()
    } else {
        def.scene.name.clone()
    };
    let ctx = spawn::SpawnContext {
        slot,
        shader_sources,
        files: scene_files,
    };
    let spawned = spawn::spawn_scene(world, &def, &ctx);
    world.resource_mut::<SceneState>().loaded = Some(LoadedScene {
        entry,
        base: base.clone(),
        stamp,
        requested: wanted.clone(),
        registered,
    });
    if let Err(e) = spawned {
        // Roll back whatever was spawned, registered and overridden.
        unload(world);
        return Err(e);
    }

    let mut status = world.resource_mut::<SceneStatus>();
    status.active = Some(name.clone());
    status.description = def.scene.description.clone();
    status.base = Some(base);
    for w in &warnings {
        warn!("bava: scene '{name}': {w}");
    }
    Ok((name, warnings))
}

/// A per-process counter so every load gets fresh asset paths.
fn next_slot() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A scene id reduced to a safe path segment.
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Every file path a scene definition references (textures, models, sounds,
/// the terrain atlas) — checked to exist before anything is spawned.
pub fn referenced_files(def: &SceneDef) -> Vec<String> {
    let mut out = Vec::new();
    let mut object = |o: &def::ObjectDef| {
        if let Some(m) = &o.model {
            out.push(m.clone());
        }
        if let Some(t) = &o.material.texture {
            out.push(t.clone());
        }
    };
    def.objects.iter().for_each(&mut object);
    def.rings.iter().for_each(|r| object(&r.template));
    out.extend(def.sounds.values().map(|s| s.path.clone()));
    if let Some(t) = &def.terrain {
        out.push(t.atlas.clone());
    }
    out.extend(def.shaders.values().cloned());
    out
}

/// Merge a scene's `[config]` table over the live settings. Returns what it
/// overrode, and warnings for keys that don't exist (a typo would otherwise be
/// silently ignored).
fn apply_overrides(
    world: &mut World,
    overrides: Option<&toml::Table>,
) -> Result<(SceneBase, Vec<String>), String> {
    let before = settings_table(world)?;
    let mut warnings = Vec::new();
    let mut owned = overrides.cloned().unwrap_or_default();
    for skip in ["scene", "gui", "audio"] {
        if owned.remove(skip).is_some() {
            warnings.push(format!("[config.{skip}] can't be set by a scene"));
        }
    }
    unknown_keys(&owned, &before, "config", &mut warnings);
    let mut table = before.clone();
    merge(&mut table, &owned);
    write_settings(world, &before, &table)?;
    let base = SceneBase {
        after: settings_table(world)?,
        before,
        owned,
        clear: world.resource::<ClearColor>().clone(),
    };
    Ok((base, warnings))
}

/// The live settings as a `config.toml` table.
fn settings_table(world: &World) -> Result<toml::Table, String> {
    let mut cfg = Config::from_settings(
        world.resource::<CavaSettings>(),
        world.resource::<VisSettings>(),
        *world.resource::<DrawingMode>(),
        world.resource::<PhysicsSettings>(),
    );
    cfg.fx = world.resource::<FxSettings>().clone();
    toml::Table::try_from(&cfg).map_err(|e| e.to_string())
}

/// Write the settings `table` describes into the live resources, touching
/// only the sections that differ from `live` (the table of what they hold
/// now). Fails without writing anything if `table` doesn't deserialize.
fn write_settings(
    world: &mut World,
    live: &toml::Table,
    table: &toml::Table,
) -> Result<(), String> {
    let cfg: Config = table
        .clone()
        .try_into()
        .map_err(|e: toml::de::Error| format!("[config]: {e}"))?;
    let changed = |section: &str| live.get(section) != table.get(section);
    if changed("cava") {
        let before = world.resource::<CavaSettings>().clone();
        // Capture-thread parameters stay pinned (see the editor's Apply).
        let cava = CavaSettings {
            rate: before.rate,
            channels: before.channels,
            frame_samples: before.frame_samples,
            source: before.source.clone(),
            follow_active_sink: before.follow_active_sink,
            ..cfg.to_cava_settings(before.debug)
        };
        if cava != before {
            *world.resource_mut::<CavaSettings>() = cava;
            world.resource_mut::<CavaRebuild>().0 = true;
        }
    }
    if changed("vis") {
        // Keep the live dynamic album palette: it isn't a setting.
        let dynamic = world.resource::<VisSettings>().dynamic_fg.clone();
        *world.resource_mut::<VisSettings>() = VisSettings {
            dynamic_fg: dynamic,
            ..cfg.to_vis_settings()
        };
        world
            .resource_mut::<DrawingMode>()
            .set_if_neq(cfg.vis_mode());
    }
    if changed("physics") {
        *world.resource_mut::<PhysicsSettings>() = cfg.to_physics_settings();
    }
    if changed("fx") {
        world
            .resource_mut::<FxSettings>()
            .set_if_neq(cfg.to_fx_settings());
    }
    Ok(())
}

/// Deep-merge `over` into `base`: tables merge key by key, anything else
/// replaces.
fn merge(base: &mut toml::Table, over: &toml::Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Collect `over` keys with no counterpart in `known`.
fn unknown_keys(over: &toml::Table, known: &toml::Table, at: &str, out: &mut Vec<String>) {
    for (k, v) in over {
        match known.get(k) {
            None => out.push(format!("unknown setting [{at}] {k}")),
            Some(toml::Value::Table(kt)) => {
                if let toml::Value::Table(ot) = v {
                    unknown_keys(ot, kt, &format!("{at}.{k}"), out);
                }
            }
            Some(_) => {}
        }
    }
}

/// Show or hide the 2D visualizer layer on the vis camera.
pub(crate) fn set_2d_hidden(world: &mut World, hidden: bool) {
    let cams: Vec<Entity> = world
        .query_filtered::<Entity, With<crate::vis::bars::VisCamera>>()
        .iter(world)
        .collect();
    for cam in cams {
        let mut e = world.entity_mut(cam);
        if hidden {
            e.insert(RenderLayers::layer(HIDDEN_2D_LAYER));
        } else {
            e.remove::<RenderLayers>();
        }
    }
}

/// Resolve the `--scene` / `[scene] name` argument for the startup log and
/// `--list-scenes`.
pub fn list_scenes() -> Vec<SceneEntry> {
    files::discover()
}

/// Read a scene's files (exposed for `--list-scenes` descriptions).
pub fn describe(entry: &SceneEntry) -> Option<String> {
    let f: SceneFiles = files::read(&entry.source).ok()?;
    let def = SceneDef::parse(f.scene_toml().ok()?).ok()?;
    Some(if def.scene.description.is_empty() {
        def.scene.name
    } else {
        format!("{} — {}", def.scene.name, def.scene.description)
    })
}

/// How to write a scene name to `config.toml`: ids as they are, and a
/// directory named by a relative path as its absolute path, so the saved config
/// still finds it from another working directory.
pub fn persistent_name(name: &str) -> String {
    match files::resolve(name) {
        Some(SceneEntry {
            id,
            source: files::SceneSource::Dir(dir),
        }) if id != name.trim() && dir.is_absolute() => dir.display().to_string(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_scene_cycles_through_none() {
        let ids = vec!["a".to_string(), "b".to_string()];
        assert_eq!(next_scene(&ids, ""), "a");
        assert_eq!(next_scene(&ids, "a"), "b");
        assert_eq!(next_scene(&ids, "b"), "");
        assert_eq!(next_scene(&ids, "/some/path"), "");
        assert_eq!(next_scene(&[], ""), "");
    }

    #[test]
    fn merge_is_deep_and_unknown_keys_are_reported() {
        let mut base: toml::Table =
            toml::from_str("[vis]\na = 1\nb = 2\n[fx]\nc = true\n").unwrap();
        let over: toml::Table = toml::from_str("[vis]\nb = 3\nzz = 1\n[nope]\nx = 1\n").unwrap();
        let mut warnings = Vec::new();
        unknown_keys(&over, &base, "config", &mut warnings);
        merge(&mut base, &over);
        assert_eq!(base["vis"]["a"].as_integer(), Some(1));
        assert_eq!(base["vis"]["b"].as_integer(), Some(3));
        assert_eq!(base["fx"]["c"].as_bool(), Some(true));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("zz")));
        assert!(warnings.iter().any(|w| w.contains("nope")));
    }

    fn settings_world() -> World {
        let mut world = World::new();
        world.insert_resource(CavaSettings::default());
        world.insert_resource(VisSettings::default());
        world.insert_resource(DrawingMode::default());
        world.insert_resource(PhysicsSettings::default());
        world.insert_resource(FxSettings::default());
        world.insert_resource(CavaRebuild::default());
        world.insert_resource(ClearColor::default());
        world
    }

    /// Switch the scene off the way `unload` does, minus the despawning.
    fn restore(world: &mut World, base: &SceneBase) {
        *world.resource_mut::<ClearColor>() = base.clear.clone();
        let live = settings_table(world).unwrap();
        write_settings(world, &live, &base.user_table(&live)).unwrap();
    }

    #[test]
    fn unload_restores_only_the_keys_the_scene_still_owns() {
        let mut world = settings_world();
        let defaults = VisSettings::default();
        let over: toml::Table = toml::from_str(
            "[vis]\ncircle_scale = 0.62\nglow_gain = 1.4\n[physics]\ngravity = 123.0\n",
        )
        .unwrap();
        let (base, warnings) = apply_overrides(&mut world, Some(&over)).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(world.resource::<VisSettings>().circle_scale, 0.62);
        assert_eq!(world.resource::<PhysicsSettings>().gravity, 123.0);

        // While the scene runs the user edits a key it never set, and one it
        // did; the scene also paints the background.
        world.resource_mut::<VisSettings>().inner_radius = 0.3;
        world.resource_mut::<PhysicsSettings>().debug_draw = true;
        world.resource_mut::<VisSettings>().glow_gain = 2.5;
        world.resource_mut::<ClearColor>().0 = Color::srgb(0.5, 0.7, 1.0);

        // Save sees the same view unload restores.
        let mut live = Config::from_settings(
            world.resource::<CavaSettings>(),
            world.resource::<VisSettings>(),
            *world.resource::<DrawingMode>(),
            world.resource::<PhysicsSettings>(),
        );
        live.fx = world.resource::<FxSettings>().clone();
        let saved = base.user_config(&live);
        assert_eq!(saved.vis.circle_scale, defaults.circle_scale);
        assert_eq!(saved.vis.inner_radius, 0.3);
        assert_eq!(saved.vis.glow_gain, 2.5);

        restore(&mut world, &base);
        let vis = world.resource::<VisSettings>();
        assert_eq!(
            vis.circle_scale, defaults.circle_scale,
            "owned key restored"
        );
        assert_eq!(vis.inner_radius, 0.3, "user edit kept");
        assert_eq!(vis.glow_gain, 2.5, "edited owned key is the user's now");
        let physics = world.resource::<PhysicsSettings>();
        assert_eq!(physics.gravity, PhysicsSettings::default().gravity);
        assert!(physics.debug_draw, "runtime toggle kept");
        assert_eq!(world.resource::<ClearColor>().0, ClearColor::default().0);
    }

    #[test]
    fn rebased_scene_keeps_the_new_settings_and_the_old_background() {
        let mut world = settings_world();
        let over: toml::Table = toml::from_str("[vis]\ncircle_scale = 0.62\n").unwrap();
        let (mut base, _) = apply_overrides(&mut world, Some(&over)).unwrap();
        world.resource_mut::<ClearColor>().0 = Color::srgb(0.5, 0.7, 1.0);

        // The editor loads a profile: every setting replaced wholesale, then
        // `apply_scene` rebases before tearing the scene down.
        world.resource_mut::<VisSettings>().circle_scale = 1.7;
        let live = settings_table(&world).unwrap();
        base.before = live.clone();
        base.after = live;
        restore(&mut world, &base);
        assert_eq!(world.resource::<VisSettings>().circle_scale, 1.7);
        assert_eq!(world.resource::<ClearColor>().0, ClearColor::default().0);
    }

    #[test]
    fn unchanged_sections_are_not_rewritten() {
        let mut world = settings_world();
        let over: toml::Table = toml::from_str("[fx]\nhalo = 2.0\n").unwrap();
        let (base, _) = apply_overrides(&mut world, Some(&over)).unwrap();
        world.resource_mut::<PhysicsSettings>().debug_draw = true;
        let tick = world.change_tick();
        world.increment_change_tick();
        restore(&mut world, &base);
        assert!(
            !world
                .resource_ref::<VisSettings>()
                .last_changed()
                .is_newer_than(tick, world.change_tick()),
            "vis untouched"
        );
        assert!(
            !world.resource::<CavaRebuild>().0,
            "no needless cava rebuild"
        );
        assert_eq!(
            world.resource::<FxSettings>().halo,
            FxSettings::default().halo
        );
    }

    #[test]
    fn unload_releases_the_registered_files() {
        let mut world = settings_world();
        world.init_resource::<SceneState>();
        world.init_resource::<SceneStatus>();
        world.init_resource::<sound::SceneSounds>();
        world.init_resource::<animate::SceneLayout>();
        world.init_resource::<EmbeddedAssetRegistry>();
        let mut scene_files = SceneFiles::default();
        scene_files
            .files
            .insert("scene.toml".into(), b"[scene]\n".to_vec().into());
        let registered = files::register(
            world.resource::<EmbeddedAssetRegistry>(),
            "t-0",
            &scene_files,
        );
        assert_eq!(registered.len(), 1);
        let (base, _) = apply_overrides(&mut world, None).unwrap();
        world.resource_mut::<SceneState>().loaded = Some(LoadedScene {
            entry: SceneEntry {
                id: "t".into(),
                source: files::SceneSource::Builtin("t"),
            },
            base,
            stamp: None,
            requested: SceneSettings::default(),
            registered: registered.clone(),
        });
        unload(&mut world);
        let registry = world.resource::<EmbeddedAssetRegistry>();
        for path in &registered {
            assert!(registry.remove_asset(path).is_none(), "{path:?} leaked");
        }
    }

    #[test]
    fn saved_scene_names_survive_a_change_of_directory() {
        assert_eq!(persistent_name("solar_system"), "solar_system");
        assert_eq!(persistent_name(""), "");
        assert_eq!(persistent_name("no-such-scene"), "no-such-scene");

        let dir = tempfile::tempdir().unwrap();
        let scene = dir.path().join("mine");
        std::fs::create_dir_all(&scene).unwrap();
        std::fs::write(scene.join("scene.toml"), "[scene]\nname = \"Mine\"\n").unwrap();
        let canonical = scene.canonicalize().unwrap().display().to_string();
        // A roundabout spelling (as a relative path would be) is written as
        // the directory itself.
        let roundabout = dir.path().join("mine/../mine/scene.toml");
        assert_eq!(persistent_name(roundabout.to_str().unwrap()), canonical);
    }

    #[test]
    fn builtin_scene_overrides_only_use_real_settings() {
        for id in files::builtin_ids() {
            let f = files::read(&files::SceneSource::Builtin(id)).unwrap();
            let def = SceneDef::parse(f.scene_toml().unwrap()).unwrap();
            let Some(over) = def.config else { continue };
            let known = toml::Table::try_from(Config::default()).unwrap();
            let mut warnings = Vec::new();
            unknown_keys(&over, &known, "config", &mut warnings);
            assert!(warnings.is_empty(), "{id}: {warnings:?}");
        }
    }
}
