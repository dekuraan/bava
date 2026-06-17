// SPDX-License-Identifier: GPL-3.0-or-later
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
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

use crate::cava::{CavaRebuild, CavaSettings};
use crate::config::{Config, ConfigHandle};
use crate::vis::physics::PhysicsSettings;
use crate::vis::{
    ColorProfile, Direction, DrawingMode, MirrorMode, Theme, VisSettings,
};

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
fn editor_ui(
    mut contexts: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut editor: ResMut<EditorState>,
    mut vis: ResMut<VisSettings>,
    mut mode: ResMut<DrawingMode>,
    mut cava: ResMut<CavaSettings>,
    mut rebuild: ResMut<CavaRebuild>,
    mut physics: ResMut<PhysicsSettings>,
    handle: Res<ConfigHandle>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return; // primary egui context not ready yet
    };

    // Toggle with the configured key (ignored while a text field has focus);
    // Escape closes.
    editor.capture_keyboard = ctx.wants_keyboard_input();
    editor.capture_pointer = ctx.wants_pointer_input();
    if keys.just_pressed(editor.toggle_key) && !editor.capture_keyboard {
        editor.open = !editor.open;
    }
    if editor.open && keys.just_pressed(KeyCode::Escape) {
        editor.open = false;
    }

    if !editor.open {
        editor.profiles_loaded = false;
        return;
    }
    // Populate the profile list once per open.
    if !editor.profiles_loaded {
        editor.profiles = Config::list_profiles();
        editor.profiles_loaded = true;
    }

    let mut open = editor.open;
    egui::Window::new("bava settings")
        .open(&mut open)
        .default_width(320.0)
        .resizable(true)
        .show(ctx, |ui| {
            persistence_section(
                ui,
                &mut editor,
                &mut vis,
                &mut mode,
                &mut cava,
                &mut rebuild,
                &mut physics,
                &handle,
            );
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                mode_section(ui, &mut mode);
                ui.separator();
                geometry_section(ui, &mut vis);
                ui.separator();
                colors_section(ui, &mut vis);
                ui.separator();
                audio_section(ui, &mut cava, &mut rebuild, &mut editor.status);
            });
            if !editor.status.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new(&editor.status).weak());
            }
        });
    editor.open = open;
}

// --- Sections ---------------------------------------------------------------

/// Save / reload / profile controls at the top of the window.
#[allow(clippy::too_many_arguments)]
fn persistence_section(
    ui: &mut egui::Ui,
    editor: &mut EditorState,
    vis: &mut VisSettings,
    mode: &mut DrawingMode,
    cava: &mut CavaSettings,
    rebuild: &mut CavaRebuild,
    physics: &mut PhysicsSettings,
    handle: &ConfigHandle,
) {
    ui.horizontal(|ui| {
        if ui.button("💾 Save").clicked() {
            let mut cfg = Config::from_settings(cava, vis, *mode, physics);
            cfg.set_gui_toggle_key(editor.toggle_key);
            editor.status = match cfg.write(&handle.path) {
                Ok(()) => format!("Saved → {}", handle.path.display()),
                Err(e) => format!("Save failed: {e}"),
            };
        }
        if ui.button("⟳ Reload").clicked() {
            match std::fs::read_to_string(&handle.path)
                .ok()
                .and_then(|t| toml::from_str::<Config>(&t).ok())
            {
                Some(cfg) => {
                    apply_config(&cfg, vis, mode, cava, rebuild, physics);
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
                .selected_text(editor.selected_profile.clone().unwrap_or_else(|| "—".into()))
                .show_ui(ui, |ui| {
                    for name in &names {
                        ui.selectable_value(
                            &mut editor.selected_profile,
                            Some(name.clone()),
                            name,
                        );
                    }
                });
            if ui.button("Load").clicked() {
                if let Some(name) = editor.selected_profile.clone() {
                    match Config::load_profile(&name) {
                        Some(cfg) => {
                            apply_config(&cfg, vis, mode, cava, rebuild, physics);
                            editor.toggle_key = cfg.gui_toggle_key();
                            editor.status = format!("Loaded profile '{name}'");
                        }
                        None => editor.status = format!("Profile '{name}' not found"),
                    }
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
                    let mut cfg = Config::from_settings(cava, vis, *mode, physics);
                    cfg.set_gui_toggle_key(editor.toggle_key);
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

    ui.add(egui::Slider::new(&mut vis.monstercat, 1.0..=4.0).text("monstercat smoothing"));

    enum_combo(ui, "Mirror", &mut vis.mirror, &[
        (MirrorMode::Off, "Off"),
        (MirrorMode::Full, "Full"),
        (MirrorMode::SplitChannels, "Split channels"),
    ]);
    enum_combo(ui, "Direction", &mut vis.direction, &[
        (Direction::TopBottom, "Top → bottom"),
        (Direction::BottomTop, "Bottom → top"),
        (Direction::LeftRight, "Left → right"),
        (Direction::RightLeft, "Right → left"),
    ]);

    ui.checkbox(&mut vis.reverse_mirror, "Reverse mirror side");
    ui.checkbox(&mut vis.reverse_order, "Reverse bar order");
    ui.checkbox(&mut vis.filling, "Fill (vs. outline)");
    ui.checkbox(&mut vis.hearts, "Hearts (spine modes)");

    ui.add(egui::Slider::new(&mut vis.line_thickness, 0.5..=40.0).text("line thickness"));
    ui.add(egui::Slider::new(&mut vis.items_offset, 0.0..=0.5).text("items offset"));
    ui.add(egui::Slider::new(&mut vis.items_roundness, 0.0..=1.0).text("items roundness"));
    ui.add(egui::Slider::new(&mut vis.inner_radius, 0.0..=1.0).text("inner radius (circle)"));
    ui.add(
        egui::Slider::new(&mut vis.rotation, 0.0..=std::f32::consts::TAU).text("rotation (circle)"),
    );
    ui.add(egui::Slider::new(&mut vis.area_margin, 0.0..=200.0).text("area margin (px)"));
    ui.horizontal(|ui| {
        ui.label("area offset");
        ui.add(egui::Slider::new(&mut vis.area_offset.x, -1.0..=1.0).text("x"));
        ui.add(egui::Slider::new(&mut vis.area_offset.y, -1.0..=1.0).text("y"));
    });
}

/// Color-profile editor: pick the active profile and edit its stops.
fn colors_section(ui: &mut egui::Ui, vis: &mut VisSettings) {
    ui.label(egui::RichText::new("Colors").strong());

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
            let mut p = ColorProfile::default();
            p.name = format!("Profile {}", len + 1);
            vis.profiles.push(p);
            vis.active_profile = len;
        }
        if len > 1 && ui.button("🗑").clicked() {
            vis.profiles.remove(active);
            vis.active_profile = vis.active_profile.min(vis.profiles.len() - 1);
            return;
        }
    });

    let idx = vis.active_profile.min(vis.profiles.len() - 1);
    let prof = &mut vis.profiles[idx];

    ui.horizontal(|ui| {
        ui.label("name");
        ui.text_edit_singleline(&mut prof.name);
    });
    enum_combo(ui, "Theme", &mut prof.theme, &[
        (Theme::Dark, "Dark"),
        (Theme::Light, "Light"),
    ]);

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
        .add(egui::Slider::new(&mut bars, 1..=128).text("bars / channel"))
        .changed()
    {
        cava.bars_per_channel = bars as usize;
    }
    ui.checkbox(&mut cava.autosens, "Auto-sensitivity");
    ui.add(egui::Slider::new(&mut cava.noise_reduction, 0.0..=1.0).text("noise reduction"));

    let mut low = cava.low_cutoff_freq;
    let mut high = cava.high_cutoff_freq;
    if ui
        .add(egui::Slider::new(&mut low, 20..=2_000).text("low cutoff (Hz)"))
        .changed()
    {
        cava.low_cutoff_freq = low;
    }
    if ui
        .add(egui::Slider::new(&mut high, 2_000..=22_000).text("high cutoff (Hz)"))
        .changed()
    {
        cava.high_cutoff_freq = high;
    }

    if ui.button("Apply audio (rebuild plan)").clicked() {
        rebuild.0 = true;
        *status = "Rebuilding cavacore plan…".into();
    }

    ui.collapsing("Capture (restart required)", |ui| {
        let mut frame = cava.frame_samples as u32;
        if ui
            .add(egui::DragValue::new(&mut frame).range(16..=8192).speed(8.0))
            .on_hover_text("frame_samples — cava update granularity")
            .changed()
        {
            cava.frame_samples = frame as usize;
        }
        ui.horizontal(|ui| {
            ui.label("rate (Hz)");
            ui.add(egui::DragValue::new(&mut cava.rate).range(8_000..=192_000).speed(100.0));
        });
        let mut chans = cava.channels as u32;
        if ui
            .add(egui::Slider::new(&mut chans, 1..=2).text("channels"))
            .changed()
        {
            cava.channels = chans as usize;
        }
        ui.horizontal(|ui| {
            ui.label("source");
            let mut src = cava.source.clone().unwrap_or_default();
            if ui.text_edit_singleline(&mut src).changed() {
                cava.source = if src.trim().is_empty() { None } else { Some(src) };
            }
        });
        ui.label(
            egui::RichText::new("Rate/channels/source apply after Save + relaunch.")
                .weak()
                .small(),
        );
    });
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

/// Push a loaded [`Config`] into the live runtime resources and request a cava
/// rebuild so the DSP params take hold.
fn apply_config(
    cfg: &Config,
    vis: &mut VisSettings,
    mode: &mut DrawingMode,
    cava: &mut CavaSettings,
    rebuild: &mut CavaRebuild,
    physics: &mut PhysicsSettings,
) {
    *vis = cfg.to_vis_settings();
    *mode = cfg.vis_mode();
    let debug = cava.debug;
    *cava = cfg.to_cava_settings(debug);
    *physics = cfg.to_physics_settings();
    rebuild.0 = true;
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
