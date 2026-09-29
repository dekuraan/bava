// SPDX-License-Identifier: MIT OR Apache-2.0
//! In-app settings editor: a floating [`egui`] window for live-tweaking the
//! visualizer, with TOML save/reload and named profiles.
//!
//! The window edits the runtime [`VisSettings`] / [`DrawingMode`] resources
//! directly, so every change is reflected the same frame. Audio/DSP edits go
//! through [`CavaRebuild`](crate::cava::CavaRebuild): the DSP params apply on an
//! explicit "Apply" press (rebuilding the cavacore plan), while rate/channels/
//! source are pinned to the running capture thread and only take effect after a
//! save + relaunch.
//!
//! Toggle the window with the `[gui] toggle_key` (default `p`), or close it with
//! its X. The key is configurable in `config.toml` and round-trips through Save.

use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};

use crate::cava::{CaptureStatus, CavaRebuild, CavaRebuildStatus, CavaSettings};
use crate::config::{Config, ConfigHandle};
use crate::scene::files::SceneEntry;
use crate::scene::{SceneSettings, SceneStatus};
use crate::vis::fx::FxSettings;
use crate::vis::physics::PhysicsSettings;
use crate::vis::{ColorProfile, Direction, DrawingMode, MirrorMode, Theme, ToneMap, VisSettings};

/// Editor window state: visibility, the toggle key, the cached profile list,
/// and transient UI scratch (a status line and the "save as" name field).
#[derive(Resource)]
pub struct EditorState {
    /// Whether the editor window is shown.
    pub open: bool,
    /// Key that toggles the window (from `[gui] toggle_key`).
    pub toggle_key: KeyCode,
    /// True while egui holds keyboard focus (e.g. a text field), so the rest of
    /// the app can suppress its own key handling (the Space mode-cycle).
    pub capture_keyboard: bool,
    /// True while egui wants the pointer (cursor over a window/widget), so the
    /// rest of the app can suppress click handling (ball spawning).
    pub capture_pointer: bool,
    /// Last action result, shown at the bottom of the window.
    status: String,
    /// "Save as profile" name field.
    new_profile_name: String,
    /// Cached list of saved profile names, refreshed when the window opens.
    profiles: Vec<String>,
    /// Profile currently selected in the load dropdown.
    selected_profile: Option<String>,
    /// Have we populated [`profiles`](Self::profiles) for this open session yet?
    profiles_loaded: bool,
    /// Scenes available to pick, refreshed when the window opens.
    scenes: Vec<SceneEntry>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            open: false,
            toggle_key: KeyCode::KeyP,
            capture_keyboard: false,
            capture_pointer: false,
            status: String::new(),
            new_profile_name: String::new(),
            profiles: Vec::new(),
            selected_profile: None,
            profiles_loaded: false,
            scenes: Vec::new(),
        }
    }
}

impl EditorState {
    /// A fresh editor state with the given visibility and toggle key.
    pub fn new(open: bool, toggle_key: KeyCode) -> Self {
        Self {
            open,
            toggle_key,
            ..Self::default()
        }
    }
}

/// Installs the egui-based settings editor.
pub struct GuiPlugin;

impl Plugin for GuiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EditorState>()
            .add_systems(EguiPrimaryContextPass, editor_ui);
    }
}

/// The editor system: handles the toggle key and, when open, draws the window.
// One parameter per edited resource; a Bevy system signature, not an API.
#[allow(clippy::too_many_arguments)]
fn editor_ui(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut editor: ResMut<EditorState>,
    mut vis: ResMut<VisSettings>,
    mut mode: ResMut<DrawingMode>,
    mut cava: ResMut<CavaSettings>,
    mut rebuild: ResMut<CavaRebuild>,
    mut rebuild_status: ResMut<CavaRebuildStatus>,
    capture_status: Option<Res<CaptureStatus>>,
    mut physics: ResMut<PhysicsSettings>,
    mut fx: ResMut<FxSettings>,
    mut scene: ResMut<SceneSettings>,
    scene_status: Res<SceneStatus>,
    handle: Res<ConfigHandle>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return; // primary egui context not ready yet
    };

    // Surface the rebuild outcome, replacing the "Rebuilding…" placeholder the
    // Apply button set — otherwise a failed rebuild looks like a success.
    if let Some(outcome) = rebuild_status.0.take() {
        editor.status = outcome;
    }

    // Toggle with the configured key (ignored while a text field has focus);
    // Escape closes.
    editor.capture_keyboard = ctx.egui_wants_keyboard_input();
    editor.capture_pointer = ctx.egui_wants_pointer_input();
    if keys.just_pressed(editor.toggle_key) && !editor.capture_keyboard {
        editor.open = !editor.open;
    }
    // Escape also closes — but not while a text field has focus, where egui's
    // own Escape-to-unfocus would otherwise vanish the whole window mid-typing.
    if editor.open && keys.just_pressed(KeyCode::Escape) && !editor.capture_keyboard {
        editor.open = false;
    }

    if !editor.open {
        editor.profiles_loaded = false;
        return;
    }
    // Populate the profile list once per open.
    if !editor.profiles_loaded {
        editor.profiles = Config::list_profiles();
        editor.scenes = crate::scene::files::discover();
        editor.profiles_loaded = true;
    }

    let mut open = editor.open;
    egui::Window::new("bava settings")
        .open(&mut open)
        .default_width(320.0)
        .resizable(true)
        .show(ctx, |ui| {
            let mut live = Live {
                vis: &mut vis,
                mode: &mut mode,
                cava: &mut cava,
                rebuild: &mut rebuild,
                physics: &mut physics,
                fx: &mut fx,
                scene: &mut scene,
            };
            persistence_section(ui, &mut editor, &mut live, &scene_status, &handle);
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                scene_section(ui, &editor.scenes, &mut scene, &scene_status);
                ui.separator();
                mode_section(ui, &mut mode);
                ui.separator();
                fx_section(ui, &mut fx);
                ui.separator();
                geometry_section(ui, &mut vis);
                ui.separator();
                colors_section(ui, &mut vis);
                ui.separator();
                image_section(ui, &mut vis);
                ui.separator();
                physics_section(ui, &mut physics);
                ui.separator();
                if let Some(status) = &capture_status {
                    ui.label(status.message());
                }
                audio_section(ui, &mut cava, &mut rebuild, &mut editor.status);
            });
            if !editor.status.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new(&editor.status).weak());
            }
        });
    editor.open = open;
}

/// A slider that clamps and snaps only what the user edits. egui's default
/// (`SliderClamping::Always`) writes the range clamp and step rounding back into
/// the bound value on every draw, so merely opening the editor would rewrite
/// an off-grid or out-of-range setting — and under an active scene, a key
/// changed that way counts as the user's (see `scene::SceneBase`).
fn slider<Num: egui::emath::Numeric>(
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
) -> egui::Slider<'_> {
    egui::Slider::new(value, range).clamping(egui::SliderClamping::Edits)
}

// --- Sections ---------------------------------------------------------------

/// The live resources the editor reads and writes, bundled so the save /
/// load paths take one argument instead of seven.
struct Live<'a> {
    vis: &'a mut VisSettings,
    mode: &'a mut DrawingMode,
    cava: &'a mut CavaSettings,
    rebuild: &'a mut CavaRebuild,
    physics: &'a mut PhysicsSettings,
    fx: &'a mut FxSettings,
    scene: &'a mut SceneSettings,
}

impl Live<'_> {
    /// The config to save. While a scene is loaded, that is the live settings
    /// minus the scene's own overrides, plus the scene's name — so the scene
    /// is restored next launch without its look being baked in.
    fn to_config(&self, status: &SceneStatus, key: KeyCode) -> Config {
        let mut cfg = Config::from_settings(self.cava, self.vis, *self.mode, self.physics);
        cfg.fx = self.fx.clone();
        if let Some(base) = &status.base {
            cfg = base.user_config(&cfg);
        }
        cfg.scene.name = crate::scene::persistent_name(&self.scene.name);
        cfg.set_gui_toggle_key(key);
        cfg
    }

    /// Push a loaded [`Config`] into the live resources, request a cava
    /// rebuild so the DSP params take hold, and have an active scene re-apply
    /// its overrides on top of the new settings.
    fn apply(&mut self, cfg: &Config) {
        // The album palette is live state, not a setting.
        let dynamic = self.vis.dynamic_fg.take();
        *self.vis = VisSettings {
            dynamic_fg: dynamic,
            ..cfg.to_vis_settings()
        };
        *self.mode = cfg.vis_mode();
        let debug = self.cava.debug;
        *self.cava = cfg.to_cava_settings(debug);
        *self.physics = cfg.to_physics_settings();
        *self.fx = cfg.to_fx_settings();
        self.rebuild.0 = true;
        self.scene.name = cfg.scene.name.clone();
        self.scene.rebase = true;
    }
}

/// Save / reload / profile controls at the top of the window.
fn persistence_section(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    live: &mut Live,
    scene_status: &SceneStatus,
    handle: &ConfigHandle,
) {
    ui.horizontal(|ui| {
        if ui.button("💾 Save").clicked() {
            let cfg = live.to_config(scene_status, editor.toggle_key);
            editor.status = match cfg.write(&handle.path) {
                Ok(()) => format!("Saved → {}", handle.path.display()),
                Err(e) => format!("Save failed: {e}"),
            };
        }
        if ui.button("⟳ Reload").clicked() {
            match Config::load(&handle.path) {
                Some(cfg) => {
                    live.apply(&cfg);
                    editor.toggle_key = cfg.gui_toggle_key();
                    editor.status = "Reloaded config".into();
                }
                None => editor.status = "Reload failed".into(),
            }
        }
    });

    ui.collapsing("Profiles", |ui| {
        // Load an existing profile.
        let names = editor.profiles.clone();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("profile_select")
                .selected_text(
                    editor
                        .selected_profile
                        .clone()
                        .unwrap_or_else(|| "—".into()),
                )
                .show_ui(ui, |ui| {
                    for name in &names {
                        ui.selectable_value(&mut editor.selected_profile, Some(name.clone()), name);
                    }
                });
            if ui.button("Load").clicked()
                && let Some(name) = editor.selected_profile.clone()
            {
                match Config::load_profile(&name) {
                    Some(cfg) => {
                        live.apply(&cfg);
                        editor.toggle_key = cfg.gui_toggle_key();
                        editor.status = format!("Loaded profile '{name}'");
                    }
                    None => editor.status = format!("Profile '{name}' not found"),
                }
            }
        });

        // Save the current settings as a new (or overwritten) profile.
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut editor.new_profile_name);
            if ui.button("Save as").clicked() {
                let name = editor.new_profile_name.trim().to_string();
                if name.is_empty() {
                    editor.status = "Enter a profile name first".into();
                } else {
                    let cfg = live.to_config(scene_status, editor.toggle_key);
                    editor.status = match cfg.save_profile(&name) {
                        Ok(path) => {
                            editor.profiles = Config::list_profiles();
                            editor.selected_profile = Some(name);
                            format!("Saved profile → {}", path.display())
                        }
                        Err(e) => format!("Save failed: {e}"),
                    };
                }
            }
        });
    });

    ui.collapsing("GUI settings", |ui| {
        enum_combo(
            ui,
            "Toggle key",
            &mut editor.toggle_key,
            &[
                (KeyCode::KeyP, "P"),
                (KeyCode::KeyO, "O"),
                (KeyCode::KeyI, "I"),
                (KeyCode::KeyG, "G"),
                (KeyCode::KeyH, "H"),
                (KeyCode::KeyJ, "J"),
                (KeyCode::KeyK, "K"),
                (KeyCode::KeyM, "M"),
                (KeyCode::KeyU, "U"),
                (KeyCode::F1, "F1"),
                (KeyCode::F2, "F2"),
                (KeyCode::F5, "F5"),
                (KeyCode::F6, "F6"),
                (KeyCode::Tab, "Tab"),
                (KeyCode::Backquote, "`"),
                (KeyCode::Insert, "Insert"),
            ],
        );
        ui.label(
            egui::RichText::new("Changes apply immediately. Save to keep them.")
                .weak()
                .small(),
        );
    });
}

/// Scene picker: none, the built-ins, and user scenes.
fn scene_section(
    ui: &mut egui::Ui,
    scenes: &[SceneEntry],
    scene: &mut SceneSettings,
    status: &SceneStatus,
) {
    ui.label(egui::RichText::new("Scene").strong());
    ui.horizontal(|ui| {
        let current = if scene.name.is_empty() {
            "None".to_string()
        } else {
            scenes
                .iter()
                .find(|e| e.id == scene.name)
                .map(SceneEntry::label)
                .unwrap_or_else(|| scene.name.clone())
        };
        egui::ComboBox::from_id_salt("scene_select")
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut scene.name, String::new(), "None");
                for entry in scenes {
                    ui.selectable_value(&mut scene.name, entry.id.clone(), entry.label());
                }
            });
        if !scene.name.is_empty() && ui.button("⟳").on_hover_text("Reload the scene").clicked() {
            scene.reload = scene.reload.wrapping_add(1);
        }
    });
    if !status.description.is_empty() {
        ui.label(egui::RichText::new(&status.description).weak().small());
    }
    if !status.message.is_empty() {
        ui.label(egui::RichText::new(&status.message).small());
    }
    let dir = crate::scene::files::user_scenes_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "~/.config/bava/scenes".into());
    ui.label(
        egui::RichText::new(format!(
            "N cycles scenes. Your own scenes go in {dir}/<name>/scene.toml and reload on save."
        ))
        .weak()
        .small(),
    );
}

/// Shader, particle and camera effects.
fn fx_section(ui: &mut egui::Ui, fx: &mut FxSettings) {
    ui.label(egui::RichText::new("Effects").strong());
    ui.checkbox(&mut fx.enabled, "Effects (shaders, particles, camera)");
    if !fx.enabled {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut fx.plasma, "plasma fill");
        ui.checkbox(&mut fx.backdrop, "starfield");
        ui.checkbox(&mut fx.shockwaves, "beat rings");
        ui.checkbox(&mut fx.sparks, "impact sparks");
        ui.checkbox(&mut fx.glossy_balls, "glossy balls");
    });
    ui.add(slider(&mut fx.blob_opacity, 0.0..=1.0).text("fill opacity"));
    ui.add(slider(&mut fx.halo, 0.0..=4.0).text("halo"));
    ui.add(slider(&mut fx.corona, 0.0..=4.0).text("corona streaks"));
    ui.add(slider(&mut fx.flares, 0.0..=4.0).text("rim flares"));
    if fx.backdrop {
        ui.add(slider(&mut fx.stars, 0.0..=1.0).text("stars"));
        ui.add(slider(&mut fx.nebula, 0.0..=4.0).text("nebula"));
    }
    ui.collapsing("Camera", |ui| {
        ui.add(
            slider(&mut fx.punch, 0.0..=0.2)
                .text("beat zoom punch")
                .step_by(0.005),
        );
        ui.add(slider(&mut fx.shake, 0.0..=40.0).text("beat shake (px)"));
        ui.add(
            slider(&mut fx.chromatic, 0.0..=0.1)
                .text("chromatic aberration")
                .step_by(0.001),
        );
        ui.add(slider(&mut fx.vignette, 0.0..=1.0).text("vignette"));
        ui.add(
            slider(&mut fx.art_zoom, 0.0..=0.3)
                .text("cover zoom on bass")
                .step_by(0.005),
        );
    });
}

/// Drawing-mode selector.
fn mode_section(ui: &mut egui::Ui, mode: &mut DrawingMode) {
    egui::ComboBox::from_label("Drawing mode")
        .selected_text(format!("{:?}", *mode))
        .show_ui(ui, |ui| {
            for m in DrawingMode::ALL {
                ui.selectable_value(mode, m, format!("{m:?}"));
            }
        });
}

/// Geometry / layout tunables.
fn geometry_section(ui: &mut egui::Ui, vis: &mut VisSettings) {
    ui.label(egui::RichText::new("Geometry").strong());

    ui.add(slider(&mut vis.monstercat, 1.0..=4.0).text("monstercat smoothing"));

    enum_combo(
        ui,
        "Mirror",
        &mut vis.mirror,
        &[
            (MirrorMode::Off, "Off"),
            (MirrorMode::Full, "Full"),
            (MirrorMode::SplitChannels, "Split channels"),
        ],
    );
    enum_combo(
        ui,
        "Direction",
        &mut vis.direction,
        &[
            (Direction::TopBottom, "Top → bottom"),
            (Direction::BottomTop, "Bottom → top"),
            (Direction::LeftRight, "Left → right"),
            (Direction::RightLeft, "Right → left"),
        ],
    );

    ui.checkbox(&mut vis.reverse_mirror, "Reverse mirror side");
    ui.checkbox(&mut vis.reverse_order, "Reverse bar order");
    ui.checkbox(&mut vis.filling, "Fill shape");
    ui.checkbox(&mut vis.hearts, "Hearts (spine modes)");

    ui.add(slider(&mut vis.line_thickness, 0.5..=40.0).text("line thickness"));
    ui.add(slider(&mut vis.items_offset, 0.0..=0.5).text("items offset"));
    ui.add(slider(&mut vis.items_roundness, 0.0..=1.0).text("items roundness"));
    ui.add(slider(&mut vis.inner_radius, 0.0..=1.0).text("inner radius (circle)"));
    ui.add(slider(&mut vis.rotation, 0.0..=std::f32::consts::TAU).text("rotation (circle)"));
    ui.add(slider(&mut vis.circle_scale, 0.1..=3.0).text("size (circle)"));
    ui.add(slider(&mut vis.area_margin, 0.0..=200.0).text("area margin (px)"));
    ui.horizontal(|ui| {
        ui.label("area offset");
        ui.add(slider(&mut vis.area_offset.x, -1.0..=1.0).text("x"));
        ui.add(slider(&mut vis.area_offset.y, -1.0..=1.0).text("y"));
    });
}

/// Color-profile editor: pick the active profile and edit its stops.
fn colors_section(ui: &mut egui::Ui, vis: &mut VisSettings) {
    ui.label(egui::RichText::new("Colors").strong());

    enum_combo(
        ui,
        "Tone mapping",
        &mut vis.tonemapping,
        &[
            (ToneMap::None, "None (hard clip)"),
            (ToneMap::TonyMcMapface, "Tony McMapface"),
            (ToneMap::AgX, "AgX"),
            (ToneMap::BlenderFilmic, "Blender Filmic"),
            (ToneMap::AcesFitted, "ACES (fitted)"),
            (ToneMap::Reinhard, "Reinhard"),
            (ToneMap::ReinhardLuminance, "Reinhard (luminance)"),
            (ToneMap::SomewhatBoringDisplayTransform, "Neutral"),
        ],
    );
    ui.add(
        slider(&mut vis.bloom_intensity, 0.0..=2.0)
            .text("bloom intensity")
            .step_by(0.01),
    );
    ui.add(
        slider(&mut vis.glow_gain, 0.0..=6.0)
            .text("glow gain (HDR)")
            .step_by(0.05),
    );
    ui.checkbox(&mut vis.dynamic_colors, "Dynamic colors (from album art)")
        .on_hover_text(
            "Use colors from the current track's cover for the foreground gradient. \
             Colors fade when the track changes.",
        );
    ui.add(slider(&mut vis.album_art_linger, 0.0..=60.0).text("cover linger (s)"))
        .on_hover_text("Keep the previous cover while waiting for new art. 0 = clear immediately.");
    if vis.dynamic_colors {
        ui.add(
            slider(
                &mut vis.dynamic_color_count,
                2..=crate::now_playing::MAX_DYNAMIC_COLORS,
            )
            .text("dynamic colors"),
        )
        .on_hover_text("Number of cover colors used for the gradient and balls.");
        ui.add(
            slider(&mut vis.dynamic_color_fade, 0.0..=5.0)
                .text("color fade (s)")
                .step_by(0.05),
        )
        .on_hover_text("Crossfade time when the palette changes on a new track. 0 = instant.");
    }
    ui.add(
        egui::Slider::new(&mut vis.art_blur, 0.0..=1.0)
            .text("art blur")
            .step_by(0.01),
    )
    .on_hover_text("Blur the album-art backdrop. 0 = sharp cover, 1 = heavy frosted wash.");
    ui.add(
        egui::Slider::new(&mut vis.art_brightness, 0.0..=1.0)
            .text("art brightness")
            .step_by(0.01),
    )
    .on_hover_text("Brightness of the album-art backdrop. Lower keeps the bars readable.");
    ui.separator();

    if vis.profiles.is_empty() {
        vis.profiles.push(ColorProfile::default());
    }
    let len = vis.profiles.len();
    let active = vis.active_profile.min(len - 1);
    vis.active_profile = active;

    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("active_profile")
            .selected_text(vis.profiles[active].name.clone())
            .show_ui(ui, |ui| {
                for i in 0..len {
                    let name = vis.profiles[i].name.clone();
                    ui.selectable_value(&mut vis.active_profile, i, name);
                }
            });
        if ui.button("+ profile").clicked() {
            let p = ColorProfile {
                name: format!("Profile {}", len + 1),
                ..Default::default()
            };
            vis.profiles.push(p);
            vis.active_profile = len;
        }
        if len > 1 && ui.button("🗑").clicked() {
            vis.profiles.remove(active);
            vis.active_profile = vis.active_profile.min(vis.profiles.len() - 1);
        }
    });

    let idx = vis.active_profile.min(vis.profiles.len() - 1);
    let prof = &mut vis.profiles[idx];

    ui.horizontal(|ui| {
        ui.label("name");
        ui.text_edit_singleline(&mut prof.name);
    });
    enum_combo(
        ui,
        "Theme",
        &mut prof.theme,
        &[(Theme::Dark, "Dark"), (Theme::Light, "Light")],
    );

    color_stops(ui, "Foreground", &mut prof.fg);
    color_stops(ui, "Background", &mut prof.bg);
}

/// Audio / DSP controls. DSP params apply on an explicit rebuild; the capture
/// rate/channels/source are restart-only.
fn audio_section(
    ui: &mut egui::Ui,
    cava: &mut CavaSettings,
    rebuild: &mut CavaRebuild,
    status: &mut String,
) {
    ui.label(egui::RichText::new("Audio / DSP").strong());

    let mut bars = cava.bars_per_channel as u32;
    if ui
        .add(slider(&mut bars, 1..=128).text("bars / channel"))
        .changed()
    {
        cava.bars_per_channel = bars as usize;
    }
    ui.checkbox(&mut cava.autosens, "Auto-sensitivity");
    ui.add(slider(&mut cava.noise_reduction, 0.0..=1.0).text("noise reduction"));

    let mut low = cava.low_cutoff_freq;
    let mut high = cava.high_cutoff_freq;
    if ui
        .add(slider(&mut low, 20..=2_000).text("low cutoff (Hz)"))
        .changed()
    {
        cava.low_cutoff_freq = low;
    }
    // Cap the slider at the running plan's Nyquist: a value above rate/2
    // always fails `CavaConfig` validation at Apply time, so offering it
    // (the old fixed 22 kHz max at, say, a 32 kHz rate) is a trap.
    let high_max = (cava.rate / 2).saturating_sub(1).max(2_001);
    if ui
        .add(slider(&mut high, 2_000..=high_max).text("high cutoff (Hz)"))
        .changed()
    {
        cava.high_cutoff_freq = high.min(high_max);
    }

    if ui.button("Apply audio settings").clicked() {
        rebuild.0 = true;
        *status = "Applying audio settings…".into();
    }

    ui.collapsing("Capture (restart required)", |ui| {
        let mut frame = cava.frame_samples as u32;
        if ui
            .add(egui::DragValue::new(&mut frame).range(16..=8192).speed(8.0))
            .on_hover_text("Audio frames per analysis update")
            .changed()
        {
            cava.frame_samples = frame as usize;
        }
        ui.horizontal(|ui| {
            ui.label("rate (Hz)");
            ui.add(
                egui::DragValue::new(&mut cava.rate)
                    .range(8_000..=192_000)
                    .speed(100.0),
            );
        });
        let mut chans = cava.channels as u32;
        if ui.add(slider(&mut chans, 1..=2).text("channels")).changed() {
            cava.channels = chans as usize;
        }
        ui.horizontal(|ui| {
            ui.label("source");
            let mut src = cava.source.clone().unwrap_or_default();
            if ui.text_edit_singleline(&mut src).changed() {
                cava.source = if src.trim().is_empty() {
                    None
                } else {
                    Some(src)
                };
            }
        });
        ui.label(
            egui::RichText::new("Save and restart to apply the sample rate, channels, and source.")
                .weak()
                .small(),
        );
    });
}

/// Image overlay controls: user-supplied background and foreground images.
fn image_section(ui: &mut egui::Ui, vis: &mut VisSettings) {
    ui.label(egui::RichText::new("Images").strong());
    image_layer_editor(
        ui,
        "Background image",
        "Use an absolute path or a path relative to the working directory.",
        &mut vis.background,
    );
    image_layer_editor(
        ui,
        "Foreground overlay",
        "Shown above the bars and below the track text.",
        &mut vis.foreground,
    );
}

/// One collapsing editor (path / clear / scale / alpha) for a user image layer.
fn image_layer_editor(
    ui: &mut egui::Ui,
    header: &str,
    help: &str,
    layer: &mut crate::vis::ImageLayer,
) {
    ui.collapsing(header, |ui| {
        let mut path_str = layer
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("path");
            if ui.text_edit_singleline(&mut path_str).changed() {
                layer.path = if path_str.trim().is_empty() {
                    None
                } else {
                    Some(std::path::PathBuf::from(path_str.trim()))
                };
            }
        });
        if layer.path.is_some() && ui.button("Clear").clicked() {
            layer.path = None;
        }
        ui.label(egui::RichText::new(help).weak().small());
        ui.add(slider(&mut layer.scale, 0.1..=4.0).text("scale"));
        ui.add(slider(&mut layer.alpha, 0.0..=1.0).text("alpha"));
    });
}

/// Physics playground tunables.
fn physics_section(ui: &mut egui::Ui, physics: &mut PhysicsSettings) {
    ui.label(egui::RichText::new("Physics").strong());
    ui.checkbox(&mut physics.enabled, "Enabled");

    if !physics.enabled {
        return;
    }

    ui.add(
        slider(&mut physics.gravity, 0.0..=5000.0)
            .text("gravity (px/s²)")
            .step_by(10.0),
    );
    ui.add(
        slider(&mut physics.restitution, 0.0..=1.0)
            .text("ball restitution")
            .step_by(0.01),
    );
    ui.add(
        slider(&mut physics.air_resistance, 0.0..=5.0)
            .text("air resistance")
            .step_by(0.01),
    );
    ui.add(
        slider(&mut physics.mass, 0.1..=10.0)
            .text("ball mass")
            .step_by(0.1),
    );
    ui.add(slider(&mut physics.radius, 2.0..=80.0).text("ball radius (px)"));

    let mut max = physics.max_balls as u32;
    if ui
        .add(slider(&mut max, 1..=2000).text("max balls"))
        .changed()
    {
        physics.max_balls = max as usize;
    }

    ui.checkbox(&mut physics.randomize, "Randomize per ball");

    let mut debounce = physics.spawn_debounce_ms as u32;
    if ui
        .add(
            slider(&mut debounce, 0..=2000)
                .text("right-click spray delay (ms)")
                .step_by(10.0),
        )
        .on_hover_text("Delay between bursts of 8 balls while right-click is held down.")
        .changed()
    {
        physics.spawn_debounce_ms = debounce as u64;
    }

    ui.collapsing("Surface / wave", |ui| {
        ui.add(
            slider(&mut physics.bar_restitution, 0.0..=2.0)
                .text("surface restitution")
                .step_by(0.01),
        );
        ui.add(
            slider(&mut physics.bar_push, 0.0..=10.0)
                .text("launch gain")
                .step_by(0.05),
        );
    });

    ui.add(
        slider(&mut physics.central_gravity, 0.0..=5000.0)
            .text("central gravity (circle)")
            .step_by(10.0),
    );

    ui.checkbox(&mut physics.ccd, "continuous collision detection")
        .on_hover_text(
            "Prevents fast balls from passing through bars or the floor. \
             Turn it off to reduce simulation work at high ball counts.",
        );

    ui.collapsing("Trails", |ui| {
        ui.checkbox(&mut physics.trails, "Ball trails");
        if physics.trails {
            let mut tlen = physics.trail_length as u32;
            if ui
                .add(slider(&mut tlen, 1..=120).text("trail length"))
                .changed()
            {
                physics.trail_length = tlen as usize;
            }
        }
    });

    ui.checkbox(&mut physics.debug_draw, "Debug colliders (F3)");
}

// --- Small UI helpers -------------------------------------------------------

/// A labelled combo box over a fixed set of `(value, label)` enum variants.
fn enum_combo<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut T,
    options: &[(T, &str)],
) {
    let selected = options
        .iter()
        .find(|(v, _)| v == current)
        .map(|(_, l)| *l)
        .unwrap_or("?");
    egui::ComboBox::from_label(label)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (value, text) in options {
                ui.selectable_value(current, *value, *text);
            }
        });
}

/// A wrapped row of color swatches with +/- to add or drop a gradient stop.
fn color_stops(ui: &mut egui::Ui, label: &str, stops: &mut Vec<Color>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        for c in stops.iter_mut() {
            let mut c32 = color_to_egui(*c);
            if ui.color_edit_button_srgba(&mut c32).changed() {
                *c = egui_to_color(c32);
            }
        }
        if ui.button("+").clicked() {
            stops.push(Color::WHITE);
        }
        if stops.len() > 1 && ui.button("−").clicked() {
            stops.pop();
        }
    });
}

/// Bevy [`Color`] → egui [`Color32`] (straight, un-premultiplied alpha).
fn color_to_egui(c: Color) -> egui::Color32 {
    let s = c.to_srgba();
    let q = crate::config::channel_to_u8;
    egui::Color32::from_rgba_unmultiplied(q(s.red), q(s.green), q(s.blue), q(s.alpha))
}

/// egui [`Color32`] → Bevy [`Color`].
fn egui_to_color(c: egui::Color32) -> Color {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Color::srgba(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawing_the_editor_never_rewrites_a_setting() {
        // Off the sliders' step grid or outside their ranges, as a hand-written
        // config or scene can set them.
        let mut vis = VisSettings {
            glow_gain: 1.42,
            monstercat: 0.5,
            line_thickness: 55.0,
            ..default()
        };
        let mut physics = PhysicsSettings {
            gravity: 1234.5,
            mass: 0.05,
            ..default()
        };
        let mut fx = FxSettings {
            halo: 5.0,
            ..default()
        };
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            geometry_section(ui, &mut vis);
            colors_section(ui, &mut vis);
            physics_section(ui, &mut physics);
            fx_section(ui, &mut fx);
        });
        // No renderer takes the font atlas upload here.
        out.textures_delta.clear();
        assert_eq!(vis.glow_gain, 1.42);
        assert_eq!(vis.monstercat, 0.5);
        assert_eq!(vis.line_thickness, 55.0);
        assert_eq!(physics.gravity, 1234.5);
        assert_eq!(physics.mass, 0.05);
        assert_eq!(fx.halo, 5.0);
    }
}
