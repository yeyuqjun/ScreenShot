#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! SnapDock pure-Rust Windows desktop client entry point (no webview, no Electron).
//!
//! Runs silently in the system tray with two persistent windows:
//! 1. Standalone settings window (opened from tray menu or global hotkey)
//! 2. Capture editor window (opened when clicking Save after capturing)

mod annotations;
mod clipboard;
mod dxgi;
mod export;
mod gif;
mod hotkeys;
mod i18n;
mod images;
mod native;
mod network;
mod ocr;
mod recording;
mod service;
mod settings;
mod state;
mod stitch;
mod translate;
mod ui;
mod vault;

use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use state::{App, AppEvent, PinState, TextTarget};
use std::collections::HashMap;
use std::sync::Arc;
use tray_icon::{MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};
use ui::editor_window::EditorState;
use ui::gpu::{EguiWindow, GpuContext};
use ui::overlay::OverlayState;
use ui::pin_window::{
    PinUiFeedback, PinWindowState, COMPANION_GAP_LOGICAL, LANG_PICKER_HEIGHT_LOGICAL,
    TEXT_PANEL_MIN_HEIGHT_LOGICAL, TEXT_PANEL_MIN_WIDTH_LOGICAL, TEXT_PANEL_WIDTH_LOGICAL,
};
use ui::recorder::RecorderState;
use ui::settings_window::{SettingsSection, SettingsWindowState};
use ui::toast::{Toast, ToastWindowState, TOAST_WIDTH};
use uuid::Uuid;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Icon as WinitIcon, Window, WindowAttributes, WindowId, WindowLevel};

const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon.png");

fn load_window_icon() -> Option<WinitIcon> {
    let img = image::load_from_memory(APP_ICON_PNG).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    WinitIcon::from_rgba(img.into_raw(), w, h).ok()
}

fn load_tray_icon() -> Result<tray_icon::Icon, String> {
    let img = image::load_from_memory(APP_ICON_PNG)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).map_err(|e| e.to_string())
}

fn ensure_gpu<'a>(
    slot: &'a mut Option<GpuContext>,
    window: &Arc<Window>,
) -> Result<&'a GpuContext, String> {
    if slot.is_none() {
        *slot = Some(GpuContext::new(window)?);
    }
    Ok(slot.as_ref().unwrap())
}

fn current_theme(app: &App) -> (bool, ui::ThemeAccent, ui::TextColorSetting) {
    let core = app.core.lock().unwrap();
    (
        ui::is_dark_theme(&core.settings),
        ui::theme_accent(&core.settings),
        ui::theme_text_color(&core.settings),
    )
}

const MAX_POOLED_OVERLAYS: usize = 4;

struct PooledOverlay {
    egui_win: EguiWindow,
    last_rect: (i32, i32, u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PinWindowRole {
    Image,
    Toolbar,
    TextPanel,
}

struct PinEntry {
    state: PinWindowState,
    image_win: EguiWindow,
    toolbar_win: Option<EguiWindow>,
    text_panel_win: Option<EguiWindow>,
    toolbar_visible: bool,
    toolbar_hide_deadline: Option<std::time::Instant>,
    toolbar_bar_size_logical: egui::Vec2,
    toolbar_picker_above: bool,
    text_panel_visible: bool,
}

struct SnapDockRunner {
    app: Arc<App>,
    gpu: Option<GpuContext>,
    _tray: Option<TrayIcon>,
    tray_initialized: bool,
    graphics_warmed: bool,
    hotkeys: hotkeys::HotkeyRegistry,
    settings_window: Option<(WindowId, EguiWindow, SettingsWindowState)>,
    editor_window: Option<(WindowId, EguiWindow, EditorState)>,
    overlays: HashMap<WindowId, (EguiWindow, OverlayState)>,
    overlay_pool: Vec<PooledOverlay>,
    pins: HashMap<String, PinEntry>,
    win_to_pin: HashMap<WindowId, (String, PinWindowRole)>,
    recorder: Option<(WindowId, EguiWindow, RecorderState)>,
    toast_window: Option<(WindowId, EguiWindow, ToastWindowState)>,
    next_repaints: HashMap<WindowId, std::time::Instant>,
}

impl SnapDockRunner {
    fn new(app: Arc<App>) -> Self {
        let initial_settings = app.core.lock().unwrap().settings.clone();
        i18n::apply_settings(&initial_settings);
        ui::set_active_accent(ui::theme_accent(&initial_settings));
        let mut hotkeys = hotkeys::HotkeyRegistry::new();
        let warnings = hotkeys.register(&initial_settings);
        app.core.lock().unwrap().hotkey_warnings = warnings.clone();
        if !warnings.is_empty() {
            app.notify(warnings.join("; "), true);
        }
        Self {
            app,
            gpu: None,
            _tray: None,
            tray_initialized: false,
            graphics_warmed: false,
            hotkeys,
            settings_window: None,
            editor_window: None,
            overlays: HashMap::new(),
            overlay_pool: Vec::new(),
            pins: HashMap::new(),
            win_to_pin: HashMap::new(),
            recorder: None,
            toast_window: None,
            next_repaints: HashMap::new(),
        }
    }

    fn panel_clear_color(&self) -> wgpu::Color {
        let (dark, accent, _) = current_theme(&self.app);
        accent.palette().clear_color(dark)
    }

    fn schedule_repaint(&mut self, id: WindowId, delay: Option<std::time::Duration>) {
        match delay {
            Some(d) => {
                self.next_repaints
                    .insert(id, std::time::Instant::now() + d);
            }
            None => {
                self.next_repaints.remove(&id);
            }
        }
    }

    fn handle_hotkey(&mut self, hk_evt: GlobalHotKeyEvent) {
        if hk_evt.state != HotKeyState::Pressed {
            return;
        }
        let Some(action) = self.hotkeys.actions.get(&hk_evt.id).cloned() else {
            return;
        };
        if matches!(action.as_str(), "capture" | "pin" | "long" | "gif") {
            if !self.overlays.is_empty() {
                if let Some((win, _)) = self.overlays.values().next() {
                    if let Some(hwnd) = win.hwnd() {
                        native::activate_window(hwnd);
                    }
                    win.window.focus_window();
                }
                return;
            }
            if let Some((_, win, _)) = &self.recorder {
                if let Some(hwnd) = win.hwnd() {
                    native::activate_window(hwnd);
                }
                win.window.focus_window();
                return;
            }
        }
        match action.as_str() {
            "capture" => {
                service::start_capture(&self.app, "region", false);
            }
            "pin" => {
                service::start_capture(&self.app, "clipboard", false);
            }
            "long" => {
                service::start_capture(&self.app, "long", false);
            }
            "gif" => {
                service::start_capture(&self.app, "gif", false);
            }
            "togglePins" => self.toggle_pins(),
            "settings" => {
                if self.overlays.is_empty() && self.recorder.is_none() {
                    self.app.emit(AppEvent::OpenSettings { section: None });
                }
            }
            _ => {}
        }
    }

    fn build_tray_menu() -> Menu {
        let capture_item = MenuItem::with_id("capture", i18n::t("Start Capture"), true, None);
        let long_item = MenuItem::with_id("long", i18n::t("Scrolling Capture"), true, None);
        let gif_item = MenuItem::with_id("gif", i18n::t("Animated Capture (GIF)"), true, None);
        let pin_item = MenuItem::with_id("pin", i18n::t("Pin Clipboard"), true, None);
        let toggle_item = MenuItem::with_id("toggle", i18n::t("Show / Hide Pins"), true, None);
        let sep1 = PredefinedMenuItem::separator();
        let import_item = MenuItem::with_id("import", i18n::t("Open Image..."), true, None);
        let settings_item = MenuItem::with_id("settings", i18n::t("Settings"), true, None);
        let sep2 = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id("quit", i18n::t("Quit"), true, None);

        let menu = Menu::new();
        let _ = menu.append_items(&[
            &capture_item,
            &long_item,
            &gif_item,
            &pin_item,
            &toggle_item,
            &sep1,
            &import_item,
            &settings_item,
            &sep2,
            &quit_item,
        ]);
        menu
    }

    fn init_tray(&mut self) {
        let menu = Self::build_tray_menu();
        if let Ok(icon) = load_tray_icon() {
            if let Ok(tray) = TrayIconBuilder::new()
                .with_tooltip(i18n::t("SnapDock"))
                .with_icon(icon)
                .with_menu(Box::new(menu))
                .build()
            {
                self._tray = Some(tray);
            }
        }
    }

    fn refresh_tray(&mut self) {
        if let Some(tray) = &self._tray {
            let menu = Self::build_tray_menu();
            tray.set_menu(Some(Box::new(menu)));
            let _ = tray.set_tooltip(Some(i18n::t("SnapDock")));
        }
    }

    fn apply_theme_all(&mut self) {
        let settings_val = self.app.core.lock().unwrap().settings.clone();
        i18n::apply_settings(&settings_val);
        self.refresh_tray();
        let (dark, accent, text_color) = current_theme(&self.app);
        ui::set_active_accent(accent);
        let clear = accent.palette().clear_color(dark);
        if let Some((_, win, _)) = &mut self.settings_window {
            win.window.set_title(i18n::t("SnapDock · Settings"));
            win.set_clear_color(clear);
            ui::apply_theme(&win.egui_ctx, dark, accent, text_color);
            win.window.request_redraw();
        }
        if let Some((_, win, state)) = &mut self.editor_window {
            win.window.set_title(&state.title());
            win.set_clear_color(clear);
            ui::apply_theme(&win.egui_ctx, dark, accent, text_color);
            win.window.request_redraw();
        }
        for (win, _) in self.overlays.values() {
            ui::apply_theme(&win.egui_ctx, dark, accent, text_color);
            win.window.request_redraw();
        }
        for entry in self.pins.values_mut() {
            ui::apply_theme(&entry.image_win.egui_ctx, dark, accent, text_color);
            entry.image_win.window.request_redraw();
            if let Some(tb) = entry.toolbar_win.as_mut() {
                ui::apply_theme(&tb.egui_ctx, true, accent, ui::TextColorSetting::Auto);
                tb.window.request_redraw();
            }
            if let Some(tp) = entry.text_panel_win.as_mut() {
                tp.set_clear_color(clear);
                ui::apply_theme(&tp.egui_ctx, dark, accent, text_color);
                tp.window.request_redraw();
            }
        }
        if let Some((_, win, _)) = &self.recorder {
            ui::apply_theme(&win.egui_ctx, true, accent, ui::TextColorSetting::Auto);
            win.window.request_redraw();
        }
        if let Some((_, win, _)) = &self.toast_window {
            ui::apply_theme(&win.egui_ctx, true, accent, ui::TextColorSetting::Auto);
            win.window.request_redraw();
        }
    }

    fn open_settings(&mut self, event_loop: &ActiveEventLoop, section: Option<&str>) {
        if let Some((_, win, state)) = self.settings_window.as_mut() {
            if let Some(sec) = section.and_then(SettingsSection::from_key) {
                state.section = sec;
            }
            win.window.set_minimized(false);
            win.window.set_visible(true);
            if let Some(hwnd) = win.hwnd() {
                native::activate_window(hwnd);
            }
            win.window.focus_window();
            win.window.request_redraw();
            return;
        }

        let mut attrs = WindowAttributes::default()
            .with_title(i18n::t("SnapDock · Settings"))
            .with_inner_size(LogicalSize::new(780.0, 560.0))
            .with_min_inner_size(LogicalSize::new(660.0, 460.0))
            .with_visible(false);
        if let Some(icon) = load_window_icon() {
            attrs = attrs.with_window_icon(Some(icon));
        }
        let Ok(window) = event_loop.create_window(attrs) else {
            return;
        };
        let window = Arc::new(window);
        let clear = self.panel_clear_color();
        let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
            return;
        };
        let Ok(mut egui_win) = EguiWindow::new(gpu, window.clone(), clear) else {
            return;
        };
        let (dark, accent, text_color) = current_theme(&self.app);
        ui::apply_theme(&egui_win.egui_ctx, dark, accent, text_color);

        // Center within the work area of the monitor containing the cursor.
        let (cx, cy) = native::cursor_pos();
        let (wx, wy, ww, wh) = native::work_area_at_point(cx, cy);
        let outer = window.outer_size();
        let px = wx + (ww - outer.width as i32) / 2;
        let py = wy + (wh - outer.height as i32) / 2;
        window.set_outer_position(PhysicalPosition::new(px.max(wx), py.max(wy)));

        let mut state = SettingsWindowState::new(&self.app, section);
        let app_ref = self.app.clone();
        let win_ref = window.clone();
        let _ = egui_win.render(gpu, |ctx| {
            let _ = state.show(ctx, &app_ref, &win_ref);
        });
        window.set_visible(true);
        if let Some(hwnd) = egui_win.hwnd() {
            native::activate_window(hwnd);
        }
        window.focus_window();
        let id = window.id();
        window.request_redraw();
        self.settings_window = Some((id, egui_win, state));
    }

    fn close_settings(&mut self) {
        if let Some((id, _, _)) = self.settings_window.take() {
            self.next_repaints.remove(&id);
        }
        // Revert any unsaved language preview back to the persisted setting.
        let saved = self.app.core.lock().unwrap().settings.clone();
        i18n::apply_settings(&saved);
    }

    fn open_editor(&mut self, event_loop: &ActiveEventLoop, capture_id: &str) {
        let Some(state) = EditorState::load(&self.app, capture_id) else {
            return;
        };
        if let Some((_, win, cur_state)) = self.editor_window.as_mut() {
            win.window.set_title(&state.title());
            *cur_state = state;
            win.window.set_minimized(false);
            win.window.set_visible(true);
            if let Some(hwnd) = win.hwnd() {
                native::activate_window(hwnd);
            }
            win.window.focus_window();
            win.window.request_redraw();
            return;
        }

        let (cx, cy) = native::cursor_pos();
        let (wx, wy, ww, wh) = native::work_area_at_point(cx, cy);
        let mut attrs = WindowAttributes::default()
            .with_title(state.title())
            .with_inner_size(LogicalSize::new(920.0, 640.0))
            .with_min_inner_size(LogicalSize::new(820.0, 500.0))
            .with_visible(false);
        if let Some(icon) = load_window_icon() {
            attrs = attrs.with_window_icon(Some(icon));
        }
        let Ok(window) = event_loop.create_window(attrs) else {
            return;
        };
        let window = Arc::new(window);
        // Fit window size to capture dimensions (capped at 86% of work area).
        let scale = window.scale_factor().max(0.5);
        let max_lw = ((ww as f64 / scale) * 0.86).max(820.0);
        let max_lh = ((wh as f64 / scale) * 0.86).max(500.0);
        let target_lw = ((state.item.width as f64 / scale) + 64.0).clamp(840.0_f64.min(max_lw), max_lw);
        let target_lh = ((state.item.height as f64 / scale) + 148.0).clamp(540.0_f64.min(max_lh), max_lh);
        let _ = window.request_inner_size(LogicalSize::new(target_lw, target_lh));
        let outer = window.outer_size();
        let px = wx + (ww - outer.width as i32) / 2;
        let py = wy + (wh - outer.height as i32) / 2;
        window.set_outer_position(PhysicalPosition::new(px.max(wx), py.max(wy)));

        let clear = self.panel_clear_color();
        let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
            return;
        };
        let Ok(mut egui_win) = EguiWindow::new(gpu, window.clone(), clear) else {
            return;
        };
        let (dark, accent, text_color) = current_theme(&self.app);
        ui::apply_theme(&egui_win.egui_ctx, dark, accent, text_color);
        let inner = window.inner_size();
        egui_win.resize(gpu, inner.width, inner.height);

        let mut state = state;
        let app_ref = self.app.clone();
        let win_ref = window.clone();
        let _ = egui_win.render(gpu, |ctx| state.show(ctx, &app_ref, &win_ref));
        window.set_visible(true);
        if let Some(hwnd) = egui_win.hwnd() {
            native::activate_window(hwnd);
        }
        window.focus_window();
        let id = window.id();
        window.request_redraw();
        self.editor_window = Some((id, egui_win, state));
    }

    fn close_editor(&mut self) {
        if let Some((id, _, _)) = self.editor_window.take() {
            self.next_repaints.remove(&id);
        }
    }

    fn show_toast(&mut self, event_loop: &ActiveEventLoop, message: String, error: bool) {
        if let Some((_, win, state)) = self.editor_window.as_mut() {
            if win.window.has_focus() {
                state.toast = Some(Toast::new(message, error));
                win.window.request_redraw();
                return;
            }
        }
        if let Some((_, win, state)) = self.settings_window.as_mut() {
            if win.window.has_focus() {
                state.toast = Some(Toast::new(message, error));
                win.window.request_redraw();
                return;
            }
        }

        let toast = Toast::new(message, error);
        if let Some((_, win, state)) = self.toast_window.as_mut() {
            let scale = win.window.scale_factor().max(0.5);
            let desired = ToastWindowState::estimate_size(&toast);
            let pw = (desired.x as f64 * scale).round() as i32;
            let ph = (desired.y as f64 * scale).round() as i32;
            let (cx, cy) = native::cursor_pos();
            let (wx, wy, ww, wh) = native::work_area_at_point(cx, cy);
            let px = wx + ww - pw - 18;
            let py = wy + wh - ph - 18;
            if let Some(hwnd) = win.hwnd() {
                native::set_window_pos_physical(hwnd, px, py, pw, ph, true, true);
            }
            if let Some(gpu) = self.gpu.as_ref() {
                win.resize(gpu, pw.max(1) as u32, ph.max(1) as u32);
            }
            state.toast = Some(toast);
            win.window.request_redraw();
            return;
        }

        let attrs = WindowAttributes::default()
            .with_title(i18n::t("SnapDock · Notification"))
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_inner_size(LogicalSize::new(TOAST_WIDTH as f64, 46.0))
            .with_visible(false);
        let Ok(window) = event_loop.create_window(attrs) else {
            return;
        };
        let window = Arc::new(window);
        let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
            return;
        };
        let Ok(mut egui_win) = EguiWindow::new(gpu, window.clone(), wgpu::Color::TRANSPARENT) else {
            return;
        };
        let (_, accent, _) = current_theme(&self.app);
        ui::apply_theme(&egui_win.egui_ctx, true, accent, ui::TextColorSetting::Auto);

        let scale = window.scale_factor().max(0.5);
        let desired = ToastWindowState::estimate_size(&toast);
        let mut pw = (desired.x as f64 * scale).round() as i32;
        let mut ph = (desired.y as f64 * scale).round() as i32;
        let (cx, cy) = native::cursor_pos();
        let (wx, wy, ww, wh) = native::work_area_at_point(cx, cy);
        let mut px = wx + ww - pw - 18;
        let mut py = wy + wh - ph - 18;

        if let Some(hwnd) = egui_win.hwnd() {
            native::set_window_tool_window(hwnd);
            native::set_window_exclude_from_capture(hwnd, true);
            native::set_window_round_corners(hwnd);
            native::set_window_pos_physical_hidden(hwnd, px, py, pw, ph, true);
        }
        egui_win.resize(gpu, pw.max(1) as u32, ph.max(1) as u32);
        let mut state = ToastWindowState::new(toast);
        let mut measured = desired;
        let _ = egui_win.render(gpu, |ctx| {
            measured = state.show(ctx);
        });
        if (measured.y - desired.y).abs() > 2.0 {
            pw = (measured.x as f64 * scale).round() as i32;
            ph = (measured.y as f64 * scale).round() as i32;
            px = wx + ww - pw - 18;
            py = wy + wh - ph - 18;
            if let Some(hwnd) = egui_win.hwnd() {
                native::set_window_pos_physical_hidden(hwnd, px, py, pw, ph, true);
            }
            egui_win.resize(gpu, pw.max(1) as u32, ph.max(1) as u32);
            let _ = egui_win.render(gpu, |ctx| {
                let _ = state.show(ctx);
            });
        }
        if let Some(hwnd) = egui_win.hwnd() {
            native::set_window_pos_physical(hwnd, px, py, pw, ph, true, true);
        } else {
            window.set_visible(true);
        }
        let id = window.id();
        window.request_redraw();
        self.toast_window = Some((id, egui_win, state));
    }

    fn close_toast(&mut self) {
        if let Some((id, _, _)) = self.toast_window.take() {
            self.next_repaints.remove(&id);
        }
    }

    /// Pre-initialize the shared GPU context and hidden overlay window pool at startup so the first
    /// hotkey press pays zero GPU init, window creation, shader compilation, or CJK font parsing cost.
    fn warmup_graphics(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics_warmed {
            return;
        }
        self.graphics_warmed = true;
        let perf = std::env::var_os("SNAPDOCK_PERF_DEBUG").is_some();
        let t0 = std::time::Instant::now();
        let mut monitors = dxgi::monitor_rects();
        if monitors.is_empty() {
            monitors.push((0, 0, 1, 1));
        }
        let target_count = monitors.len().min(MAX_POOLED_OVERLAYS);
        for (mx, my, mw, mh) in monitors.into_iter().take(target_count) {
            if self.overlay_pool.len() >= MAX_POOLED_OVERLAYS {
                break;
            }
            let attrs = WindowAttributes::default()
                .with_title(i18n::t("SnapDock · Capture Overlay"))
                .with_decorations(false)
                .with_resizable(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_position(PhysicalPosition::new(mx, my))
                .with_inner_size(PhysicalSize::new(1, 1))
                .with_visible(false);
            let Ok(window) = event_loop.create_window(attrs) else {
                break;
            };
            let window = Arc::new(window);
            let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
                break;
            };
            let Ok(mut egui_win) = EguiWindow::new(gpu, window.clone(), wgpu::Color::BLACK) else {
                break;
            };
            if let Some(h) = egui_win.hwnd() {
                native::set_window_tool_window(h);
            }
            egui_win.warmup_fonts(gpu);
            self.overlay_pool.push(PooledOverlay {
                egui_win,
                last_rect: (mx, my, mw, mh),
            });
        }
        if perf {
            eprintln!(
                "[perf] warmup_graphics: {:?} (pooled={})",
                t0.elapsed(),
                self.overlay_pool.len()
            );
        }
    }

    /// Hide active capture overlays, drop screenshot buffers, and return warm `EguiWindow`s to the pool.
    fn release_overlays(&mut self) {
        for (id, (mut egui_win, state)) in self.overlays.drain() {
            self.next_repaints.remove(&id);
            egui_win.window.set_visible(false);
            let last_rect = {
                let pos = egui_win
                    .window
                    .outer_position()
                    .unwrap_or(PhysicalPosition::new(0, 0));
                (pos.x, pos.y, state.screen_image.width, state.screen_image.height)
            };
            drop(state);
            if self.overlay_pool.len() < MAX_POOLED_OVERLAYS {
                if let Some(h) = egui_win.hwnd() {
                    native::resize_window_physical(h, 1, 1);
                }
                if let Some(gpu) = self.gpu.as_ref() {
                    egui_win.park_for_pool(gpu);
                }
                self.overlay_pool.push(PooledOverlay {
                    egui_win,
                    last_rect,
                });
            }
        }
    }

    fn take_pooled_overlay(&mut self, rect: (i32, i32, u32, u32)) -> Option<EguiWindow> {
        if let Some(pos) = self
            .overlay_pool
            .iter()
            .position(|entry| entry.last_rect == rect)
        {
            return Some(self.overlay_pool.swap_remove(pos).egui_win);
        }
        self.overlay_pool.pop().map(|entry| entry.egui_win)
    }

    fn open_overlays(&mut self, event_loop: &ActiveEventLoop) {
        let perf = std::env::var_os("SNAPDOCK_PERF_DEBUG").is_some();
        let t0 = std::time::Instant::now();
        self.release_overlays();

        let (mode, mut entries_info) = {
            let core = self.app.core.lock().unwrap();
            let Some(active) = core.active_capture.as_ref() else {
                return;
            };
            let list: Vec<_> = active
                .entries
                .iter()
                .map(|e| {
                    (
                        e.index,
                        e.monitor_x,
                        e.monitor_y,
                        e.monitor_width,
                        e.monitor_height,
                        e.image.clone(),
                    )
                })
                .collect();
            (active.mode.clone(), list)
        };
        let (dark, accent, text_color) = current_theme(&self.app);
        let (cx, cy) = native::cursor_pos();

        // Prepare and reveal the cursor's monitor first for lowest perceived latency on multi-monitor setups.
        if let Some(cur_pos) = entries_info.iter().position(|&(_, mx, my, mw, mh, _)| {
            cx >= mx && cy >= my && cx < mx + mw as i32 && cy < my + mh as i32
        }) {
            if cur_pos != 0 {
                entries_info.swap(0, cur_pos);
            }
        }

        let mut first_win: Option<(Arc<Window>, Option<isize>)> = None;
        let mut cursor_win: Option<(Arc<Window>, Option<isize>)> = None;

        for (idx, mx, my, mw, mh, img) in entries_info {
            let mut reused = false;
            let mut egui_win = if let Some(win) = self.take_pooled_overlay((mx, my, mw, mh)) {
                reused = true;
                win
            } else {
                let attrs = WindowAttributes::default()
                    .with_title(i18n::t("SnapDock · Capture Overlay"))
                    .with_decorations(false)
                    .with_resizable(false)
                    .with_window_level(WindowLevel::AlwaysOnTop)
                    .with_position(PhysicalPosition::new(mx, my))
                    .with_inner_size(PhysicalSize::new(mw, mh))
                    .with_visible(false);
                let Ok(window) = event_loop.create_window(attrs) else {
                    continue;
                };
                let window = Arc::new(window);
                let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
                    continue;
                };
                let Ok(win) = EguiWindow::new(gpu, window.clone(), wgpu::Color::BLACK) else {
                    continue;
                };
                if let Some(h) = win.hwnd() {
                    native::set_window_tool_window(h);
                }
                win
            };

            let window = egui_win.window.clone();
            let Some(gpu) = self.gpu.as_ref() else {
                continue;
            };
            let hwnd = egui_win.hwnd();
            if let Some(h) = hwnd {
                native::set_window_pos_physical_hidden(h, mx, my, mw as i32, mh as i32, true);
            }
            if reused {
                egui_win.prepare_for_reuse();
            }
            ui::apply_theme(&egui_win.egui_ctx, dark, accent, text_color);
            egui_win.resize(gpu, mw, mh);
            let mut state = OverlayState::new(&self.app, idx, mode.clone(), img);
            // Render the first frozen frame while hidden to eliminate black-screen flicker.
            let app_ref = self.app.clone();
            let win_ref = window.clone();
            let _ = egui_win.render(gpu, |ctx| state.show(ctx, &app_ref, &win_ref));
            window.set_visible(true);
            let on_this_monitor =
                cx >= mx && cy >= my && cx < mx + mw as i32 && cy < my + mh as i32;
            if on_this_monitor {
                cursor_win = Some((window.clone(), hwnd));
            }
            if first_win.is_none() {
                first_win = Some((window.clone(), hwnd));
            }
            let id = window.id();
            window.request_redraw();
            self.overlays.insert(id, (egui_win, state));
        }

        if self.overlays.is_empty() {
            service::cancel_capture(&self.app);
            self.app
                .notify(i18n::t("Unable to create capture overlay window"), true);
            return;
        }
        if let Some((w, hwnd)) = cursor_win.or(first_win) {
            if let Some(h) = hwnd {
                native::activate_window(h);
            }
            w.focus_window();
        }
        if perf {
            eprintln!(
                "[perf] open_overlays: {:?} (count={})",
                t0.elapsed(),
                self.overlays.len()
            );
        }
    }

    fn pin_physical_rect(entry: &PinEntry) -> (i32, i32, i32, i32) {
        if let Some(rc) = entry
            .image_win
            .hwnd()
            .and_then(native::window_rect_physical)
        {
            if rc.2 > 0 && rc.3 > 0 {
                return rc;
            }
        }
        let pos = entry
            .image_win
            .window
            .outer_position()
            .map(|p| (p.x, p.y))
            .unwrap_or((0, 0));
        let size = entry.image_win.window.inner_size();
        (pos.0, pos.1, size.width.max(1) as i32, size.height.max(1) as i32)
    }

    fn create_pin(&mut self, event_loop: &ActiveEventLoop, capture_id: &str) {
        let pin_id = format!("pin-{}", Uuid::new_v4());
        let mut state = match PinWindowState::new(&self.app, pin_id.clone(), capture_id.to_string())
        {
            Ok(s) => s,
            Err(e) => {
                self.app.notify(e, true);
                return;
            }
        };
        let capture_origin = {
            let core = self.app.core.lock().unwrap();
            core.captures
                .get(capture_id)
                .and_then(|c| c.capture_origin)
        };
        let cursor = native::cursor_pos();
        let ref_pt = capture_origin
            .map(|(ox, oy)| {
                (
                    ox + (state.base_width as i32) / 2,
                    oy + (state.base_height as i32) / 2,
                )
            })
            .unwrap_or(cursor);
        let work_area = native::work_area_at_point(ref_pt.0, ref_pt.1);
        let (px, py, width, height) = ui::pin_window::compute_pin_initial_rect(
            state.base_width,
            state.base_height,
            state.scale,
            capture_origin,
            cursor,
            work_area,
        );

        let attrs = WindowAttributes::default()
            .with_title(i18n::t("SnapDock · Pin"))
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(PhysicalPosition::new(px, py))
            .with_inner_size(PhysicalSize::new(width, height))
            .with_visible(false);
        let Ok(window) = event_loop.create_window(attrs) else {
            self.app.notify(i18n::t("Unable to create pin window"), true);
            return;
        };
        let window = Arc::new(window);
        let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
            self.app
                .notify(i18n::t("Failed to initialize graphics context"), true);
            return;
        };
        let Ok(mut egui_win) = EguiWindow::new(gpu, window.clone(), wgpu::Color::TRANSPARENT)
        else {
            return;
        };
        let (dark, accent, text_color) = current_theme(&self.app);
        ui::apply_theme(&egui_win.egui_ctx, dark, accent, text_color);
        let hwnd = egui_win.hwnd();
        if let Some(h) = hwnd {
            native::set_window_tool_window(h);
            let _ = native::set_window_opacity(
                h,
                ((state.opacity.clamp(20, 100) as u32) * 255 / 100) as u8,
            );
            native::set_window_pos_physical_hidden(h, px, py, width as i32, height as i32, true);
        }
        egui_win.resize(gpu, width, height);
        // Pre-render the first frame while hidden so the pin appears seamlessly without black flash.
        let app_ref = self.app.clone();
        let win_ref = window.clone();
        let _ = egui_win.render(gpu, |ctx| {
            let _ = state.show_image(ctx, &app_ref, &win_ref, hwnd);
        });
        if let Some(h) = hwnd {
            native::set_window_pos_physical(h, px, py, width as i32, height as i32, true, false);
        } else {
            window.set_visible(true);
        }
        let win_id = window.id();
        {
            let mut core = self.app.core.lock().unwrap();
            core.pins.insert(
                pin_id.clone(),
                PinState {
                    pin_id: pin_id.clone(),
                    capture_id: capture_id.to_string(),
                    scale: state.scale,
                    opacity: state.opacity,
                    passthrough: false,
                },
            );
        }
        let initial_bar_size = state.estimated_toolbar_bar_size();
        window.request_redraw();
        self.win_to_pin
            .insert(win_id, (pin_id.clone(), PinWindowRole::Image));
        self.pins.insert(
            pin_id,
            PinEntry {
                state,
                image_win: egui_win,
                toolbar_win: None,
                text_panel_win: None,
                toolbar_visible: false,
                toolbar_hide_deadline: None,
                toolbar_bar_size_logical: initial_bar_size,
                toolbar_picker_above: false,
                text_panel_visible: false,
            },
        );
    }

    fn show_pin_toolbar(&mut self, event_loop: &ActiveEventLoop, pin_id: &str) {
        if self.app.core.lock().unwrap().pins_hidden {
            return;
        }
        let Some(entry) = self.pins.get_mut(pin_id) else {
            return;
        };
        if entry.state.passthrough {
            return;
        }
        entry.toolbar_hide_deadline = None;

        let pin_rect = Self::pin_physical_rect(entry);
        let scale = entry.image_win.window.scale_factor().max(0.5);
        let gap = (COMPANION_GAP_LOGICAL as f64 * scale).round() as i32;
        let bar_w = (entry.toolbar_bar_size_logical.x as f64 * scale).round() as i32;
        let bar_h = (entry.toolbar_bar_size_logical.y as f64 * scale).round() as i32;
        let picker_extra_h = if entry.state.lang_picker_open() {
            (LANG_PICKER_HEIGHT_LOGICAL as f64 * scale).round() as i32
        } else {
            0
        };
        let work_area =
            native::work_area_at_point(pin_rect.0 + pin_rect.2 / 2, pin_rect.1 + pin_rect.3 / 2);
        let (tx, ty, tw, th, picker_above) = ui::pin_window::compute_toolbar_rect(
            pin_rect,
            (bar_w, bar_h),
            picker_extra_h,
            gap,
            work_area,
        );
        entry.toolbar_picker_above = picker_above;

        let created_now = entry.toolbar_win.is_none();
        if created_now {
            let attrs = WindowAttributes::default()
                .with_title(i18n::t("SnapDock · Pin Toolbar"))
                .with_decorations(false)
                .with_resizable(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_position(PhysicalPosition::new(tx, ty))
                .with_inner_size(PhysicalSize::new(tw.max(1) as u32, th.max(1) as u32))
                .with_visible(false);
            let Ok(window) = event_loop.create_window(attrs) else {
                return;
            };
            let window = Arc::new(window);
            let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
                return;
            };
            let (_, accent, _) = current_theme(&self.app);
            let pal = accent.palette();
            let clear = wgpu::Color {
                r: pal.floating_bg.r() as f64 / 255.0,
                g: pal.floating_bg.g() as f64 / 255.0,
                b: pal.floating_bg.b() as f64 / 255.0,
                a: 1.0,
            };
            let Ok(tb_win) = EguiWindow::new(gpu, window.clone(), clear) else {
                return;
            };
            ui::apply_theme(&tb_win.egui_ctx, true, accent, ui::TextColorSetting::Auto);
            if let Some(h) = tb_win.hwnd() {
                native::set_window_tool_window(h);
                native::set_window_round_corners(h);
            }
            let tb_id = tb_win.window.id();
            self.win_to_pin
                .insert(tb_id, (pin_id.to_string(), PinWindowRole::Toolbar));
            if let Some(entry) = self.pins.get_mut(pin_id) {
                entry.toolbar_win = Some(tb_win);
            }
        }

        let Some(entry) = self.pins.get_mut(pin_id) else {
            return;
        };
        let pin_hwnd = entry.image_win.hwnd();
        let Some(tb_win) = entry.toolbar_win.as_mut() else {
            return;
        };

        if !entry.toolbar_visible {
            if let Some(h) = tb_win.hwnd() {
                native::set_window_pos_physical_hidden(h, tx, ty, tw, th, true);
            }
            if let Some(gpu) = self.gpu.as_ref() {
                tb_win.resize(gpu, tw.max(1) as u32, th.max(1) as u32);
                let app_ref = self.app.clone();
                let mut fb = PinUiFeedback::default();
                let _ = tb_win.render(gpu, |ctx| {
                    fb = entry.state.show_toolbar(ctx, &app_ref, pin_hwnd, picker_above);
                });
                // Adjust immediately if first measured bar width/height differs from estimate.
                if let Some(sz) = fb.toolbar_bar_size {
                    if (sz.x - entry.toolbar_bar_size_logical.x).abs() > 2.0
                        || (sz.y - entry.toolbar_bar_size_logical.y).abs() > 2.0
                    {
                        entry.toolbar_bar_size_logical = sz;
                        let nw = (sz.x as f64 * scale).round() as i32;
                        let nh = (sz.y as f64 * scale).round() as i32;
                        let (nx, ny, ntw, nth, npa) = ui::pin_window::compute_toolbar_rect(
                            pin_rect,
                            (nw, nh),
                            picker_extra_h,
                            gap,
                            work_area,
                        );
                        entry.toolbar_picker_above = npa;
                        if let Some(h) = tb_win.hwnd() {
                            native::set_window_pos_physical_hidden(h, nx, ny, ntw, nth, true);
                        }
                        tb_win.resize(gpu, ntw.max(1) as u32, nth.max(1) as u32);
                        let _ = tb_win.render(gpu, |ctx| {
                            let _ = entry.state.show_toolbar(ctx, &app_ref, pin_hwnd, npa);
                        });
                        if let Some(h) = tb_win.hwnd() {
                            native::set_window_pos_physical(h, nx, ny, ntw, nth, true, true);
                        } else {
                            tb_win.window.set_visible(true);
                        }
                        entry.toolbar_visible = true;
                        tb_win.window.request_redraw();
                        return;
                    }
                }
            }
            if let Some(h) = tb_win.hwnd() {
                native::set_window_pos_physical(h, tx, ty, tw, th, true, true);
            } else {
                tb_win.window.set_visible(true);
            }
            entry.toolbar_visible = true;
            tb_win.window.request_redraw();
        } else {
            if let Some(h) = tb_win.hwnd() {
                native::set_window_pos_physical(h, tx, ty, tw, th, true, true);
            }
            if let Some(gpu) = self.gpu.as_ref() {
                tb_win.resize(gpu, tw.max(1) as u32, th.max(1) as u32);
            }
            tb_win.window.request_redraw();
        }
    }

    fn hide_pin_toolbar(&mut self, pin_id: &str) {
        if let Some(entry) = self.pins.get_mut(pin_id) {
            entry.toolbar_visible = false;
            entry.toolbar_hide_deadline = None;
            entry.state.close_lang_picker();
            if let Some(tb) = &entry.toolbar_win {
                tb.window.set_visible(false);
            }
        }
    }

    fn sync_pin_text_panel(&mut self, event_loop: &ActiveEventLoop, pin_id: &str) {
        let pins_hidden = self.app.core.lock().unwrap().pins_hidden;
        let Some(entry) = self.pins.get_mut(pin_id) else {
            return;
        };
        let want_show = entry.state.wants_text_panel() && !pins_hidden;
        if !want_show {
            entry.text_panel_visible = false;
            if let Some(tp) = &entry.text_panel_win {
                tp.window.set_visible(false);
            }
            return;
        }

        let pin_rect = Self::pin_physical_rect(entry);
        let scale = entry.image_win.window.scale_factor().max(0.5);
        let gap = (COMPANION_GAP_LOGICAL as f64 * scale).round() as i32;
        let desired_w = (TEXT_PANEL_WIDTH_LOGICAL as f64 * scale).round() as i32;
        let min_w = (TEXT_PANEL_MIN_WIDTH_LOGICAL as f64 * scale).round() as i32;
        let min_h = (TEXT_PANEL_MIN_HEIGHT_LOGICAL as f64 * scale).round() as i32;
        let work_area =
            native::work_area_at_point(pin_rect.0 + pin_rect.2 / 2, pin_rect.1 + pin_rect.3 / 2);
        let (px, py, pw, ph) = ui::pin_window::compute_text_panel_rect(
            pin_rect,
            desired_w,
            min_w,
            min_h,
            gap,
            work_area,
            entry.state.text_panel_offset,
        );

        let created_now = entry.text_panel_win.is_none();
        if created_now {
            let attrs = WindowAttributes::default()
                .with_title(i18n::t("SnapDock · Plain Text"))
                .with_decorations(false)
                .with_resizable(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_position(PhysicalPosition::new(px, py))
                .with_inner_size(PhysicalSize::new(pw.max(1) as u32, ph.max(1) as u32))
                .with_visible(false);
            let Ok(window) = event_loop.create_window(attrs) else {
                return;
            };
            let window = Arc::new(window);
            let clear = self.panel_clear_color();
            let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
                return;
            };
            let Ok(tp_win) = EguiWindow::new(gpu, window.clone(), clear) else {
                return;
            };
            let (dark, accent, text_color) = current_theme(&self.app);
            ui::apply_theme(&tp_win.egui_ctx, dark, accent, text_color);
            if let Some(h) = tp_win.hwnd() {
                native::set_window_tool_window(h);
                native::set_window_round_corners(h);
            }
            let tp_id = tp_win.window.id();
            self.win_to_pin
                .insert(tp_id, (pin_id.to_string(), PinWindowRole::TextPanel));
            if let Some(entry) = self.pins.get_mut(pin_id) {
                entry.text_panel_win = Some(tp_win);
            }
        }

        let Some(entry) = self.pins.get_mut(pin_id) else {
            return;
        };
        let Some(tp_win) = entry.text_panel_win.as_mut() else {
            return;
        };
        if !entry.text_panel_visible {
            if let Some(h) = tp_win.hwnd() {
                native::set_window_pos_physical_hidden(h, px, py, pw, ph, true);
            }
            if let Some(gpu) = self.gpu.as_ref() {
                tp_win.resize(gpu, pw.max(1) as u32, ph.max(1) as u32);
                let app_ref = self.app.clone();
                let _ = tp_win.render(gpu, |ctx| {
                    let _ = entry.state.show_text_panel(ctx, &app_ref);
                });
            }
            if let Some(h) = tp_win.hwnd() {
                native::set_window_pos_physical(h, px, py, pw, ph, true, true);
            } else {
                tp_win.window.set_visible(true);
            }
            entry.text_panel_visible = true;
            tp_win.window.request_redraw();
        } else {
            if let Some(h) = tp_win.hwnd() {
                native::set_window_pos_physical(h, px, py, pw, ph, true, true);
            }
            if let Some(gpu) = self.gpu.as_ref() {
                tp_win.resize(gpu, pw.max(1) as u32, ph.max(1) as u32);
            }
            tp_win.window.request_redraw();
        }
    }

    fn reposition_pin_companions(&mut self, pin_id: &str) {
        let Some(entry) = self.pins.get_mut(pin_id) else {
            return;
        };
        let pin_rect = Self::pin_physical_rect(entry);
        let scale = entry.image_win.window.scale_factor().max(0.5);
        let gap = (COMPANION_GAP_LOGICAL as f64 * scale).round() as i32;
        let work_area =
            native::work_area_at_point(pin_rect.0 + pin_rect.2 / 2, pin_rect.1 + pin_rect.3 / 2);

        if entry.toolbar_visible {
            let bar_w = (entry.toolbar_bar_size_logical.x as f64 * scale).round() as i32;
            let bar_h = (entry.toolbar_bar_size_logical.y as f64 * scale).round() as i32;
            let picker_extra_h = if entry.state.lang_picker_open() {
                (LANG_PICKER_HEIGHT_LOGICAL as f64 * scale).round() as i32
            } else {
                0
            };
            let (tx, ty, tw, th, picker_above) = ui::pin_window::compute_toolbar_rect(
                pin_rect,
                (bar_w, bar_h),
                picker_extra_h,
                gap,
                work_area,
            );
            entry.toolbar_picker_above = picker_above;
            if let Some(tb) = entry.toolbar_win.as_mut() {
                if let Some(h) = tb.hwnd() {
                    native::set_window_pos_physical(h, tx, ty, tw, th, true, true);
                }
                if let Some(gpu) = self.gpu.as_ref() {
                    tb.resize(gpu, tw.max(1) as u32, th.max(1) as u32);
                }
                tb.window.request_redraw();
            }
        }

        if entry.text_panel_visible {
            let desired_w = (TEXT_PANEL_WIDTH_LOGICAL as f64 * scale).round() as i32;
            let min_w = (TEXT_PANEL_MIN_WIDTH_LOGICAL as f64 * scale).round() as i32;
            let min_h = (TEXT_PANEL_MIN_HEIGHT_LOGICAL as f64 * scale).round() as i32;
            let (px, py, pw, ph) = ui::pin_window::compute_text_panel_rect(
                pin_rect,
                desired_w,
                min_w,
                min_h,
                gap,
                work_area,
                entry.state.text_panel_offset,
            );
            if let Some(tp) = entry.text_panel_win.as_mut() {
                if let Some(h) = tp.hwnd() {
                    native::set_window_pos_physical(h, px, py, pw, ph, true, true);
                }
                if let Some(gpu) = self.gpu.as_ref() {
                    tp.resize(gpu, pw.max(1) as u32, ph.max(1) as u32);
                }
                tp.window.request_redraw();
            }
        }
    }

    fn apply_pin_feedback(
        &mut self,
        event_loop: &ActiveEventLoop,
        pin_id: &str,
        fb: PinUiFeedback,
    ) {
        let mut need_reposition = fb.geometry_changed;
        if let Some(entry) = self.pins.get_mut(pin_id) {
            if let Some(sz) = fb.toolbar_bar_size {
                if (sz.x - entry.toolbar_bar_size_logical.x).abs() > 2.0
                    || (sz.y - entry.toolbar_bar_size_logical.y).abs() > 2.0
                {
                    entry.toolbar_bar_size_logical = sz;
                    need_reposition = true;
                }
            }
            if fb.keep_toolbar_open {
                entry.toolbar_hide_deadline = None;
            }
        }

        if fb.hide_toolbar {
            self.hide_pin_toolbar(pin_id);
        }
        if fb.text_panel_visible.is_some() {
            self.sync_pin_text_panel(event_loop, pin_id);
        }
        if need_reposition {
            self.reposition_pin_companions(pin_id);
        }

        if let Some(entry) = self.pins.get(pin_id) {
            if fb.redraw_image {
                entry.image_win.window.request_redraw();
            }
            if fb.redraw_toolbar {
                if let Some(tb) = &entry.toolbar_win {
                    tb.window.request_redraw();
                }
            }
            if fb.redraw_text_panel {
                if let Some(tp) = &entry.text_panel_win {
                    tp.window.request_redraw();
                }
            }
        }
    }

    fn close_pin(&mut self, pin_id: &str) {
        if let Some(entry) = self.pins.remove(pin_id) {
            let img_id = entry.image_win.window.id();
            self.win_to_pin.remove(&img_id);
            self.next_repaints.remove(&img_id);
            if let Some(tb) = &entry.toolbar_win {
                let tb_id = tb.window.id();
                self.win_to_pin.remove(&tb_id);
                self.next_repaints.remove(&tb_id);
            }
            if let Some(tp) = &entry.text_panel_win {
                let tp_id = tp.window.id();
                self.win_to_pin.remove(&tp_id);
                self.next_repaints.remove(&tp_id);
            }
        }
        let capture_id = self
            .app
            .core
            .lock()
            .unwrap()
            .pins
            .remove(pin_id)
            .map(|p| p.capture_id);
        if let Some(cid) = capture_id {
            service::release_capture(&self.app, &cid);
        }
    }

    fn toggle_pins(&mut self) {
        let hidden = {
            let mut core = self.app.core.lock().unwrap();
            core.pins_hidden = !core.pins_hidden;
            core.pins_hidden
        };
        for entry in self.pins.values_mut() {
            if hidden {
                entry.image_win.window.set_visible(false);
                entry.toolbar_visible = false;
                entry.toolbar_hide_deadline = None;
                entry.state.close_lang_picker();
                if let Some(tb) = &entry.toolbar_win {
                    tb.window.set_visible(false);
                }
                if let Some(tp) = &entry.text_panel_win {
                    tp.window.set_visible(false);
                }
            } else {
                if let Some(h) = entry.image_win.hwnd() {
                    native::set_window_click_through(h, false);
                }
                entry.state.passthrough = false;
                if let Some(ps) = self
                    .app
                    .core
                    .lock()
                    .unwrap()
                    .pins
                    .get_mut(&entry.state.pin_id)
                {
                    ps.passthrough = false;
                }
                entry.image_win.window.set_visible(true);
                entry.image_win.window.request_redraw();
                if entry.state.wants_text_panel() {
                    if let Some(tp) = &entry.text_panel_win {
                        tp.window.set_visible(true);
                        tp.window.request_redraw();
                        entry.text_panel_visible = true;
                    }
                }
            }
        }
    }

    fn open_recorder(&mut self, event_loop: &ActiveEventLoop) {
        self.recorder = None;
        let (mode, origin, size) = {
            let core = self.app.core.lock().unwrap();
            let Some(rec) = core.recording.as_ref() else {
                return;
            };
            let g = rec.lock().unwrap();
            (g.mode.clone(), g.control_origin, g.control_size)
        };
        let attrs = WindowAttributes::default()
            .with_title(i18n::t("SnapDock · Recorder"))
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(PhysicalPosition::new(origin.0, origin.1))
            .with_inner_size(PhysicalSize::new(size.0 as u32, size.1 as u32))
            .with_visible(false);
        let Ok(window) = event_loop.create_window(attrs) else {
            return;
        };
        let window = Arc::new(window);
        let Ok(gpu) = ensure_gpu(&mut self.gpu, &window) else {
            return;
        };
        let Ok(egui_win) = EguiWindow::new(gpu, window.clone(), wgpu::Color::TRANSPARENT) else {
            return;
        };
        let (_, accent, _) = current_theme(&self.app);
        ui::apply_theme(&egui_win.egui_ctx, true, accent, ui::TextColorSetting::Auto);
        if let Some(hwnd) = egui_win.hwnd() {
            native::set_window_tool_window(hwnd);
            native::set_window_exclude_from_capture(hwnd, true);
            native::set_window_round_corners(hwnd);
            native::set_window_pos_physical(hwnd, origin.0, origin.1, size.0, size.1, true, true);
        }
        window.set_visible(true);
        let id = window.id();
        window.request_redraw();
        self.recorder = Some((id, egui_win, RecorderState::new(mode)));
    }
}

impl ApplicationHandler<AppEvent> for SnapDockRunner {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        if !self.tray_initialized {
            self.init_tray();
            self.tray_initialized = true;
            self.app.emit(AppEvent::WarmupGraphics);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::OpenSettings { section } => {
                self.open_settings(event_loop, section.as_deref());
            }
            AppEvent::RestoreSession => {
                if let Some((_, win, _)) = &self.recorder {
                    win.window.set_visible(true);
                    win.window.focus_window();
                } else if !self.overlays.is_empty() {
                    for (win, _) in self.overlays.values() {
                        win.window.set_visible(true);
                        win.window.focus_window();
                    }
                } else if let Some((_, win, _)) = &self.editor_window {
                    win.window.set_minimized(false);
                    win.window.set_visible(true);
                    if let Some(hwnd) = win.hwnd() {
                        native::activate_window(hwnd);
                    }
                    win.window.focus_window();
                } else {
                    self.open_settings(event_loop, None);
                }
            }
            AppEvent::WarmupGraphics => self.warmup_graphics(event_loop),
            AppEvent::StartCapture { mode, from_tray } => {
                service::start_capture(&self.app, &mode, from_tray);
            }
            AppEvent::OpenOverlays => self.open_overlays(event_loop),
            AppEvent::CancelCapture => self.release_overlays(),
            AppEvent::OpenResult { id } => self.open_editor(event_loop, &id),
            AppEvent::OpenResultOcr { id } => {
                self.open_editor(event_loop, &id);
                if let Some((_, win, state)) = self.editor_window.as_mut() {
                    if state.id == id {
                        state.start_ocr(&self.app);
                        win.window.request_redraw();
                    }
                }
            }
            AppEvent::ResultClosed { id } => {
                if self
                    .editor_window
                    .as_ref()
                    .is_some_and(|(_, _, state)| state.id == id)
                {
                    self.close_editor();
                }
            }
            AppEvent::ImageUpdated { id } => {
                if let Some((_, win, state)) = self.editor_window.as_mut() {
                    if state.id == id {
                        state.refresh_item_metadata(&self.app);
                        win.window.request_redraw();
                    }
                }
                for entry in self.pins.values_mut() {
                    if entry.state.capture_id == id {
                        entry.state.image_dirty = true;
                        entry.image_win.window.request_redraw();
                    }
                }
            }
            AppEvent::SettingsChanged => {
                if let Some((_, win, state)) = self.settings_window.as_mut() {
                    state.reload_from_app(&self.app);
                    win.window.request_redraw();
                }
                self.apply_theme_all();
            }
            AppEvent::AccountChanged => {
                if let Some((_, win, _)) = &self.settings_window {
                    win.window.request_redraw();
                }
            }
            AppEvent::OcrModelsChanged | AppEvent::TranslationPacksChanged => {
                if let Some((_, win, _)) = &self.settings_window {
                    win.window.request_redraw();
                }
            }
            AppEvent::CreatePin { id } => self.create_pin(event_loop, &id),
            AppEvent::ClosePin { pin_id } => self.close_pin(&pin_id),
            AppEvent::TogglePins => self.toggle_pins(),
            AppEvent::OpenRecorder => self.open_recorder(event_loop),
            AppEvent::RecordingProgress(progress) => {
                if let Some((_, win, state)) = self.recorder.as_mut() {
                    state.progress = progress;
                    win.window.request_redraw();
                }
            }
            AppEvent::CloseRecording => {
                if let Some((id, _, _)) = self.recorder.take() {
                    self.next_repaints.remove(&id);
                }
            }
            AppEvent::Toast { message, error } => {
                self.show_toast(event_loop, message, error);
            }
            AppEvent::TextResult {
                target,
                kind,
                result,
            } => match target {
                TextTarget::Editor { capture_id } => {
                    if let Some((_, win, state)) = self.editor_window.as_mut() {
                        if state.id == capture_id {
                            state.apply_text_result(&kind, result);
                            win.window.request_redraw();
                        }
                    }
                }
                TextTarget::Pin { pin_id } => {
                    if let Some(entry) = self.pins.get_mut(&pin_id) {
                        if let Err(e) = result {
                            entry.state.set_message(e);
                        }
                        entry.image_win.window.request_redraw();
                    }
                }
                TextTarget::SettingsTest => match result {
                    Ok(text) => {
                        let preview: String = text.chars().take(120).collect();
                        self.show_toast(
                            event_loop,
                            i18n::tf("Provider test succeeded: {preview}", &[("preview", &preview)]),
                            false,
                        );
                    }
                    Err(e) => self.show_toast(event_loop, e, true),
                },
            },
            AppEvent::PinOverlayResult {
                pin_id,
                mode,
                result,
            } => {
                if let Some(entry) = self.pins.get_mut(&pin_id) {
                    let lang_key = {
                        let core = self.app.core.lock().unwrap();
                        ui::pin_window::current_lang_key(&core.settings)
                    };
                    entry.state.apply_overlay_result(mode, lang_key, result);
                    entry.image_win.window.request_redraw();
                    if let Some(tb) = &entry.toolbar_win {
                        tb.window.request_redraw();
                    }
                    if let Some(tp) = &entry.text_panel_win {
                        tp.window.request_redraw();
                    }
                }
                self.sync_pin_text_panel(event_loop, &pin_id);
            }
            AppEvent::AccountMetaResult(res) => {
                if let Some((_, win, state)) = self.settings_window.as_mut() {
                    state.meta_registration_enabled = res.ok();
                    win.window.request_redraw();
                }
            }
            AppEvent::ReregisterHotkeys => {
                let settings_val = self.app.core.lock().unwrap().settings.clone();
                let warnings = self.hotkeys.register(&settings_val);
                self.app.core.lock().unwrap().hotkey_warnings = warnings.clone();
                if !warnings.is_empty() {
                    self.show_toast(event_loop, warnings.join("; "), true);
                }
            }
            AppEvent::Hotkey(hk_evt) => self.handle_hotkey(hk_evt),
            AppEvent::ImportImage => service::import_image(&self.app),
            AppEvent::Quit => {
                self.app.core.lock().unwrap().quitting = true;
                self.overlays.clear();
                self.overlay_pool.clear();
                self.pins.clear();
                self.win_to_pin.clear();
                self.recorder = None;
                self.editor_window = None;
                self.settings_window = None;
                self.toast_window = None;
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // 1. Standalone settings window.
        if self
            .settings_window
            .as_ref()
            .is_some_and(|(id, _, _)| *id == window_id)
        {
            match &event {
                WindowEvent::CloseRequested => {
                    self.close_settings();
                    return;
                }
                WindowEvent::Resized(size) => {
                    if let (Some(gpu), Some((_, win, _))) =
                        (self.gpu.as_ref(), self.settings_window.as_mut())
                    {
                        win.resize(gpu, size.width, size.height);
                    }
                }
                WindowEvent::RedrawRequested => {
                    let mut close_req = false;
                    let delay = if let (Some(gpu), Some((_, win, state))) =
                        (self.gpu.as_ref(), self.settings_window.as_mut())
                    {
                        let app = self.app.clone();
                        let win_ref = win.window.clone();
                        win.render(gpu, |ctx| {
                            if state.show(ctx, &app, &win_ref) {
                                close_req = true;
                            }
                        })
                    } else {
                        None
                    };
                    if close_req {
                        self.close_settings();
                    } else {
                        self.schedule_repaint(window_id, delay);
                    }
                    return;
                }
                _ => {}
            }
            if let Some((_, win, _)) = self.settings_window.as_mut() {
                let _ = win.handle_event(&event);
                win.window.request_redraw();
            }
            return;
        }

        // 2. Capture editor window.
        if self
            .editor_window
            .as_ref()
            .is_some_and(|(id, _, _)| *id == window_id)
        {
            match &event {
                WindowEvent::CloseRequested => {
                    let id = self
                        .editor_window
                        .as_ref()
                        .map(|(_, _, s)| s.id.clone());
                    self.close_editor();
                    service::close_result(&self.app, id.as_deref());
                    return;
                }
                WindowEvent::Resized(size) => {
                    if let (Some(gpu), Some((_, win, _))) =
                        (self.gpu.as_ref(), self.editor_window.as_mut())
                    {
                        win.resize(gpu, size.width, size.height);
                    }
                }
                WindowEvent::RedrawRequested => {
                    let delay = if let (Some(gpu), Some((_, win, state))) =
                        (self.gpu.as_ref(), self.editor_window.as_mut())
                    {
                        let app = self.app.clone();
                        let win_ref = win.window.clone();
                        win.render(gpu, |ctx| state.show(ctx, &app, &win_ref))
                    } else {
                        None
                    };
                    self.schedule_repaint(window_id, delay);
                    return;
                }
                _ => {}
            }
            if let Some((_, win, _)) = self.editor_window.as_mut() {
                let _ = win.handle_event(&event);
                win.window.request_redraw();
            }
            return;
        }

        // 3. Fullscreen capture overlay window.
        if let Some((egui_win, overlay_state)) = self.overlays.get_mut(&window_id) {
            match &event {
                WindowEvent::CloseRequested => {
                    service::cancel_capture(&self.app);
                    return;
                }
                WindowEvent::Resized(size) => {
                    if let Some(gpu) = self.gpu.as_ref() {
                        egui_win.resize(gpu, size.width, size.height);
                    }
                }
                WindowEvent::RedrawRequested => {
                    let delay = if let Some(gpu) = self.gpu.as_ref() {
                        let app = self.app.clone();
                        let win_ref = egui_win.window.clone();
                        egui_win.render(gpu, |ctx| overlay_state.show(ctx, &app, &win_ref))
                    } else {
                        None
                    };
                    self.schedule_repaint(window_id, delay);
                    return;
                }
                _ => {}
            }
            let _ = egui_win.handle_event(&event);
            egui_win.window.request_redraw();
            return;
        }

        // 4. Desktop pin windows (Image, companion Bottom Toolbar, companion Right Plain-Text Panel).
        if let Some((pin_id, role)) = self.win_to_pin.get(&window_id).cloned() {
            match role {
                PinWindowRole::Image => {
                    match &event {
                        WindowEvent::CloseRequested => {
                            self.close_pin(&pin_id);
                            return;
                        }
                        WindowEvent::Resized(size) => {
                            if let (Some(gpu), Some(entry)) =
                                (self.gpu.as_ref(), self.pins.get_mut(&pin_id))
                            {
                                entry.image_win.resize(gpu, size.width, size.height);
                            }
                            self.reposition_pin_companions(&pin_id);
                        }
                        WindowEvent::Moved(_) => {
                            self.reposition_pin_companions(&pin_id);
                        }
                        WindowEvent::CursorEntered { .. } | WindowEvent::CursorMoved { .. } => {
                            let need_show = self
                                .pins
                                .get(&pin_id)
                                .is_some_and(|e| !e.state.passthrough && !e.toolbar_visible);
                            if need_show {
                                self.show_pin_toolbar(event_loop, &pin_id);
                            } else if let Some(entry) = self.pins.get_mut(&pin_id) {
                                entry.toolbar_hide_deadline = None;
                            }
                        }
                        WindowEvent::CursorLeft { .. } => {
                            if let Some(entry) = self.pins.get_mut(&pin_id) {
                                if entry.toolbar_visible
                                    && !entry.state.lang_picker_open()
                                    && entry.state.drag_origin.is_none()
                                {
                                    entry.toolbar_hide_deadline = Some(
                                        std::time::Instant::now()
                                            + std::time::Duration::from_millis(260),
                                    );
                                }
                            }
                        }
                        WindowEvent::RedrawRequested => {
                            let mut fb = PinUiFeedback::default();
                            let delay = if let (Some(gpu), Some(entry)) =
                                (self.gpu.as_ref(), self.pins.get_mut(&pin_id))
                            {
                                let app = self.app.clone();
                                let win_ref = entry.image_win.window.clone();
                                let hwnd = entry.image_win.hwnd();
                                entry.image_win.render(gpu, |ctx| {
                                    fb = entry.state.show_image(ctx, &app, &win_ref, hwnd);
                                })
                            } else {
                                None
                            };
                            self.apply_pin_feedback(event_loop, &pin_id, fb);
                            self.schedule_repaint(window_id, delay);
                            return;
                        }
                        _ => {}
                    }
                    if let Some(entry) = self.pins.get_mut(&pin_id) {
                        let _ = entry.image_win.handle_event(&event);
                        entry.image_win.window.request_redraw();
                    }
                    return;
                }
                PinWindowRole::Toolbar => {
                    match &event {
                        WindowEvent::CloseRequested => {
                            self.hide_pin_toolbar(&pin_id);
                            return;
                        }
                        WindowEvent::Resized(size) => {
                            if let (Some(gpu), Some(entry)) =
                                (self.gpu.as_ref(), self.pins.get_mut(&pin_id))
                            {
                                if let Some(tb) = entry.toolbar_win.as_mut() {
                                    tb.resize(gpu, size.width, size.height);
                                }
                            }
                        }
                        WindowEvent::CursorEntered { .. } | WindowEvent::CursorMoved { .. } => {
                            if let Some(entry) = self.pins.get_mut(&pin_id) {
                                entry.toolbar_hide_deadline = None;
                            }
                        }
                        WindowEvent::CursorLeft { .. } => {
                            if let Some(entry) = self.pins.get_mut(&pin_id) {
                                if entry.toolbar_visible && !entry.state.lang_picker_open() {
                                    entry.toolbar_hide_deadline = Some(
                                        std::time::Instant::now()
                                            + std::time::Duration::from_millis(260),
                                    );
                                }
                            }
                        }
                        WindowEvent::Focused(false) => {
                            let closed_picker = self
                                .pins
                                .get_mut(&pin_id)
                                .is_some_and(|e| e.state.close_lang_picker());
                            if closed_picker {
                                self.reposition_pin_companions(&pin_id);
                                if let Some(entry) = self.pins.get_mut(&pin_id) {
                                    entry.toolbar_hide_deadline = Some(
                                        std::time::Instant::now()
                                            + std::time::Duration::from_millis(260),
                                    );
                                }
                            }
                        }
                        WindowEvent::RedrawRequested => {
                            let mut fb = PinUiFeedback::default();
                            let delay = if let (Some(gpu), Some(entry)) =
                                (self.gpu.as_ref(), self.pins.get_mut(&pin_id))
                            {
                                let app = self.app.clone();
                                let pin_hwnd = entry.image_win.hwnd();
                                let picker_above = entry.toolbar_picker_above;
                                if let Some(tb) = entry.toolbar_win.as_mut() {
                                    tb.render(gpu, |ctx| {
                                        fb = entry
                                            .state
                                            .show_toolbar(ctx, &app, pin_hwnd, picker_above);
                                    })
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            self.apply_pin_feedback(event_loop, &pin_id, fb);
                            self.schedule_repaint(window_id, delay);
                            return;
                        }
                        _ => {}
                    }
                    if let Some(entry) = self.pins.get_mut(&pin_id) {
                        if let Some(tb) = entry.toolbar_win.as_mut() {
                            let _ = tb.handle_event(&event);
                            tb.window.request_redraw();
                        }
                    }
                    return;
                }
                PinWindowRole::TextPanel => {
                    match &event {
                        WindowEvent::CloseRequested => {
                            if let Some(entry) = self.pins.get_mut(&pin_id) {
                                if let Some(ov) = entry.state.text_overlay.as_mut() {
                                    ov.show_text_panel = false;
                                }
                                if let Some(tb) = &entry.toolbar_win {
                                    tb.window.request_redraw();
                                }
                            }
                            self.sync_pin_text_panel(event_loop, &pin_id);
                            return;
                        }
                        WindowEvent::Resized(size) => {
                            if let (Some(gpu), Some(entry)) =
                                (self.gpu.as_ref(), self.pins.get_mut(&pin_id))
                            {
                                if let Some(tp) = entry.text_panel_win.as_mut() {
                                    tp.resize(gpu, size.width, size.height);
                                }
                            }
                        }
                        WindowEvent::RedrawRequested => {
                            let mut fb = PinUiFeedback::default();
                            let delay = if let (Some(gpu), Some(entry)) =
                                (self.gpu.as_ref(), self.pins.get_mut(&pin_id))
                            {
                                let app = self.app.clone();
                                if let Some(tp) = entry.text_panel_win.as_mut() {
                                    tp.render(gpu, |ctx| {
                                        fb = entry.state.show_text_panel(ctx, &app);
                                    })
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                            self.apply_pin_feedback(event_loop, &pin_id, fb);
                            self.schedule_repaint(window_id, delay);
                            return;
                        }
                        _ => {}
                    }
                    if let Some(entry) = self.pins.get_mut(&pin_id) {
                        if let Some(tp) = entry.text_panel_win.as_mut() {
                            let _ = tp.handle_event(&event);
                            tp.window.request_redraw();
                        }
                    }
                    return;
                }
            }
        }

        // 5. Recording control bar window.
        if self
            .recorder
            .as_ref()
            .is_some_and(|(id, _, _)| *id == window_id)
        {
            match &event {
                WindowEvent::CloseRequested => {
                    recording::abort(&self.app);
                    return;
                }
                WindowEvent::Resized(size) => {
                    if let (Some(gpu), Some((_, win, _))) =
                        (self.gpu.as_ref(), self.recorder.as_mut())
                    {
                        win.resize(gpu, size.width, size.height);
                    }
                }
                WindowEvent::RedrawRequested => {
                    let delay = if let (Some(gpu), Some((_, win, state))) =
                        (self.gpu.as_ref(), self.recorder.as_mut())
                    {
                        let app = self.app.clone();
                        let win_ref = win.window.clone();
                        let hwnd = win.hwnd();
                        win.render(gpu, |ctx| state.show(ctx, &app, &win_ref, hwnd))
                    } else {
                        None
                    };
                    self.schedule_repaint(window_id, delay);
                    return;
                }
                _ => {}
            }
            if let Some((_, win, _)) = self.recorder.as_mut() {
                let _ = win.handle_event(&event);
                win.window.request_redraw();
            }
            return;
        }

        // 6. Floating toast window.
        if self
            .toast_window
            .as_ref()
            .is_some_and(|(id, _, _)| *id == window_id)
        {
            match &event {
                WindowEvent::CloseRequested => {
                    self.close_toast();
                    return;
                }
                WindowEvent::Resized(size) => {
                    if let (Some(gpu), Some((_, win, _))) =
                        (self.gpu.as_ref(), self.toast_window.as_mut())
                    {
                        win.resize(gpu, size.width, size.height);
                    }
                }
                WindowEvent::RedrawRequested => {
                    let mut finished = false;
                    let delay = if let (Some(gpu), Some((_, win, state))) =
                        (self.gpu.as_ref(), self.toast_window.as_mut())
                    {
                        let d = win.render(gpu, |ctx| {
                            let _ = state.show(ctx);
                        });
                        finished = state.finished();
                        d
                    } else {
                        None
                    };
                    if finished {
                        self.close_toast();
                    } else {
                        self.schedule_repaint(window_id, delay);
                    }
                    return;
                }
                _ => {}
            }
            if let Some((_, win, _)) = self.toast_window.as_mut() {
                let _ = win.handle_event(&event);
                win.window.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self
            .toast_window
            .as_ref()
            .is_some_and(|(_, _, state)| state.finished())
        {
            self.close_toast();
        }

        let now = std::time::Instant::now();

        // Check expired pin hover-toolbar grace timers (with bridge-gap check so crossing from pin to toolbar stays open).
        let cursor = native::cursor_pos();
        let mut to_hide_toolbars: Vec<String> = Vec::new();
        for (pin_id, entry) in self.pins.iter_mut() {
            if !entry.toolbar_visible {
                entry.toolbar_hide_deadline = None;
                continue;
            }
            let Some(deadline) = entry.toolbar_hide_deadline else {
                continue;
            };
            if now < deadline {
                continue;
            }
            if entry.state.lang_picker_open() || entry.state.drag_origin.is_some() {
                entry.toolbar_hide_deadline = None;
                continue;
            }
            let pr = Self::pin_physical_rect(entry);
            let tr = entry
                .toolbar_win
                .as_ref()
                .and_then(|tb| tb.hwnd().and_then(native::window_rect_physical));
            let in_rect = |rc: (i32, i32, i32, i32), pad: i32| -> bool {
                cursor.0 >= rc.0 - pad
                    && cursor.0 <= rc.0 + rc.2 + pad
                    && cursor.1 >= rc.1 - pad
                    && cursor.1 <= rc.1 + rc.3 + pad
            };
            let in_pin = in_rect(pr, 2);
            let in_tb = tr.is_some_and(|rc| in_rect(rc, 2));
            let in_bridge = tr.is_some_and(|rc| {
                let bx0 = pr.0.min(rc.0) - 4;
                let bx1 = (pr.0 + pr.2).max(rc.0 + rc.2) + 4;
                let by0 = (pr.1 + pr.3).min(rc.1 + rc.3) - 2;
                let by1 = pr.1.max(rc.1) + 2;
                cursor.0 >= bx0 && cursor.0 <= bx1 && cursor.1 >= by0 && cursor.1 <= by1
            });
            if in_pin || in_tb {
                entry.toolbar_hide_deadline = None;
            } else if in_bridge {
                entry.toolbar_hide_deadline = Some(now + std::time::Duration::from_millis(140));
            } else {
                to_hide_toolbars.push(pin_id.clone());
            }
        }
        for pin_id in to_hide_toolbars {
            self.hide_pin_toolbar(&pin_id);
        }

        let due: Vec<WindowId> = self
            .next_repaints
            .iter()
            .filter_map(|(id, &when)| if when <= now { Some(*id) } else { None })
            .collect();
        for id in due {
            self.next_repaints.remove(&id);
            if let Some((sid, win, _)) = &self.settings_window {
                if *sid == id {
                    win.window.request_redraw();
                    continue;
                }
            }
            if let Some((eid, win, _)) = &self.editor_window {
                if *eid == id {
                    win.window.request_redraw();
                    continue;
                }
            }
            if let Some((win, _)) = self.overlays.get(&id) {
                win.window.request_redraw();
                continue;
            }
            if let Some((pin_id, role)) = self.win_to_pin.get(&id) {
                if let Some(entry) = self.pins.get(pin_id) {
                    match role {
                        PinWindowRole::Image => entry.image_win.window.request_redraw(),
                        PinWindowRole::Toolbar => {
                            if let Some(tb) = &entry.toolbar_win {
                                tb.window.request_redraw();
                            }
                        }
                        PinWindowRole::TextPanel => {
                            if let Some(tp) = &entry.text_panel_win {
                                tp.window.request_redraw();
                            }
                        }
                    }
                }
                continue;
            }
            if let Some((rid, win, _)) = &self.recorder {
                if *rid == id {
                    win.window.request_redraw();
                    continue;
                }
            }
            if let Some((tid, win, _)) = &self.toast_window {
                if *tid == id {
                    win.window.request_redraw();
                }
            }
        }
        let earliest_repaint = self.next_repaints.values().copied().min();
        let earliest_tb_hide = self
            .pins
            .values()
            .filter_map(|e| e.toolbar_hide_deadline)
            .min();
        let earliest = match (earliest_repaint, earliest_tb_hide) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        if let Some(earliest) = earliest {
            event_loop.set_control_flow(ControlFlow::WaitUntil(earliest));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn main() {
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("snapdock");
    let app = Arc::new(App::new(data_dir));

    // Single-instance guard: wake the running instance and exit if already active.
    let wake_app = app.clone();
    let Some(_single_instance) = native::SingleInstanceGuard::acquire(move || {
        wake_app.emit(AppEvent::RestoreSession);
    }) else {
        return;
    };

    // Preload system CJK font bytes on a background thread while finishing startup.
    std::thread::spawn(ui::preload_fonts);

    let _ = std::fs::create_dir_all(&app.capture_dir);
    let _ = std::fs::create_dir_all(&app.record_dir);

    if let Err(error) = vault::read_vault(&app) {
        rfd::MessageDialog::new()
            .set_title(i18n::t("SnapDock · Failed to Read Settings"))
            .set_description(i18n::tf(
                "Failed to read local settings: {err}. The original encrypted configuration was not overwritten.",
                &[("err", &error)],
            ))
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
        std::process::exit(1);
    }
    vault::read_history(&app);
    service::start_auto_sync_loop(&app);

    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .expect("failed to create event loop");
    let proxy = event_loop.create_proxy();
    app.set_proxy(proxy.clone());

    // Forward global hotkey events directly into the winit event loop (zero polling).
    let hk_proxy = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        let _ = hk_proxy.send_event(AppEvent::Hotkey(event));
    }));

    // Forward tray menu and double-click events into the winit event loop.
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.0.as_str() {
        "capture" => {
            let _ = menu_proxy.send_event(AppEvent::StartCapture {
                mode: "region".into(),
                from_tray: true,
            });
        }
        "long" => {
            let _ = menu_proxy.send_event(AppEvent::StartCapture {
                mode: "long".into(),
                from_tray: true,
            });
        }
        "gif" => {
            let _ = menu_proxy.send_event(AppEvent::StartCapture {
                mode: "gif".into(),
                from_tray: true,
            });
        }
        "pin" => {
            let _ = menu_proxy.send_event(AppEvent::StartCapture {
                mode: "clipboard".into(),
                from_tray: true,
            });
        }
        "toggle" => {
            let _ = menu_proxy.send_event(AppEvent::TogglePins);
        }
        "import" => {
            let _ = menu_proxy.send_event(AppEvent::ImportImage);
        }
        "settings" => {
            let _ = menu_proxy.send_event(AppEvent::OpenSettings { section: None });
        }
        "quit" => {
            let _ = menu_proxy.send_event(AppEvent::Quit);
        }
        _ => {}
    }));

    let tray_proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if matches!(
            event,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        ) {
            let _ = tray_proxy.send_event(AppEvent::RestoreSession);
        }
    }));

    let mut runner = SnapDockRunner::new(app);
    let _ = event_loop.run_app(&mut runner);
}
