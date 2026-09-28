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
    /// The user's own settings underneath the active scene's `[config]`
    /// overrides — what the editor saves while a scene is loaded, so a scene's
    /// look is never baked into `config.toml`.
    pub base: Option<SceneBase>,
}

/// Every entity a scene spawned; despawned when the scene unloads.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SceneEntity;

/// Marks the scene's 3D camera.
#[derive(Component, Clone, Copy, Debug)]
pub struct SceneCamera3d;

/// Settings a scene may override, captured before it did, restored after.
#[derive(Clone, Debug)]
pub struct SceneBase {
    pub vis: VisSettings,
    pub mode: DrawingMode,
    pub physics: PhysicsSettings,
    pub fx: FxSettings,
    pub cava: CavaSettings,
    pub clear: ClearColor,
}

impl SceneBase {
    fn capture(world: &World) -> Self {
        Self {
            vis: world.resource::<VisSettings>().clone(),
            mode: *world.resource::<DrawingMode>(),
            physics: world.resource::<PhysicsSettings>().clone(),
            fx: world.resource::<FxSettings>().clone(),
            cava: world.resource::<CavaSettings>().clone(),
            clear: world.resource::<ClearColor>().clone(),
        }
    }
}

/// The scene currently built into the world.
struct LoadedScene {
    entry: SceneEntry,
    snapshot: SceneBase,
    stamp: Option<u64>,
    requested: SceneSettings,
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
            .add_observer(spawn::start_model_animation)
            .add_systems(
                Update,
                (
                    apply_scene,
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
        let base = SceneBase::capture(world);
        if let Some(loaded) = world.resource_mut::<SceneState>().loaded.as_mut() {
            loaded.snapshot = base;
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
    match load(world, &wanted) {
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
            if let Some(entry) = files::resolve(&wanted.name) {
                let stamp = files::stamp(&entry.source);
                world.resource_mut::<SceneState>().failed = Some((entry.source, stamp));
            }
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

    world.resource_mut::<SceneStatus>().base = None;
    let s = loaded.snapshot;
    let cava_changed = *world.resource::<CavaSettings>() != s.cava;
    // Keep the live dynamic album palette: the snapshot's copy is stale.
    let dynamic = world.resource::<VisSettings>().dynamic_fg.clone();
    *world.resource_mut::<VisSettings>() = VisSettings {
        dynamic_fg: dynamic,
        ..s.vis
    };
    *world.resource_mut::<DrawingMode>() = s.mode;
    *world.resource_mut::<PhysicsSettings>() = s.physics;
    *world.resource_mut::<FxSettings>() = s.fx;
    *world.resource_mut::<ClearColor>() = s.clear;
    if cava_changed {
        *world.resource_mut::<CavaSettings>() = s.cava;
        world.resource_mut::<CavaRebuild>().0 = true;
    }
}

/// Build the scene `wanted` names. Returns its display name and any warnings.
fn load(world: &mut World, wanted: &SceneSettings) -> Result<(String, Vec<String>), String> {
    let entry = files::resolve(&wanted.name).ok_or_else(|| {
        let ids: Vec<String> = files::discover().into_iter().map(|e| e.id).collect();
        format!("not found (available: {})", ids.join(", "))
    })?;
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

    // A unique slot per load: re-registering the same paths would hand back
    // the asset server's cached copies after a hot reload.
    world.resource_mut::<SceneState>().since_poll = 0.0;
    let slot = format!("{}-{}", sanitize(&entry.id), next_slot());
    if let Some(registry) = world.get_resource::<EmbeddedAssetRegistry>() {
        files::register(registry, &slot, &scene_files);
    }

    let snapshot = SceneBase::capture(world);
    let mut warnings = Vec::new();
    if let Some(overrides) = &def.config {
        warnings = apply_overrides(world, overrides)?;
    }

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
    if let Err(e) = spawn::spawn_scene(world, &def, &ctx) {
        // Roll back whatever was spawned and overridden.
        world.resource_mut::<SceneState>().loaded = Some(LoadedScene {
            entry: entry.clone(),
            snapshot,
            stamp: None,
            requested: wanted.clone(),
        });
        unload(world);
        return Err(e);
    }

    let stamp = files::stamp(&entry.source);
    let mut state = world.resource_mut::<SceneState>();
    state.loaded = Some(LoadedScene {
        entry,
        snapshot,
        stamp,
        requested: wanted.clone(),
    });
    let base = world
        .resource::<SceneState>()
        .loaded
        .as_ref()
        .map(|l| l.snapshot.clone());
    let mut status = world.resource_mut::<SceneStatus>();
    status.active = Some(name.clone());
    status.description = def.scene.description.clone();
    status.base = base;
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

/// Merge a scene's `[config]` table over the live settings. Returns warnings
/// for keys that don't exist (a typo would otherwise be silently ignored).
fn apply_overrides(world: &mut World, overrides: &toml::Table) -> Result<Vec<String>, String> {
    let mut base = Config::from_settings(
        world.resource::<CavaSettings>(),
        world.resource::<VisSettings>(),
        *world.resource::<DrawingMode>(),
        world.resource::<PhysicsSettings>(),
    );
    base.fx = world.resource::<FxSettings>().clone();
    let mut table = toml::Table::try_from(&base).map_err(|e| e.to_string())?;
    let mut warnings = Vec::new();
    let mut overrides = overrides.clone();
    for skip in ["scene", "gui", "audio"] {
        if overrides.remove(skip).is_some() {
            warnings.push(format!("[config.{skip}] can't be set by a scene"));
        }
    }
    unknown_keys(&overrides, &table, "config", &mut warnings);
    merge(&mut table, &overrides);
    let merged: Config = table
        .try_into()
        .map_err(|e: toml::de::Error| format!("[config]: {e}"))?;

    let cava_before = world.resource::<CavaSettings>().clone();
    let debug = cava_before.debug;
    let cava = merged.to_cava_settings(debug);
    // Capture-thread parameters stay pinned (see the editor's Apply).
    let cava = CavaSettings {
        rate: cava_before.rate,
        channels: cava_before.channels,
        frame_samples: cava_before.frame_samples,
        source: cava_before.source.clone(),
        follow_active_sink: cava_before.follow_active_sink,
        ..cava
    };
    if cava != cava_before {
        *world.resource_mut::<CavaSettings>() = cava;
        world.resource_mut::<CavaRebuild>().0 = true;
    }
    let dynamic = world.resource::<VisSettings>().dynamic_fg.clone();
    *world.resource_mut::<VisSettings>() = VisSettings {
        dynamic_fg: dynamic,
        ..merged.to_vis_settings()
    };
    *world.resource_mut::<DrawingMode>() = merged.vis_mode();
    *world.resource_mut::<PhysicsSettings>() = merged.to_physics_settings();
    *world.resource_mut::<FxSettings>() = merged.to_fx_settings();
    Ok(warnings)
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
