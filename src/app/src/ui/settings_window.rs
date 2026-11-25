//! Standalone settings window opened from the tray menu or global hotkey.
//!
//! Two-pane layout: left navigation sidebar with 7 sections and version badge;
//! right pane with section controls and top save/revert action bar.

use crate::i18n;
use crate::network;
use crate::service;
use crate::state::{account_view, App, TextTarget};
use crate::ui;
use crate::ui::icons::{self, Icon};
use crate::ui::toast::{self, Toast};
use egui::{Color32, CornerRadius, RichText};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Capture,
    Pin,
    Shortcuts,
    Storage,
    Providers,
    Account,
}

impl SettingsSection {
    pub fn all() -> &'static [(SettingsSection, &'static str)] {
        &[
            (SettingsSection::General, "General"),
            (SettingsSection::Capture, "Capture"),
            (SettingsSection::Pin, "Pin"),
            (SettingsSection::Shortcuts, "Shortcuts"),
            (SettingsSection::Storage, "Storage"),
            (SettingsSection::Providers, "Text & Translation"),
            (SettingsSection::Account, "Account & Sync"),
        ]
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "general" => Some(Self::General),
            "capture" => Some(Self::Capture),
            "pin" => Some(Self::Pin),
            "shortcuts" => Some(Self::Shortcuts),
            "storage" => Some(Self::Storage),
            "providers" => Some(Self::Providers),
            "account" => Some(Self::Account),
            _ => None,
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::General => Icon::SettingsGeneral,
            Self::Capture => Icon::Brand,
            Self::Pin => Icon::Pin,
            Self::Shortcuts => Icon::Keyboard,
            Self::Storage => Icon::Folder,
            Self::Providers => Icon::Translate,
            Self::Account => Icon::Cloud,
        }
    }

    pub fn label(self) -> &'static str {
        let key = Self::all()
            .iter()
            .find(|(s, _)| *s == self)
            .map(|(_, l)| *l)
            .unwrap_or("Settings");
        i18n::t(key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OcrModelFilter {
    All,
    Installed,
    NotInstalled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PackFilter {
    All,
    Multilingual,
    Installed,
    NotInstalled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairDirectionFilter {
    All,
    FromFocus,
    ToFocus,
}

pub struct SettingsWindowState {
    pub section: SettingsSection,
    pub draft: Value,
    pub password: String,
    pub show_ocr_key: bool,
    pub show_trans_key: bool,
    pub provider_tab: usize, // 0 = ocr, 1 = translation
    pub meta_registration_enabled: Option<bool>,
    pub toast: Option<Toast>,
    applied_preview: Option<(bool, ui::ThemeAccent, ui::TextColorSetting)>,
    applied_lang_preview: Option<i18n::EffectiveLanguage>,
    src_lang_query: String,
    tgt_lang_query: String,
    pack_search: String,
    pack_filter: PackFilter,
    ocr_filter: OcrModelFilter,
    pair_direction: PairDirectionFilter,
}

impl SettingsWindowState {
    pub fn new(app: &App, section: Option<&str>) -> Self {
        let settings = app.core.lock().unwrap().settings.clone();
        Self {
            section: section
                .and_then(SettingsSection::from_key)
                .unwrap_or(SettingsSection::General),
            draft: settings,
            password: String::new(),
            show_ocr_key: false,
            show_trans_key: false,
            provider_tab: 0,
            meta_registration_enabled: None,
            toast: None,
            applied_preview: None,
            applied_lang_preview: None,
            src_lang_query: String::new(),
            tgt_lang_query: String::new(),
            pack_search: String::new(),
            pack_filter: PackFilter::All,
            ocr_filter: OcrModelFilter::All,
            pair_direction: PairDirectionFilter::All,
        }
    }

    fn sync_preview_theme(&mut self, ctx: &egui::Context, window: &winit::window::Window) {
        let target = (
            ui::is_dark_theme(&self.draft),
            ui::theme_accent(&self.draft),
            ui::theme_text_color(&self.draft),
        );
        if self.applied_preview != Some(target) {
            ui::apply_theme(ctx, target.0, target.1, target.2);
            self.applied_preview = Some(target);
            ctx.request_repaint();
        }
        let eff_lang = i18n::apply_settings(&self.draft);
        if self.applied_lang_preview != Some(eff_lang) {
            self.applied_lang_preview = Some(eff_lang);
            window.set_title(i18n::t("SnapDock · Settings"));
            ctx.request_repaint();
        }
    }

    pub fn reload_from_app(&mut self, app: &App) {
        self.draft = app.core.lock().unwrap().settings.clone();
    }

    pub fn notify(&mut self, message: impl Into<String>, error: bool) {
        self.toast = Some(Toast::new(message, error));
    }

    fn save_draft(&mut self, app: &Arc<App>) {
        match service::update_settings(app, &self.draft) {
            Ok(()) => {
                self.reload_from_app(app);
                self.notify(i18n::t("Settings saved"), false);
            }
            Err(e) => self.notify(e, true),
        }
    }

    /// Render the settings window; returns `true` to request closing the window (e.g. Esc).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        app: &Arc<App>,
        window: &winit::window::Window,
    ) -> bool {
        self.sync_preview_theme(ctx, window);
        let palette = ui::ctx_palette(ctx);
        let accent_text = palette.accent_text(ctx.style().visuals.dark_mode);

        let (saved_settings, account, hotkey_warnings) = {
            let core = app.core.lock().unwrap();
            (
                core.settings.clone(),
                account_view(&core),
                core.hotkey_warnings.clone(),
            )
        };
        let changed = self.draft != saved_settings;
        let packaged = app.packaged;

        // Window shortcuts: Ctrl+S to save, Esc to close.
        let typing = ctx.wants_keyboard_input();
        let (ctrl_s, esc) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if ctrl_s && changed {
            self.save_draft(app);
        }
        if esc && !typing {
            return true;
        }

        // Left navigation sidebar.
        egui::SidePanel::left("settings_nav")
            .resizable(false)
            .exact_width(168.0)
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(12, 14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    icons::paint_inline(ui, Icon::Brand, 18.0, accent_text);
                    ui.label(
                        RichText::new(i18n::t("SnapDock"))
                            .size(15.5)
                            .strong()
                            .color(accent_text),
                    );
                });
                ui.add_space(12.0);
                for &(sec, label_key) in SettingsSection::all() {
                    let active = self.section == sec;
                    if icons::icon_nav_button(ui, sec.icon(), i18n::t(label_key), active, 144.0)
                        .clicked()
                    {
                        self.section = sec;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                            .weak()
                            .small(),
                    );
                });
            });

        // Right main content area.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ctx, |ui| {
                // Header title and save/revert controls.
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.section.label()).size(17.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(changed, |ui| {
                            if icons::icon_text_button(
                                ui,
                                Icon::Save,
                                i18n::t("Save"),
                                false,
                                changed,
                                None,
                            )
                            .on_hover_text(i18n::t("Save settings Ctrl+S"))
                            .clicked()
                            {
                                self.save_draft(app);
                            }
                        });
                        if changed {
                            if icons::icon_text_button(
                                ui,
                                Icon::Undo,
                                i18n::t("Revert"),
                                false,
                                false,
                                None,
                            )
                            .clicked()
                            {
                                self.draft = saved_settings.clone();
                            }
                            ui.colored_label(
                                Color32::from_rgb(0xd9, 0x82, 0x2b),
                                i18n::t("Unsaved"),
                            );
                        }
                    });
                });
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(10.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.section {
                        SettingsSection::General => self.show_general(ui, packaged),
                        SettingsSection::Capture => self.show_capture(ui),
                        SettingsSection::Pin => self.show_pin(ui),
                        SettingsSection::Shortcuts => self.show_shortcuts(ui, &hotkey_warnings),
                        SettingsSection::Storage => self.show_storage(ui, app, window),
                        SettingsSection::Providers => self.show_providers(ui, app, window),
                        SettingsSection::Account => self.show_account(ui, app, window, account),
                    });
            });

        self.sync_preview_theme(ctx, window);
        toast::show_in_window(ctx, &mut self.toast, 16.0);
        false
    }

    fn show_general(&mut self, ui: &mut egui::Ui, packaged: bool) {
        section_card(ui, i18n::t("Language"), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(i18n::t("Interface Language")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cur = i18n::AppLanguage::from_setting(&get_str(
                        &self.draft,
                        "general",
                        "language",
                    ))
                    .as_key();
                    // Added in reverse order because of right-to-left layout -> displays:
                    // Follow System | English | <Simplified Chinese native name>
                    for (val, lbl) in [
                        ("zh-CN", i18n::zh_cn_native_name()),
                        ("en", "English"),
                        ("system", i18n::t("Follow System")),
                    ] {
                        if ui.selectable_label(cur == val, lbl).clicked() {
                            set_str(&mut self.draft, "general", "language", val.to_string());
                        }
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(i18n::t(
                    "Default is English. Switch language for instant preview, then click Save to apply to all windows and tray menu.",
                ))
                .weak()
                .small(),
            );
        });

        section_card(ui, i18n::t("Appearance"), |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(i18n::t("Theme Mode")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cur = get_str(&self.draft, "general", "theme");
                    for (val, lbl) in [
                        ("system", i18n::t("Follow System")),
                        ("dark", i18n::t("Dark")),
                        ("light", i18n::t("Light")),
                    ] {
                        if ui.selectable_label(cur == val, lbl).clicked() {
                            set_str(&mut self.draft, "general", "theme", val.to_string());
                        }
                    }
                });
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            let cur_accent = ui::theme_accent(&self.draft);
            let dark_mode = ui.visuals().dark_mode;
            ui.horizontal(|ui| {
                ui.label(RichText::new(i18n::t("Accent Color")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cur_p = cur_accent.palette();
                    ui.label(
                        RichText::new(cur_accent.label())
                            .color(cur_p.accent_text(dark_mode))
                            .strong(),
                    );
                    let (dot_rect, _) =
                        ui.allocate_exact_size(egui::Vec2::splat(10.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot_rect.center(), 4.5, cur_p.accent);
                });
            });

            ui.add_space(8.0);
            let cols = 4usize;
            let gap = 8.0_f32;
            let card_w = ((ui.available_width() - gap * (cols - 1) as f32) / cols as f32).max(96.0);
            let card_h = 38.0_f32;
            for row in ui::ThemeAccent::ALL.chunks(cols) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for &preset in row {
                        let selected = preset == cur_accent;
                        let p = preset.palette();
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::Vec2::new(card_w, card_h),
                            egui::Sense::click(),
                        );
                        if ui.is_rect_visible(rect) {
                            let visuals = ui.visuals();
                            let (fill, stroke) = if selected {
                                (
                                    p.accent.gamma_multiply(if dark_mode { 0.22 } else { 0.12 }),
                                    egui::Stroke::new(1.5_f32, p.accent),
                                )
                            } else if resp.hovered() {
                                (
                                    visuals.widgets.hovered.bg_fill,
                                    egui::Stroke::new(1.0_f32, p.accent.gamma_multiply(0.7)),
                                )
                            } else {
                                (
                                    visuals.widgets.inactive.bg_fill,
                                    visuals.widgets.inactive.bg_stroke,
                                )
                            };
                            ui.painter().rect(
                                rect,
                                CornerRadius::same(8),
                                fill,
                                stroke,
                                egui::StrokeKind::Inside,
                            );

                            let dot_center = egui::Pos2::new(rect.min.x + 19.0, rect.center().y);
                            ui.painter().circle_filled(dot_center, 9.0, p.accent);
                            if selected {
                                let check_rect = egui::Rect::from_center_size(
                                    dot_center,
                                    egui::Vec2::splat(10.5),
                                );
                                icons::paint(ui.painter(), check_rect, Icon::Check, Color32::WHITE);
                            } else {
                                let ring_color = if dark_mode {
                                    Color32::from_white_alpha(45)
                                } else {
                                    Color32::from_black_alpha(30)
                                };
                                ui.painter().circle_stroke(
                                    dot_center,
                                    9.0,
                                    egui::Stroke::new(1.0_f32, ring_color),
                                );
                            }

                            let text_color = if selected {
                                p.accent_text(dark_mode)
                            } else {
                                visuals.text_color()
                            };
                            let font_id = egui::TextStyle::Body.resolve(ui.style());
                            let galley = ui.painter().layout_no_wrap(
                                preset.label().to_string(),
                                font_id,
                                text_color,
                            );
                            let text_pos = egui::Pos2::new(
                                rect.min.x + 35.0,
                                rect.center().y - galley.size().y * 0.5,
                            );
                            ui.painter().galley(text_pos, galley, text_color);
                        }
                        if resp.clicked() {
                            set_str(
                                &mut self.draft,
                                "general",
                                "accent",
                                preset.as_str().to_string(),
                            );
                        }
                    }
                });
                ui.add_space(6.0);
            }

            ui.add_space(6.0);
            ui.separator();
            ui.add_space(10.0);

            let cur_text_color = ui::theme_text_color(&self.draft);
            let cur_palette = cur_accent.palette();
            ui.horizontal(|ui| {
                ui.label(RichText::new(i18n::t("Text Color")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(cur_text_color.display_label(dark_mode))
                            .weak()
                            .small(),
                    );
                });
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                let is_auto = cur_text_color == ui::TextColorSetting::Auto;
                if ui
                    .selectable_label(is_auto, i18n::t("Auto (Dark on Light / Light on Dark)"))
                    .clicked()
                {
                    set_str(&mut self.draft, "general", "textColor", "auto".into());
                }
                ui.separator();

                let (resolved_body, _) = cur_text_color.resolve(dark_mode);
                let mut rgb = [resolved_body.r(), resolved_body.g(), resolved_body.b()];
                if ui
                    .color_edit_button_srgb(&mut rgb)
                    .on_hover_text(i18n::t("Custom text color"))
                    .changed()
                {
                    let hex = ui::TextColorSetting::Custom(Color32::from_rgb(
                        rgb[0], rgb[1], rgb[2],
                    ))
                    .as_hex_or_auto();
                    set_str(&mut self.draft, "general", "textColor", hex);
                }

                for &(hex, name) in ui::TEXT_COLOR_PRESETS {
                    let parsed = ui::TextColorSetting::from_setting_str(hex);
                    let swatch_color = match parsed {
                        ui::TextColorSetting::Custom(c) => c,
                        ui::TextColorSetting::Auto => Color32::BLACK,
                    };
                    let selected = cur_text_color == parsed;
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::Vec2::splat(20.0), egui::Sense::click());
                    if ui.is_rect_visible(rect) {
                        ui.painter().circle_filled(rect.center(), 8.0, swatch_color);
                        if selected {
                            ui.painter().circle_stroke(
                                rect.center(),
                                9.5,
                                egui::Stroke::new(2.0_f32, cur_palette.accent),
                            );
                        } else {
                            let ring = if dark_mode {
                                Color32::from_white_alpha(65)
                            } else {
                                Color32::from_black_alpha(55)
                            };
                            ui.painter().circle_stroke(
                                rect.center(),
                                8.0,
                                egui::Stroke::new(1.0_f32, ring),
                            );
                        }
                    }
                    if resp
                        .on_hover_text(format!("{} ({})", i18n::t(name), hex.to_ascii_uppercase()))
                        .clicked()
                    {
                        set_str(&mut self.draft, "general", "textColor", hex.to_string());
                    }
                }
            });

            if cur_text_color.has_low_contrast(dark_mode, cur_palette) {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        Color32::from_rgb(0xd9, 0x82, 0x2b),
                        i18n::t(
                            "The current text color has low contrast against the background and may be hard to read.",
                        ),
                    );
                    if ui.small_button(i18n::t("Reset to Auto")).clicked() {
                        set_str(&mut self.draft, "general", "textColor", "auto".into());
                    }
                });
            }

            ui.add_space(6.0);
            ui.label(
                RichText::new(i18n::t(
                    "Theme, accent, and text color changes preview immediately. Click Save in the top-right to apply to all windows.",
                ))
                .weak()
                .small(),
            );
        });

        section_card(ui, i18n::t("Startup & Tray"), |ui| {
            let mut launch = get_bool(&self.draft, "general", "launchAtLogin");
            setting_switch_row(
                ui,
                i18n::t("Launch at Login"),
                if packaged {
                    i18n::t("Start silently in the system tray when signing in to Windows.")
                } else {
                    i18n::t("Available in packaged release builds only.")
                },
                &mut launch,
                packaged,
            );
            set_bool(&mut self.draft, "general", "launchAtLogin", launch);
        });

        ui.label(
            RichText::new(i18n::t(
                "Closing windows keeps SnapDock running in the system tray. Access it anytime via the tray menu or global hotkeys.",
            ))
            .weak()
            .small(),
        );
    }

    fn show_capture(&mut self, ui: &mut egui::Ui) {
        section_card(ui, i18n::t("Region & Detection"), |ui| {
            let mut detect_w = get_bool(&self.draft, "capture", "detectWindows");
            setting_switch_row(
                ui,
                i18n::t("Auto-Detect Windows"),
                i18n::t("Snap selection to visible window bounds on hover."),
                &mut detect_w,
                true,
            );
            set_bool(&mut self.draft, "capture", "detectWindows", detect_w);

            let mut detect_c = get_bool(&self.draft, "capture", "detectControls");
            setting_switch_row(
                ui,
                i18n::t("Detect UI Controls"),
                i18n::t("Snap to child UI elements inside windows (requires window detection)."),
                &mut detect_c,
                detect_w,
            );
            set_bool(&mut self.draft, "capture", "detectControls", detect_c);

            let mut mag = get_bool(&self.draft, "capture", "magnifier");
            setting_switch_row(
                ui,
                i18n::t("Show Pixel Magnifier"),
                i18n::t("Show zoomed pixel loupe and color readout while selecting (press C to copy HEX)."),
                &mut mag,
                true,
            );
            set_bool(&mut self.draft, "capture", "magnifier", mag);

            let mut delay = get_i64(&self.draft, "capture", "delay", 0);
            setting_number_row(
                ui,
                i18n::t("Capture Delay"),
                None,
                &mut delay,
                0..=10,
                i18n::t("sec"),
            );
            set_i64(&mut self.draft, "capture", "delay", delay);

            let mut copy = get_bool(&self.draft, "capture", "copyOnCapture");
            setting_switch_row(
                ui,
                i18n::t("Auto-Copy on Capture"),
                i18n::t("Automatically copy completed captures (including scrolling captures) to clipboard."),
                &mut copy,
                true,
            );
            set_bool(&mut self.draft, "capture", "copyOnCapture", copy);
        });

        section_card(ui, i18n::t("Scrolling Capture"), |ui| {
            let mut mf = get_i64(&self.draft, "long", "maxFrames", 30);
            setting_number_row(
                ui,
                i18n::t("Max Frames"),
                None,
                &mut mf,
                2..=80,
                i18n::t("frames"),
            );
            set_i64(&mut self.draft, "long", "maxFrames", mf);

            let mut iv = get_i64(&self.draft, "long", "intervalMs", 250);
            setting_number_row(
                ui,
                i18n::t("Scroll Wait Interval"),
                None,
                &mut iv,
                250..=3000,
                i18n::t("ms"),
            );
            set_i64(&mut self.draft, "long", "intervalMs", iv);

            let mut st = get_i64(&self.draft, "long", "stickyTop", 0);
            setting_number_row(
                ui,
                i18n::t("Sticky Header Height"),
                None,
                &mut st,
                0..=500,
                i18n::t("px"),
            );
            set_i64(&mut self.draft, "long", "stickyTop", st);

            let mut sb = get_i64(&self.draft, "long", "stickyBottom", 0);
            setting_number_row(
                ui,
                i18n::t("Sticky Footer Height"),
                None,
                &mut sb,
                0..=500,
                i18n::t("px"),
            );
            set_i64(&mut self.draft, "long", "stickyBottom", sb);
        });

        section_card(ui, i18n::t("Animated Capture (GIF)"), |ui| {
            let mut fps = get_i64(&self.draft, "record", "fps", 8);
            setting_number_row(ui, i18n::t("Frame Rate"), None, &mut fps, 2..=15, "FPS");
            set_i64(&mut self.draft, "record", "fps", fps);

            let mut secs = get_i64(&self.draft, "record", "maxSeconds", 15);
            setting_number_row(
                ui,
                i18n::t("Max Duration"),
                None,
                &mut secs,
                1..=60,
                i18n::t("sec"),
            );
            set_i64(&mut self.draft, "record", "maxSeconds", secs);

            let mut mw = get_i64(&self.draft, "record", "maxWidth", 1280);
            setting_number_row(
                ui,
                i18n::t("Max Output Width"),
                None,
                &mut mw,
                320..=1920,
                i18n::t("px"),
            );
            set_i64(&mut self.draft, "record", "maxWidth", mw);
        });
    }

    fn show_pin(&mut self, ui: &mut egui::Ui) {
        section_card(ui, i18n::t("Desktop Pins"), |ui| {
            let mut op = get_i64(&self.draft, "pin", "opacity", 100);
            ui.horizontal(|ui| {
                ui.label(RichText::new(i18n::t("Default Opacity")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Slider::new(&mut op, 20..=100).suffix("%"));
                });
            });
            set_i64(&mut self.draft, "pin", "opacity", op);

            ui.add_space(6.0);
            let mut scale = get_i64(&self.draft, "pin", "scale", 100);
            ui.horizontal(|ui| {
                ui.label(RichText::new(i18n::t("Default Scale")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("pin_scale_combo")
                        .selected_text(format!("{}%", scale))
                        .show_ui(ui, |ui| {
                            for n in [25, 50, 75, 100, 125, 150, 200] {
                                ui.selectable_value(&mut scale, n, format!("{}%", n));
                            }
                        });
                });
            });
            set_i64(&mut self.draft, "pin", "scale", scale);

            ui.add_space(6.0);
            let mut dbl = get_bool(&self.draft, "pin", "doubleClickClose");
            setting_switch_row(
                ui,
                i18n::t("Double-Click to Close Pin"),
                i18n::t("When disabled, double-clicking a pin opens it in the editor."),
                &mut dbl,
                true,
            );
            set_bool(&mut self.draft, "pin", "doubleClickClose", dbl);
        });

        ui.label(
            RichText::new(i18n::t(
                "Drag anywhere to move a pin, scroll to zoom, and hover the bottom bar to copy, edit/save, OCR, or toggle click-through.",
            ))
            .weak()
            .small(),
        );
    }

    fn show_shortcuts(&mut self, ui: &mut egui::Ui, warnings: &[String]) {
        if !warnings.is_empty() {
            egui::Frame::NONE
                .fill(ui::DANGER.gamma_multiply(0.14))
                .stroke(egui::Stroke::new(1.0_f32, ui::DANGER))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.colored_label(ui::DANGER, warnings.join("; "));
                });
            ui.add_space(8.0);
        }

        section_card(ui, i18n::t("Global Shortcuts"), |ui| {
            for (key, title_key) in [
                ("capture", "Region Capture"),
                ("pin", "Pin Clipboard"),
                ("long", "Scrolling Capture"),
                ("gif", "Animated Capture (GIF)"),
                ("togglePins", "Show / Hide Pins"),
                ("settings", "Open Settings"),
            ] {
                let mut val = get_str(&self.draft, "shortcuts", key);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(i18n::t(title_key)).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut val)
                                    .desired_width(180.0)
                                    .hint_text(i18n::t("Leave empty to disable")),
                            )
                            .changed()
                        {
                            set_str(&mut self.draft, "shortcuts", key, val);
                        }
                    });
                });
                ui.add_space(4.0);
            }
        });

        ui.label(
            RichText::new(i18n::t(
                "Example format: Alt+Shift+A. During selection: Enter/double-click to copy, Ctrl+S to edit & save, right-click/Esc to cancel.",
            ))
            .weak()
            .small(),
        );
    }

    fn show_storage(
        &mut self,
        ui: &mut egui::Ui,
        app: &Arc<App>,
        window: &winit::window::Window,
    ) {
        section_card(ui, i18n::t("Default Save Options"), |ui| {
            let dir = get_str(&self.draft, "storage", "directory");
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(i18n::t("Save Directory")).strong());
                    ui.label(
                        RichText::new(if dir.is_empty() {
                            i18n::t("System Pictures Folder").to_string()
                        } else {
                            dir.clone()
                        })
                        .weak()
                        .small(),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::icon_text_button(
                        ui,
                        Icon::Folder,
                        i18n::t("Browse..."),
                        false,
                        false,
                        None,
                    )
                    .clicked()
                    {
                        let mut dlg = rfd::FileDialog::new()
                            .set_title(i18n::t("Choose Default Save Directory"))
                            .set_parent(window);
                        if !dir.is_empty() {
                            dlg = dlg.set_directory(&dir);
                        }
                        if let Some(picked) = dlg.pick_folder() {
                            set_str(
                                &mut self.draft,
                                "storage",
                                "directory",
                                picked.to_string_lossy().to_string(),
                            );
                        }
                    }
                });
            });
            ui.add_space(6.0);

            let mut fmt = get_str(&self.draft, "storage", "format");
            if fmt != "jpg" {
                fmt = "png".into();
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(i18n::t("Default Format")).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(fmt == "jpg", "JPEG").clicked() {
                        set_str(&mut self.draft, "storage", "format", "jpg".into());
                    }
                    if ui.selectable_label(fmt == "png", "PNG").clicked() {
                        set_str(&mut self.draft, "storage", "format", "png".into());
                    }
                });
            });
            if get_str(&self.draft, "storage", "format") == "jpg" {
                ui.add_space(4.0);
                let mut q = get_i64(&self.draft, "storage", "quality", 90);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(i18n::t("JPEG Quality")).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::Slider::new(&mut q, 10..=100).suffix("%"));
                    });
                });
                set_i64(&mut self.draft, "storage", "quality", q);
            }

            ui.add_space(6.0);
            let mut auto_save = get_bool(&self.draft, "storage", "autoSave");
            setting_switch_row(
                ui,
                i18n::t("Auto-Save Screenshots"),
                i18n::t("Save completed captures automatically; when off, save manually in the editor."),
                &mut auto_save,
                true,
            );
            set_bool(&mut self.draft, "storage", "autoSave", auto_save);

            let mut limit = get_i64(&self.draft, "capture", "historyLimit", 100);
            setting_number_row(
                ui,
                i18n::t("In-Memory Session Limit"),
                None,
                &mut limit,
                5..=500,
                i18n::t("items"),
            );
            set_i64(&mut self.draft, "capture", "historyLimit", limit);
        });

        section_card(ui, i18n::t("Backup & Reset"), |ui| {
            ui.horizontal(|ui| {
                if icons::icon_text_button(
                    ui,
                    Icon::Save,
                    i18n::t("Export Settings..."),
                    false,
                    false,
                    None,
                )
                .clicked()
                {
                    service::export_settings(app);
                }
                if icons::icon_text_button(
                    ui,
                    Icon::Upload,
                    i18n::t("Import Settings..."),
                    false,
                    false,
                    None,
                )
                .clicked()
                {
                    service::import_settings(app);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::icon_text_button_danger(
                        ui,
                        Icon::Reselect,
                        i18n::t("Restore Defaults"),
                    )
                    .clicked()
                    {
                        let answer = rfd::MessageDialog::new()
                            .set_parent(window)
                            .set_title(i18n::t("Restore Default Settings"))
                            .set_description(i18n::t(
                                "This will reset preferences and provider settings while keeping your account and save directory. Continue?",
                            ))
                            .set_buttons(rfd::MessageButtons::YesNo)
                            .show();
                        if answer == rfd::MessageDialogResult::Yes {
                            service::reset_settings(app);
                            self.reload_from_app(app);
                        }
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(i18n::t(
                    "Exported files exclude API keys and login tokens; local secrets are encrypted with Windows DPAPI.",
                ))
                .weak()
                .small(),
            );
        });
    }

    fn show_providers(
        &mut self,
        ui: &mut egui::Ui,
        app: &Arc<App>,
        window: &winit::window::Window,
    ) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.provider_tab, 0, i18n::t("Text Recognition (OCR)"));
            ui.selectable_value(&mut self.provider_tab, 1, i18n::t("Translation"));
        });
        ui.add_space(8.0);

        let is_ocr = self.provider_tab == 0;
        let pkey = if is_ocr { "ocr" } else { "translation" };
        let default_mode = "local";
        let mode = self.draft["providers"][pkey]["mode"]
            .as_str()
            .unwrap_or(default_mode)
            .to_string();

        let (active_ocr_downloads, active_downloads): (
            std::collections::HashMap<String, crate::ocr::OcrDownloadProgress>,
            std::collections::HashMap<String, crate::translate::PackDownloadProgress>,
        ) = {
            let core = app.core.lock().unwrap();
            let ocr_dl = if is_ocr {
                core.ocr_downloads
                    .iter()
                    .map(|(k, v)| (k.clone(), v.progress.clone()))
                    .collect()
            } else {
                std::collections::HashMap::new()
            };
            let trans_dl = if !is_ocr {
                core.pack_downloads
                    .iter()
                    .map(|(k, v)| (k.clone(), v.progress.clone()))
                    .collect()
            } else {
                std::collections::HashMap::new()
            };
            (ocr_dl, trans_dl)
        };

        section_card(
            ui,
            if is_ocr {
                i18n::t("OCR Provider")
            } else {
                i18n::t("Translation Provider")
            },
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(i18n::t("Mode")).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.selectable_label(mode == "ai", i18n::t("AI Model")).clicked() {
                            self.draft["providers"][pkey]["mode"] = json!("ai");
                        }
                        if ui.selectable_label(mode == "api", i18n::t("API Endpoint")).clicked() {
                            self.draft["providers"][pkey]["mode"] = json!("api");
                        }
                        let local_label = if is_ocr {
                            i18n::t("Local RapidOCR")
                        } else {
                            i18n::t("Local Offline")
                        };
                        if ui.selectable_label(mode == "local", local_label).clicked() {
                            self.draft["providers"][pkey]["mode"] = json!("local");
                        }
                    });
                });
                ui.add_space(6.0);

                let mode_now = self.draft["providers"][pkey]["mode"]
                    .as_str()
                    .unwrap_or(default_mode)
                    .to_string();

                if is_ocr && mode_now == "local" {
                    let palette = ui::ctx_palette(ui.ctx());
                    let accent_text = palette.accent_text(ui.visuals().dark_mode);
                    let status = crate::ocr::inspect_status(&app.data_dir, &self.draft);
                    let has_dll = status.runtime_dll.is_some();
                    let status_color = if !has_dll {
                        ui::DANGER
                    } else if status.ready {
                        palette.accent
                    } else {
                        Color32::from_rgb(0xd9, 0x82, 0x2b)
                    };
                    egui::Frame::NONE
                        .fill(status_color.gamma_multiply(0.10))
                        .stroke(egui::Stroke::new(1.0_f32, status_color.gamma_multiply(0.55)))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            if !has_dll {
                                ui.colored_label(
                                    ui::DANGER,
                                    RichText::new(i18n::t("Missing onnxruntime.dll runtime"))
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(i18n::t(
                                        "Ensure onnxruntime.dll exists in the application directory (shared with local RapidOCR).",
                                    ))
                                    .weak()
                                    .small(),
                                );
                            } else if status.ready {
                                let family = status.model_set_family.unwrap_or("RapidOCR");
                                let name = status.model_set_name.unwrap_or("default");
                                ui.colored_label(
                                    accent_text,
                                    RichText::new(i18n::tf(
                                        "Local RapidOCR ready ({family} · {name} · {installed} / {total} models)",
                                        &[
                                            ("family", family),
                                            ("name", name),
                                            ("installed", &status.installed_count.to_string()),
                                            ("total", &status.models.len().to_string()),
                                        ],
                                    ))
                                    .strong(),
                                );
                            } else if status.installed_count == 0 && status.model_dir.is_none() {
                                ui.colored_label(
                                    status_color,
                                    RichText::new(i18n::t(
                                        "Runtime ready; no OCR models downloaded yet",
                                    ))
                                    .strong(),
                                );
                                ui.label(
                                    RichText::new(i18n::t(
                                        "OCR models are downloaded on demand. Choose a download mirror and download a model below.",
                                    ))
                                    .weak()
                                    .small(),
                                );
                            } else {
                                ui.colored_label(
                                    ui::DANGER,
                                    RichText::new(i18n::tf(
                                        "Incomplete models or runtime, missing: {missing}",
                                        &[("missing", &status.missing.join(", "))],
                                    ))
                                    .strong(),
                                );
                            }
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(i18n::tf(
                                        "Storage directory: {path}",
                                        &[("path", &status.storage_dir.display().to_string())],
                                    ))
                                    .weak()
                                    .small(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button(i18n::t("Open Folder"))
                                            .on_hover_text(i18n::t(
                                                "View downloaded OCR models in File Explorer",
                                            ))
                                            .clicked()
                                        {
                                            service::open_directory(app, &status.storage_dir);
                                        }
                                    },
                                );
                            });
                            if let Some(dll) = &status.runtime_dll {
                                ui.label(
                                    RichText::new(i18n::tf(
                                        "Shared runtime: {path}",
                                        &[("path", &dll.display().to_string())],
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                        });

                    ui.add_space(8.0);
                    let ocr_mirror = self.draft["providers"]["ocr"]["local"]["mirror"]
                        .as_str()
                        .unwrap_or("modelscope-cn")
                        .to_string();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(i18n::t("Download Mirror")).strong());
                        for (val, label) in [
                            ("modelscope-cn", i18n::t("ModelScope (China)")),
                            ("modelscope-ai", i18n::t("ModelScope Global")),
                            ("hf-mirror", i18n::t("HF-Mirror (China Mirror)")),
                            ("huggingface", i18n::t("HuggingFace Official")),
                        ] {
                            let selected = if val == "modelscope-cn" {
                                !matches!(
                                    ocr_mirror.as_str(),
                                    "modelscope-ai" | "hf-mirror" | "huggingface"
                                )
                            } else {
                                ocr_mirror == val
                            };
                            if ui.selectable_label(selected, label).clicked() {
                                self.draft["providers"]["ocr"]["local"]["mirror"] = json!(val);
                            }
                        }
                    });

                    ui.add_space(6.0);
                    let mut model_dir = self.draft["providers"]["ocr"]["local"]["modelDir"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    ui.label(i18n::t("Custom Model Directory (Optional)"));
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut model_dir)
                                    .desired_width(ui.available_width() - 96.0)
                                    .hint_text(i18n::t(
                                        "Leave empty to download into models\\ocr next to the app",
                                    )),
                            )
                            .changed()
                        {
                            self.draft["providers"]["ocr"]["local"]["modelDir"] = json!(model_dir);
                        }
                        if icons::icon_text_button(
                            ui,
                            Icon::Folder,
                            i18n::t("Browse..."),
                            false,
                            false,
                            None,
                        )
                        .clicked()
                        {
                            let mut dlg = rfd::FileDialog::new()
                                .set_title(i18n::t("Select RapidOCR Model Directory"))
                                .set_parent(window);
                            if !model_dir.trim().is_empty() {
                                dlg = dlg.set_directory(model_dir.trim());
                            } else if status.storage_dir.is_dir() {
                                dlg = dlg.set_directory(&status.storage_dir);
                            }
                            if let Some(picked) = dlg.pick_folder() {
                                self.draft["providers"]["ocr"]["local"]["modelDir"] =
                                    json!(picked.to_string_lossy().to_string());
                            }
                        }
                    });

                    ui.add_space(6.0);
                    let mut use_cls = self.draft["providers"]["ocr"]["local"]["useCls"]
                        .as_bool()
                        .unwrap_or(false);
                    setting_switch_row(
                        ui,
                        i18n::t("Enable Orientation Classifier (180° Correction)"),
                        i18n::t("Enable only for upside-down text; requires the orientation model in the model directory."),
                        &mut use_cls,
                        true,
                    );
                    self.draft["providers"]["ocr"]["local"]["useCls"] = json!(use_cls);

                    ui.label(
                        RichText::new(i18n::t(
                            "Local RapidOCR runs completely offline without internet access or an API key.",
                        ))
                        .weak()
                        .small(),
                    );
                } else if !is_ocr && mode_now == "local" {
                    let palette = ui::ctx_palette(ui.ctx());
                    let accent_text = palette.accent_text(ui.visuals().dark_mode);
                    let status = crate::translate::inspect_status(&app.data_dir, &self.draft);
                    let has_dll = status.runtime_dll.is_some();
                    let ready = has_dll && status.installed_count > 0;
                    let status_color = if !has_dll {
                        ui::DANGER
                    } else if ready {
                        palette.accent
                    } else {
                        Color32::from_rgb(0xd9, 0x82, 0x2b)
                    };
                    egui::Frame::NONE
                        .fill(status_color.gamma_multiply(0.10))
                        .stroke(egui::Stroke::new(1.0_f32, status_color.gamma_multiply(0.55)))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            if !has_dll {
                                ui.colored_label(
                                    ui::DANGER,
                                    RichText::new(i18n::t("Missing onnxruntime.dll runtime"))
                                        .strong(),
                                );
                                ui.label(
                                    RichText::new(i18n::t(
                                        "Ensure onnxruntime.dll exists in the application directory (shared with local RapidOCR).",
                                    ))
                                    .weak()
                                    .small(),
                                );
                            } else if ready {
                                ui.colored_label(
                                    accent_text,
                                    RichText::new(i18n::tf(
                                        "Local offline translation ready ({installed} / {total} packs installed)",
                                        &[
                                            ("installed", &status.installed_count.to_string()),
                                            ("total", &status.packs.len().to_string()),
                                        ],
                                    ))
                                    .strong(),
                                );
                            } else {
                                ui.colored_label(
                                    status_color,
                                    RichText::new(i18n::t(
                                        "Runtime ready; no translation language packs downloaded yet",
                                    ))
                                    .strong(),
                                );
                                ui.label(
                                    RichText::new(i18n::t(
                                        "Language packs are downloaded on demand. Download the language pairs you need below.",
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(i18n::tf(
                                        "Storage directory: {path}",
                                        &[("path", &status.storage_dir.display().to_string())],
                                    ))
                                    .weak()
                                    .small(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button(i18n::t("Open Folder"))
                                            .on_hover_text(i18n::t(
                                                "View downloaded language packs in File Explorer",
                                            ))
                                            .clicked()
                                        {
                                            service::open_directory(app, &status.storage_dir);
                                        }
                                    },
                                );
                            });
                            if let Some(dll) = &status.runtime_dll {
                                ui.label(
                                    RichText::new(i18n::tf(
                                        "Shared runtime: {path}",
                                        &[("path", &dll.display().to_string())],
                                    ))
                                    .weak()
                                    .small(),
                                );
                            }
                        });

                    ui.add_space(8.0);
                    let mirror = self.draft["providers"]["translation"]["local"]["mirror"]
                        .as_str()
                        .unwrap_or("hf-mirror")
                        .to_string();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(i18n::t("Download Mirror")).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .selectable_label(
                                    mirror == "huggingface",
                                    i18n::t("HuggingFace Official"),
                                )
                                .clicked()
                            {
                                self.draft["providers"]["translation"]["local"]["mirror"] =
                                    json!("huggingface");
                            }
                            if ui
                                .selectable_label(
                                    mirror != "huggingface",
                                    i18n::t("HF-Mirror (China Mirror)"),
                                )
                                .clicked()
                            {
                                self.draft["providers"]["translation"]["local"]["mirror"] =
                                    json!("hf-mirror");
                            }
                        });
                    });

                    ui.add_space(6.0);
                    let mut model_dir = self.draft["providers"]["translation"]["local"]["modelDir"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    ui.label(i18n::t("Custom Pack Directory (Optional)"));
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut model_dir)
                                    .desired_width(ui.available_width() - 96.0)
                                    .hint_text(i18n::t(
                                        "Leave empty to download into models\\translate next to the app",
                                    )),
                            )
                            .changed()
                        {
                            self.draft["providers"]["translation"]["local"]["modelDir"] =
                                json!(model_dir);
                        }
                        if icons::icon_text_button(
                            ui,
                            Icon::Folder,
                            i18n::t("Browse..."),
                            false,
                            false,
                            None,
                        )
                        .clicked()
                        {
                            let mut dlg = rfd::FileDialog::new()
                                .set_title(i18n::t("Select Local Translation Pack Directory"))
                                .set_parent(window);
                            if !model_dir.trim().is_empty() {
                                dlg = dlg.set_directory(model_dir.trim());
                            } else if status.storage_dir.is_dir() {
                                dlg = dlg.set_directory(&status.storage_dir);
                            }
                            if let Some(picked) = dlg.pick_folder() {
                                self.draft["providers"]["translation"]["local"]["modelDir"] =
                                    json!(picked.to_string_lossy().to_string());
                            }
                        }
                    });
                } else {
                    let mut url = self.draft["providers"][pkey][&mode_now]["url"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    ui.label(i18n::t("Endpoint URL"));
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut url)
                                .desired_width(f32::INFINITY)
                                .hint_text(if mode_now == "ai" {
                                    "https://your-service/v1/chat/completions"
                                } else {
                                    "https://your-service/api/ocr"
                                }),
                        )
                        .changed()
                    {
                        self.draft["providers"][pkey][&mode_now]["url"] = json!(url);
                    }
                    ui.add_space(6.0);

                    let mut key = self.draft["providers"][pkey][&mode_now]["key"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let show_key = if is_ocr {
                        &mut self.show_ocr_key
                    } else {
                        &mut self.show_trans_key
                    };
                    ui.label(i18n::t("API Key"));
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut key)
                                    .password(!*show_key)
                                    .desired_width(ui.available_width() - 88.0)
                                    .hint_text(i18n::t("Optional, encrypted locally")),
                            )
                            .changed()
                        {
                            self.draft["providers"][pkey][&mode_now]["key"] = json!(key);
                        }
                        if icons::icon_text_button(
                            ui,
                            if *show_key { Icon::EyeOff } else { Icon::Eye },
                            if *show_key {
                                i18n::t("Hide")
                            } else {
                                i18n::t("Show")
                            },
                            false,
                            false,
                            None,
                        )
                        .clicked()
                        {
                            *show_key = !*show_key;
                        }
                    });
                    ui.add_space(6.0);

                    if mode_now == "ai" {
                        let mut model = self.draft["providers"][pkey]["ai"]["model"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        ui.label(i18n::t("Model Name"));
                        if ui
                            .add(egui::TextEdit::singleline(&mut model).desired_width(f32::INFINITY))
                            .changed()
                        {
                            self.draft["providers"][pkey]["ai"]["model"] = json!(model);
                        }
                        ui.add_space(6.0);

                        let mut prompt = self.draft["providers"][pkey]["ai"]["prompt"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        ui.label(i18n::t("Prompt"));
                        if ui
                            .add(
                                egui::TextEdit::multiline(&mut prompt)
                                    .desired_rows(3)
                                    .desired_width(f32::INFINITY),
                            )
                            .changed()
                        {
                            self.draft["providers"][pkey]["ai"]["prompt"] = json!(prompt);
                        }
                    } else {
                        let mut rpath = self.draft["providers"][pkey]["api"]["responsePath"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        ui.label(i18n::t("Response Text Field"));
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut rpath)
                                    .desired_width(f32::INFINITY)
                                    .hint_text(i18n::t("e.g. data.text")),
                            )
                            .changed()
                        {
                            self.draft["providers"][pkey]["api"]["responsePath"] = json!(rpath);
                        }
                    }
                }

                if !is_ocr {
                    ui.add_space(8.0);
                    let src_val = self.draft["providers"]["translation"]["source"]
                        .as_str()
                        .unwrap_or("auto")
                        .to_string();
                    let tgt_val = self.draft["providers"]["translation"]["target"]
                        .as_str()
                        .unwrap_or("en")
                        .to_string();
                    let src_code = crate::translate::normalize_lang_code(&src_val);
                    let tgt_code = crate::translate::normalize_lang_code(&tgt_val);

                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(i18n::t("Source")).strong());
                        if let Some(picked) = ui::searchable_language_picker(
                            ui,
                            "trans_src_picker",
                            &src_code,
                            &src_val,
                            true,
                            168.0,
                            &mut self.src_lang_query,
                        ) {
                            self.draft["providers"]["translation"]["source"] = json!(picked);
                        }

                        if ui
                            .button("\u{21C4}")
                            .on_hover_text(i18n::t("Swap source and target languages"))
                            .clicked()
                        {
                            let new_src = if tgt_code == "auto" {
                                "zh".to_string()
                            } else {
                                tgt_code.clone()
                            };
                            let new_tgt_code = if src_code == "auto" {
                                if new_src == "en" { "zh" } else { "en" }
                            } else {
                                src_code.as_str()
                            };
                            self.draft["providers"]["translation"]["source"] = json!(new_src);
                            self.draft["providers"]["translation"]["target"] = json!(new_tgt_code);
                        }

                        ui.label(RichText::new(i18n::t("Target")).strong());
                        if let Some(picked) = ui::searchable_language_picker(
                            ui,
                            "trans_tgt_picker",
                            &tgt_code,
                            &tgt_val,
                            false,
                            168.0,
                            &mut self.tgt_lang_query,
                        ) {
                            self.draft["providers"]["translation"]["target"] = json!(picked);
                        }
                    });

                    // Live route status and one-click missing-pack download in local mode.
                    if mode_now == "local" {
                        let status = crate::translate::inspect_status(&app.data_dir, &self.draft);
                        let mirror = self.draft["providers"]["translation"]["local"]["mirror"]
                            .as_str()
                            .unwrap_or("hf-mirror")
                            .to_string();
                        let src_now = crate::translate::normalize_lang_code(
                            self.draft["providers"]["translation"]["source"]
                                .as_str()
                                .unwrap_or("auto"),
                        );
                        let tgt_now = crate::translate::normalize_lang_code(
                            self.draft["providers"]["translation"]["target"]
                                .as_str()
                                .unwrap_or("en"),
                        );
                        let palette = ui::ctx_palette(ui.ctx());
                        let accent_text = palette.accent_text(ui.visuals().dark_mode);
                        ui.add_space(6.0);
                        if src_now == "auto" {
                            let has_mul_en = status
                                .packs
                                .iter()
                                .any(|p| p.spec.id == "mul-en" && p.installed);
                            let has_en_mul = status
                                .packs
                                .iter()
                                .any(|p| p.spec.id == "en-mul" && p.installed);
                            if has_mul_en && has_en_mul {
                                ui.colored_label(
                                    accent_text,
                                    RichText::new(i18n::tf(
                                        "Universal multilingual packs ready: auto-detect any language -> {target}",
                                        &[("target", crate::translate::lang_display_name(&tgt_now))],
                                    ))
                                    .small(),
                                );
                            } else {
                                let preview_src = if tgt_now == "en" { "zh" } else { "en" };
                                if let Ok(plan) = crate::translate::plan_translation_route(
                                    preview_src,
                                    &tgt_now,
                                    &status.storage_dir,
                                ) {
                                    if plan.ready() {
                                        ui.colored_label(
                                            accent_text,
                                            RichText::new(i18n::tf(
                                                "Auto-detect -> {target} (Tip: download mul-en + en-mul to unlock all 42 languages)",
                                                &[("target", crate::translate::lang_display_name(&tgt_now))],
                                            ))
                                            .small(),
                                        );
                                    } else {
                                        let names: Vec<String> = plan
                                            .missing
                                            .iter()
                                            .map(|p| p.display_label())
                                            .collect();
                                        ui.horizontal_wrapped(|ui| {
                                            ui.colored_label(
                                                Color32::from_rgb(0xd9, 0x82, 0x2b),
                                                RichText::new(i18n::tf(
                                                    "Target language {target} is missing pack(s): {packs}",
                                                    &[
                                                        ("target", crate::translate::lang_display_name(&tgt_now)),
                                                        ("packs", &names.join(", ")),
                                                    ],
                                                ))
                                                .small(),
                                            );
                                            if ui
                                                .small_button(i18n::t("Download Required Packs"))
                                                .clicked()
                                            {
                                                for pack in &plan.missing {
                                                    service::download_translation_pack(
                                                        app,
                                                        pack.id.to_string(),
                                                        status.storage_dir.clone(),
                                                        mirror.clone(),
                                                    );
                                                }
                                            }
                                        });
                                    }
                                }
                            }
                        } else if src_now == tgt_now {
                            ui.label(
                                RichText::new(i18n::t(
                                    "Source and target languages are the same; original text will be returned unchanged.",
                                ))
                                .weak()
                                .small(),
                            );
                        } else if let Ok(plan) = crate::translate::plan_translation_route(
                            &src_now,
                            &tgt_now,
                            &status.storage_dir,
                        ) {
                            let route_desc: Vec<String> = plan
                                .steps
                                .iter()
                                .map(|s| format!("{} ({})", s.pack.display_label(), s.pack.id))
                                .collect();
                            if plan.ready() {
                                ui.colored_label(
                                    accent_text,
                                    RichText::new(i18n::tf(
                                        "Offline route ready: {route}",
                                        &[("route", &route_desc.join("  ->  "))],
                                    ))
                                    .small(),
                                );
                            } else {
                                let names: Vec<String> = plan
                                    .missing
                                    .iter()
                                    .map(|p| p.display_label())
                                    .collect();
                                ui.horizontal_wrapped(|ui| {
                                    ui.colored_label(
                                        Color32::from_rgb(0xd9, 0x82, 0x2b),
                                        RichText::new(i18n::tf(
                                            "Missing pack(s) for this pair: {packs}",
                                            &[("packs", &names.join(", "))],
                                        ))
                                        .small(),
                                    );
                                    if ui
                                        .small_button(i18n::t("Download Required Packs"))
                                        .clicked()
                                    {
                                        for pack in &plan.missing {
                                            service::download_translation_pack(
                                                app,
                                                pack.id.to_string(),
                                                status.storage_dir.clone(),
                                                mirror.clone(),
                                            );
                                        }
                                    }
                                });
                            }
                        }
                    }
                }

                ui.add_space(8.0);
                if icons::icon_text_button(
                    ui,
                    Icon::Check,
                    i18n::t("Save & Test"),
                    false,
                    false,
                    None,
                )
                .clicked()
                {
                    if let Err(e) = service::update_settings(app, &self.draft) {
                        self.notify(e, true);
                    } else {
                        self.reload_from_app(app);
                        if is_ocr {
                            let latest_id = app
                                .core
                                .lock()
                                .unwrap()
                                .captures_order
                                .last()
                                .cloned();
                            let local_mode = self.draft["providers"]["ocr"]["mode"]
                                .as_str()
                                .unwrap_or("local")
                                == "local";
                            if latest_id.is_some() || local_mode {
                                service::run_text_process(
                                    app,
                                    TextTarget::SettingsTest,
                                    "ocr".into(),
                                    latest_id,
                                    None,
                                );
                            } else {
                                self.notify(i18n::t("Capture an image first to test OCR"), true);
                            }
                        } else {
                            let src = crate::translate::normalize_lang_code(
                                self.draft["providers"]["translation"]["source"]
                                    .as_str()
                                    .unwrap_or("auto"),
                            );
                            let tgt = crate::translate::normalize_lang_code(
                                self.draft["providers"]["translation"]["target"]
                                    .as_str()
                                    .unwrap_or("en"),
                            );
                            let sample = i18n::sample_text_for_lang(&src, &tgt);
                            service::run_text_process(
                                app,
                                TextTarget::SettingsTest,
                                "translation".into(),
                                None,
                                Some(sample.into()),
                            );
                        }
                    }
                }
            },
        );

        let mode_after = self.draft["providers"][pkey]["mode"]
            .as_str()
            .unwrap_or(default_mode)
            .to_string();
        if is_ocr && mode_after == "local" {
            let status = crate::ocr::inspect_status(&app.data_dir, &self.draft);
            let mirror = self.draft["providers"]["ocr"]["local"]["mirror"]
                .as_str()
                .unwrap_or("modelscope-cn")
                .to_string();
            let use_cls = self.draft["providers"]["ocr"]["local"]["useCls"]
                .as_bool()
                .unwrap_or(false);
            let palette = ui::ctx_palette(ui.ctx());
            let accent_text = palette.accent_text(ui.visuals().dark_mode);

            section_card(
                ui,
                i18n::t("Available OCR Models (RapidOCR ONNX)"),
                |ui| {
                    ui.label(
                        RichText::new(i18n::t(
                            "Models are saved under models\\ocr. PP-OCRv5 Mobile or PP-OCRv6 Small is recommended for most scenarios.",
                        ))
                        .weak()
                        .small(),
                    );
                    ui.add_space(8.0);

                    let total_count = status.models.len();
                    let installed_count = status.installed_count;
                    let not_installed_count = total_count.saturating_sub(installed_count);

                    let filtered: Vec<&crate::ocr::OcrModelInstallState> = status
                        .models
                        .iter()
                        .filter(|m| match self.ocr_filter {
                            OcrModelFilter::All => true,
                            OcrModelFilter::Installed => m.installed,
                            OcrModelFilter::NotInstalled => !m.installed,
                        })
                        .collect();

                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(
                            &mut self.ocr_filter,
                            OcrModelFilter::All,
                            i18n::tf("All ({count})", &[("count", &total_count.to_string())]),
                        );
                        ui.selectable_value(
                            &mut self.ocr_filter,
                            OcrModelFilter::Installed,
                            i18n::tf(
                                "Installed ({count})",
                                &[("count", &installed_count.to_string())],
                            ),
                        );
                        ui.selectable_value(
                            &mut self.ocr_filter,
                            OcrModelFilter::NotInstalled,
                            i18n::tf(
                                "Not Installed ({count})",
                                &[("count", &not_installed_count.to_string())],
                            ),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    RichText::new(i18n::tf(
                                        "Shown {shown} / {total}",
                                        &[
                                            ("shown", &filtered.len().to_string()),
                                            ("total", &total_count.to_string()),
                                        ],
                                    ))
                                    .weak()
                                    .small(),
                                );
                            },
                        );
                    });
                    ui.add_space(8.0);

                    if filtered.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(i18n::t("No matching OCR models found.")).weak(),
                            );
                            if self.ocr_filter != OcrModelFilter::All
                                && ui.small_button(i18n::t("Show All")).clicked()
                            {
                                self.ocr_filter = OcrModelFilter::All;
                            }
                        });
                    }

                    for model in filtered {
                        let downloading = active_ocr_downloads.get(model.pack.id);
                        let is_active =
                            model.installed && status.model_set_name == Some(model.pack.id);
                        let row_stroke = if is_active {
                            palette.accent.gamma_multiply(0.70)
                        } else if model.installed {
                            palette.accent.gamma_multiply(0.42)
                        } else if model.pack.tier == crate::ocr::OcrModelTier::Recommended {
                            palette.accent.gamma_multiply(0.28)
                        } else {
                            ui.visuals().widgets.noninteractive.bg_stroke.color
                        };

                        egui::Frame::NONE
                            .fill(ui.visuals().faint_bg_color)
                            .stroke(egui::Stroke::new(1.0_f32, row_stroke))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                RichText::new(model.pack.display_label()).strong(),
                                            );
                                            match model.pack.tier {
                                                crate::ocr::OcrModelTier::Recommended => {
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t("Recommended"))
                                                            .small()
                                                            .strong(),
                                                    );
                                                }
                                                crate::ocr::OcrModelTier::HighAccuracy => {
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t("High Accuracy"))
                                                            .small(),
                                                    );
                                                }
                                                crate::ocr::OcrModelTier::Lightweight => {
                                                    ui.label(
                                                        RichText::new(i18n::t("Lightweight"))
                                                            .weak()
                                                            .small(),
                                                    );
                                                }
                                            }
                                            ui.label(
                                                RichText::new(model.pack.display_languages())
                                                    .weak()
                                                    .small(),
                                            );
                                            ui.label(
                                                RichText::new(model.pack.id).weak().small(),
                                            );
                                        });

                                        let subtitle = if let Some(prog) = downloading {
                                            i18n::tf(
                                                "Downloading ({idx}/{count}): {file} · {done} / {total} MB",
                                                &[
                                                    ("idx", &prog.file_index.max(1).to_string()),
                                                    ("count", &prog.file_count.to_string()),
                                                    ("file", &prog.current_file),
                                                    (
                                                        "done",
                                                        &format!(
                                                            "{:.1}",
                                                            prog.downloaded_bytes as f64
                                                                / (1024.0 * 1024.0)
                                                        ),
                                                    ),
                                                    (
                                                        "total",
                                                        &format!(
                                                            "{:.1}",
                                                            prog.total_bytes as f64
                                                                / (1024.0 * 1024.0)
                                                        ),
                                                    ),
                                                ],
                                            )
                                        } else if model.installed {
                                            i18n::tf(
                                                "Installed · {mb} MB · {desc}",
                                                &[
                                                    (
                                                        "mb",
                                                        &format!(
                                                            "{:.1}",
                                                            model.disk_bytes as f64
                                                                / (1024.0 * 1024.0)
                                                        ),
                                                    ),
                                                    ("desc", model.pack.display_description()),
                                                ],
                                            )
                                        } else if use_cls && model.disk_bytes > 0 && !model.has_cls {
                                            i18n::tf(
                                                "Missing orientation model · Click Download to complete ({desc})",
                                                &[("desc", model.pack.display_description())],
                                            )
                                        } else {
                                            i18n::tf(
                                                "Not installed · ~{mb} MB · {desc}",
                                                &[
                                                    ("mb", &model.pack.approx_mb.to_string()),
                                                    ("desc", model.pack.display_description()),
                                                ],
                                            )
                                        };
                                        ui.label(RichText::new(subtitle).weak().small());
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if downloading.is_some() {
                                                if icons::icon_text_button_danger(
                                                    ui,
                                                    Icon::Close,
                                                    i18n::t("Cancel"),
                                                )
                                                .clicked()
                                                {
                                                    service::cancel_ocr_model_download(
                                                        app,
                                                        model.pack.id,
                                                    );
                                                }
                                            } else if model.installed {
                                                if model.managed
                                                    && icons::icon_text_button_danger(
                                                        ui,
                                                        Icon::Trash,
                                                        i18n::t("Delete"),
                                                    )
                                                    .clicked()
                                                {
                                                    let answer = rfd::MessageDialog::new()
                                                        .set_parent(window)
                                                        .set_title(i18n::t("Delete OCR Model"))
                                                        .set_description(i18n::tf(
                                                            "Delete downloaded OCR model [{label}]? You can re-download it anytime.",
                                                            &[(
                                                                "label",
                                                                model.pack.display_label(),
                                                            )],
                                                        ))
                                                        .set_buttons(rfd::MessageButtons::YesNo)
                                                        .show();
                                                    if answer == rfd::MessageDialogResult::Yes {
                                                        if self.draft["providers"]["ocr"]["local"]
                                                            ["model"]
                                                            .as_str()
                                                            == Some(model.pack.id)
                                                        {
                                                            self.draft["providers"]["ocr"]["local"]
                                                                ["model"] = json!("");
                                                        }
                                                        service::delete_ocr_model(
                                                            app,
                                                            model.pack.id,
                                                            &status.storage_dir,
                                                        );
                                                    }
                                                }
                                                if is_active {
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t("Active"))
                                                            .small()
                                                            .strong(),
                                                    );
                                                } else if ui
                                                    .small_button(i18n::t("Use"))
                                                    .on_hover_text(i18n::t(
                                                        "Set as active local OCR model",
                                                    ))
                                                    .clicked()
                                                {
                                                    self.draft["providers"]["ocr"]["local"]
                                                        ["model"] = json!(model.pack.id);
                                                }
                                            } else if icons::icon_text_button(
                                                ui,
                                                Icon::Save,
                                                i18n::t("Download"),
                                                false,
                                                true,
                                                None,
                                            )
                                            .on_hover_text(i18n::tf(
                                                "Download to {path}",
                                                &[("path", &model.dir.display().to_string())],
                                            ))
                                            .clicked()
                                            {
                                                service::download_ocr_model(
                                                    app,
                                                    model.pack.id.to_string(),
                                                    status.storage_dir.clone(),
                                                    mirror.clone(),
                                                );
                                            }
                                        },
                                    );
                                });

                                if let Some(prog) = downloading {
                                    ui.add_space(6.0);
                                    ui.add(
                                        egui::ProgressBar::new(prog.fraction())
                                            .show_percentage()
                                            .desired_height(12.0),
                                    );
                                }
                            });
                        ui.add_space(6.0);
                    }
                },
            );
        } else if !is_ocr && mode_after == "local" {
            let status = crate::translate::inspect_status(&app.data_dir, &self.draft);
            let mirror = self.draft["providers"]["translation"]["local"]["mirror"]
                .as_str()
                .unwrap_or("hf-mirror")
                .to_string();

            section_card(
                ui,
                i18n::t("Available Translation Packs & Pairs (OPUS-MT INT8 ONNX)"),
                |ui| {
                    ui.label(
                        RichText::new(i18n::t(
                            "Download the two universal packs (mul-en & en-mul) to unlock all 42 languages, or search a language (e.g. Japanese, ja->zh) to list all translation directions to and from it.",
                        ))
                        .weak()
                        .small(),
                    );
                    ui.add_space(8.0);

                    // Search bar with clear button.
                    ui.horizontal(|ui| {
                        let clear_w = if self.pack_search.is_empty() { 0.0 } else { 28.0 };
                        let search_resp = ui.add(
                            egui::TextEdit::singleline(&mut self.pack_search)
                                .desired_width((ui.available_width() - clear_w).max(160.0))
                                .hint_text(i18n::t(
                                    "Search language or pair (e.g. Japanese, Chinese, ja->zh, mul)...",
                                )),
                        );
                        if search_resp.changed() {
                            self.pair_direction = PairDirectionFilter::All;
                        }
                        if !self.pack_search.is_empty()
                            && icons::small_icon_button(ui, Icon::Close)
                                .on_hover_text(i18n::t("Clear search"))
                                .clicked()
                        {
                            self.pack_search.clear();
                            self.pair_direction = PairDirectionFilter::All;
                        }
                    });
                    ui.add_space(6.0);

                    let palette = ui::ctx_palette(ui.ctx());
                    let accent_text = palette.accent_text(ui.visuals().dark_mode);

                    // When the search query targets one or two languages, show expanded directed pairs.
                    if let Some(pair_res) = crate::translate::search_translation_pairs(
                        &self.pack_search,
                        &status.storage_dir,
                    ) {
                        let total_pairs = pair_res.pairs.len();
                        let ready_pairs = pair_res.pairs.iter().filter(|p| p.ready()).count();
                        let missing_pairs = total_pairs.saturating_sub(ready_pairs);

                        if let Some(focus) = pair_res.focus_lang {
                            let focus_name = crate::translate::lang_display_name(focus);
                            let from_count = pair_res
                                .pairs
                                .iter()
                                .filter(|p| p.source_code == focus)
                                .count();
                            let to_count = pair_res
                                .pairs
                                .iter()
                                .filter(|p| p.target_code == focus)
                                .count();
                            ui.horizontal_wrapped(|ui| {
                                ui.selectable_value(
                                    &mut self.pair_direction,
                                    PairDirectionFilter::All,
                                    i18n::tf(
                                        "All Directions ({count})",
                                        &[("count", &total_pairs.to_string())],
                                    ),
                                );
                                ui.selectable_value(
                                    &mut self.pair_direction,
                                    PairDirectionFilter::FromFocus,
                                    i18n::tf(
                                        "{lang} -> Other ({count})",
                                        &[
                                            ("lang", focus_name),
                                            ("count", &from_count.to_string()),
                                        ],
                                    ),
                                );
                                ui.selectable_value(
                                    &mut self.pair_direction,
                                    PairDirectionFilter::ToFocus,
                                    i18n::tf(
                                        "Other -> {lang} ({count})",
                                        &[
                                            ("lang", focus_name),
                                            ("count", &to_count.to_string()),
                                        ],
                                    ),
                                );
                            });
                            ui.add_space(4.0);
                        }

                        if self.pack_filter == PackFilter::Multilingual {
                            self.pack_filter = PackFilter::All;
                        }

                        let filtered_pairs: Vec<&crate::translate::TranslationPairEntry> = pair_res
                            .pairs
                            .iter()
                            .filter(|pair| {
                                let pass_dir = match (pair_res.focus_lang, self.pair_direction) {
                                    (Some(f), PairDirectionFilter::FromFocus) => {
                                        pair.source_code == f
                                    }
                                    (Some(f), PairDirectionFilter::ToFocus) => {
                                        pair.target_code == f
                                    }
                                    _ => true,
                                };
                                let pass_status = match self.pack_filter {
                                    PackFilter::All | PackFilter::Multilingual => true,
                                    PackFilter::Installed => pair.ready(),
                                    PackFilter::NotInstalled => !pair.ready(),
                                };
                                pass_dir && pass_status
                            })
                            .collect();

                        ui.horizontal_wrapped(|ui| {
                            ui.selectable_value(
                                &mut self.pack_filter,
                                PackFilter::All,
                                i18n::tf("All ({count})", &[("count", &total_pairs.to_string())]),
                            );
                            ui.selectable_value(
                                &mut self.pack_filter,
                                PackFilter::Installed,
                                i18n::tf(
                                    "Installed ({count})",
                                    &[("count", &ready_pairs.to_string())],
                                ),
                            );
                            ui.selectable_value(
                                &mut self.pack_filter,
                                PackFilter::NotInstalled,
                                i18n::tf(
                                    "Not Installed ({count})",
                                    &[("count", &missing_pairs.to_string())],
                                ),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(i18n::tf(
                                            "Shown {shown} / {total}",
                                            &[
                                                ("shown", &filtered_pairs.len().to_string()),
                                                ("total", &total_pairs.to_string()),
                                            ],
                                        ))
                                        .weak()
                                        .small(),
                                    );
                                },
                            );
                        });
                        ui.add_space(8.0);

                        if filtered_pairs.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(i18n::t("No matching translation pairs found."))
                                        .weak(),
                                );
                                if ui.small_button(i18n::t("Show All")).clicked() {
                                    self.pack_filter = PackFilter::All;
                                    self.pair_direction = PairDirectionFilter::All;
                                }
                            });
                        }

                        for pair in filtered_pairs {
                            let active_step_downloads: Vec<&crate::translate::PackDownloadProgress> =
                                pair.plan
                                    .steps
                                    .iter()
                                    .filter_map(|s| active_downloads.get(s.pack.id))
                                    .collect();
                            let is_downloading = !active_step_downloads.is_empty();
                            let pack_ids: Vec<&str> =
                                pair.plan.steps.iter().map(|s| s.pack.id).collect();
                            let pack_ids_str = pack_ids.join(" + ");
                            let row_stroke = if pair.ready() {
                                palette.accent.gamma_multiply(0.45)
                            } else {
                                ui.visuals().widgets.noninteractive.bg_stroke.color
                            };

                            egui::Frame::NONE
                                .fill(ui.visuals().faint_bg_color)
                                .stroke(egui::Stroke::new(1.0_f32, row_stroke))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                let title = format!(
                                                    "{} -> {}",
                                                    crate::translate::lang_display_name(
                                                        pair.source_code
                                                    ),
                                                    crate::translate::lang_display_name(
                                                        pair.target_code
                                                    )
                                                );
                                                ui.label(RichText::new(title).strong());
                                                if pair.is_bridged() {
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t("Via English"))
                                                            .small(),
                                                    );
                                                } else {
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t("Direct"))
                                                            .small()
                                                            .strong(),
                                                    );
                                                }
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} -> {}",
                                                        pair.source_code, pair.target_code
                                                    ))
                                                    .weak()
                                                    .small(),
                                                );
                                            });

                                            let subtitle = if let Some(prog) =
                                                active_step_downloads.first()
                                            {
                                                i18n::tf(
                                                    "Downloading ({idx}/{count}): {file} · {done} / {total} MB",
                                                    &[
                                                        ("idx", &prog.file_index.max(1).to_string()),
                                                        ("count", &prog.file_count.to_string()),
                                                        ("file", &prog.current_file),
                                                        (
                                                            "done",
                                                            &format!(
                                                                "{:.1}",
                                                                prog.downloaded_bytes as f64
                                                                    / (1024.0 * 1024.0)
                                                            ),
                                                        ),
                                                        (
                                                            "total",
                                                            &format!(
                                                                "{:.1}",
                                                                prog.total_bytes as f64
                                                                    / (1024.0 * 1024.0)
                                                            ),
                                                        ),
                                                    ],
                                                )
                                            } else if pair.ready() {
                                                let mb = pair.installed_bytes(&status.storage_dir)
                                                    as f64
                                                    / (1024.0 * 1024.0);
                                                i18n::tf(
                                                    "Installed · {mb} MB · {packs}",
                                                    &[
                                                        ("mb", &format!("{:.1}", mb)),
                                                        ("packs", &pack_ids_str),
                                                    ],
                                                )
                                            } else if pair.plan.missing.len()
                                                < pair.plan.steps.len()
                                            {
                                                i18n::tf(
                                                    "Partially installed · ~{mb} MB needed · {packs}",
                                                    &[
                                                        ("mb", &pair.missing_mb().to_string()),
                                                        ("packs", &pack_ids_str),
                                                    ],
                                                )
                                            } else {
                                                i18n::tf(
                                                    "Not installed · ~{mb} MB · {packs}",
                                                    &[
                                                        ("mb", &pair.total_mb().to_string()),
                                                        ("packs", &pack_ids_str),
                                                    ],
                                                )
                                            };
                                            ui.label(RichText::new(subtitle).weak().small());
                                        });

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if is_downloading {
                                                    if icons::icon_text_button_danger(
                                                        ui,
                                                        Icon::Close,
                                                        i18n::t("Cancel"),
                                                    )
                                                    .clicked()
                                                    {
                                                        for step in &pair.plan.steps {
                                                            service::cancel_translation_pack_download(
                                                                app,
                                                                step.pack.id,
                                                            );
                                                        }
                                                    }
                                                } else if pair.ready() {
                                                    if !pair.is_bridged() {
                                                        let sole_pack = pair.plan.steps[0].pack;
                                                        if icons::icon_text_button_danger(
                                                            ui,
                                                            Icon::Trash,
                                                            i18n::t("Delete"),
                                                        )
                                                        .clicked()
                                                        {
                                                            let answer = rfd::MessageDialog::new()
                                                                .set_parent(window)
                                                                .set_title(i18n::t(
                                                                    "Delete Language Pack",
                                                                ))
                                                                .set_description(i18n::tf(
                                                                    "Delete downloaded language pack [{label}]? You can re-download it anytime.",
                                                                    &[(
                                                                        "label",
                                                                        &sole_pack.display_label(),
                                                                    )],
                                                                ))
                                                                .set_buttons(
                                                                    rfd::MessageButtons::YesNo,
                                                                )
                                                                .show();
                                                            if answer
                                                                == rfd::MessageDialogResult::Yes
                                                            {
                                                                service::delete_translation_pack(
                                                                    app,
                                                                    sole_pack.id,
                                                                    &status.storage_dir,
                                                                );
                                                            }
                                                        }
                                                    }
                                                    if ui
                                                        .small_button(i18n::t("Use"))
                                                        .on_hover_text(i18n::t(
                                                            "Set as current source and target languages",
                                                        ))
                                                        .clicked()
                                                    {
                                                        self.draft["providers"]["translation"]
                                                            ["source"] = json!(pair.source_code);
                                                        self.draft["providers"]["translation"]
                                                            ["target"] = json!(pair.target_code);
                                                    }
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t("Ready"))
                                                            .small()
                                                            .strong(),
                                                    );
                                                } else if icons::icon_text_button(
                                                    ui,
                                                    Icon::Save,
                                                    i18n::t("Download"),
                                                    false,
                                                    true,
                                                    None,
                                                )
                                                .on_hover_text(i18n::tf(
                                                    "Download required pack(s): {packs}",
                                                    &[(
                                                        "packs",
                                                        &pair
                                                            .plan
                                                            .missing
                                                            .iter()
                                                            .map(|p| p.id)
                                                            .collect::<Vec<_>>()
                                                            .join(" + "),
                                                    )],
                                                ))
                                                .clicked()
                                                {
                                                    for pack in &pair.plan.missing {
                                                        service::download_translation_pack(
                                                            app,
                                                            pack.id.to_string(),
                                                            status.storage_dir.clone(),
                                                            mirror.clone(),
                                                        );
                                                    }
                                                }
                                            },
                                        );
                                    });

                                    if let Some(prog) = active_step_downloads.first() {
                                        ui.add_space(6.0);
                                        ui.add(
                                            egui::ProgressBar::new(prog.fraction())
                                                .show_percentage()
                                                .desired_height(12.0),
                                        );
                                    }
                                });
                            ui.add_space(6.0);
                        }
                    } else {
                        // Standard underlying OPUS-MT pack catalog view.
                        let total_count = status.packs.len();
                        let installed_count = status.installed_count;
                        let not_installed_count = total_count.saturating_sub(installed_count);
                        let multi_count = status
                            .packs
                            .iter()
                            .filter(|p| p.spec.kind != crate::translate::PackKind::Dedicated)
                            .count();

                        let filtered: Vec<&crate::translate::PackInstallState> = status
                            .packs
                            .iter()
                            .filter(|pack| {
                                let pass_filter = match self.pack_filter {
                                    PackFilter::All => true,
                                    PackFilter::Multilingual => {
                                        pack.spec.kind != crate::translate::PackKind::Dedicated
                                    }
                                    PackFilter::Installed => pack.installed,
                                    PackFilter::NotInstalled => !pack.installed,
                                };
                                pass_filter
                                    && crate::translate::matches_pack_query(
                                        pack.spec,
                                        &self.pack_search,
                                    )
                            })
                            .collect();

                        ui.horizontal_wrapped(|ui| {
                            ui.selectable_value(
                                &mut self.pack_filter,
                                PackFilter::All,
                                i18n::tf("All ({count})", &[("count", &total_count.to_string())]),
                            );
                            ui.selectable_value(
                                &mut self.pack_filter,
                                PackFilter::Multilingual,
                                i18n::tf(
                                    "Universal ({count})",
                                    &[("count", &multi_count.to_string())],
                                ),
                            );
                            ui.selectable_value(
                                &mut self.pack_filter,
                                PackFilter::Installed,
                                i18n::tf(
                                    "Installed ({count})",
                                    &[("count", &installed_count.to_string())],
                                ),
                            );
                            ui.selectable_value(
                                &mut self.pack_filter,
                                PackFilter::NotInstalled,
                                i18n::tf(
                                    "Not Installed ({count})",
                                    &[("count", &not_installed_count.to_string())],
                                ),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(i18n::tf(
                                            "Shown {shown} / {total}",
                                            &[
                                                ("shown", &filtered.len().to_string()),
                                                ("total", &total_count.to_string()),
                                            ],
                                        ))
                                        .weak()
                                        .small(),
                                    );
                                },
                            );
                        });
                        ui.add_space(8.0);

                        if filtered.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(i18n::t("No matching language packs found."))
                                        .weak(),
                                );
                                if (!self.pack_search.is_empty()
                                    || self.pack_filter != PackFilter::All)
                                    && ui.small_button(i18n::t("Show All")).clicked()
                                {
                                    self.pack_search.clear();
                                    self.pack_filter = PackFilter::All;
                                }
                            });
                        }
                        for pack in filtered {
                            let downloading = active_downloads.get(pack.spec.id);
                            let row_stroke = if pack.installed {
                                palette.accent.gamma_multiply(0.45)
                            } else if pack.spec.kind == crate::translate::PackKind::Multilingual {
                                palette.accent.gamma_multiply(0.32)
                            } else {
                                ui.visuals().widgets.noninteractive.bg_stroke.color
                            };
                            egui::Frame::NONE
                                .fill(ui.visuals().faint_bg_color)
                                .stroke(egui::Stroke::new(1.0_f32, row_stroke))
                                .corner_radius(CornerRadius::same(8))
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label(
                                                    RichText::new(pack.spec.display_label())
                                                        .strong(),
                                                );
                                                if pack.spec.kind
                                                    == crate::translate::PackKind::Multilingual
                                                {
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t(
                                                            "Recommended · Any Pair",
                                                        ))
                                                        .small()
                                                        .strong(),
                                                    );
                                                } else if pack.spec.kind
                                                    == crate::translate::PackKind::Group
                                                {
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t("Language Group"))
                                                            .small(),
                                                    );
                                                }
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} -> {}",
                                                        pack.spec.source_lang,
                                                        pack.spec.target_lang
                                                    ))
                                                    .weak()
                                                    .small(),
                                                );
                                            });
                                            let subtitle = if let Some(prog) = downloading {
                                                i18n::tf(
                                                    "Downloading ({idx}/{count}): {file} · {done} / {total} MB",
                                                    &[
                                                        ("idx", &prog.file_index.max(1).to_string()),
                                                        ("count", &prog.file_count.to_string()),
                                                        ("file", &prog.current_file),
                                                        (
                                                            "done",
                                                            &format!(
                                                                "{:.1}",
                                                                prog.downloaded_bytes as f64
                                                                    / (1024.0 * 1024.0)
                                                            ),
                                                        ),
                                                        (
                                                            "total",
                                                            &format!(
                                                                "{:.1}",
                                                                prog.total_bytes as f64
                                                                    / (1024.0 * 1024.0)
                                                            ),
                                                        ),
                                                    ],
                                                )
                                            } else if pack.installed {
                                                i18n::tf(
                                                    "Installed · {mb} MB · {repo}",
                                                    &[
                                                        (
                                                            "mb",
                                                            &format!(
                                                                "{:.1}",
                                                                pack.disk_bytes as f64
                                                                    / (1024.0 * 1024.0)
                                                            ),
                                                        ),
                                                        ("repo", pack.spec.repo),
                                                    ],
                                                )
                                            } else {
                                                i18n::tf(
                                                    "Not installed · ~{mb} MB · {repo}",
                                                    &[
                                                        ("mb", &pack.spec.approx_mb.to_string()),
                                                        ("repo", pack.spec.repo),
                                                    ],
                                                )
                                            };
                                            ui.label(RichText::new(subtitle).weak().small());
                                        });

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if downloading.is_some() {
                                                    if icons::icon_text_button_danger(
                                                        ui,
                                                        Icon::Close,
                                                        i18n::t("Cancel"),
                                                    )
                                                    .clicked()
                                                    {
                                                        service::cancel_translation_pack_download(
                                                            app,
                                                            pack.spec.id,
                                                        );
                                                    }
                                                } else if pack.installed {
                                                    if icons::icon_text_button_danger(
                                                        ui,
                                                        Icon::Trash,
                                                        i18n::t("Delete"),
                                                    )
                                                    .clicked()
                                                    {
                                                        let answer = rfd::MessageDialog::new()
                                                            .set_parent(window)
                                                            .set_title(i18n::t(
                                                                "Delete Language Pack",
                                                            ))
                                                            .set_description(i18n::tf(
                                                                "Delete downloaded language pack [{label}]? You can re-download it anytime.",
                                                                &[(
                                                                    "label",
                                                                    &pack.spec.display_label(),
                                                                )],
                                                            ))
                                                            .set_buttons(
                                                                rfd::MessageButtons::YesNo,
                                                            )
                                                            .show();
                                                        if answer == rfd::MessageDialogResult::Yes {
                                                            service::delete_translation_pack(
                                                                app,
                                                                pack.spec.id,
                                                                &status.storage_dir,
                                                            );
                                                        }
                                                    }
                                                    ui.colored_label(
                                                        accent_text,
                                                        RichText::new(i18n::t("Ready"))
                                                            .small()
                                                            .strong(),
                                                    );
                                                } else if icons::icon_text_button(
                                                    ui,
                                                    Icon::Save,
                                                    i18n::t("Download"),
                                                    false,
                                                    true,
                                                    None,
                                                )
                                                .on_hover_text(i18n::tf(
                                                    "Download to {path}",
                                                    &[("path", &pack.dir.display().to_string())],
                                                ))
                                                .clicked()
                                                {
                                                    service::download_translation_pack(
                                                        app,
                                                        pack.spec.id.to_string(),
                                                        status.storage_dir.clone(),
                                                        mirror.clone(),
                                                    );
                                                }
                                            },
                                        );
                                    });

                                    if let Some(prog) = downloading {
                                        ui.add_space(6.0);
                                        ui.add(
                                            egui::ProgressBar::new(prog.fraction())
                                                .show_percentage()
                                                .desired_height(12.0),
                                        );
                                    }
                                });
                            ui.add_space(6.0);
                        }
                    }
                },
            );
        }
    }

    fn show_account(
        &mut self,
        ui: &mut egui::Ui,
        app: &Arc<App>,
        window: &winit::window::Window,
        account: Option<crate::state::AccountView>,
    ) {
        let changed = self.draft != app.core.lock().unwrap().settings;
        section_card(ui, i18n::t("Encrypted Sync Server"), |ui| {
            let mut url = get_str(&self.draft, "service", "url");
            ui.label(i18n::t("Server URL"));
            if ui
                .add(
                    egui::TextEdit::singleline(&mut url)
                        .desired_width(f32::INFINITY)
                        .hint_text("https://127.0.0.1:8899"),
                )
                .changed()
            {
                set_str(&mut self.draft, "service", "url", url);
                self.meta_registration_enabled = None;
            }
            ui.add_space(6.0);

            let ca_pem = get_str(&self.draft, "service", "caPem");
            ui.horizontal(|ui| {
                ui.label(if ca_pem.is_empty() {
                    i18n::t("Certificate: No custom CA imported")
                } else {
                    i18n::t("Certificate: PEM certificate configured")
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(if ca_pem.is_empty() {
                            i18n::t("Import Certificate")
                        } else {
                            i18n::t("Replace")
                        })
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_parent(window)
                            .set_title(i18n::t("Select Server Public Certificate (PEM)"))
                            .add_filter(i18n::t("Certificate"), &["crt", "pem", "cer"])
                            .pick_file()
                        {
                            match std::fs::read_to_string(&path)
                                .map_err(|e| e.to_string())
                                .and_then(|pem| {
                                    network::parse_certificate(&pem)
                                        .map(|(_, subject)| (pem, subject))
                                }) {
                                Ok((pem, subject)) => {
                                    set_str(&mut self.draft, "service", "caPem", pem);
                                    self.meta_registration_enabled = None;
                                    self.notify(
                                        i18n::tf(
                                            "Selected certificate: {subject}",
                                            &[("subject", &subject)],
                                        ),
                                        false,
                                    );
                                }
                                Err(e) => self.notify(e, true),
                            }
                        }
                    }
                    if !ca_pem.is_empty() && ui.button(i18n::t("Clear")).clicked() {
                        set_str(&mut self.draft, "service", "caPem", String::new());
                        self.meta_registration_enabled = None;
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let palette = ui::ctx_palette(ui.ctx());
                let accent_text = palette.accent_text(ui.visuals().dark_mode);
                match self.meta_registration_enabled {
                    Some(reg) => {
                        let status_text = if reg {
                            i18n::t("Connected · Registration Open")
                        } else {
                            i18n::t("Connected · Registration Closed")
                        };
                        ui.colored_label(accent_text, format!("\u{25CF} {}", status_text));
                    }
                    None => {
                        ui.label(
                            RichText::new(format!("\u{25CB} {}", i18n::t("Not checked"))).weak(),
                        );
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(i18n::t("Check Connection")).clicked() {
                        service::check_service_meta(
                            app,
                            get_str(&self.draft, "service", "url"),
                            get_str(&self.draft, "service", "caPem"),
                        );
                    }
                });
            });
        });

        if let Some(acc) = account {
            section_card(ui, i18n::t("Signed-In Account"), |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&acc.username).strong().size(15.0));
                    ui.label(RichText::new(format!("v{}", acc.revision)).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(i18n::t("Sign Out")).clicked() {
                            service::logout(app);
                        }
                    });
                });
                if !acc.last_sync.is_empty() {
                    ui.label(
                        RichText::new(i18n::tf(
                            "Last sync: {time}",
                            &[("time", &acc.last_sync)],
                        ))
                        .weak()
                        .small(),
                    );
                }
                ui.add_space(6.0);
                let mut auto_sync = get_bool(&self.draft, "service", "autoSync");
                setting_switch_row(
                    ui,
                    i18n::t("Auto-Sync Settings"),
                    i18n::t("Check for changes every 2 minutes; keep local settings on conflict."),
                    &mut auto_sync,
                    true,
                );
                set_bool(&mut self.draft, "service", "autoSync", auto_sync);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if icons::icon_text_button(
                        ui,
                        Icon::Save,
                        i18n::t("Download Cloud Settings"),
                        false,
                        false,
                        None,
                    )
                    .clicked()
                    {
                        let answer = rfd::MessageDialog::new()
                            .set_parent(window)
                            .set_title(i18n::t("Download Cloud Settings"))
                            .set_description(i18n::t(
                                "This will replace your local preferences, shortcuts, and provider settings. Continue?",
                            ))
                            .set_buttons(rfd::MessageButtons::YesNo)
                            .show();
                        if answer == rfd::MessageDialogResult::Yes {
                            service::sync_config(app, "download", false);
                        }
                    }
                    ui.add_enabled_ui(!changed, |ui| {
                        if icons::icon_text_button(
                            ui,
                            Icon::Upload,
                            i18n::t("Upload Local Settings"),
                            false,
                            !changed,
                            None,
                        )
                        .on_disabled_hover_text(i18n::t("Please save settings first"))
                        .clicked()
                        {
                            service::sync_config(app, "upload", false);
                        }
                    });
                });
            });
        } else {
            section_card(ui, i18n::t("Sign In"), |ui| {
                let mut username = get_str(&self.draft, "service", "username");
                ui.label(i18n::t("Username (3-40 characters)"));
                if ui
                    .add(egui::TextEdit::singleline(&mut username).desired_width(260.0))
                    .changed()
                {
                    set_str(&mut self.draft, "service", "username", username.clone());
                }
                ui.add_space(4.0);
                ui.label(i18n::t("Password (at least 10 characters)"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(260.0),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let can_submit = !username.trim().is_empty() && !self.password.is_empty();
                    let login_fill = ui::ctx_palette(ui.ctx()).accent;
                    if ui
                        .add_enabled(
                            can_submit,
                            egui::Button::new(
                                RichText::new(i18n::t("Sign In"))
                                    .color(Color32::WHITE)
                                    .strong(),
                            )
                            .fill(login_fill),
                        )
                        .clicked()
                    {
                        if let Err(e) = service::update_settings(app, &self.draft) {
                            self.notify(e, true);
                        } else {
                            service::login_or_register(
                                app,
                                get_str(&self.draft, "service", "url"),
                                get_str(&self.draft, "service", "caPem"),
                                username.clone(),
                                std::mem::take(&mut self.password),
                                false,
                            );
                        }
                    }
                    let reg_open = self.meta_registration_enabled == Some(true);
                    if ui
                        .add_enabled(
                            can_submit && reg_open,
                            egui::Button::new(i18n::t("Register")),
                        )
                        .clicked()
                    {
                        if let Err(e) = service::update_settings(app, &self.draft) {
                            self.notify(e, true);
                        } else {
                            service::login_or_register(
                                app,
                                get_str(&self.draft, "service", "url"),
                                get_str(&self.draft, "service", "caPem"),
                                username,
                                std::mem::take(&mut self.password),
                                true,
                            );
                        }
                    }
                });
            });
        }
    }
}

fn get_bool(val: &Value, p1: &str, p2: &str) -> bool {
    val[p1][p2].as_bool().unwrap_or(false)
}
fn set_bool(val: &mut Value, p1: &str, p2: &str, v: bool) {
    val[p1][p2] = json!(v);
}
fn get_i64(val: &Value, p1: &str, p2: &str, def: i64) -> i64 {
    val[p1][p2].as_i64().unwrap_or(def)
}
fn set_i64(val: &mut Value, p1: &str, p2: &str, v: i64) {
    val[p1][p2] = json!(v);
}
fn get_str(val: &Value, p1: &str, p2: &str) -> String {
    val[p1][p2].as_str().unwrap_or("").to_string()
}
fn set_str(val: &mut Value, p1: &str, p2: &str, v: String) {
    val[p1][p2] = json!(v);
}

fn section_card(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(14))
        .corner_radius(CornerRadius::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(14.5));
            ui.add_space(6.0);
            add_contents(ui);
        });
    ui.add_space(10.0);
}

fn setting_switch_row(
    ui: &mut egui::Ui,
    title: &str,
    desc: &str,
    value: &mut bool,
    enabled: bool,
) {
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).strong());
                if !desc.is_empty() {
                    ui.label(RichText::new(desc).weak().small());
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui::toggle_switch(ui, value);
            });
        });
    });
    ui.add_space(4.0);
}

fn setting_number_row(
    ui: &mut egui::Ui,
    title: &str,
    desc: Option<&str>,
    value: &mut i64,
    range: std::ops::RangeInclusive<i64>,
    suffix: &str,
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).strong());
            if let Some(d) = desc {
                ui.label(RichText::new(d).weak().small());
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(suffix);
            ui.add(egui::DragValue::new(value).range(range));
        });
    });
    ui.add_space(4.0);
}
