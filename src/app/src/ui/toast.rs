//! Lightweight toast banner: bottom-right overlay in active windows + floating window when no window is focused.

use crate::ui;
use crate::ui::icons::{self, Icon};
use egui::{Color32, CornerRadius, RichText, Vec2};
use std::time::{Duration, Instant};

pub const TOAST_WIDTH: f32 = 340.0;

#[derive(Clone)]
pub struct Toast {
    pub message: String,
    pub error: bool,
    pub expires: Instant,
}

impl Toast {
    pub fn new(message: impl Into<String>, error: bool) -> Self {
        let dur = if error {
            Duration::from_millis(6500)
        } else {
            Duration::from_millis(2800)
        };
        Self {
            message: message.into(),
            error,
            expires: Instant::now() + dur,
        }
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }

    fn background(&self, ctx: &egui::Context) -> Color32 {
        if self.error {
            Color32::from_rgb(0x9b, 0x22, 0x26)
        } else {
            ui::ctx_palette(ctx).toast_bg
        }
    }
}

/// Clear expired toast or schedule a repaint when it expires.
pub fn tick(ctx: &egui::Context, toast: &mut Option<Toast>) {
    if let Some(t) = toast {
        if t.expired() {
            *toast = None;
        } else {
            ctx.request_repaint_after(t.expires.saturating_duration_since(Instant::now()));
        }
    }
}

fn toast_body(ui: &mut egui::Ui, toast: &Toast) -> bool {
    let mut dismiss = false;
    ui.horizontal(|ui| {
        icons::paint_inline(
            ui,
            if toast.error {
                Icon::Alert
            } else {
                Icon::Check
            },
            15.0,
            Color32::WHITE,
        );
        ui.add(
            egui::Label::new(RichText::new(&toast.message).color(Color32::WHITE)).wrap(),
        );
    });
    if ui.ui_contains_pointer() && ui.input(|i| i.pointer.primary_clicked()) {
        dismiss = true;
    }
    dismiss
}

/// Measure desired logical size for an in-window toast clamped to `max_width`.
fn measure_in_window_size(ctx: &egui::Context, toast: &Toast, max_width: f32) -> Vec2 {
    let spacing_x = ctx.style().spacing.item_spacing.x;
    let text_max_w = (max_width - 28.0 - 15.0 - spacing_x).max(80.0);
    let font = egui::TextStyle::Body.resolve(&ctx.style());
    let galley = ctx.fonts(|f| f.layout(toast.message.clone(), font, Color32::WHITE, text_max_w));
    let box_w = (galley.size().x + 15.0 + spacing_x + 28.0 + 4.0)
        .min(max_width)
        .ceil();
    let box_h = (galley.size().y.max(18.0) + 18.0 + 4.0).ceil();
    Vec2::new(box_w, box_h)
}

/// Draw a bottom-right toast inside an existing window (click to dismiss).
pub fn show_in_window(ctx: &egui::Context, toast: &mut Option<Toast>, bottom_offset: f32) {
    tick(ctx, toast);
    let Some(t) = toast.as_ref() else { return };
    let screen = ctx.screen_rect();
    let max_w = TOAST_WIDTH.min((screen.width() - 32.0).max(160.0));
    let desired = measure_in_window_size(ctx, t, max_w);
    let pos = egui::pos2(
        (screen.right() - 16.0 - desired.x).max(screen.left() + 16.0),
        (screen.bottom() - bottom_offset - desired.y).max(screen.top() + 16.0),
    );
    let mut dismiss = false;
    egui::Area::new(egui::Id::new("snapdock_toast"))
        .fixed_pos(pos)
        .constrain(true)
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            ui.set_max_width(desired.x);
            egui::Frame::NONE
                .fill(t.background(ctx))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    dismiss = toast_body(ui, t);
                });
        });
    if dismiss {
        *toast = None;
    }
}

/// Floating toast window state (used when no application window is focused).
pub struct ToastWindowState {
    pub toast: Option<Toast>,
}

impl ToastWindowState {
    pub fn new(toast: Toast) -> Self {
        Self { toast: Some(toast) }
    }

    pub fn finished(&self) -> bool {
        self.toast.as_ref().is_none_or(|t| t.expired())
    }

    /// Estimate logical toast size safely before the first `egui::Context::run()` call.
    pub fn estimate_size(toast: &Toast) -> Vec2 {
        let text_w = TOAST_WIDTH - 28.0 - 24.0;
        let mut total_lines: usize = 0;
        for paragraph in toast.message.split('\n') {
            let mut line_w = 0.0_f32;
            let mut lines = 1_usize;
            for ch in paragraph.chars() {
                let cw = if ch.is_ascii() { 8.0 } else { 14.5 };
                if line_w + cw > text_w && line_w > 0.0 {
                    lines += 1;
                    line_w = cw;
                } else {
                    line_w += cw;
                }
            }
            total_lines += lines;
        }
        let h = (total_lines.max(1) as f32 * 20.0 + 22.0).clamp(44.0, 260.0);
        Vec2::new(TOAST_WIDTH, h.ceil())
    }

    /// Measure exact laid-out toast height inside an `egui::Context::run()` pass.
    fn measure_in_pass(ctx: &egui::Context, toast: &Toast) -> Vec2 {
        let text_w = TOAST_WIDTH - 28.0 - 24.0;
        let font = egui::TextStyle::Body.resolve(&ctx.style());
        let galley = ctx.fonts(|f| f.layout(toast.message.clone(), font, Color32::WHITE, text_w));
        Vec2::new(TOAST_WIDTH, (galley.size().y + 22.0).clamp(44.0, 260.0).ceil())
    }

    /// Render the floating toast window and return its desired logical size.
    pub fn show(&mut self, ctx: &egui::Context) -> Vec2 {
        tick(ctx, &mut self.toast);
        let Some(t) = self.toast.clone() else {
            return Vec2::new(TOAST_WIDTH, 44.0);
        };
        let size = Self::measure_in_pass(ctx, &t);
        let mut dismiss = false;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(t.background(ctx))
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, |ui| {
                dismiss = toast_body(ui, &t);
            });
        if dismiss {
            self.toast = None;
        }
        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_size_works_without_egui_context_and_wraps_long_text() {
        let short = Toast::new("Screenshot copied to clipboard", false);
        let s = ToastWindowState::estimate_size(&short);
        assert_eq!(s.x, TOAST_WIDTH);
        assert!(s.y >= 42.0 && s.y <= 60.0);

        let long = Toast::new(
            "Auto-save failed; you can save again from the editor: unable to write target file, please check disk permissions and free space.",
            true,
        );
        let l = ToastWindowState::estimate_size(&long);
        assert_eq!(l.x, TOAST_WIDTH);
        assert!(l.y > s.y);
    }

    #[test]
    fn estimate_size_respects_newlines_and_clamps_height() {
        let multi = Toast::new("Line one\nLine two\nLine three", false);
        let m = ToastWindowState::estimate_size(&multi);
        assert!(m.y >= 80.0);

        let huge = Toast::new("A very long error message\n".repeat(40), true);
        let h = ToastWindowState::estimate_size(&huge);
        assert!(h.y <= 260.0);
    }

    #[test]
    fn toast_window_renders_first_frame_on_fresh_context_without_panic() {
        let ctx = egui::Context::default();
        let toast = Toast::new("Screenshot copied to clipboard", false);
        let estimated = ToastWindowState::estimate_size(&toast);
        assert!(estimated.y >= 44.0);
        let mut state = ToastWindowState::new(toast);
        let mut measured = Vec2::ZERO;
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            measured = state.show(ctx);
        });
        assert!(!out.shapes.is_empty());
        assert_eq!(measured.x, TOAST_WIDTH);
        assert!(measured.y >= 44.0);
    }

    #[test]
    fn show_in_window_stays_within_bounds_after_short_to_long_transition() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(780.0, 560.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };

        // Frame 1: short toast ("Settings saved")
        let mut toast = Some(Toast::new("Settings saved", false));
        let _ = ctx.run(input(), |ctx| {
            show_in_window(ctx, &mut toast, 16.0);
        });

        // Frame 2: immediately switch to a long multi-line provider test success toast.
        toast = Some(Toast::new(
            "Provider test succeeded: SnapDock Local OCR Test\nHello RapidOCR 2026 with a much longer preview line that wraps",
            false,
        ));
        let out = ctx.run(input(), |ctx| {
            show_in_window(ctx, &mut toast, 16.0);
        });
        assert!(!out.shapes.is_empty());

        let rect = ctx
            .memory(|m| m.area_rect(egui::Id::new("snapdock_toast")))
            .expect("toast area rect should be recorded");
        assert!(rect.left() >= screen.left() + 15.0, "left={}", rect.left());
        assert!(rect.top() >= screen.top() + 15.0, "top={}", rect.top());
        assert!(rect.right() <= screen.right() - 14.0, "right={}", rect.right());
        assert!(rect.bottom() <= screen.bottom() - 14.0, "bottom={}", rect.bottom());
    }
}
