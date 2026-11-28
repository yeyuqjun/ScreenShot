//! Fullscreen capture overlay window (per-monitor 1:1 physical alignment, window/control snapping,
//! pixel magnifier, selection drag/resize, inline annotations, and scrolling capture prep bar).

use crate::annotations::Tool;
use crate::clipboard;
use crate::i18n;
use crate::images::{self, PixelRect, Rgba};
use crate::service::{self, SelectAction};
use crate::state::App;
use crate::ui;
use crate::ui::canvas::{self, AnnotationState, ToolbarAction};
use crate::ui::icons::{self, Icon};
use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, Stroke as EguiStroke,
    TextureHandle, TextureOptions, Vec2,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragKind {
    New,
    Move,
    Resize { left: bool, right: bool, top: bool, bottom: bool },
}

struct DragOp {
    kind: DragKind,
    start_px: Pos2,
    initial_px: Rect,
}

type AsyncHoverSlot = Arc<Mutex<Option<(u64, Option<(Rect, String)>)>>>;

pub struct OverlayState {
    pub entry_index: usize,
    pub mode: String,
    pub screen_image: Arc<Rgba>,
    pub screen_tex: Option<TextureHandle>,
    /// Selection rectangle in physical pixel coordinates (`0..width x 0..height`).
    pub selection_px: Option<Rect>,
    /// Hovered window snap target (physical pixel rectangle + title).
    pub hover_window: Option<(Rect, String)>,
    pub last_hover_check: Instant,
    pub detect_windows: bool,
    pub detect_controls: bool,
    hover_seq: u64,
    async_hover: AsyncHoverSlot,
    pub show_magnifier: bool,
    drag: Option<DragOp>,
    pub cropped_base: Option<Rgba>,
    pub last_crop_rect: Option<PixelRect>,
    pub annotations: AnnotationState,
    pub long_ready: bool,
    pub scroll_mode: String, // "auto" | "manual"
    pub error: String,
    pub copy_feedback: Option<(String, Instant)>,
    last_toolbar_size: Vec2,
}

impl OverlayState {
    pub fn new(app: &App, entry_index: usize, mode: String, screen_image: Arc<Rgba>) -> Self {
        let (detect_windows, detect_controls, show_magnifier) = {
            let core = app.core.lock().unwrap();
            (
                core.settings["capture"]["detectWindows"]
                    .as_bool()
                    .unwrap_or(true),
                core.settings["capture"]["detectControls"]
                    .as_bool()
                    .unwrap_or(false),
                core.settings["capture"]["magnifier"]
                    .as_bool()
                    .unwrap_or(true),
            )
        };
        let long_ready = mode == "long";
        Self {
            entry_index,
            mode,
            screen_image,
            screen_tex: None,
            selection_px: None,
            hover_window: None,
            last_hover_check: Instant::now() - Duration::from_secs(1),
            detect_windows,
            detect_controls,
            hover_seq: 0,
            async_hover: Arc::new(Mutex::new(None)),
            show_magnifier,
            drag: None,
            cropped_base: None,
            last_crop_rect: None,
            annotations: AnnotationState::new(app),
            long_ready,
            scroll_mode: "auto".into(),
            error: String::new(),
            copy_feedback: None,
            last_toolbar_size: Vec2::new(490.0, 44.0),
        }
    }

    fn current_pixel_rect(&self) -> Option<PixelRect> {
        let sel = self.selection_px?;
        let x = (sel.min.x.floor() as i32).clamp(0, self.screen_image.width as i32 - 1) as u32;
        let y = (sel.min.y.floor() as i32).clamp(0, self.screen_image.height as i32 - 1) as u32;
        let w = (sel.width().round() as u32).clamp(1, self.screen_image.width - x);
        let h = (sel.height().round() as u32).clamp(1, self.screen_image.height - y);
        if w < 3 || h < 3 {
            return None;
        }
        Some(PixelRect {
            x,
            y,
            width: w,
            height: h,
        })
    }

    fn ensure_cropped_base(&mut self) {
        let Some(rect) = self.current_pixel_rect() else {
            self.cropped_base = None;
            self.last_crop_rect = None;
            return;
        };
        let same = self.last_crop_rect.is_some_and(|r| {
            r.x == rect.x && r.y == rect.y && r.width == rect.width && r.height == rect.height
        });
        if !same || self.cropped_base.is_none() {
            if let Ok(cropped) = images::crop_pixels(&self.screen_image, rect) {
                self.cropped_base = Some(cropped);
                self.last_crop_rect = Some(rect);
                self.annotations.texture_dirty = true;
            }
        }
    }

    fn default_confirm_action(&self) -> SelectAction {
        if self.mode == "gif" {
            SelectAction::Gif
        } else if self.long_ready {
            SelectAction::Long
        } else if self.mode == "pin" {
            SelectAction::Pin
        } else {
            SelectAction::Copy
        }
    }

    fn commit_selection(&mut self, app: &Arc<App>, action: SelectAction) {
        let Some(rect) = self.current_pixel_rect() else {
            return;
        };
        if action == SelectAction::Long && !self.long_ready {
            self.annotations.finish_text();
            self.annotations.tool = Tool::Select;
            self.long_ready = true;
            return;
        }
        self.annotations.finish_text();
        let strokes = self.annotations.all_strokes_snapshot();
        let effective_action = if self.long_ready {
            SelectAction::Long
        } else {
            action
        };
        if let Err(e) = service::select_capture(
            app,
            self.entry_index,
            rect,
            strokes,
            effective_action,
            &self.scroll_mode,
        ) {
            self.error = e;
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        app: &Arc<App>,
        _window: &winit::window::Window,
    ) {
        let screen = ctx.screen_rect();
        let img_w = self.screen_image.width.max(1) as f32;
        let img_h = self.screen_image.height.max(1) as f32;
        let screen_w = screen.width().max(1.0);
        let screen_h = screen.height().max(1.0);

        // Exact logical <-> physical pixel coordinate conversion (avoids fractional DPI drift).
        let to_px = |p: Pos2| -> Pos2 {
            Pos2::new(
                ((p.x - screen.min.x) * img_w / screen_w).clamp(0.0, img_w),
                ((p.y - screen.min.y) * img_h / screen_h).clamp(0.0, img_h),
            )
        };
        let to_logical_rect = |r: Rect| -> Rect {
            Rect::from_min_max(
                Pos2::new(
                    screen.min.x + r.min.x * screen_w / img_w,
                    screen.min.y + r.min.y * screen_h / img_h,
                ),
                Pos2::new(
                    screen.min.x + r.max.x * screen_w / img_w,
                    screen.min.y + r.max.y * screen_h / img_h,
                ),
            )
        };

        // Clear expired color-copy feedback banner.
        if let Some((_, exp)) = &self.copy_feedback {
            let now = Instant::now();
            if now >= *exp {
                self.copy_feedback = None;
            } else {
                ctx.request_repaint_after(exp.saturating_duration_since(now));
            }
        }

        // Poll async UI control detection result (avoids blocking the UI thread when detectControls is enabled).
        if self.detect_controls && self.selection_px.is_none() {
            if let Ok(mut slot) = self.async_hover.lock() {
                if let Some((seq, result)) = slot.take() {
                    if seq == self.hover_seq {
                        self.hover_window = result;
                    }
                }
            }
        }

        // Upload frozen fullscreen background texture once (clamped to GPU max texture dimension).
        if self.screen_tex.is_none() {
            let max_side = ctx.input(|i| i.max_texture_side);
            let color_img = ui::rgba_opaque_to_color_image_fit(&self.screen_image, max_side);
            self.screen_tex = Some(ctx.load_texture(
                format!("overlay_bg_{}", self.entry_index),
                color_img,
                TextureOptions::NEAREST,
            ));
        }

        // Keyboard shortcuts.
        {
            let wants_kb = ctx.wants_keyboard_input();
            let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if esc {
                if self.annotations.has_selection() {
                    self.annotations.finish_text();
                    return;
                }
                service::cancel_capture(app);
                return;
            }
            if !wants_kb {
                let (enter, ctrl_a, ctrl_z, ctrl_c, ctrl_s, key_c, del, shift, arrow) =
                    ctx.input(|i| {
                        let dx = (i.key_pressed(egui::Key::ArrowRight) as i32)
                            - (i.key_pressed(egui::Key::ArrowLeft) as i32);
                        let dy = (i.key_pressed(egui::Key::ArrowDown) as i32)
                            - (i.key_pressed(egui::Key::ArrowUp) as i32);
                        (
                            i.key_pressed(egui::Key::Enter),
                            i.modifiers.command && i.key_pressed(egui::Key::A),
                            i.modifiers.command && i.key_pressed(egui::Key::Z),
                            i.modifiers.command && i.key_pressed(egui::Key::C),
                            i.modifiers.command && i.key_pressed(egui::Key::S),
                            !i.modifiers.any() && i.key_pressed(egui::Key::C),
                            i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
                            i.modifiers.shift,
                            (dx, dy),
                        )
                    });
                if ctrl_a {
                    self.annotations.reset();
                    self.annotations.tool = Tool::Select;
                    self.hover_window = None;
                    self.selection_px =
                        Some(Rect::from_min_max(Pos2::ZERO, Pos2::new(img_w, img_h)));
                } else if enter && self.selection_px.is_some() {
                    let act = self.default_confirm_action();
                    self.commit_selection(app, act);
                    return;
                } else if del && self.annotations.has_selection() {
                    self.annotations.delete_selected();
                } else if ctrl_z {
                    self.annotations.undo();
                } else if ctrl_c && self.selection_px.is_some() && !self.long_ready {
                    self.commit_selection(app, SelectAction::Copy);
                    return;
                } else if ctrl_s && self.selection_px.is_some() && !self.long_ready {
                    self.commit_selection(app, SelectAction::Save);
                } else if key_c {
                    // Press C to copy the HEX color under the cursor.
                    if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                        let px = to_px(pos);
                        let (r, g, b) = sample_pixel(&self.screen_image, px.x as u32, px.y as u32);
                        let hex = format!("#{:02X}{:02X}{:02X}", r, g, b);
                        if clipboard::write_text(&hex).is_ok() {
                            self.copy_feedback = Some((
                                i18n::tf("Copied color {hex}", &[("hex", &hex)]),
                                Instant::now() + Duration::from_millis(1500),
                            ));
                            ctx.request_repaint_after(Duration::from_millis(1500));
                        }
                    }
                } else if arrow.0 != 0 || arrow.1 != 0 {
                    let step = if shift { 10.0 } else { 1.0 };
                    if self.annotations.has_selection() {
                        self.annotations
                            .nudge_selected(arrow.0 as f32 * step, arrow.1 as f32 * step);
                    } else if let Some(sel) = self.selection_px {
                        let w = sel.width();
                        let h = sel.height();
                        let nx = (sel.min.x + arrow.0 as f32 * step).clamp(0.0, img_w - w);
                        let ny = (sel.min.y + arrow.1 as f32 * step).clamp(0.0, img_h - h);
                        self.selection_px =
                            Some(Rect::from_min_size(Pos2::new(nx, ny), Vec2::new(w, h)));
                    }
                }
            }
        }

        let is_annotating = self.selection_px.is_some()
            && !self.long_ready
            && self.drag.is_none()
            && (self.annotations.tool != Tool::Select || self.annotations.has_selection());

        // Right-click shortcut: deselect active element or exit annotation mode first, otherwise cancel capture.
        if !ctx.is_pointer_over_area()
            && ctx.input(|i| i.pointer.secondary_clicked())
        {
            if self.annotations.has_selection() {
                self.annotations.finish_text();
            } else if is_annotating {
                self.annotations.tool = Tool::Select;
            } else {
                service::cancel_capture(app);
                return;
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                // 1. Draw frozen fullscreen image.
                if let Some(tex) = &self.screen_tex {
                    ui.painter().image(
                        tex.id(),
                        screen,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                // 2. Draw outer dim shade.
                let palette = ui::ctx_palette(ctx);
                let shade = palette.overlay_shade_color();
                let active_rect_logical = self
                    .selection_px
                    .filter(|r| r.width() >= 2.0 && r.height() >= 2.0)
                    .map(to_logical_rect)
                    .or_else(|| self.hover_window.as_ref().map(|(r, _)| to_logical_rect(*r)));

                if let Some(sel) = active_rect_logical {
                    // Four shaded strips around the selection to keep the selected area at full brightness.
                    ui.painter().rect_filled(
                        Rect::from_min_max(screen.min, Pos2::new(screen.max.x, sel.min.y)),
                        0.0,
                        shade,
                    );
                    ui.painter().rect_filled(
                        Rect::from_min_max(Pos2::new(screen.min.x, sel.max.y), screen.max),
                        0.0,
                        shade,
                    );
                    ui.painter().rect_filled(
                        Rect::from_min_max(
                            Pos2::new(screen.min.x, sel.min.y),
                            Pos2::new(sel.min.x, sel.max.y),
                        ),
                        0.0,
                        shade,
                    );
                    ui.painter().rect_filled(
                        Rect::from_min_max(
                            Pos2::new(sel.max.x, sel.min.y),
                            Pos2::new(screen.max.x, sel.max.y),
                        ),
                        0.0,
                        shade,
                    );
                } else {
                    ui.painter().rect_filled(screen, 0.0, shade);
                }

                // 3. Delegate interaction inside the selection to the shared annotation canvas when annotating.
                if self.selection_px.is_some() && !self.long_ready {
                    self.ensure_cropped_base();
                    if let (Some(sel_px), Some(base)) = (self.selection_px, self.cropped_base.clone())
                    {
                        let sel_logical = to_logical_rect(sel_px);
                        if !self.annotations.strokes.is_empty()
                            || self.annotations.draft.is_some()
                            || self.annotations.has_selection()
                            || is_annotating
                        {
                            canvas::show_annotation_canvas(
                                ui,
                                &mut self.annotations,
                                &base,
                                sel_logical,
                                !is_annotating,
                            );
                        }
                    }
                }

                // 4. Fullscreen selection interaction (draw / move / 8-way resize / window snap).
                if !is_annotating {
                    let resp = ui.allocate_rect(screen, Sense::click_and_drag());

                    if resp.double_clicked() && self.selection_px.is_some() {
                        let act = self.default_confirm_action();
                        self.commit_selection(app, act);
                        return;
                    }

                    let pointer_pos = ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));

                    // Dynamic cursor feedback (crosshair / grab / 8-way resize).
                    if !ctx.is_pointer_over_area() {
                        let icon = if let Some(drag) = &self.drag {
                            cursor_for_drag(drag.kind, true)
                        } else if let (Some(sel), Some(pos)) = (self.selection_px, pointer_pos) {
                            hit_test_selection(to_logical_rect(sel), pos)
                                .map(|k| cursor_for_drag(k, false))
                                .unwrap_or(CursorIcon::Crosshair)
                        } else {
                            CursorIcon::Crosshair
                        };
                        ctx.set_cursor_icon(icon);
                    }

                    // Window/control hover snapping (polled every 85ms while no region is selected).
                    if self.selection_px.is_none()
                        && self.drag.is_none()
                        && self.detect_windows
                        && self.last_hover_check.elapsed().as_millis() >= 85
                    {
                        if let Some(pos) = pointer_pos {
                            self.last_hover_check = Instant::now();
                            let px = to_px(pos);
                            if self.detect_controls {
                                self.hover_seq = self.hover_seq.wrapping_add(1);
                                let seq = self.hover_seq;
                                let app_bg = app.clone();
                                let entry_index = self.entry_index;
                                let slot = self.async_hover.clone();
                                let ctx_clone = ctx.clone();
                                let (ix, iy) = (px.x as i32, px.y as i32);
                                app.runtime.spawn_blocking(move || {
                                    let found = service::hover_window(&app_bg, entry_index, ix, iy)
                                        .map(|(x, y, w, h, title)| {
                                            (
                                                Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)),
                                                title,
                                            )
                                        });
                                    if let Ok(mut guard) = slot.lock() {
                                        *guard = Some((seq, found));
                                    }
                                    ctx_clone.request_repaint();
                                });
                            } else {
                                self.hover_window = service::hover_window(
                                    app,
                                    self.entry_index,
                                    px.x as i32,
                                    px.y as i32,
                                )
                                .map(|(x, y, w, h, title)| {
                                    (Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)), title)
                                });
                            }
                        }
                    }

                    if resp.drag_started_by(egui::PointerButton::Primary) {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let px = to_px(pos);
                            let kind = self
                                .selection_px
                                .and_then(|sel| hit_test_selection(to_logical_rect(sel), pos))
                                .unwrap_or(DragKind::New);
                            if kind == DragKind::New {
                                self.annotations.reset();
                                self.selection_px = Some(Rect::from_min_max(px, px));
                            }
                            let initial_px =
                                self.selection_px.unwrap_or_else(|| Rect::from_min_max(px, px));
                            self.drag = Some(DragOp {
                                kind,
                                start_px: px,
                                initial_px,
                            });
                        }
                    } else if resp.dragged_by(egui::PointerButton::Primary) {
                        if let (Some(pos), Some(drag)) = (resp.interact_pointer_pos(), &self.drag) {
                            let px = to_px(pos);
                            let dx = px.x - drag.start_px.x;
                            let dy = px.y - drag.start_px.y;
                            let next = match drag.kind {
                                DragKind::New => Rect::from_two_pos(drag.start_px, px),
                                DragKind::Move => {
                                    let w = drag.initial_px.width();
                                    let h = drag.initial_px.height();
                                    let nx = (drag.initial_px.min.x + dx).clamp(0.0, img_w - w);
                                    let ny = (drag.initial_px.min.y + dy).clamp(0.0, img_h - h);
                                    Rect::from_min_size(Pos2::new(nx, ny), Vec2::new(w, h))
                                }
                                DragKind::Resize {
                                    left,
                                    right,
                                    top,
                                    bottom,
                                } => {
                                    let l = if left {
                                        (drag.initial_px.min.x + dx)
                                            .clamp(0.0, drag.initial_px.max.x - 4.0)
                                    } else {
                                        drag.initial_px.min.x
                                    };
                                    let r = if right {
                                        (drag.initial_px.max.x + dx)
                                            .clamp(drag.initial_px.min.x + 4.0, img_w)
                                    } else {
                                        drag.initial_px.max.x
                                    };
                                    let t = if top {
                                        (drag.initial_px.min.y + dy)
                                            .clamp(0.0, drag.initial_px.max.y - 4.0)
                                    } else {
                                        drag.initial_px.min.y
                                    };
                                    let b = if bottom {
                                        (drag.initial_px.max.y + dy)
                                            .clamp(drag.initial_px.min.y + 4.0, img_h)
                                    } else {
                                        drag.initial_px.max.y
                                    };
                                    Rect::from_min_max(Pos2::new(l, t), Pos2::new(r, b))
                                }
                            };
                            self.selection_px = Some(next);
                        }
                    } else if resp.drag_stopped() {
                        self.drag = None;
                        if let Some(sel) = self.selection_px {
                            if sel.width() < 4.0 || sel.height() < 4.0 {
                                // Single click selects the snapped hover window when no region exists yet.
                                if let Some((wrect, _)) = self.hover_window.take() {
                                    self.selection_px = Some(wrect);
                                } else {
                                    self.selection_px = None;
                                }
                            } else {
                                self.hover_window = None;
                            }
                        }
                    } else if resp.clicked() && self.selection_px.is_none() {
                        if let Some((wrect, _)) = self.hover_window.take() {
                            self.selection_px = Some(wrect);
                        }
                    }
                } else if is_annotating && !ctx.is_pointer_over_area() {
                    let icon = self.annotations.last_cursor.unwrap_or_else(|| {
                        if self.annotations.tool.is_text() {
                            CursorIcon::Text
                        } else {
                            CursorIcon::Crosshair
                        }
                    });
                    ctx.set_cursor_icon(icon);
                }

                // 5. Draw selection border, 8 resize handles, and dimension badge.
                if let Some(sel_px) = self.selection_px.filter(|r| r.width() >= 2.0 && r.height() >= 2.0) {
                    let sel = to_logical_rect(sel_px);
                    let border = EguiStroke::new(2.0_f32, palette.accent);
                    ui.painter().rect_stroke(sel, 0.0, border, egui::StrokeKind::Outside);

                    // 8 resize handles.
                    if !is_annotating {
                        for pt in [
                            sel.left_top(),
                            sel.center_top(),
                            sel.right_top(),
                            sel.right_center(),
                            sel.right_bottom(),
                            sel.center_bottom(),
                            sel.left_bottom(),
                            sel.left_center(),
                        ] {
                            ui.painter().circle_filled(pt, 4.5, Color32::WHITE);
                            ui.painter()
                                .circle_stroke(pt, 4.5, EguiStroke::new(1.5_f32, palette.accent));
                        }
                    }

                    // Selection dimensions badge.
                    let dim_text = format!(
                        "{} \u{00D7} {} px",
                        sel_px.width().round() as u32,
                        sel_px.height().round() as u32
                    );
                    let pill_pos = Pos2::new(
                        sel.min.x.clamp(8.0, (screen.max.x - 130.0).max(8.0)),
                        if sel.min.y > 30.0 {
                            sel.min.y - 26.0
                        } else {
                            sel.min.y + 8.0
                        },
                    );
                    draw_pill(ui.painter(), pill_pos, &dim_text);
                } else if let Some((hrect, title)) = &self.hover_window {
                    let hr = to_logical_rect(*hrect);
                    ui.painter().rect_stroke(
                        hr,
                        0.0,
                        EguiStroke::new(2.0_f32, palette.accent_bright),
                        egui::StrokeKind::Inside,
                    );
                    let label = format!(
                        "{} ({}\u{00D7}{})",
                        title.chars().take(28).collect::<String>(),
                        hrect.width().round() as u32,
                        hrect.height().round() as u32
                    );
                    draw_pill(
                        ui.painter(),
                        Pos2::new(hr.min.x + 8.0, (hr.min.y + 8.0).min(screen.max.y - 32.0)),
                        &label,
                    );
                }

                // 6. Pixel magnifier (shown before selection or while dragging).
                if self.show_magnifier && (self.selection_px.is_none() || self.drag.is_some()) {
                    if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                        let px = to_px(pos);
                        draw_magnifier(
                            ui.painter(),
                            &self.screen_image,
                            pos,
                            (px.x as i32, px.y as i32),
                            screen,
                        );
                    }
                }
            });

        // Top instruction banner (shown before a region is selected).
        let palette = ui::ctx_palette(ctx);
        if self.selection_px.is_none() && self.error.is_empty() {
            let mode_title = match self.mode.as_str() {
                "long" => i18n::t("Select scrolling content area"),
                "gif" => i18n::t("Select animated recording area"),
                "pin" => i18n::t("Select area to pin to desktop"),
                _ => i18n::t("Drag to select capture area"),
            };
            egui::Area::new(egui::Id::new("overlay_instructions"))
                .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 18.0))
                .order(egui::Order::Foreground)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(palette.floating_bg_alpha(220))
                        .stroke(EguiStroke::new(1.0_f32, palette.accent.gamma_multiply(0.75)))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(egui::Margin::symmetric(16, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(mode_title).color(Color32::WHITE).strong());
                                ui.label(
                                    RichText::new(i18n::t(
                                        "·  Click window  ·  Ctrl+A Fullscreen  ·  Double-click / Enter Confirm  ·  Ctrl+S Edit & Save  ·  Right-click / Esc Cancel",
                                    ))
                                    .color(Color32::from_gray(205))
                                    .small(),
                                );
                            });
                        });
                });
        }

        // Transient color-copy feedback banner.
        if let Some((msg, _)) = &self.copy_feedback {
            let text = msg.clone();
            egui::Area::new(egui::Id::new("overlay_copy_feedback"))
                .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 62.0))
                .order(egui::Order::Tooltip)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(palette.toast_bg)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(14, 7))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                icons::paint_inline(ui, Icon::Check, 14.0, Color32::WHITE);
                                ui.label(RichText::new(text).color(Color32::WHITE).strong());
                            });
                        });
                });
        }

        // 7. Floating toolbar / scrolling capture prep bar (clamped inside screen bounds).
        if let Some(sel_px) = self.selection_px {
            if sel_px.width() >= 4.0 && sel_px.height() >= 4.0 && self.drag.is_none() {
                let sel = to_logical_rect(sel_px);
                let tb_w = self.last_toolbar_size.x.max(240.0);
                let tb_h = self.last_toolbar_size.y.max(40.0);
                let tb_y = if sel.max.y + tb_h + 12.0 <= screen.max.y - 8.0 {
                    sel.max.y + 10.0
                } else if sel.min.y - tb_h - 12.0 >= screen.min.y + 8.0 {
                    sel.min.y - tb_h - 10.0
                } else {
                    (sel.max.y - tb_h - 10.0).clamp(screen.min.y + 8.0, (screen.max.y - tb_h - 8.0).max(8.0))
                };
                let preferred_x = sel.max.x - tb_w;
                let tb_x = preferred_x.clamp(
                    screen.min.x + 8.0,
                    (screen.max.x - tb_w - 8.0).max(screen.min.x + 8.0),
                );

                if self.long_ready {
                    let mut start_long = false;
                    let mut back_normal = false;
                    let mut cancel = false;
                    let area_resp = egui::Area::new(egui::Id::new("overlay_long_ready_bar"))
                        .fixed_pos(Pos2::new(tb_x, tb_y))
                        .order(egui::Order::Foreground)
                        .show(ctx, |ui| {
                            egui::Frame::window(ui.style())
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        icons::paint_inline(
                                            ui,
                                            Icon::LongCapture,
                                            16.0,
                                            palette.accent_text(ui.visuals().dark_mode),
                                        );
                                        ui.label(
                                            RichText::new(i18n::t("Scrolling Capture Ready"))
                                                .strong(),
                                        );
                                        ui.separator();
                                        ui.selectable_value(
                                            &mut self.scroll_mode,
                                            "auto".to_string(),
                                            i18n::t("Auto Scroll"),
                                        );
                                        ui.selectable_value(
                                            &mut self.scroll_mode,
                                            "manual".to_string(),
                                            i18n::t("Manual Scroll"),
                                        );
                                        ui.separator();
                                        if icons::icon_text_button(
                                            ui,
                                            Icon::Play,
                                            i18n::t("Start Scrolling Capture (Enter)"),
                                            false,
                                            true,
                                            None,
                                        )
                                        .clicked()
                                        {
                                            start_long = true;
                                        }
                                        if self.mode != "long"
                                            && icons::icon_text_button(
                                                ui,
                                                Icon::Back,
                                                i18n::t("Back to Annotate"),
                                                false,
                                                false,
                                                None,
                                            )
                                            .clicked()
                                        {
                                            back_normal = true;
                                        }
                                        if icons::icon_text_button(
                                            ui,
                                            Icon::Close,
                                            i18n::t("Cancel"),
                                            false,
                                            false,
                                            None,
                                        )
                                        .clicked()
                                        {
                                            cancel = true;
                                        }
                                    });
                                });
                        });
                    self.last_toolbar_size = area_resp.response.rect.size();
                    if start_long {
                        self.commit_selection(app, SelectAction::Long);
                    } else if back_normal {
                        self.long_ready = false;
                    } else if cancel {
                        service::cancel_capture(app);
                    }
                } else {
                    let confirm = match self.mode.as_str() {
                        "gif" => (
                            Icon::Play,
                            i18n::t("Start Recording"),
                            i18n::t("Start GIF recording (Enter / Double-click)"),
                        ),
                        "pin" => (
                            Icon::Check,
                            i18n::t("Pin"),
                            i18n::t("Pin to desktop (Enter / Double-click)"),
                        ),
                        _ => (
                            Icon::Copy,
                            i18n::t("Copy"),
                            i18n::t("Copy to clipboard (Enter / Double-click)"),
                        ),
                    };
                    let mut triggered = None;
                    let area_resp = egui::Area::new(egui::Id::new("overlay_floating_toolbar"))
                        .fixed_pos(Pos2::new(tb_x, tb_y))
                        .order(egui::Order::Foreground)
                        .show(ctx, |ui| {
                            egui::Frame::window(ui.style())
                                .inner_margin(egui::Margin::symmetric(8, 6))
                                .show(ui, |ui| {
                                    triggered = canvas::show_annotation_toolbar(
                                        ui,
                                        app,
                                        &mut self.annotations,
                                        false,
                                        &[
                                            (
                                                ToolbarAction::Ocr,
                                                Icon::Ocr,
                                                i18n::t("OCR"),
                                                i18n::t("Extract text from selection (OCR)"),
                                            ),
                                            (
                                                ToolbarAction::Gif,
                                                Icon::RecordGif,
                                                i18n::t("Record"),
                                                i18n::t("Record this region as GIF"),
                                            ),
                                            (
                                                ToolbarAction::Long,
                                                Icon::LongCapture,
                                                i18n::t("Scroll"),
                                                i18n::t("Scrolling capture downward"),
                                            ),
                                            (
                                                ToolbarAction::Pin,
                                                Icon::Pin,
                                                i18n::t("Pin"),
                                                i18n::t("Pin to desktop"),
                                            ),
                                            (
                                                ToolbarAction::Save,
                                                Icon::Save,
                                                i18n::t("Save"),
                                                i18n::t("Open editor to save (Ctrl+S)"),
                                            ),
                                            (
                                                ToolbarAction::Reselect,
                                                Icon::Reselect,
                                                i18n::t("Reselect"),
                                                i18n::t("Reselect region"),
                                            ),
                                            (
                                                ToolbarAction::Close,
                                                Icon::Close,
                                                i18n::t("Cancel"),
                                                i18n::t("Cancel (Esc)"),
                                            ),
                                        ],
                                        false,
                                        Some(confirm),
                                        true,
                                    );
                                });
                        });
                    self.last_toolbar_size = area_resp.response.rect.size();
                    if let Some(act) = triggered {
                        match act {
                            ToolbarAction::Confirm => {
                                let default_act = self.default_confirm_action();
                                self.commit_selection(app, default_act);
                            }
                            ToolbarAction::Ocr => {
                                self.commit_selection(app, SelectAction::Ocr)
                            }
                            ToolbarAction::Gif => {
                                self.commit_selection(app, SelectAction::Gif)
                            }
                            ToolbarAction::Long => {
                                self.commit_selection(app, SelectAction::Long)
                            }
                            ToolbarAction::Pin => {
                                self.commit_selection(app, SelectAction::Pin)
                            }
                            ToolbarAction::Save => {
                                self.commit_selection(app, SelectAction::Save)
                            }
                            ToolbarAction::Close => service::cancel_capture(app),
                            ToolbarAction::Reselect => {
                                self.selection_px = None;
                                self.cropped_base = None;
                                self.hover_window = None;
                                self.annotations.reset();
                                self.annotations.tool = Tool::Select;
                            }
                        }
                    }
                }
            }
        }

        // Error floating banner.
        if !self.error.is_empty() {
            let err_msg = self.error.clone();
            egui::Area::new(egui::Id::new("overlay_error"))
                .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, 20.0))
                .order(egui::Order::Tooltip)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(ui::DANGER)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(14, 8))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                icons::paint_inline(ui, Icon::Alert, 15.0, Color32::WHITE);
                                ui.label(RichText::new(err_msg).color(Color32::WHITE));
                                if icons::dark_icon_text_button(
                                    ui,
                                    Icon::Close,
                                    i18n::t("Close"),
                                    Color32::from_rgba_unmultiplied(0, 0, 0, 55),
                                    Color32::from_rgba_unmultiplied(0, 0, 0, 95),
                                )
                                .clicked()
                                {
                                    service::cancel_capture(app);
                                }
                            });
                        });
                });
        }
    }
}

fn cursor_for_drag(kind: DragKind, dragging: bool) -> CursorIcon {
    match kind {
        DragKind::New => CursorIcon::Crosshair,
        DragKind::Move => {
            if dragging {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            }
        }
        DragKind::Resize {
            left,
            right,
            top,
            bottom,
        } => match (left || right, top || bottom) {
            (true, true) => {
                if (left && top) || (right && bottom) {
                    CursorIcon::ResizeNwSe
                } else {
                    CursorIcon::ResizeNeSw
                }
            }
            (true, false) => CursorIcon::ResizeHorizontal,
            (false, true) => CursorIcon::ResizeVertical,
            (false, false) => CursorIcon::Crosshair,
        },
    }
}

fn hit_test_selection(sel: Rect, pos: Pos2) -> Option<DragKind> {
    let tol = 9.0;
    let near_l = (pos.x - sel.min.x).abs() <= tol;
    let near_r = (pos.x - sel.max.x).abs() <= tol;
    let near_t = (pos.y - sel.min.y).abs() <= tol;
    let near_b = (pos.y - sel.max.y).abs() <= tol;
    let in_x = pos.x >= sel.min.x - tol && pos.x <= sel.max.x + tol;
    let in_y = pos.y >= sel.min.y - tol && pos.y <= sel.max.y + tol;
    if (near_l || near_r || near_t || near_b) && in_x && in_y {
        return Some(DragKind::Resize {
            left: near_l,
            right: near_r,
            top: near_t,
            bottom: near_b,
        });
    }
    if sel.contains(pos) {
        return Some(DragKind::Move);
    }
    None
}

fn sample_pixel(img: &Rgba, x: u32, y: u32) -> (u8, u8, u8) {
    let cx = x.min(img.width.saturating_sub(1));
    let cy = y.min(img.height.saturating_sub(1));
    let idx = ((cy * img.width + cx) * 4) as usize;
    if idx + 2 < img.pixels.len() {
        (img.pixels[idx], img.pixels[idx + 1], img.pixels[idx + 2])
    } else {
        (0, 0, 0)
    }
}

fn draw_pill(painter: &egui::Painter, pos: Pos2, text: &str) {
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(12.5),
        Color32::WHITE,
    );
    let rect = Rect::from_min_size(pos, galley.size() + Vec2::new(14.0, 6.0));
    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_black_alpha(200));
    painter.galley(pos + Vec2::new(7.0, 3.0), galley, Color32::WHITE);
}

fn draw_magnifier(
    painter: &egui::Painter,
    img: &Rgba,
    cursor_logical: Pos2,
    cursor_px: (i32, i32),
    screen: Rect,
) {
    let grid: i32 = 11;
    let half = grid / 2;
    let cell = 11.0;
    let grid_size = grid as f32 * cell;
    let box_w = grid_size + 12.0;
    let box_h = grid_size + 46.0;

    let mut origin = Pos2::new(cursor_logical.x + 20.0, cursor_logical.y + 20.0);
    if origin.x + box_w > screen.max.x - 8.0 {
        origin.x = cursor_logical.x - box_w - 20.0;
    }
    if origin.y + box_h > screen.max.y - 8.0 {
        origin.y = cursor_logical.y - box_h - 20.0;
    }
    let palette = ui::ctx_palette(painter.ctx());
    let outer = Rect::from_min_size(origin, Vec2::new(box_w, box_h));
    painter.rect_filled(outer, CornerRadius::same(8), palette.floating_bg);
    painter.rect_stroke(
        outer,
        CornerRadius::same(8),
        EguiStroke::new(1.5_f32, palette.accent),
        egui::StrokeKind::Inside,
    );

    let g_min = Pos2::new(origin.x + 6.0, origin.y + 6.0);
    for dy in -half..=half {
        for dx in -half..=half {
            let sx = cursor_px.0 + dx;
            let sy = cursor_px.1 + dy;
            let col = if sx >= 0 && sy >= 0 && (sx as u32) < img.width && (sy as u32) < img.height {
                let (r, g, b) = sample_pixel(img, sx as u32, sy as u32);
                Color32::from_rgb(r, g, b)
            } else {
                Color32::from_gray(24)
            };
            let cell_rect = Rect::from_min_size(
                Pos2::new(
                    g_min.x + (dx + half) as f32 * cell,
                    g_min.y + (dy + half) as f32 * cell,
                ),
                Vec2::splat(cell),
            );
            painter.rect_filled(cell_rect, 0.0, col);
        }
    }
    // Center crosshair.
    let center_rect = Rect::from_min_size(
        Pos2::new(g_min.x + half as f32 * cell, g_min.y + half as f32 * cell),
        Vec2::splat(cell),
    );
    painter.rect_stroke(
        center_rect,
        0.0,
        EguiStroke::new(1.5_f32, palette.accent),
        egui::StrokeKind::Outside,
    );

    let (r, g, b) = sample_pixel(
        img,
        cursor_px.0.max(0) as u32,
        cursor_px.1.max(0) as u32,
    );
    let info1 = format!("({}, {})  #{:02X}{:02X}{:02X}", cursor_px.0, cursor_px.1, r, g, b);
    let info2 = i18n::t("Press C to copy HEX · Right-click / Esc to cancel");
    painter.text(
        Pos2::new(origin.x + 8.0, g_min.y + grid_size + 5.0),
        egui::Align2::LEFT_TOP,
        info1,
        egui::FontId::monospace(11.0),
        Color32::WHITE,
    );
    painter.text(
        Pos2::new(origin.x + 8.0, g_min.y + grid_size + 21.0),
        egui::Align2::LEFT_TOP,
        info2,
        egui::FontId::proportional(10.5),
        Color32::from_gray(175),
    );
}
