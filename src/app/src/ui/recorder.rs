//! Floating recording control bar for scrolling capture and GIF recording (excluded from screen capture).

use crate::i18n;
use crate::native;
use crate::recording;
use crate::state::{App, RecordingProgress};
use crate::ui;
use crate::ui::icons::{self, Icon};
use egui::{Color32, CornerRadius, RichText, Sense, Vec2};
use std::sync::Arc;

pub struct RecorderState {
    pub progress: RecordingProgress,
    pub stopping: bool,
    pub drag_origin: Option<((i32, i32), (i32, i32))>,
}

impl RecorderState {
    pub fn new(mode: String) -> Self {
        Self {
            progress: RecordingProgress {
                mode,
                frames: 0,
                elapsed: 0,
                encoding: false,
                progress: 0.0,
                scroll_mode: Some("auto".into()),
                hint: Some(i18n::t("Preparing capture...").into()),
            },
            stopping: false,
            drag_origin: None,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        app: &Arc<App>,
        window: &winit::window::Window,
        hwnd: Option<isize>,
    ) {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape))
            && (!self.stopping || self.progress.encoding)
        {
            self.stopping = true;
            recording::abort(app);
            return;
        }

        let palette = ui::ctx_palette(ctx);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(palette.floating_bg)
                    .stroke(egui::Stroke::new(1.5_f32, palette.accent))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                // Allow dragging empty background area to reposition the control bar.
                let bg_resp = ui.interact(
                    ui.max_rect(),
                    egui::Id::new("recorder_drag_bg"),
                    Sense::drag(),
                );
                if bg_resp.drag_started() {
                    let cursor = native::cursor_pos();
                    let wpos = window
                        .outer_position()
                        .map(|p| (p.x, p.y))
                        .unwrap_or((0, 0));
                    self.drag_origin = Some((cursor, wpos));
                } else if bg_resp.dragged() {
                    if let (Some((c0, w0)), Some(h)) = (self.drag_origin, hwnd) {
                        let c1 = native::cursor_pos();
                        native::move_window_physical(h, w0.0 + (c1.0 - c0.0), w0.1 + (c1.1 - c0.1));
                    }
                } else if bg_resp.drag_stopped() {
                    self.drag_origin = None;
                }

                ui.horizontal(|ui| {
                    let dot_color = if self.progress.encoding {
                        Color32::from_rgb(0xf6, 0xca, 0x45)
                    } else {
                        Color32::from_rgb(0xef, 0x53, 0x50)
                    };
                    let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().circle_filled(dot_rect.center(), 5.0, dot_color);

                    ui.vertical(|ui| {
                        let heading = if self.progress.encoding {
                            i18n::tf(
                                "Encoding GIF {pct}%",
                                &[("pct", &((self.progress.progress * 100.0).round() as i32).to_string())],
                            )
                        } else if self.progress.mode == "long" {
                            if self.progress.scroll_mode.as_deref() == Some("manual") {
                                i18n::t("Scroll down inside the selected region").to_string()
                            } else {
                                i18n::t("Auto-scrolling & stitching").to_string()
                            }
                        } else {
                            i18n::t("Recording GIF").to_string()
                        };
                        ui.label(RichText::new(heading).color(Color32::WHITE).strong());
                        let sub = if self.stopping && !self.progress.encoding {
                            i18n::tf(
                                "{sec} s · {frames} frames · Finishing...",
                                &[
                                    ("sec", &self.progress.elapsed.to_string()),
                                    ("frames", &self.progress.frames.to_string()),
                                ],
                            )
                        } else {
                            i18n::tf(
                                "{sec} s · {frames} frames",
                                &[
                                    ("sec", &self.progress.elapsed.to_string()),
                                    ("frames", &self.progress.frames.to_string()),
                                ],
                            )
                        };
                        ui.label(RichText::new(sub).color(Color32::from_gray(190)).small());
                        if self.progress.mode == "long" {
                            if let Some(hint) = &self.progress.hint {
                                ui.label(
                                    RichText::new(hint)
                                        .color(palette.accent_hint)
                                        .small(),
                                );
                            }
                        }
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!self.stopping || self.progress.encoding, |ui| {
                            if icons::dark_icon_text_button(
                                ui,
                                Icon::Close,
                                i18n::t("Cancel"),
                                Color32::from_rgb(0x3a, 0x24, 0x24),
                                Color32::from_rgb(0x52, 0x2e, 0x2e),
                            )
                            .clicked()
                            {
                                self.stopping = true;
                                recording::abort(app);
                            }
                        });
                        ui.add_enabled_ui(!self.stopping && !self.progress.encoding, |ui| {
                            if icons::dark_icon_text_button(
                                ui,
                                Icon::Stop,
                                i18n::t("Done"),
                                palette.accent,
                                palette.accent_hover,
                            )
                            .clicked()
                            {
                                self.stopping = true;
                                recording::stop(app);
                            }
                        });
                        if self.progress.mode == "long"
                            && self.progress.scroll_mode.as_deref() == Some("auto")
                        {
                            ui.add_enabled_ui(!self.stopping, |ui| {
                                if icons::dark_icon_text_button(
                                    ui,
                                    Icon::ScrollManual,
                                    i18n::t("Manual Scroll"),
                                    palette.floating_btn,
                                    palette.floating_btn_hover,
                                )
                                .clicked()
                                {
                                    recording::set_manual(
                                        app,
                                        i18n::t("Switched to manual scroll; scroll down slowly inside the selected region"),
                                    );
                                }
                            });
                        }
                    });
                });
            });
    }
}
