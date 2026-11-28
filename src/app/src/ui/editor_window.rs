//! Capture editor window opened when clicking Save after capturing.
//!
//! Layout: top annotation toolbar, central canvas, and bottom save bar (filename / format / folder / save).
//! Closes automatically on save; closing directly discards the capture unless held by a pin.

use crate::clipboard;
use crate::export::{self, SaveOptions};
use crate::i18n;
use crate::images::Rgba;
use crate::native;
use crate::service;
use crate::state::{App, AppEvent, CaptureItem, TextTarget};
use crate::ui::canvas::{self, AnnotationState};
use crate::ui::icons::{self, Icon};
use crate::ui::toast::{self, Toast};
use crate::ui::{self, GifPlayer};
use egui::{Color32, CornerRadius, Pos2, Rect, RichText, Vec2};
use serde_json::json;
use std::sync::Arc;

/// Bottom save bar state (remembers last directory, format, and quality).
pub struct SaveForm {
    pub animated: bool,
    pub file_name: String,
    pub directory: String,
    pub format: String,
    pub quality: i64,
    pub error: String,
}

impl SaveForm {
    fn new(app: &App, item: &CaptureItem) -> Self {
        let (directory, saved_fmt, quality) = service::save_defaults(app);
        let animated = item.kind == "gif";
        let format = if animated {
            "gif"
        } else if saved_fmt == "jpg" {
            "jpg"
        } else {
            "png"
        };
        Self {
            animated,
            file_name: format!("SnapDock_{}", native::local_timestamp()),
            directory,
            format: format.to_string(),
            quality,
            error: String::new(),
        }
    }

    fn validate(&self) -> Result<SaveOptions, String> {
        export::save_options(
            &json!({
                "directory": self.directory,
                "fileName": self.file_name,
                "format": self.format,
                "quality": self.quality,
            }),
            self.animated,
        )
    }
}

/// Right-hand OCR / translation side panel.
struct TextPanel {
    tab: usize, // 0 = ocr, 1 = translation
    ocr: String,
    translated: String,
    busy: Option<String>,
    src_lang_query: String,
    tgt_lang_query: String,
}

pub struct EditorState {
    pub id: String,
    pub item: CaptureItem,
    base: Option<Rgba>,
    annotations: AnnotationState,
    gif_player: Option<GifPlayer>,
    save: SaveForm,
    text: Option<TextPanel>,
    pub toast: Option<Toast>,
}

impl EditorState {
    /// Load capture result from the in-memory session store.
    pub fn load(app: &App, id: &str) -> Option<Self> {
        let (item, base, strokes, gif_frames) = {
            let core = app.core.lock().unwrap();
            let s = core.captures.get(id)?;
            (
                s.item.clone(),
                s.source_rgba.clone().or_else(|| s.composed_rgba.clone()),
                s.annotations.clone(),
                s.gif_frames.clone(),
            )
        };
        let mut annotations = AnnotationState::new(app);
        annotations.load(strokes);
        let gif_player = if item.kind == "gif" {
            gif_frames.and_then(GifPlayer::new)
        } else {
            None
        };
        Some(Self {
            id: id.to_string(),
            save: SaveForm::new(app, &item),
            base: base.or_else(|| service::static_rgba(app, id).ok()),
            item,
            annotations,
            gif_player,
            text: None,
            toast: None,
        })
    }

    pub fn is_gif(&self) -> bool {
        self.item.kind == "gif"
    }

    pub fn title(&self) -> String {
        let action = if self.is_gif() {
            i18n::t("Edit GIF")
        } else {
            i18n::t("Edit")
        };
        format!(
            "{} · {}  {} \u{00D7} {}",
            i18n::t("SnapDock"),
            action,
            self.item.width,
            self.item.height
        )
    }

    pub fn refresh_item_metadata(&mut self, app: &App) {
        if let Some(s) = app.core.lock().unwrap().captures.get(&self.id) {
            self.item = s.item.clone();
        }
    }

    pub fn apply_text_result(&mut self, kind: &str, result: Result<String, String>) {
        let Some(panel) = self.text.as_mut() else { return };
        panel.busy = None;
        match result {
            Ok(text) if kind == "ocr" => panel.ocr = text,
            Ok(text) => panel.translated = text,
            Err(e) => self.toast = Some(Toast::new(e, true)),
        }
    }

    /// Commit pending annotation edits back to the session (pins refresh automatically).
    pub fn commit_annotations(&mut self, app: &Arc<App>) {
        if self.is_gif() {
            return;
        }
        self.annotations.finish_text();
        if self.annotations.dirty {
            let strokes = self.annotations.all_strokes_snapshot();
            match service::update_image_annotations(app, &self.id, strokes) {
                Ok(()) => self.annotations.dirty = false,
                Err(e) => self.toast = Some(Toast::new(e, true)),
            }
        }
    }

    fn copy(&mut self, app: &Arc<App>) {
        self.commit_annotations(app);
        self.toast = Some(match service::copy_image(app, &self.id) {
            Ok(()) => Toast::new(i18n::t("Copied to clipboard"), false),
            Err(e) => Toast::new(e, true),
        });
    }

    fn pin(&mut self, app: &Arc<App>) {
        self.commit_annotations(app);
        app.emit(AppEvent::CreatePin {
            id: self.id.clone(),
        });
        self.toast = Some(Toast::new(i18n::t("Pinned to desktop"), false));
    }

    fn save(&mut self, app: &Arc<App>, window: &winit::window::Window) {
        self.commit_annotations(app);
        let opts = match self.save.validate() {
            Ok(o) => o,
            Err(e) => {
                self.save.error = e;
                return;
            }
        };
        match service::save_image_with_options(app, &self.id, &opts, Some(window)) {
            Ok(true) => {
                // Close window first so the confirmation appears as a floating toast.
                service::close_result(app, Some(&self.id));
                app.notify(
                    i18n::tf("Saved: {path}", &[("path", &opts.file_path)]),
                    false,
                );
            }
            Ok(false) => {} // Overwrite cancelled.
            Err(e) => self.save.error = e,
        }
    }

    pub fn start_ocr(&mut self, app: &Arc<App>) {
        self.run_text(app, "ocr");
    }

    fn run_text(&mut self, app: &Arc<App>, kind: &str) {
        let panel = self.text.get_or_insert_with(|| TextPanel {
            tab: 0,
            ocr: String::new(),
            translated: String::new(),
            busy: None,
            src_lang_query: String::new(),
            tgt_lang_query: String::new(),
        });
        panel.tab = if kind == "ocr" { 0 } else { 1 };
        panel.busy = Some(kind.to_string());
        let existing = (kind == "translation" && !panel.ocr.trim().is_empty())
            .then(|| panel.ocr.clone());
        service::run_text_process(
            app,
            TextTarget::Editor {
                capture_id: self.id.clone(),
            },
            kind.into(),
            Some(self.id.clone()),
            existing,
        );
    }

    pub fn show(&mut self, ctx: &egui::Context, app: &Arc<App>, window: &winit::window::Window) {
        self.handle_shortcuts(ctx, app, window);
        self.show_toolbar(ctx, app);
        self.show_save_bar(ctx, app, window);
        self.show_text_panel(ctx, app);
        self.show_canvas(ctx);
        toast::show_in_window(ctx, &mut self.toast, 64.0);
    }

    fn handle_shortcuts(
        &mut self,
        ctx: &egui::Context,
        app: &Arc<App>,
        window: &winit::window::Window,
    ) {
        let typing = ctx.wants_keyboard_input();
        let (esc, ctrl_z, ctrl_c, ctrl_s, del, shift, arrow) = ctx.input(|i| {
            let dx = (i.key_pressed(egui::Key::ArrowRight) as i32)
                - (i.key_pressed(egui::Key::ArrowLeft) as i32);
            let dy = (i.key_pressed(egui::Key::ArrowDown) as i32)
                - (i.key_pressed(egui::Key::ArrowUp) as i32);
            (
                i.key_pressed(egui::Key::Escape),
                i.modifiers.command && i.key_pressed(egui::Key::Z),
                i.modifiers.command && i.key_pressed(egui::Key::C),
                i.modifiers.command && i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
                i.modifiers.shift,
                (dx, dy),
            )
        });
        if ctrl_s {
            self.save(app, window);
            return;
        }
        if esc {
            if self.annotations.has_selection() {
                self.annotations.finish_text();
            } else if !typing {
                service::close_result(app, Some(&self.id));
            }
            return;
        }
        if typing {
            return;
        }
        if del && self.annotations.has_selection() {
            self.annotations.delete_selected();
        } else if (arrow.0 != 0 || arrow.1 != 0) && self.annotations.has_selection() {
            let step = if shift { 10.0 } else { 1.0 };
            self.annotations
                .nudge_selected(arrow.0 as f32 * step, arrow.1 as f32 * step);
        } else if ctrl_z {
            self.annotations.undo();
        } else if ctrl_c {
            self.copy(app);
        }
    }

    fn show_toolbar(&mut self, ctx: &egui::Context, app: &Arc<App>) {
        let gif = self.is_gif();
        let mut action: Option<&'static str> = None;
        egui::TopBottomPanel::top("editor_toolbar")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if gif {
                        let frames = self.gif_player.as_ref().map(|p| p.frames.len()).unwrap_or(0);
                        let accent_text =
                            ui::ctx_palette(ctx).accent_text(ctx.style().visuals.dark_mode);
                        icons::paint_inline(ui, Icon::RecordGif, 16.0, accent_text);
                        ui.label(RichText::new(i18n::t("Animated GIF")).strong());
                        ui.label(
                            RichText::new(i18n::tf(
                                "{count} frames",
                                &[("count", &frames.to_string())],
                            ))
                            .weak(),
                        );
                    } else {
                        canvas::show_tool_buttons(ui, &mut self.annotations, false);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::icon_text_button(
                            ui,
                            Icon::Copy,
                            i18n::t("Copy"),
                            false,
                            false,
                            None,
                        )
                        .on_hover_text(i18n::t("Copy to clipboard Ctrl+C"))
                        .clicked()
                        {
                            action = Some("copy");
                        }
                        if icons::icon_text_button(
                            ui,
                            Icon::Pin,
                            i18n::t("Pin"),
                            false,
                            false,
                            None,
                        )
                        .on_hover_text(i18n::t("Pin to desktop"))
                        .clicked()
                        {
                            action = Some("pin");
                        }
                        let busy = self.text.as_ref().is_some_and(|t| t.busy.is_some());
                        ui.add_enabled_ui(!busy, |ui| {
                            icons::icon_menu_button(
                                ui,
                                "editor_text_menu",
                                Icon::Ocr,
                                i18n::t("Text"),
                                |ui| {
                                    if icons::icon_text_button(
                                        ui,
                                        Icon::Ocr,
                                        i18n::t("Text Recognition (OCR)"),
                                        false,
                                        false,
                                        None,
                                    )
                                    .clicked()
                                    {
                                        action = Some("ocr");
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                    if icons::icon_text_button(
                                        ui,
                                        Icon::Translate,
                                        i18n::t("Translate"),
                                        false,
                                        false,
                                        None,
                                    )
                                    .clicked()
                                    {
                                        action = Some("translation");
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                },
                            );
                        });
                    });
                });
                if !gif {
                    canvas::show_tool_options(ui, app, &mut self.annotations);
                }
            });
        match action {
            Some("copy") => self.copy(app),
            Some("pin") => self.pin(app),
            Some(kind @ ("ocr" | "translation")) => self.run_text(app, kind),
            _ => {}
        }
    }

    fn show_save_bar(&mut self, ctx: &egui::Context, app: &Arc<App>, window: &winit::window::Window) {
        let mut do_save = false;
        egui::TopBottomPanel::bottom("editor_save_bar")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ctx, |ui| {
                let form = &mut self.save;
                ui.horizontal(|ui| {
                    ui.label(i18n::t("Filename"));
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut form.file_name)
                            .desired_width(170.0)
                            .char_limit(180),
                    );
                    if resp.changed() {
                        form.error.clear();
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        do_save = true;
                    }
                    if form.animated {
                        ui.label(RichText::new(".gif").strong());
                    } else {
                        egui::ComboBox::from_id_salt("editor_fmt")
                            .selected_text(format!(".{}", form.format))
                            .width(58.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut form.format,
                                    "png".to_string(),
                                    i18n::t(".png  Lossless"),
                                )
                                .on_hover_text(i18n::t("Preserves transparency"));
                                ui.selectable_value(
                                    &mut form.format,
                                    "jpg".to_string(),
                                    i18n::t(".jpg  Compact"),
                                )
                                .on_hover_text(i18n::t("White background, ideal for sharing"));
                            });
                        if form.format == "jpg" {
                            ui.add(
                                egui::DragValue::new(&mut form.quality)
                                    .range(10..=100)
                                    .suffix("%"),
                            )
                            .on_hover_text(i18n::t("JPEG Quality"));
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(i18n::t("Location"));
                    let dir_w = (ui.available_width() - 150.0).max(120.0);
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut form.directory)
                                .desired_width(dir_w)
                                .hint_text(i18n::t("Save folder")),
                        )
                        .changed()
                    {
                        form.error.clear();
                    }
                    if icons::small_icon_button(ui, Icon::Folder)
                        .on_hover_text(i18n::t("Choose folder"))
                        .clicked()
                    {
                        let mut dlg = rfd::FileDialog::new()
                            .set_title(i18n::t("Select Save Folder"))
                            .set_parent(window);
                        if !form.directory.is_empty() {
                            dlg = dlg.set_directory(&form.directory);
                        }
                        if let Some(dir) = dlg.pick_folder() {
                            form.directory = dir.to_string_lossy().to_string();
                            form.error.clear();
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::icon_text_button(
                            ui,
                            Icon::Save,
                            i18n::t("Save"),
                            false,
                            true,
                            None,
                        )
                        .on_hover_text(i18n::t("Save and close Ctrl+S"))
                        .clicked()
                        {
                            do_save = true;
                        }
                    });
                });
                if !form.error.is_empty() {
                    ui.colored_label(ui::DANGER, &form.error);
                }
            });
        if do_save {
            self.save(app, window);
        }
    }

    fn show_text_panel(&mut self, ctx: &egui::Context, app: &Arc<App>) {
        let Some(panel) = self.text.as_mut() else { return };
        let mut close = false;
        let mut copy: Option<String> = None;
        let mut retranslate = false;
        let (mut cur_src_val, mut cur_tgt_val) = {
            let core = app.core.lock().unwrap();
            (
                core.settings["providers"]["translation"]["source"]
                    .as_str()
                    .unwrap_or("auto")
                    .to_string(),
                core.settings["providers"]["translation"]["target"]
                    .as_str()
                    .unwrap_or("en")
                    .to_string(),
            )
        };
        let mut langs_changed = false;

        egui::SidePanel::right("editor_text_panel")
            .resizable(true)
            .default_width(300.0)
            .min_width(240.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if icons::small_icon_text_button(
                        ui,
                        Icon::Ocr,
                        i18n::t("Recognized Text"),
                        panel.tab == 0,
                        false,
                    )
                    .clicked()
                    {
                        panel.tab = 0;
                    }
                    if icons::small_icon_text_button(
                        ui,
                        Icon::Translate,
                        i18n::t("Translation Result"),
                        panel.tab == 1,
                        false,
                    )
                    .clicked()
                    {
                        panel.tab = 1;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::small_icon_button(ui, Icon::Close)
                            .on_hover_text(i18n::t("Close"))
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
                ui.separator();

                if panel.tab == 1 {
                    let src_code = crate::translate::normalize_lang_code(&cur_src_val);
                    let tgt_code = crate::translate::normalize_lang_code(&cur_tgt_val);
                    let picker_w = ((ui.available_width() - 44.0) * 0.5).clamp(84.0, 150.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if let Some(picked) = ui::searchable_language_picker(
                            ui,
                            "editor_trans_src",
                            &src_code,
                            &cur_src_val,
                            true,
                            picker_w,
                            &mut panel.src_lang_query,
                        ) {
                            cur_src_val = picked;
                            langs_changed = true;
                        }
                        if icons::icon_button_sized(
                            ui,
                            Icon::Swap,
                            Vec2::splat(28.0),
                            15.0,
                            false,
                            false,
                        )
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
                            cur_src_val = new_src;
                            cur_tgt_val = new_tgt_code.to_string();
                            langs_changed = true;
                        }
                        if let Some(picked) = ui::searchable_language_picker(
                            ui,
                            "editor_trans_tgt",
                            &tgt_code,
                            &cur_tgt_val,
                            false,
                            picker_w,
                            &mut panel.tgt_lang_query,
                        ) {
                            cur_tgt_val = picked;
                            langs_changed = true;
                        }
                    });
                    ui.add_space(4.0);
                }

                if let Some(busy) = &panel.busy {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                        ui.label(if busy == "ocr" {
                            i18n::t("Recognizing...")
                        } else {
                            i18n::t("Translating...")
                        });
                    });
                    return;
                }
                let buf = if panel.tab == 0 {
                    &mut panel.ocr
                } else {
                    &mut panel.translated
                };
                let text_h = (ui.available_height() - 40.0).max(80.0);
                egui::ScrollArea::vertical().max_height(text_h).show(ui, |ui| {
                    ui.add_sized(
                        [ui.available_width(), text_h],
                        egui::TextEdit::multiline(buf)
                            .hint_text(i18n::t("Results will appear here.")),
                    );
                });
                ui.horizontal(|ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        icons::paint_inline(
                            ui,
                            Icon::SettingsGeneral,
                            13.0,
                            ui.visuals().hyperlink_color,
                        );
                        if ui.link(i18n::t("Provider Settings")).clicked() {
                            app.emit(AppEvent::OpenSettings {
                                section: Some("providers".into()),
                            });
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!buf.is_empty(), |ui| {
                            if icons::icon_text_button(
                                ui,
                                Icon::Copy,
                                i18n::t("Copy Text"),
                                false,
                                false,
                                None,
                            )
                            .clicked()
                            {
                                copy = Some(buf.clone());
                            }
                        });
                        if panel.tab == 1
                            && icons::icon_text_button(
                                ui,
                                Icon::Translate,
                                i18n::t("Translate"),
                                false,
                                true,
                                None,
                            )
                            .on_hover_text(i18n::t("Re-translate with current languages"))
                            .clicked()
                        {
                            retranslate = true;
                        }
                    });
                });
            });

        if langs_changed {
            let mut next_settings = app.core.lock().unwrap().settings.clone();
            next_settings["providers"]["translation"]["source"] = json!(cur_src_val);
            next_settings["providers"]["translation"]["target"] = json!(cur_tgt_val);
            if let Err(e) = service::update_settings(app, &next_settings) {
                self.toast = Some(Toast::new(e, true));
            }
        }
        if retranslate {
            self.run_text(app, "translation");
        }
        if close {
            self.text = None;
        }
        if let Some(text) = copy {
            self.toast = Some(match clipboard::write_text(&text) {
                Ok(()) => Toast::new(i18n::t("Text copied"), false),
                Err(e) => Toast::new(e, true),
            });
        }
    }

    fn show_canvas(&mut self, ctx: &egui::Context) {
        let ppp = ctx.pixels_per_point();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(ctx.style().visuals.faint_bg_color)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                if let Some(player) = self.gif_player.as_mut() {
                    if let Some(tex) = player.current_texture(ui.ctx(), "editor_gif") {
                        let rect = fit_rect(
                            avail,
                            self.item.width.max(1) as f32 / ppp,
                            self.item.height.max(1) as f32 / ppp,
                        );
                        paint_shadow(ui, rect);
                        ui.painter().image(
                            tex.id(),
                            rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                } else if let Some(base) = self.base.as_ref() {
                    // Display 1:1 in physical pixels, scaling down proportionally if larger than viewport.
                    let rect = fit_rect(
                        avail,
                        base.width.max(1) as f32 / ppp,
                        base.height.max(1) as f32 / ppp,
                    );
                    paint_shadow(ui, rect);
                    canvas::show_annotation_canvas(ui, &mut self.annotations, base, rect, false);
                }
            });
    }
}

fn paint_shadow(ui: &egui::Ui, rect: Rect) {
    ui.painter().rect_filled(
        rect.expand(1.0).translate(Vec2::new(0.0, 2.0)),
        CornerRadius::same(2),
        Color32::from_black_alpha(40),
    );
}

fn fit_rect(avail: Rect, w: f32, h: f32) -> Rect {
    let scale = (avail.width() / w).min(avail.height() / h).min(1.0);
    Rect::from_center_size(avail.center(), Vec2::new((w * scale).max(1.0), (h * scale).max(1.0)))
}
