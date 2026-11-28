//! Borderless always-on-top desktop pin window:
//! - Main image window (exact original capture position, drag, wheel zoom, opacity, click-through,
//!   GIF playback, and non-overlapping interactive OCR / translation overlay).
//! - Companion bottom toolbar window positioned below the pin image (zero image occlusion).
//! - Companion right-side plain-text window displaying the generated OCR / translation plain text.

use crate::clipboard;
use crate::i18n;
use crate::native;
use crate::ocr::{OcrOverlayData, OcrTextBlock};
use crate::service;
use crate::state::{App, AppEvent, PinOverlayMode};
use crate::ui;
use crate::ui::icons::{self, Icon};
use crate::ui::GifPlayer;
use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Galley, Pos2, Rect, RichText, Sense, Stroke,
    TextureHandle, TextureOptions, Vec2,
};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Logical gap between pin image window and companion toolbar / plain-text panel.
#[allow(dead_code)]
pub const COMPANION_GAP_LOGICAL: f32 = 8.0;
/// Logical shadow margin extending outward on all four sides around the pinned image.
pub const PIN_SHADOW_PAD_LOGICAL: f32 = 12.0;
/// Logical gap between the outer edge of the pin window (including shadow) and the companion toolbar.
pub const TOOLBAR_SHADOW_GAP_LOGICAL: f32 = 3.0;
/// Logical gap between the outer edge of the pin window (including shadow) and the companion text panel.
pub const TEXT_PANEL_SHADOW_GAP_LOGICAL: f32 = 4.0;
/// Logical height reserved for the inline target language picker inside the toolbar window.
pub const LANG_PICKER_HEIGHT_LOGICAL: f32 = 228.0;
/// Preferred logical width of the right-side plain-text panel.
pub const TEXT_PANEL_WIDTH_LOGICAL: f32 = 320.0;
pub const TEXT_PANEL_MIN_WIDTH_LOGICAL: f32 = 220.0;
pub const TEXT_PANEL_MIN_HEIGHT_LOGICAL: f32 = 220.0;

/// Physical-pixel shadow margin around each side of the pinned image for a given monitor scale factor.
pub fn pin_shadow_pad_physical(scale_factor: f64) -> i32 {
    (PIN_SHADOW_PAD_LOGICAL as f64 * scale_factor.max(0.5)).round() as i32
}

/// Expand a physical image rect `(x, y, w, h)` outward by `pad_px` on all four sides to obtain
/// the physical pin window rect while keeping the image anchored at `(x, y)`.
pub fn pin_window_rect_from_image_rect(
    image_rect: (i32, i32, u32, u32),
    pad_px: i32,
) -> (i32, i32, u32, u32) {
    let pad = pad_px.max(0);
    let pad_u = pad as u32;
    (
        image_rect.0 - pad,
        image_rect.1 - pad,
        image_rect.2.saturating_add(pad_u.saturating_mul(2)),
        image_rect.3.saturating_add(pad_u.saturating_mul(2)),
    )
}

/// Inset a physical pin window rect `(x, y, w, h)` by `pad_px` on all four sides to recover the
/// physical image rect.
pub fn pin_image_rect_from_window_rect(
    window_rect: (i32, i32, i32, i32),
    pad_px: i32,
) -> (i32, i32, i32, i32) {
    let max_pad_x = ((window_rect.2 - 1) / 2).max(0);
    let max_pad_y = ((window_rect.3 - 1) / 2).max(0);
    let px = pad_px.max(0).min(max_pad_x);
    let py = pad_px.max(0).min(max_pad_y);
    (
        window_rect.0 + px,
        window_rect.1 + py,
        (window_rect.2 - px * 2).max(1),
        (window_rect.3 - py * 2).max(1),
    )
}

/// Logical image rect inside a layered pin window, snapped to exact integer physical shadow padding
/// so 100%-scale captures map 1:1 to screen pixels with zero subpixel blur.
pub fn pin_image_rect_logical(screen: Rect, pixels_per_point: f32) -> Rect {
    let ppp = pixels_per_point.max(0.5);
    let pad = pin_shadow_pad_physical(ppp as f64) as f32 / ppp;
    let max_pad_x = ((screen.width() - 4.0) * 0.5).max(0.0);
    let max_pad_y = ((screen.height() - 4.0) * 0.5).max(0.0);
    let px = pad.min(max_pad_x);
    let py = pad.min(max_pad_y);
    Rect::from_min_max(
        Pos2::new(screen.min.x + px, screen.min.y + py),
        Pos2::new(screen.max.x - px, screen.max.y - py),
    )
}

/// Compute initial physical `(x, y, width, height)` for a pin image window.
///
/// When `capture_origin` is provided (screen region selection), the pinned image stays at its
/// exact original screen position (0px deviation at 100% scale) instead of jumping to the cursor.
pub fn compute_pin_initial_rect(
    base_width: u32,
    base_height: u32,
    scale: i64,
    capture_origin: Option<(i32, i32)>,
    cursor: (i32, i32),
    work_area: (i32, i32, i32, i32),
) -> (i32, i32, u32, u32) {
    let clamped_scale = scale.clamp(25, 200);
    let bw = base_width.max(1);
    let bh = base_height.max(1);

    if let Some((ox, oy)) = capture_origin {
        let s = clamped_scale as f64 / 100.0;
        let width = ((bw as f64 * s).round() as u32).max(40);
        let height = ((bh as f64 * s).round() as u32).max(40);
        let px = ox + (bw as i32 - width as i32) / 2;
        let py = oy + (bh as i32 - height as i32) / 2;
        return (px, py, width, height);
    }

    let (wx, wy, ww, wh) = work_area;
    let fit = (clamped_scale as f64 / 100.0)
        .min(((ww - 40).max(80) as f64) / (bw as f64))
        .min(((wh - 40).max(80) as f64) / (bh as f64));
    let width = ((bw as f64 * fit).round() as u32).max(60);
    let height = ((bh as f64 * fit).round() as u32).max(60);
    let px = (cursor.0 - (width as i32) / 2).clamp(wx, (wx + ww - width as i32).max(wx));
    let py = (cursor.1 - (height as i32) / 2).clamp(wy, (wy + wh - height as i32).max(wy));
    (px, py, width, height)
}

/// Compute physical `(x, y, width, height, picker_above)` for the companion bottom toolbar window.
///
/// Prefers placing the toolbar directly below the pin image window so it never occludes the image.
/// Flips above only if there is insufficient room below within the monitor work area.
/// When `picker_extra_h > 0` (language selector expanded), extends downward or upward while keeping
/// the toolbar button row anchored in place.
pub fn compute_toolbar_rect(
    pin_rect: (i32, i32, i32, i32),
    bar_size: (i32, i32),
    picker_extra_h: i32,
    gap: i32,
    work_area: (i32, i32, i32, i32),
) -> (i32, i32, i32, i32, bool) {
    let (px, py, pw, ph) = pin_rect;
    let (wx, wy, ww, wh) = work_area;
    let work_right = wx + ww;
    let work_bottom = wy + wh;

    let tw = bar_size.0.clamp(120, (ww - 8).max(120));
    let bar_h = bar_size.1.max(30);
    let x = (px + (pw - tw) / 2).clamp(wx + 4, (work_right - tw - 4).max(wx));

    let placed_above = py + ph + gap + bar_h > work_bottom - 2 && py - gap - bar_h >= wy + 2;
    let base_y = if placed_above {
        py - gap - bar_h
    } else if py + ph + gap + bar_h <= work_bottom - 2 {
        py + ph + gap
    } else {
        (work_bottom - bar_h - 4).max(wy + 2)
    };

    if picker_extra_h <= 0 {
        return (x, base_y, tw, bar_h, false);
    }

    let total_h = (bar_h + picker_extra_h).min((wh - 8).max(bar_h));
    let extra = (total_h - bar_h).max(0);

    // Prefer extending downward when toolbar is below the pin and room allows,
    // otherwise extend upward while keeping the toolbar button row at `base_y`.
    if !placed_above && base_y + total_h <= work_bottom - 2 {
        (x, base_y, tw, total_h, false)
    } else if base_y - extra >= wy + 2 {
        (x, base_y - extra, tw, total_h, true)
    } else {
        let clamped_y = base_y.clamp(wy + 2, (work_bottom - total_h - 2).max(wy + 2));
        (x, clamped_y, tw, total_h, placed_above)
    }
}

/// Compute physical `(x, y, width, height)` for the right-side plain-text panel window.
///
/// Prefers placing the panel to the right of the pin image without moving the pin itself;
/// falls back to the left only when the monitor's right edge lacks space.
pub fn compute_text_panel_rect(
    pin_rect: (i32, i32, i32, i32),
    desired_width: i32,
    min_width: i32,
    min_height: i32,
    gap: i32,
    work_area: (i32, i32, i32, i32),
    user_offset: (i32, i32),
) -> (i32, i32, i32, i32) {
    let (px, py, pw, ph) = pin_rect;
    let (wx, wy, ww, wh) = work_area;
    let work_right = wx + ww;
    let work_bottom = wy + wh;

    let min_w = min_width.max(160);
    let min_h = min_height.max(140);
    let h = ph.clamp(min_h, (wh - 20).max(min_h));

    let right_avail = work_right - (px + pw + gap) - 4;
    let left_avail = (px - gap) - wx - 4;

    let (base_x, w) = if right_avail >= min_w {
        let w = desired_width.min(right_avail).max(min_w);
        (px + pw + gap, w)
    } else if left_avail >= min_w {
        let w = desired_width.min(left_avail).max(min_w);
        (px - gap - w, w)
    } else {
        let w = desired_width.min((ww - 16).max(min_w));
        ((work_right - w - 6).max(wx + 4), w)
    };

    let base_y = py.clamp(wy + 4, (work_bottom - h - 4).max(wy + 4));
    let x = (base_x + user_offset.0).clamp(wx + 2, (work_right - w - 2).max(wx + 2));
    let y = (base_y + user_offset.1).clamp(wy + 2, (work_bottom - h - 2).max(wy + 2));
    (x, y, w, h)
}

/// UI feedback returned by pin surfaces so `main.rs` can synchronize companion windows in the same frame.
#[derive(Default, Debug)]
pub struct PinUiFeedback {
    /// Pin image window moved or resized (zoom/drag); reposition companions.
    pub geometry_changed: bool,
    /// Pin image window should be redrawn.
    pub redraw_image: bool,
    /// Companion toolbar window should be redrawn / re-measured.
    pub redraw_toolbar: bool,
    /// Companion right-side plain-text window should be redrawn.
    pub redraw_text_panel: bool,
    /// Latest measured logical size of the compact toolbar bar row.
    pub toolbar_bar_size: Option<Vec2>,
    /// Keep toolbar visible (e.g. while language selector is open).
    pub keep_toolbar_open: bool,
    /// Hide toolbar immediately (e.g. click-through enabled).
    pub hide_toolbar: bool,
    /// Explicit visibility change requested for the right-side plain-text panel.
    pub text_panel_visible: Option<bool>,
    /// Global layered-window alpha (0..=255) updated from the toolbar opacity slider.
    pub opacity_changed: Option<u8>,
}

/// Cross-block character selection inside a text-overlay pin window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: (usize, usize), // (block_idx, char_idx)
    pub focus: (usize, usize),  // (block_idx, char_idx)
}

impl TextSelection {
    pub fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// Character range `[start, end)` selected within `block_idx`, if non-empty.
    pub fn block_range(&self, block_idx: usize, char_len: usize) -> Option<(usize, usize)> {
        let ((sb, sc), (eb, ec)) = self.ordered();
        if block_idx < sb || block_idx > eb || char_len == 0 {
            return None;
        }
        let start = if block_idx == sb { sc.min(char_len) } else { 0 };
        let end = if block_idx == eb {
            ec.min(char_len)
        } else {
            char_len
        };
        (end > start).then_some((start, end))
    }
}

/// Interactive text layer attached to a pin window once OCR or Translation is activated.
pub struct PinTextOverlay {
    pub mode: PinOverlayMode,
    pub data: Option<OcrOverlayData>,
    pub busy: bool,
    pub error: Option<String>,
    pub show_layer: bool,
    pub show_text_panel: bool,
    pub selection: Option<TextSelection>,
    pub selecting: bool,
    pub lang_picker_open: bool,
    pub lang_query: String,
    pub translated_lang_key: String,
    /// Selectable plain-text buffer for OCR results displayed in the right-side panel.
    pub ocr_plain_buf: String,
    /// Selectable plain-text buffer for Translation results displayed in the right-side panel.
    pub translated_plain_buf: String,
    /// Brief copy confirmation inside the right-side plain-text panel header.
    pub panel_notice: Option<(String, Instant)>,
}

impl PinTextOverlay {
    pub fn new(mode: PinOverlayMode, lang_key: String) -> Self {
        Self {
            mode,
            data: None,
            busy: true,
            error: None,
            show_layer: true,
            show_text_panel: true,
            selection: None,
            selecting: false,
            lang_picker_open: false,
            lang_query: String::new(),
            translated_lang_key: lang_key,
            ocr_plain_buf: String::new(),
            translated_plain_buf: String::new(),
            panel_notice: None,
        }
    }

    /// True when we have per-block boxes ready to render directly over the image.
    pub fn has_positioned_blocks(&self) -> bool {
        let Some(data) = &self.data else {
            return false;
        };
        if data.blocks.is_empty() {
            return false;
        }
        match self.mode {
            PinOverlayMode::Ocr => true,
            PinOverlayMode::Translation => data.blocks.iter().any(|b| b.translated.is_some()),
        }
    }

    pub fn active_full_text(&self) -> String {
        match self.mode {
            PinOverlayMode::Ocr => {
                if !self.ocr_plain_buf.is_empty() {
                    self.ocr_plain_buf.clone()
                } else {
                    self.data
                        .as_ref()
                        .map(|d| d.full_text.clone())
                        .unwrap_or_default()
                }
            }
            PinOverlayMode::Translation => {
                if !self.translated_plain_buf.is_empty() {
                    self.translated_plain_buf.clone()
                } else {
                    self.data
                        .as_ref()
                        .and_then(|d| d.full_translated.clone())
                        .or_else(|| self.data.as_ref().map(|d| d.full_text.clone()))
                        .unwrap_or_default()
                }
            }
        }
    }

    pub fn active_plain_buf_mut(&mut self) -> &mut String {
        match self.mode {
            PinOverlayMode::Ocr => &mut self.ocr_plain_buf,
            PinOverlayMode::Translation => &mut self.translated_plain_buf,
        }
    }

    pub fn sync_plain_bufs(&mut self) {
        let Some(data) = &self.data else {
            return;
        };
        self.ocr_plain_buf = data.full_text.clone();
        if let Some(tr) = &data.full_translated {
            self.translated_plain_buf = tr.clone();
        }
    }

    pub fn block_text<'a>(&self, block: &'a OcrTextBlock) -> &'a str {
        match self.mode {
            PinOverlayMode::Ocr => &block.text,
            PinOverlayMode::Translation => block.translated.as_deref().unwrap_or(&block.text),
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let sel = self.selection.filter(|s| !s.is_empty())?;
        let data = self.data.as_ref()?;
        let mut out = String::new();
        let mut prev_row: Option<usize> = None;
        for (idx, block) in data.blocks.iter().enumerate() {
            let text = self.block_text(block);
            let chars: Vec<char> = text.chars().collect();
            let Some((c0, c1)) = sel.block_range(idx, chars.len()) else {
                continue;
            };
            if !out.is_empty() {
                if prev_row == Some(block.row) {
                    out.push(' ');
                } else {
                    out.push('\n');
                }
            }
            for ch in &chars[c0..c1] {
                out.push(*ch);
            }
            prev_row = Some(block.row);
        }
        (!out.is_empty()).then_some(out)
    }

    pub fn select_all(&mut self) {
        let Some(data) = &self.data else { return };
        if data.blocks.is_empty() {
            return;
        }
        let last_idx = data.blocks.len() - 1;
        let last_len = self.block_text(&data.blocks[last_idx]).chars().count();
        self.selection = Some(TextSelection {
            anchor: (0, 0),
            focus: (last_idx, last_len),
        });
    }
}

struct RenderedBlock {
    idx: usize,
    row: usize,
    orig_rect: Rect,
    hit_rect: Rect,
    patch_rect: Rect,
    text_pos: Pos2,
    galley: Arc<Galley>,
    /// Horizontal scale factor used in OCR mode to align galley glyphs with the original image box.
    x_scale: f32,
    bg_color: Color32,
    fg_color: Color32,
    char_len: usize,
    full_text: String,
    truncated: bool,
}

impl RenderedBlock {
    fn char_index_at(&self, pos: Pos2, is_ocr: bool) -> usize {
        let local_x = if is_ocr {
            (pos.x - self.orig_rect.min.x) / self.x_scale.max(0.05)
        } else {
            pos.x - self.text_pos.x
        };
        let local_y = (pos.y - self.text_pos.y).clamp(0.0, self.galley.size().y.max(1.0));
        self.galley
            .cursor_from_pos(Vec2::new(local_x, local_y))
            .ccursor
            .index
            .min(self.char_len)
    }

    fn selection_rects(&self, c0: usize, c1: usize, is_ocr: bool) -> Vec<Rect> {
        if c1 <= c0 || self.char_len == 0 {
            return Vec::new();
        }
        if is_ocr {
            let p0 = self
                .galley
                .pos_from_cursor(&self.galley.from_ccursor(egui::text::CCursor::new(c0)));
            let p1 = self
                .galley
                .pos_from_cursor(&self.galley.from_ccursor(egui::text::CCursor::new(c1)));
            let x0 = (self.orig_rect.min.x + p0.min.x * self.x_scale)
                .clamp(self.orig_rect.min.x, self.orig_rect.max.x);
            let x1 = (self.orig_rect.min.x + p1.min.x * self.x_scale)
                .clamp(x0, self.orig_rect.max.x);
            return vec![Rect::from_min_max(
                Pos2::new(x0, self.orig_rect.min.y),
                Pos2::new(x1.max(x0 + 2.0).min(self.orig_rect.max.x), self.orig_rect.max.y),
            )];
        }

        let mut rects = Vec::new();
        let mut row_start = 0usize;
        for (row_idx, row) in self.galley.rows.iter().enumerate() {
            let row_chars = row.char_count_excluding_newline();
            let row_end = row_start + row_chars;
            let s = c0.max(row_start).min(row_end);
            let e = c1.max(row_start).min(row_end);
            if e > s {
                let p0 = self.galley.pos_from_cursor(&self.galley.from_rcursor(
                    egui::epaint::text::cursor::RCursor {
                        row: row_idx,
                        column: s - row_start,
                    },
                ));
                let p1 = self.galley.pos_from_cursor(&self.galley.from_rcursor(
                    egui::epaint::text::cursor::RCursor {
                        row: row_idx,
                        column: e - row_start,
                    },
                ));
                let min_x = (self.text_pos.x + p0.min.x).max(self.patch_rect.min.x);
                let max_x = (self.text_pos.x + p1.min.x)
                    .max(min_x + 2.0)
                    .min(self.patch_rect.max.x);
                let min_y = (self.text_pos.y + row.rect.min.y).max(self.patch_rect.min.y);
                let max_y = (self.text_pos.y + row.rect.max.y).min(self.patch_rect.max.y);
                if max_x > min_x && max_y > min_y {
                    rects.push(Rect::from_min_max(
                        Pos2::new(min_x, min_y),
                        Pos2::new(max_x, max_y),
                    ));
                }
            }
            row_start = row_end + usize::from(row.ends_with_newline);
        }
        rects
    }
}

/// Resolve pairwise overlaps among base OCR bounding boxes in screen coordinates so neither
/// highlight frames nor translation slots overlap across tightly spaced lines or columns.
fn resolve_base_rect_overlaps(rects: &mut [Rect], rows: &[usize], bounds: Rect) {
    let n = rects.len();
    if n == 0 {
        return;
    }

    for r in rects.iter_mut() {
        let min_x = r.min.x.clamp(bounds.min.x, bounds.max.x - 2.0);
        let min_y = r.min.y.clamp(bounds.min.y, bounds.max.y - 2.0);
        let max_x = r.max.x.clamp(min_x + 2.0, bounds.max.x);
        let max_y = r.max.y.clamp(min_y + 2.0, bounds.max.y);
        *r = Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y));
    }

    if n < 2 {
        return;
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        rows[a]
            .cmp(&rows[b])
            .then_with(|| rects[a].min.x.total_cmp(&rects[b].min.x))
            .then_with(|| rects[a].min.y.total_cmp(&rects[b].min.y))
    });

    // Two settlement passes ensure transitive chains of tight lines become cleanly disjoint.
    for _ in 0..2 {
        // 1. Resolve same-row horizontal overlaps.
        for pair in order.windows(2) {
            let a = pair[0];
            let b = pair[1];
            if rows[a] != rows[b] {
                continue;
            }
            let v_overlap = rects[a].max.y.min(rects[b].max.y) - rects[a].min.y.max(rects[b].min.y);
            if v_overlap <= 0.0 {
                continue;
            }
            if rects[a].max.x > rects[b].min.x - 1.0 {
                let mid = (rects[a].max.x + rects[b].min.x) * 0.5;
                let a_max = (mid - 0.5).max(rects[a].min.x + 2.0);
                let b_min = (mid + 0.5).min(rects[b].max.x - 2.0);
                if b_min > a_max {
                    rects[a].max.x = a_max;
                    rects[b].min.x = b_min;
                } else {
                    rects[a].max.x = mid.max(rects[a].min.x + 2.0);
                    rects[b].min.x = rects[a].max.x.min(rects[b].max.x - 2.0);
                }
            }
        }

        // 2. Resolve cross-row vertical overlaps for horizontally overlapping blocks.
        let mut y_order: Vec<usize> = (0..n).collect();
        y_order.sort_by(|&a, &b| {
            rects[a]
                .center()
                .y
                .total_cmp(&rects[b].center().y)
                .then_with(|| rects[a].min.x.total_cmp(&rects[b].min.x))
        });

        for i in 0..n {
            let a = y_order[i];
            for &b in &y_order[(i + 1)..] {
                if rows[a] == rows[b] {
                    continue;
                }
                let h_overlap =
                    rects[a].max.x.min(rects[b].max.x) - rects[a].min.x.max(rects[b].min.x);
                if h_overlap <= 0.0 {
                    continue;
                }
                if rects[a].max.y > rects[b].min.y - 1.0 {
                    let mid = (rects[a].max.y + rects[b].min.y) * 0.5;
                    let a_max = (mid - 0.5).max(rects[a].min.y + 4.0);
                    let b_min = (mid + 0.5).min(rects[b].max.y - 4.0);
                    if b_min >= a_max {
                        rects[a].max.y = a_max;
                        rects[b].min.y = b_min;
                    } else {
                        let split = mid.clamp(rects[a].min.y + 2.0, (rects[b].max.y - 2.0).max(rects[a].min.y + 2.0));
                        rects[a].max.y = split;
                        rects[b].min.y = split;
                    }
                }
            }
        }
    }
}

/// Final collision resolution pass for translation patches so no two painted patches overlap.
fn resolve_patch_overlaps(blocks: &mut [RenderedBlock], bounds: Rect) {
    let n = blocks.len();
    if n < 2 {
        return;
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        blocks[a]
            .row
            .cmp(&blocks[b].row)
            .then_with(|| blocks[a].orig_rect.min.y.total_cmp(&blocks[b].orig_rect.min.y))
            .then_with(|| blocks[a].orig_rect.min.x.total_cmp(&blocks[b].orig_rect.min.x))
    });

    for pos in 0..n {
        let cur = order[pos];
        for &prev in &order[..pos] {
            let a = blocks[prev].patch_rect;
            let b = blocks[cur].patch_rect;
            let ox = a.max.x.min(b.max.x) - a.min.x.max(b.min.x);
            let oy = a.max.y.min(b.max.y) - a.min.y.max(b.min.y);
            if ox <= 0.0 || oy <= 0.0 {
                continue;
            }

            let same_row = blocks[prev].row == blocks[cur].row;
            if same_row || (ox < oy && blocks[cur].orig_rect.min.x >= blocks[prev].orig_rect.max.x - 2.0) {
                let target_min_x = (a.max.x + 1.0).min(bounds.max.x - 4.0);
                if target_min_x < b.max.x - 3.0 {
                    blocks[cur].patch_rect.min.x = target_min_x;
                } else {
                    let mid = (a.max.x + b.min.x) * 0.5;
                    blocks[prev].patch_rect.max.x = (mid - 0.5).max(a.min.x + 4.0);
                    blocks[cur].patch_rect.min.x = (mid + 0.5).min(b.max.x - 4.0);
                }
            } else {
                let target_min_y = (a.max.y + 1.0).min(bounds.max.y - 4.0);
                if target_min_y < b.max.y - 4.0 {
                    blocks[cur].patch_rect.min.y = target_min_y;
                } else {
                    let mid = (a.max.y + b.min.y) * 0.5;
                    blocks[prev].patch_rect.max.y = (mid - 0.5).max(a.min.y + 4.0);
                    blocks[cur].patch_rect.min.y = (mid + 0.5).min(b.max.y - 4.0);
                }
            }
        }
    }

    // Re-sync text positions and hit_rects inside the adjusted non-overlapping patch_rects.
    for rb in blocks.iter_mut() {
        let p = rb.patch_rect;
        let gs = rb.galley.size();
        let tx = if gs.x <= p.width() - 2.0 {
            rb.text_pos.x.clamp(p.min.x + 1.0, (p.max.x - 1.0 - gs.x).max(p.min.x + 1.0))
        } else {
            p.min.x + 1.0
        };
        let ty = if gs.y <= p.height() {
            (p.center().y - gs.y * 0.5).clamp(p.min.y, (p.max.y - gs.y).max(p.min.y))
        } else {
            p.min.y
        };
        rb.text_pos = Pos2::new(tx, ty);
        rb.hit_rect = p;
    }
}

fn layout_wrapped_clamped(
    painter: &egui::Painter,
    text: &str,
    font_size: f32,
    fg_color: Color32,
    max_width: f32,
    max_rows: usize,
) -> (Arc<Galley>, bool) {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat::simple(FontId::proportional(font_size), fg_color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(12.0),
        max_rows: max_rows.max(1),
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let truncated = galley.elided;
    (galley, truncated)
}

pub struct PinWindowState {
    pub pin_id: String,
    pub capture_id: String,
    pub base_width: u32,
    pub base_height: u32,
    pub scale: i64,
    pub opacity: i64,
    pub passthrough: bool,
    pub double_click_close: bool,
    pub texture: Option<TextureHandle>,
    pub image_dirty: bool,
    pub gif_player: Option<GifPlayer>,
    pub drag_origin: Option<((i32, i32), (i32, i32))>, // (cursor_start, window_start)
    pub panel_drag_origin: Option<((i32, i32), (i32, i32))>, // (cursor_start, initial_offset)
    pub text_panel_offset: (i32, i32),
    pub message: Option<(String, Instant)>,
    pub text_overlay: Option<PinTextOverlay>,
}

impl PinWindowState {
    pub fn new(app: &App, pin_id: String, capture_id: String) -> Result<Self, String> {
        let (item, opacity, scale, dbl_close, gif_frames) = {
            let core = app.core.lock().unwrap();
            let item = service::find_image(&core, app, &capture_id)?;
            let gf = core
                .captures
                .get(&capture_id)
                .and_then(|s| s.gif_frames.clone());
            (
                item,
                core.settings["pin"]["opacity"].as_i64().unwrap_or(100),
                core.settings["pin"]["scale"].as_i64().unwrap_or(100),
                core.settings["pin"]["doubleClickClose"]
                    .as_bool()
                    .unwrap_or(false),
                gf,
            )
        };
        let rgba = service::static_rgba(app, &capture_id)?;
        let gif_player = if item.kind == "gif" {
            gif_frames.and_then(GifPlayer::new)
        } else {
            None
        };
        Ok(Self {
            pin_id,
            capture_id,
            base_width: rgba.width.max(1),
            base_height: rgba.height.max(1),
            scale: scale.clamp(25, 200),
            opacity: opacity.clamp(20, 100),
            passthrough: false,
            double_click_close: dbl_close,
            texture: None,
            image_dirty: true,
            gif_player,
            drag_origin: None,
            panel_drag_origin: None,
            text_panel_offset: (0, 0),
            message: None,
            text_overlay: None,
        })
    }

    pub fn is_text_overlay(&self) -> bool {
        self.text_overlay.is_some()
    }

    pub fn wants_text_panel(&self) -> bool {
        self.text_overlay
            .as_ref()
            .is_some_and(|ov| ov.show_text_panel)
    }

    pub fn lang_picker_open(&self) -> bool {
        self.text_overlay
            .as_ref()
            .is_some_and(|ov| ov.lang_picker_open)
    }

    pub fn close_lang_picker(&mut self) -> bool {
        if let Some(ov) = self.text_overlay.as_mut() {
            if ov.lang_picker_open {
                ov.lang_picker_open = false;
                return true;
            }
        }
        false
    }

    /// Dismiss the active overlay error banner; if no recognized text exists yet (e.g. blank page),
    /// also close the companion plain-text panel and return the pin to normal mode.
    pub fn dismiss_overlay_error(&mut self, fb: &mut PinUiFeedback) -> bool {
        let has_err = self
            .text_overlay
            .as_ref()
            .is_some_and(|ov| !ov.busy && ov.error.is_some());
        if !has_err {
            return false;
        }
        let no_data = self
            .text_overlay
            .as_ref()
            .is_some_and(|ov| ov.data.is_none());
        if no_data {
            self.text_overlay = None;
            fb.text_panel_visible = Some(false);
            fb.geometry_changed = true;
            fb.redraw_toolbar = true;
        } else if let Some(ov) = self.text_overlay.as_mut() {
            ov.error = None;
        }
        fb.redraw_image = true;
        fb.redraw_text_panel = true;
        true
    }

    pub fn set_message(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), Instant::now() + Duration::from_secs(4)));
    }

    /// Activate OCR or Translation in-place on this pin window (never shifting the pin's position)
    /// and open the right-side plain-text companion panel.
    pub fn activate_overlay_mode(
        &mut self,
        app: &Arc<App>,
        mode: PinOverlayMode,
        force_refresh: bool,
    ) {
        let lang_key = {
            let core = app.core.lock().unwrap();
            current_lang_key(&core.settings)
        };
        if self.text_overlay.is_none() {
            self.text_overlay = Some(PinTextOverlay::new(mode, lang_key.clone()));
            self.text_panel_offset = (0, 0);
            service::run_pin_overlay_process(
                app,
                self.pin_id.clone(),
                self.capture_id.clone(),
                mode,
                None,
            );
            return;
        }
        self.trigger_overlay_mode(app, mode, force_refresh);
    }

    pub fn apply_overlay_result(
        &mut self,
        mode: PinOverlayMode,
        lang_key: String,
        result: Result<OcrOverlayData, String>,
    ) {
        let Some(overlay) = self.text_overlay.as_mut() else {
            return;
        };
        overlay.busy = false;
        overlay.mode = mode;
        overlay.selection = None;
        overlay.selecting = false;
        match result {
            Ok(data) => {
                overlay.error = None;
                if mode == PinOverlayMode::Translation {
                    overlay.translated_lang_key = lang_key;
                }
                overlay.data = Some(data);
                overlay.sync_plain_bufs();
            }
            Err(err) => {
                overlay.error = Some(err.clone());
                self.set_message(err);
            }
        }
    }

    /// Render the main pin image window (pure image + non-overlapping interactive text overlay).
    /// The toolbar lives in a separate companion window below the pin so the image is never occluded.
    pub fn show_image(
        &mut self,
        ctx: &egui::Context,
        app: &Arc<App>,
        window: &winit::window::Window,
        hwnd: Option<isize>,
    ) -> PinUiFeedback {
        let mut fb = PinUiFeedback::default();

        if let Some((_, exp)) = &self.message {
            let now = Instant::now();
            if now >= *exp {
                self.message = None;
            } else {
                ctx.request_repaint_after(exp.saturating_duration_since(now));
            }
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.close_lang_picker() {
                fb.redraw_toolbar = true;
            } else if self.dismiss_overlay_error(&mut fb) {
                ctx.request_repaint();
            } else {
                app.emit(AppEvent::ClosePin {
                    pin_id: self.pin_id.clone(),
                });
                return fb;
            }
        }

        // Refresh static image texture.
        let freeze_gif = self
            .text_overlay
            .as_ref()
            .is_some_and(|ov| ov.show_layer);
        let max_side = ctx.input(|i| i.max_texture_side);
        if (self.gif_player.is_none() || freeze_gif) && (self.image_dirty || self.texture.is_none())
        {
            if let Ok(rgba) = service::static_rgba(app, &self.capture_id) {
                let img = ui::rgba_to_color_image_fit(&rgba, max_side);
                if let Some(tex) = self.texture.as_mut() {
                    tex.set(img, TextureOptions::LINEAR);
                } else {
                    self.texture = Some(ctx.load_texture(
                        format!("pin_tex_{}", self.pin_id),
                        img,
                        TextureOptions::LINEAR,
                    ));
                }
                self.image_dirty = false;
            }
        }

        // Keyboard shortcuts on text-overlay pin (Ctrl+C / Ctrl+A).
        if let Some(overlay) = self.text_overlay.as_mut() {
            if !ctx.wants_keyboard_input() && overlay.show_layer && !overlay.busy {
                let (ctrl_a, ctrl_c) = ctx.input(|i| {
                    (
                        i.modifiers.command && i.key_pressed(egui::Key::A),
                        i.modifiers.command && i.key_pressed(egui::Key::C),
                    )
                });
                if ctrl_a && overlay.has_positioned_blocks() {
                    overlay.select_all();
                } else if ctrl_c {
                    let to_copy = overlay
                        .selected_text()
                        .unwrap_or_else(|| overlay.active_full_text());
                    if !to_copy.trim().is_empty() {
                        match clipboard::write_text(&to_copy) {
                            Ok(()) => self.set_message(i18n::t("Text copied")),
                            Err(e) => self.set_message(e),
                        }
                    }
                }
            }
        }

        let screen = ctx.screen_rect();
        let image_rect = pin_image_rect_logical(screen, ctx.pixels_per_point());
        let palette = crate::ui::ctx_palette(ctx);

        // Mouse wheel zoom.
        {
            let scroll_y = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll_y.abs() > 0.5 {
                let delta = if scroll_y > 0.0 { 10 } else { -10 };
                if self.apply_zoom(
                    app,
                    hwnd,
                    (self.scale + delta).clamp(25, 200),
                    window.scale_factor(),
                ) {
                    fb.geometry_changed = true;
                }
            }
        }

        let mut selection_anchor_rect: Option<Rect> = None;
        let mut hovered_truncated_tip: Option<(Pos2, String)> = None;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                // 0. Draw soft elevation shadow extending outward on all four sides around the image.
                paint_pin_shadow(ui.painter(), image_rect);

                // 1. Draw base image (or animated GIF when text overlay is not active).
                if !freeze_gif {
                    if let Some(player) = self.gif_player.as_mut() {
                        if let Some(tex) =
                            player.current_texture(ctx, &format!("pin_gif_{}", self.pin_id))
                        {
                            ui.painter().image(
                                tex.id(),
                                image_rect,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                    } else if let Some(tex) = &self.texture {
                        ui.painter().image(
                            tex.id(),
                            image_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                } else if let Some(tex) = &self.texture {
                    ui.painter().image(
                        tex.id(),
                        image_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                // 2. Positioned non-overlapping interactive text overlay layer (OCR or in-place Translation).
                let has_positioned = self.text_overlay.as_ref().is_some_and(|ov| {
                    ov.show_layer && !ov.busy && ov.has_positioned_blocks()
                });

                if has_positioned {
                    let rendered = self.build_rendered_blocks(ui.painter(), image_rect);
                    let resp = ui.allocate_rect(screen, Sense::click_and_drag());
                    let pointer_pos =
                        ui.input(|i| i.pointer.hover_pos().or(i.pointer.interact_pos()));
                    let alt_down = ui.input(|i| i.modifiers.alt);
                    let is_ocr = self
                        .text_overlay
                        .as_ref()
                        .is_some_and(|o| o.mode == PinOverlayMode::Ocr);

                    let hovered_block_idx = if resp.hovered() && !alt_down {
                        pointer_pos.and_then(|pos| {
                            hit_text_block(&rendered, image_rect, pos).map(|b| b.idx)
                        })
                    } else {
                        None
                    };

                    if let (Some(h_idx), Some(pos)) = (hovered_block_idx, pointer_pos) {
                        if !is_ocr {
                            if let Some(rb) = rendered.iter().find(|b| b.idx == h_idx && b.truncated)
                            {
                                hovered_truncated_tip = Some((pos, rb.full_text.clone()));
                            }
                        }
                    }

                    // Cursor feedback: I-beam over text blocks, Grabbing when dragging window.
                    if self.drag_origin.is_some() {
                        ctx.set_cursor_icon(CursorIcon::Grabbing);
                    } else if self
                        .text_overlay
                        .as_ref()
                        .is_some_and(|o| o.selecting)
                        || hovered_block_idx.is_some()
                    {
                        ctx.set_cursor_icon(CursorIcon::Text);
                    }

                    // Double-click handling: double-click a text block selects the whole block;
                    // double-click empty image follows normal pin double-click behavior.
                    if resp.double_clicked() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            if let Some(block) = hit_text_block(&rendered, image_rect, pos) {
                                if let Some(ov) = self.text_overlay.as_mut() {
                                    ov.selection = Some(TextSelection {
                                        anchor: (block.idx, 0),
                                        focus: (block.idx, block.char_len),
                                    });
                                    ov.selecting = false;
                                }
                            } else if self.double_click_close {
                                app.emit(AppEvent::ClosePin {
                                    pin_id: self.pin_id.clone(),
                                });
                            } else {
                                let _ = service::open_result(app, &self.capture_id);
                            }
                        }
                    } else if resp.clicked() {
                        if let Some(ov) = self.text_overlay.as_mut() {
                            ov.selection = None;
                            ov.selecting = false;
                        }
                    }

                    // Drag handling:
                    // - Middle drag, Alt+Primary drag, outer shadow rim drag, or drag starting outside text blocks -> move window.
                    // - Primary drag starting on (or right beside) a text block -> select text across one or more blocks.
                    let middle_started = resp.drag_started_by(egui::PointerButton::Middle);
                    let primary_started = resp.drag_started_by(egui::PointerButton::Primary);
                    if middle_started {
                        self.begin_window_drag(window);
                    } else if primary_started {
                        let start_pos = resp.interact_pointer_pos();
                        let hit = if alt_down {
                            None
                        } else {
                            start_pos.and_then(|pos| {
                                hit_text_block(&rendered, image_rect, pos)
                                    .map(|b| (b.idx, b.char_index_at(pos, is_ocr)))
                            })
                        };
                        if let Some(anchor) = hit {
                            if let Some(ov) = self.text_overlay.as_mut() {
                                ov.selection = Some(TextSelection {
                                    anchor,
                                    focus: anchor,
                                });
                                ov.selecting = true;
                            }
                            self.drag_origin = None;
                        } else {
                            if let Some(ov) = self.text_overlay.as_mut() {
                                ov.selection = None;
                                ov.selecting = false;
                            }
                            self.begin_window_drag(window);
                        }
                    } else if resp.dragged_by(egui::PointerButton::Primary)
                        || resp.dragged_by(egui::PointerButton::Middle)
                    {
                        if self.drag_origin.is_some() {
                            if self.continue_window_drag(hwnd) {
                                fb.geometry_changed = true;
                            }
                        } else if let Some(pos) = resp.interact_pointer_pos() {
                            if let Some(focus) = nearest_text_cursor(&rendered, pos, is_ocr) {
                                if let Some(ov) = self.text_overlay.as_mut() {
                                    if ov.selecting {
                                        if let Some(sel) = ov.selection.as_mut() {
                                            sel.focus = focus;
                                        }
                                    }
                                }
                            }
                        }
                    } else if resp.drag_stopped() {
                        self.drag_origin = None;
                        if let Some(ov) = self.text_overlay.as_mut() {
                            ov.selecting = false;
                            if ov.selection.is_some_and(|s| s.is_empty()) {
                                ov.selection = None;
                            }
                        }
                    }

                    // Paint non-overlapping text blocks and selection highlights.
                    let selection = self.text_overlay.as_ref().and_then(|o| o.selection);
                    let sel_fill = Color32::from_rgba_unmultiplied(
                        palette.accent.r(),
                        palette.accent.g(),
                        palette.accent.b(),
                        105,
                    );
                    let mut sel_union: Option<Rect> = None;

                    if is_ocr {
                        for rb in &rendered {
                            let is_hovered = hovered_block_idx == Some(rb.idx);
                            let (fill, stroke) = if is_hovered {
                                (
                                    Color32::from_rgba_unmultiplied(
                                        palette.accent.r(),
                                        palette.accent.g(),
                                        palette.accent.b(),
                                        34,
                                    ),
                                    Stroke::new(
                                        1.0_f32,
                                        Color32::from_rgba_unmultiplied(
                                            palette.accent.r(),
                                            palette.accent.g(),
                                            palette.accent.b(),
                                            140,
                                        ),
                                    ),
                                )
                            } else {
                                (
                                    Color32::from_rgba_unmultiplied(
                                        palette.accent.r(),
                                        palette.accent.g(),
                                        palette.accent.b(),
                                        14,
                                    ),
                                    Stroke::new(
                                        0.75_f32,
                                        Color32::from_rgba_unmultiplied(
                                            palette.accent.r(),
                                            palette.accent.g(),
                                            palette.accent.b(),
                                            62,
                                        ),
                                    ),
                                )
                            };
                            ui.painter().rect(
                                rb.orig_rect,
                                CornerRadius::same(2),
                                fill,
                                stroke,
                                egui::StrokeKind::Inside,
                            );

                            if let Some(sel) = selection {
                                if let Some((c0, c1)) = sel.block_range(rb.idx, rb.char_len) {
                                    for sr in rb.selection_rects(c0, c1, true) {
                                        ui.painter().rect_filled(
                                            sr,
                                            CornerRadius::same(2),
                                            sel_fill,
                                        );
                                        sel_union = Some(match sel_union {
                                            Some(u) => u.union(sr),
                                            None => sr,
                                        });
                                    }
                                }
                            }
                        }
                    } else {
                        // Two-pass painting for Translation mode:
                        // Pass 1: Paint all background patches first so no patch ever covers another block's glyphs.
                        for rb in &rendered {
                            ui.painter().rect_filled(
                                rb.patch_rect,
                                CornerRadius::same(2),
                                rb.bg_color,
                            );
                        }
                        // Pass 2: Hover borders and selection highlights.
                        for rb in &rendered {
                            if hovered_block_idx == Some(rb.idx) {
                                ui.painter().rect_stroke(
                                    rb.patch_rect,
                                    CornerRadius::same(2),
                                    Stroke::new(
                                        1.0_f32,
                                        Color32::from_rgba_unmultiplied(
                                            palette.accent.r(),
                                            palette.accent.g(),
                                            palette.accent.b(),
                                            135,
                                        ),
                                    ),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if let Some(sel) = selection {
                                if let Some((c0, c1)) = sel.block_range(rb.idx, rb.char_len) {
                                    for sr in rb.selection_rects(c0, c1, false) {
                                        ui.painter().rect_filled(
                                            sr,
                                            CornerRadius::same(2),
                                            sel_fill,
                                        );
                                        sel_union = Some(match sel_union {
                                            Some(u) => u.union(sr),
                                            None => sr,
                                        });
                                    }
                                }
                            }
                        }
                        // Pass 3: Paint all translated galleys clipped strictly to their non-overlapping patch_rect.
                        for rb in &rendered {
                            ui.painter().with_clip_rect(rb.patch_rect).galley(
                                rb.text_pos,
                                rb.galley.clone(),
                                rb.fg_color,
                            );
                        }
                    }

                    if self.text_overlay.as_ref().is_some_and(|o| {
                        !o.selecting && o.selection.is_some_and(|s| !s.is_empty())
                    }) {
                        selection_anchor_rect = sel_union;
                    }
                } else {
                    // Normal pin (or overlay busy / layer hidden): full-area drag and double-click.
                    let resp = ui.allocate_rect(screen, Sense::click_and_drag());
                    if resp.double_clicked() {
                        if self.double_click_close {
                            app.emit(AppEvent::ClosePin {
                                pin_id: self.pin_id.clone(),
                            });
                        } else {
                            let _ = service::open_result(app, &self.capture_id);
                        }
                    }
                    if resp.drag_started_by(egui::PointerButton::Primary)
                        || resp.drag_started_by(egui::PointerButton::Middle)
                    {
                        self.begin_window_drag(window);
                    } else if resp.dragged_by(egui::PointerButton::Primary)
                        || resp.dragged_by(egui::PointerButton::Middle)
                    {
                        if self.continue_window_drag(hwnd) {
                            fb.geometry_changed = true;
                        }
                    } else if resp.drag_stopped() {
                        self.drag_origin = None;
                    }
                }
            });

        // 3. Hover tooltip showing complete text when a cramped translation block was ellipsized.
        if let Some((pos, full_text)) = hovered_truncated_tip {
            let max_w = (screen.width() - 16.0).clamp(140.0, 320.0);
            let tip_x = (pos.x + 10.0)
                .clamp(screen.min.x + 6.0, (screen.max.x - max_w - 6.0).max(screen.min.x + 6.0));
            let tip_y = if pos.y + 18.0 + 48.0 <= screen.max.y {
                pos.y + 18.0
            } else {
                (pos.y - 44.0).max(screen.min.y + 6.0)
            };
            egui::Area::new(egui::Id::new(format!("pin_trunc_tip_{}", self.pin_id)))
                .fixed_pos(Pos2::new(tip_x, tip_y))
                .order(egui::Order::Tooltip)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(palette.floating_bg_alpha(242))
                        .stroke(Stroke::new(1.0_f32, palette.accent.gamma_multiply(0.75)))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.set_max_width(max_w);
                            ui.label(RichText::new(full_text).color(Color32::WHITE).small());
                        });
                });
        }

        // 4. Floating mini "Copy" pill next to active text selection.
        if let Some(sel_rect) = selection_anchor_rect {
            let pill_w = 72.0;
            let pill_h = 26.0;
            let pill_x = (sel_rect.center().x - pill_w * 0.5).clamp(
                screen.min.x + 6.0,
                (screen.max.x - pill_w - 6.0).max(screen.min.x + 6.0),
            );
            let pill_y = if sel_rect.min.y - pill_h - 6.0 >= screen.min.y + 4.0 {
                sel_rect.min.y - pill_h - 6.0
            } else {
                (sel_rect.max.y + 6.0).min((screen.max.y - pill_h - 4.0).max(screen.min.y + 4.0))
            };
            let mut do_copy_sel = false;
            egui::Area::new(egui::Id::new(format!("pin_copy_sel_{}", self.pin_id)))
                .fixed_pos(Pos2::new(pill_x, pill_y))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(palette.floating_bg_alpha(238))
                        .stroke(Stroke::new(1.0_f32, palette.accent.gamma_multiply(0.85)))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(4, 2))
                        .show(ui, |ui| {
                            if icons::dark_icon_text_button(
                                ui,
                                Icon::Copy,
                                i18n::t("Copy"),
                                palette.accent,
                                palette.accent_hover,
                            )
                            .clicked()
                            {
                                do_copy_sel = true;
                            }
                        });
                });
            if do_copy_sel {
                if let Some(text) = self.text_overlay.as_ref().and_then(|o| o.selected_text()) {
                    match clipboard::write_text(&text) {
                        Ok(()) => {
                            self.set_message(i18n::t("Text copied"));
                            if let Some(ov) = self.text_overlay.as_mut() {
                                ov.selection = None;
                            }
                        }
                        Err(e) => self.set_message(e),
                    }
                }
            }
        }

        // 5. Loading indicator badge while OCR / translation is running.
        if let Some(ov) = &self.text_overlay {
            if ov.busy {
                let label = if ov.mode == PinOverlayMode::Ocr {
                    i18n::t("Recognizing...")
                } else {
                    i18n::t("Translating...")
                };
                egui::Area::new(egui::Id::new(format!("pin_busy_{}", self.pin_id)))
                    .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                    .order(egui::Order::Foreground)
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::NONE
                            .fill(palette.floating_bg_alpha(230))
                            .stroke(Stroke::new(1.0_f32, palette.accent.gamma_multiply(0.8)))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(14, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(RichText::new(label).color(Color32::WHITE).strong());
                                });
                            });
                    });
            }
        }

        // 6. Error banner with quick Retry / Provider Settings / Close action when overlay processing failed.
        if let Some(err_text) = self
            .text_overlay
            .as_ref()
            .filter(|o| !o.busy)
            .and_then(|o| o.error.clone())
        {
            let mut retry_mode: Option<PinOverlayMode> = None;
            let mut open_settings = false;
            let mut dismiss_err = false;
            let dark = ctx.style().visuals.dark_mode;
            let card_bg = if dark {
                Color32::from_rgba_unmultiplied(
                    palette.dark_window.r(),
                    palette.dark_window.g(),
                    palette.dark_window.b(),
                    245,
                )
            } else {
                Color32::from_rgba_unmultiplied(0xfc, 0xfd, 0xfd, 248)
            };
            egui::Area::new(egui::Id::new(format!("pin_err_{}", self.pin_id)))
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let max_w = (screen.width() - 16.0).clamp(120.0, 340.0);
                    egui::Frame::NONE
                        .fill(card_bg)
                        .stroke(Stroke::new(1.2_f32, ui::DANGER))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(max_w);
                            ui.horizontal(|ui| {
                                icons::paint_inline(ui, Icon::Alert, 14.0, ui::DANGER);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Min),
                                    |ui| {
                                        if icons::small_icon_button(ui, Icon::Close)
                                            .on_hover_text(i18n::t("Close"))
                                            .clicked()
                                        {
                                            dismiss_err = true;
                                        }
                                        ui.with_layout(
                                            egui::Layout::left_to_right(egui::Align::Min),
                                            |ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(err_text).small(),
                                                    )
                                                    .wrap(),
                                                );
                                            },
                                        );
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                if icons::small_icon_text_button(
                                    ui,
                                    Icon::Reselect,
                                    i18n::t("Retry"),
                                    false,
                                    true,
                                )
                                .clicked()
                                {
                                    retry_mode = self.text_overlay.as_ref().map(|o| o.mode);
                                }
                                if icons::small_icon_text_button(
                                    ui,
                                    Icon::SettingsGeneral,
                                    i18n::t("Provider Settings"),
                                    false,
                                    false,
                                )
                                .clicked()
                                {
                                    open_settings = true;
                                }
                                if icons::small_icon_text_button(
                                    ui,
                                    Icon::Close,
                                    i18n::t("Close"),
                                    false,
                                    false,
                                )
                                .clicked()
                                {
                                    dismiss_err = true;
                                }
                            });
                        });
                });
            if let Some(mode) = retry_mode {
                self.trigger_overlay_mode(app, mode, true);
                fb.redraw_image = true;
                fb.redraw_toolbar = true;
                fb.redraw_text_panel = true;
            }
            if open_settings {
                app.emit(AppEvent::OpenSettings {
                    section: Some("providers".into()),
                });
            }
            if dismiss_err {
                self.dismiss_overlay_error(&mut fb);
                ctx.request_repaint();
            }
        }

        // 7. Inline toast message.
        if let Some((msg, _)) = &self.message {
            let top_offset = (image_rect.min.y - screen.min.y) + 8.0;
            egui::Area::new(egui::Id::new(format!("pin_msg_{}", self.pin_id)))
                .anchor(egui::Align2::CENTER_TOP, Vec2::new(0.0, top_offset))
                .order(egui::Order::Tooltip)
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(palette.toast_bg_alpha(230))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(10, 5))
                        .show(ui, |ui| {
                            ui.label(RichText::new(msg).color(Color32::WHITE).small());
                        });
                });
        }

        fb.keep_toolbar_open = self.lang_picker_open() || self.passthrough;
        fb
    }

    /// Estimated compact logical size of the bottom toolbar window for initial creation.
    pub fn estimated_toolbar_bar_size(&self) -> Vec2 {
        if self.is_text_overlay() {
            Vec2::new(438.0, 38.0)
        } else {
            Vec2::new(336.0, 38.0)
        }
    }

    /// Render the companion bottom toolbar window (placed outside/below the pin image).
    pub fn show_toolbar(
        &mut self,
        ctx: &egui::Context,
        app: &Arc<App>,
        pin_hwnd: Option<isize>,
        pin_scale_factor: f64,
        picker_above: bool,
    ) -> PinUiFeedback {
        let mut fb = PinUiFeedback::default();

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.close_lang_picker() {
                fb.redraw_toolbar = true;
                fb.geometry_changed = true;
                return fb;
            } else {
                app.emit(AppEvent::ClosePin {
                    pin_id: self.pin_id.clone(),
                });
                return fb;
            }
        }

        let palette = ui::ctx_palette(ctx);
        let dark = ctx.style().visuals.dark_mode;
        let bar_bg = if dark {
            palette.dark_window
        } else {
            Color32::from_rgb(0xfc, 0xfd, 0xfd)
        };
        let border_color = if dark {
            palette.dark_stroke
        } else {
            palette.light_stroke
        };
        let is_overlay = self.is_text_overlay();
        let picker_open = self.lang_picker_open();

        let cur_tgt_code = if is_overlay {
            let core = app.core.lock().unwrap();
            let raw = core.settings["providers"]["translation"]["target"]
                .as_str()
                .unwrap_or("en");
            crate::translate::normalize_lang_code(raw)
        } else {
            String::new()
        };

        let mut activate_mode: Option<(PinOverlayMode, bool)> = None;
        let mut picked_target_lang: Option<String> = None;
        let mut toggled_picker = false;
        let mut measured_bar = self.estimated_toolbar_bar_size();

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(bar_bg)
                    .stroke(Stroke::new(1.0_f32, border_color))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 5)),
            )
            .show(ctx, |ui| {
                let draw_picker = |ui: &mut egui::Ui,
                                   ov: &mut PinTextOverlay,
                                   cur_tgt: &str,
                                   picked: &mut Option<String>,
                                   pin_id: &str| {
                    let avail_w = ui.available_width().max(180.0);
                    ui.horizontal(|ui| {
                        let clear_w = if ov.lang_query.is_empty() { 0.0 } else { 24.0 };
                        ui.add(
                            egui::TextEdit::singleline(&mut ov.lang_query)
                                .desired_width((avail_w - clear_w - 6.0).max(120.0))
                                .hint_text(i18n::t("Search language (name / code)...")),
                        );
                        if !ov.lang_query.is_empty()
                            && icons::small_icon_button(ui, Icon::Close)
                                .on_hover_text(i18n::t("Clear search"))
                                .clicked()
                        {
                            ov.lang_query.clear();
                        }
                    });
                    ui.add_space(4.0);
                    ui.separator();
                    let list_h = (LANG_PICKER_HEIGHT_LOGICAL - 48.0).max(120.0);
                    let mut any = false;
                    egui::ScrollArea::vertical()
                        .id_salt(format!("pin_tb_lang_scroll_{}", pin_id))
                        .max_height(list_h)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_min_width((avail_w - 12.0).max(150.0));
                            for lang in crate::translate::LANGUAGES {
                                if !crate::translate::matches_language_query(lang, &ov.lang_query) {
                                    continue;
                                }
                                any = true;
                                let selected = cur_tgt == lang.code;
                                if ui.selectable_label(selected, lang.option_label()).clicked() {
                                    *picked = Some(lang.code.to_string());
                                }
                            }
                            if !any {
                                ui.label(
                                    RichText::new(i18n::t("No matching languages found"))
                                        .weak()
                                        .small(),
                                );
                            }
                        });
                };

                if picker_open && picker_above {
                    if let Some(ov) = self.text_overlay.as_mut() {
                        draw_picker(
                            ui,
                            ov,
                            &cur_tgt_code,
                            &mut picked_target_lang,
                            &self.pin_id,
                        );
                    }
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                }

                // Single horizontal toolbar row.
                let bar_resp = ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(5.0, 0.0);

                    if is_overlay {
                        let (mode, show_layer, show_panel, busy) = self
                            .text_overlay
                            .as_ref()
                            .map(|o| (o.mode, o.show_layer, o.show_text_panel, o.busy))
                            .unwrap();

                        if toolbar_icon_button(
                            ui,
                            Icon::Ocr,
                            i18n::t("Extract Text"),
                            show_layer && mode == PinOverlayMode::Ocr,
                        )
                        .clicked()
                            && !busy
                        {
                            activate_mode = Some((PinOverlayMode::Ocr, false));
                        }

                        if toolbar_icon_button(
                            ui,
                            Icon::Translate,
                            i18n::t("Translate"),
                            show_layer && mode == PinOverlayMode::Translation,
                        )
                        .clicked()
                            && !busy
                        {
                            activate_mode = Some((PinOverlayMode::Translation, false));
                        }

                        let short_code = cur_tgt_code.to_ascii_uppercase();
                        if toolbar_lang_button(ui, &short_code, picker_open)
                            .on_hover_text(i18n::t("Target Language"))
                            .clicked()
                        {
                            toggled_picker = true;
                        }

                        if toolbar_icon_button(ui, Icon::Copy, i18n::t("Copy Text"), false)
                            .clicked()
                        {
                            let text = self.text_overlay.as_ref().and_then(|ov| {
                                ov.selected_text().or_else(|| {
                                    let all = ov.active_full_text();
                                    (!all.trim().is_empty()).then_some(all)
                                })
                            });
                            if let Some(t) = text {
                                match clipboard::write_text(&t) {
                                    Ok(()) => self.set_message(i18n::t("Text copied")),
                                    Err(e) => self.set_message(e),
                                }
                            } else {
                                self.set_message(i18n::t("No text detected in the image"));
                            }
                            fb.redraw_image = true;
                        }

                        if toolbar_icon_button(
                            ui,
                            if show_layer { Icon::Eye } else { Icon::EyeOff },
                            if show_layer {
                                i18n::t("Hide Text Layer")
                            } else {
                                i18n::t("Show Text Layer")
                            },
                            show_layer,
                        )
                        .clicked()
                        {
                            if let Some(ov) = self.text_overlay.as_mut() {
                                ov.show_layer = !ov.show_layer;
                                if !ov.show_layer {
                                    ov.selection = None;
                                    ov.selecting = false;
                                }
                            }
                            fb.redraw_image = true;
                            fb.redraw_toolbar = true;
                        }

                        if toolbar_icon_button(
                            ui,
                            Icon::TextPanel,
                            if show_panel {
                                i18n::t("Hide Plain Text Panel")
                            } else {
                                i18n::t("Show Plain Text Panel")
                            },
                            show_panel,
                        )
                        .clicked()
                        {
                            let hiding_error_without_data = self.text_overlay.as_ref().is_some_and(
                                |ov| ov.show_text_panel && ov.error.is_some() && ov.data.is_none(),
                            );
                            if hiding_error_without_data {
                                self.dismiss_overlay_error(&mut fb);
                            } else if let Some(ov) = self.text_overlay.as_mut() {
                                ov.show_text_panel = !ov.show_text_panel;
                                fb.text_panel_visible = Some(ov.show_text_panel);
                                if ov.show_text_panel {
                                    fb.redraw_text_panel = true;
                                }
                                fb.redraw_toolbar = true;
                            }
                        }
                    } else {
                        // Standard pin toolbar actions.
                        if toolbar_icon_button(ui, Icon::Copy, i18n::t("Copy"), false).clicked() {
                            match service::copy_image(app, &self.capture_id) {
                                Ok(()) => self.set_message(i18n::t("Copied")),
                                Err(e) => self.set_message(e),
                            }
                            fb.redraw_image = true;
                        }
                        if toolbar_icon_button(ui, Icon::Save, i18n::t("Edit & Save"), false)
                            .clicked()
                        {
                            if let Err(e) = service::open_result(app, &self.capture_id) {
                                self.set_message(e);
                                fb.redraw_image = true;
                            }
                        }
                        if toolbar_icon_button(ui, Icon::Ocr, i18n::t("Extract Text"), false)
                            .clicked()
                        {
                            activate_mode = Some((PinOverlayMode::Ocr, false));
                        }
                        if toolbar_icon_button(ui, Icon::Translate, i18n::t("Translate"), false)
                            .clicked()
                        {
                            activate_mode = Some((PinOverlayMode::Translation, false));
                        }
                    }

                    if is_overlay
                        && toolbar_icon_button(ui, Icon::Save, i18n::t("Edit & Save"), false)
                            .clicked()
                    {
                        if let Err(e) = service::open_result(app, &self.capture_id) {
                            self.set_message(e);
                            fb.redraw_image = true;
                        }
                    }

                    if toolbar_icon_button(ui, Icon::ZoomOut, i18n::t("Zoom Out"), false).clicked()
                        && self.apply_zoom(
                            app,
                            pin_hwnd,
                            (self.scale - 10).clamp(25, 200),
                            pin_scale_factor,
                        )
                    {
                        fb.geometry_changed = true;
                        fb.redraw_image = true;
                    }
                    if toolbar_icon_button(ui, Icon::ZoomIn, i18n::t("Zoom In"), false).clicked()
                        && self.apply_zoom(
                            app,
                            pin_hwnd,
                            (self.scale + 10).clamp(25, 200),
                            pin_scale_factor,
                        )
                    {
                        fb.geometry_changed = true;
                        fb.redraw_image = true;
                    }
                    if toolbar_icon_button(
                        ui,
                        Icon::Passthrough,
                        if self.passthrough {
                            i18n::t("Disable click-through")
                        } else {
                            i18n::t("Enable click-through")
                        },
                        self.passthrough,
                    )
                    .clicked()
                    {
                        if let Some(h) = pin_hwnd {
                            let next = !self.passthrough;
                            native::set_window_click_through(h, next);
                            self.passthrough = next;
                            self.drag_origin = None;
                            if let Some(ps) = app.core.lock().unwrap().pins.get_mut(&self.pin_id) {
                                ps.passthrough = next;
                            }
                            if next {
                                self.set_message(i18n::t(
                                    "Click-through enabled; click again or press Show/Hide Pins shortcut to restore",
                                ));
                            } else {
                                self.set_message(i18n::t("Click-through disabled"));
                            }
                            fb.redraw_image = true;
                            fb.redraw_toolbar = true;
                        }
                    }

                    let mut op = self.opacity;
                    ui.spacing_mut().slider_width = 54.0;
                    if ui
                        .add(
                            egui::Slider::new(&mut op, 20..=100)
                                .show_value(false)
                                .trailing_fill(true),
                        )
                        .on_hover_text(i18n::tf("Opacity {pct}%", &[("pct", &op.to_string())]))
                        .changed()
                    {
                        self.opacity = op;
                        if let Some(ps) = app.core.lock().unwrap().pins.get_mut(&self.pin_id) {
                            ps.opacity = op;
                        }
                        fb.opacity_changed = Some(((op as u32) * 255 / 100) as u8);
                    }

                    if toolbar_icon_button(ui, Icon::Close, i18n::t("Close Pin Esc"), false)
                        .clicked()
                    {
                        app.emit(AppEvent::ClosePin {
                            pin_id: self.pin_id.clone(),
                        });
                    }
                });

                measured_bar = Vec2::new(
                    (bar_resp.response.rect.width() + 18.0).ceil(),
                    (bar_resp.response.rect.height() + 12.0).ceil().max(36.0),
                );

                if picker_open && !picker_above {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                    if let Some(ov) = self.text_overlay.as_mut() {
                        draw_picker(
                            ui,
                            ov,
                            &cur_tgt_code,
                            &mut picked_target_lang,
                            &self.pin_id,
                        );
                    }
                }
            });

        if toggled_picker {
            if let Some(ov) = self.text_overlay.as_mut() {
                ov.lang_picker_open = !ov.lang_picker_open;
                if ov.lang_picker_open {
                    ov.lang_query.clear();
                }
            }
            fb.geometry_changed = true;
            fb.redraw_toolbar = true;
        }

        if let Some(next_tgt) = picked_target_lang {
            if let Some(ov) = self.text_overlay.as_mut() {
                ov.lang_picker_open = false;
            }
            let mut next_settings = app.core.lock().unwrap().settings.clone();
            next_settings["providers"]["translation"]["target"] = json!(next_tgt);
            if let Err(e) = service::update_settings(app, &next_settings) {
                self.set_message(e);
            } else {
                self.activate_overlay_mode(app, PinOverlayMode::Translation, true);
                fb.text_panel_visible = Some(true);
            }
            fb.geometry_changed = true;
            fb.redraw_image = true;
            fb.redraw_toolbar = true;
            fb.redraw_text_panel = true;
        } else if let Some((mode, force)) = activate_mode {
            let was_overlay = self.is_text_overlay();
            self.activate_overlay_mode(app, mode, force);
            if let Some(ov) = self.text_overlay.as_mut() {
                ov.show_text_panel = true;
            }
            fb.text_panel_visible = Some(true);
            fb.geometry_changed = !was_overlay;
            if !was_overlay {
                measured_bar = self.estimated_toolbar_bar_size();
            }
            fb.redraw_image = true;
            fb.redraw_toolbar = true;
            fb.redraw_text_panel = true;
        }

        fb.toolbar_bar_size = Some(measured_bar);
        fb.keep_toolbar_open = self.lang_picker_open() || self.passthrough;
        fb
    }

    /// Render the companion right-side plain-text window showing the generated OCR / translated plain text.
    pub fn show_text_panel(
        &mut self,
        ctx: &egui::Context,
        app: &Arc<App>,
    ) -> PinUiFeedback {
        let mut fb = PinUiFeedback::default();

        if let Some(ov) = self.text_overlay.as_mut() {
            if let Some((_, exp)) = &ov.panel_notice {
                let now = Instant::now();
                if now >= *exp {
                    ov.panel_notice = None;
                } else {
                    ctx.request_repaint_after(exp.saturating_duration_since(now));
                }
            }
        }

        // Pressing Esc inside the plain-text panel hides the panel (and clears error if blank page).
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if !self.dismiss_overlay_error(&mut fb) {
                if let Some(ov) = self.text_overlay.as_mut() {
                    ov.show_text_panel = false;
                }
                fb.text_panel_visible = Some(false);
                fb.redraw_toolbar = true;
            }
            return fb;
        }

        let palette = ui::ctx_palette(ctx);
        let dark = ctx.style().visuals.dark_mode;
        let panel_bg = if dark {
            palette.dark_window
        } else {
            Color32::from_rgb(0xfc, 0xfd, 0xfd)
        };
        let header_bg = if dark {
            palette.dark_panel
        } else {
            palette.light_panel
        };
        let border_color = if dark {
            palette.dark_stroke
        } else {
            palette.light_stroke
        };

        let mut switch_mode: Option<(PinOverlayMode, bool)> = None;
        let mut copy_requested = false;
        let mut close_panel = false;
        let mut open_settings = false;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(panel_bg)
                    .stroke(Stroke::new(1.0_f32, border_color))
                    .corner_radius(CornerRadius::same(8)),
            )
            .show(ctx, |ui| {
                // Header bar (empty space between left & right buttons is draggable without shadowing buttons).
                let header_frame = egui::Frame::NONE
                    .fill(header_bg)
                    .inner_margin(egui::Margin::symmetric(10, 7));
                let mut header_drag_rect: Option<Rect> = None;
                let header_resp = header_frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(5.0, 0.0);

                        let (mode, busy) = self
                            .text_overlay
                            .as_ref()
                            .map(|o| (o.mode, o.busy))
                            .unwrap_or((PinOverlayMode::Ocr, false));

                        if icons::icon_text_button(
                            ui,
                            Icon::Ocr,
                            i18n::t("Extract Text"),
                            mode == PinOverlayMode::Ocr,
                            false,
                            None,
                        )
                        .clicked()
                            && !busy
                        {
                            switch_mode = Some((PinOverlayMode::Ocr, false));
                        }

                        if icons::icon_text_button(
                            ui,
                            Icon::Translate,
                            i18n::t("Translate"),
                            mode == PinOverlayMode::Translation,
                            false,
                            None,
                        )
                        .clicked()
                            && !busy
                        {
                            switch_mode = Some((PinOverlayMode::Translation, false));
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icons::small_icon_button(ui, Icon::Close)
                                .on_hover_text(i18n::t("Hide Plain Text Panel"))
                                .clicked()
                            {
                                close_panel = true;
                            }
                            if icons::small_icon_button(ui, Icon::Copy)
                                .on_hover_text(i18n::t("Copy All"))
                                .clicked()
                            {
                                copy_requested = true;
                            }
                            let gap = ui.available_rect_before_wrap();
                            if gap.width() >= 4.0 && gap.height() >= 4.0 {
                                header_drag_rect = Some(gap);
                            }
                        });
                    });
                });

                // Dragging the empty middle gap of the header adjusts the panel's relative offset.
                if let Some(gap) = header_drag_rect {
                    let drag_area = Rect::from_min_max(
                        Pos2::new(gap.min.x, header_resp.response.rect.min.y),
                        Pos2::new(gap.max.x, header_resp.response.rect.max.y),
                    );
                    let drag_resp = ui.interact(
                        drag_area,
                        egui::Id::new(format!("pin_panel_hdr_drag_{}", self.pin_id)),
                        Sense::drag(),
                    );
                    if drag_resp.hovered() && self.panel_drag_origin.is_none() {
                        ctx.set_cursor_icon(CursorIcon::Grab);
                    }
                    if drag_resp.drag_started_by(egui::PointerButton::Primary) {
                        self.panel_drag_origin =
                            Some((native::cursor_pos(), self.text_panel_offset));
                        ctx.set_cursor_icon(CursorIcon::Grabbing);
                    } else if drag_resp.dragged_by(egui::PointerButton::Primary) {
                        if let Some((c0, off0)) = self.panel_drag_origin {
                            let c1 = native::cursor_pos();
                            self.text_panel_offset =
                                (off0.0 + (c1.0 - c0.0), off0.1 + (c1.1 - c0.1));
                            fb.geometry_changed = true;
                        }
                        ctx.set_cursor_icon(CursorIcon::Grabbing);
                    } else if drag_resp.drag_stopped() {
                        self.panel_drag_origin = None;
                    }
                }

                ui.painter().line_segment(
                    [
                        Pos2::new(header_resp.response.rect.min.x, header_resp.response.rect.max.y),
                        Pos2::new(header_resp.response.rect.max.x, header_resp.response.rect.max.y),
                    ],
                    Stroke::new(1.0_f32, border_color),
                );

                // Body area.
                let footer_h = 26.0;
                let body_h = (ui.available_height() - footer_h - 1.0).max(60.0);

                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_min_height(body_h - 16.0);
                        ui.set_max_height(body_h - 16.0);

                        let Some(ov) = self.text_overlay.as_mut() else {
                            return;
                        };

                        if ov.busy {
                            ui.centered_and_justified(|ui| {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    let label = if ov.mode == PinOverlayMode::Ocr {
                                        i18n::t("Recognizing...")
                                    } else {
                                        i18n::t("Translating...")
                                    };
                                    ui.label(RichText::new(label).strong());
                                });
                            });
                        } else if let Some(err) = ov.error.clone() {
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    icons::paint_inline(ui, Icon::Alert, 14.0, ui::DANGER);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(err).color(ui::DANGER).small(),
                                        )
                                        .wrap(),
                                    );
                                });
                                ui.add_space(8.0);
                                ui.horizontal_wrapped(|ui| {
                                    if icons::icon_text_button(
                                        ui,
                                        Icon::Reselect,
                                        i18n::t("Retry"),
                                        false,
                                        true,
                                        None,
                                    )
                                    .clicked()
                                    {
                                        switch_mode = Some((ov.mode, true));
                                    }
                                    if icons::icon_text_button(
                                        ui,
                                        Icon::SettingsGeneral,
                                        i18n::t("Provider Settings"),
                                        false,
                                        false,
                                        None,
                                    )
                                    .clicked()
                                    {
                                        open_settings = true;
                                    }
                                    if icons::icon_text_button(
                                        ui,
                                        Icon::Close,
                                        i18n::t("Close"),
                                        false,
                                        false,
                                        None,
                                    )
                                    .clicked()
                                    {
                                        close_panel = true;
                                    }
                                });
                            });
                        } else {
                            let scroll_h = ui.available_height().max(40.0);
                            let buf = ov.active_plain_buf_mut();
                            egui::ScrollArea::vertical()
                                .id_salt(format!("pin_plain_text_scroll_{}", self.pin_id))
                                .max_height(scroll_h)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.add_sized(
                                        [ui.available_width(), scroll_h],
                                        egui::TextEdit::multiline(buf)
                                            .frame(false)
                                            .hint_text(i18n::t("No text to display"))
                                            .font(egui::TextStyle::Body),
                                    );
                                });
                        }
                    });

                // Subtle bottom footer bar with char/line stats & copy status.
                let sep_y = ui.cursor().min.y;
                ui.painter().line_segment(
                    [
                        Pos2::new(ui.max_rect().min.x, sep_y),
                        Pos2::new(ui.max_rect().max.x, sep_y),
                    ],
                    Stroke::new(1.0_f32, border_color),
                );
                egui::Frame::NONE
                    .fill(header_bg)
                    .inner_margin(egui::Margin::symmetric(10, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(ov) = &self.text_overlay {
                                if let Some((notice, _)) = &ov.panel_notice {
                                    ui.label(
                                        RichText::new(notice)
                                            .color(palette.accent_text(dark))
                                            .small()
                                            .strong(),
                                    );
                                } else {
                                    let text = ov.active_full_text();
                                    let char_count = text.chars().filter(|c| !c.is_control()).count();
                                    let line_count = if text.is_empty() {
                                        0
                                    } else {
                                        text.lines().count()
                                    };
                                    ui.label(
                                        RichText::new(i18n::tf(
                                            "{chars} chars · {lines} lines",
                                            &[
                                                ("chars", &char_count.to_string()),
                                                ("lines", &line_count.to_string()),
                                            ],
                                        ))
                                        .weak()
                                        .small(),
                                    );
                                }
                            }
                        });
                    });
            });

        if close_panel {
            if !self.dismiss_overlay_error(&mut fb) {
                if let Some(ov) = self.text_overlay.as_mut() {
                    ov.show_text_panel = false;
                }
                fb.text_panel_visible = Some(false);
                fb.redraw_toolbar = true;
            }
        } else if copy_requested {
            if let Some(ov) = self.text_overlay.as_mut() {
                let text = ov.active_full_text();
                if text.trim().is_empty() {
                    ov.panel_notice = Some((
                        i18n::t("No text to display").to_string(),
                        Instant::now() + Duration::from_secs(2),
                    ));
                } else {
                    match clipboard::write_text(&text) {
                        Ok(()) => {
                            ov.panel_notice = Some((
                                i18n::t("Text copied").to_string(),
                                Instant::now() + Duration::from_secs(2),
                            ));
                        }
                        Err(e) => {
                            ov.panel_notice =
                                Some((e, Instant::now() + Duration::from_secs(3)));
                        }
                    }
                }
            }
        }

        if let Some((mode, force)) = switch_mode {
            self.trigger_overlay_mode(app, mode, force);
            fb.redraw_image = true;
            fb.redraw_toolbar = true;
            fb.redraw_text_panel = true;
        }
        if open_settings {
            app.emit(AppEvent::OpenSettings {
                section: Some("providers".into()),
            });
        }

        fb
    }

    fn trigger_overlay_mode(&mut self, app: &Arc<App>, mode: PinOverlayMode, force_refresh: bool) {
        let lang_key = {
            let core = app.core.lock().unwrap();
            current_lang_key(&core.settings)
        };
        let Some(ov) = self.text_overlay.as_mut() else {
            return;
        };
        ov.show_layer = true;
        ov.selection = None;
        ov.selecting = false;
        ov.error = None;

        // Instant switch when result is already cached and up-to-date.
        if !force_refresh {
            match mode {
                PinOverlayMode::Ocr => {
                    if ov
                        .data
                        .as_ref()
                        .is_some_and(|d| !d.full_text.trim().is_empty())
                    {
                        ov.mode = PinOverlayMode::Ocr;
                        if ov.ocr_plain_buf.is_empty() {
                            ov.sync_plain_bufs();
                        }
                        return;
                    }
                }
                PinOverlayMode::Translation => {
                    if ov.translated_lang_key == lang_key
                        && ov
                            .data
                            .as_ref()
                            .and_then(|d| d.full_translated.as_deref())
                            .is_some_and(|t| !t.trim().is_empty())
                    {
                        ov.mode = PinOverlayMode::Translation;
                        if ov.translated_plain_buf.is_empty() {
                            ov.sync_plain_bufs();
                        }
                        return;
                    }
                }
            }
        }

        ov.mode = mode;
        ov.busy = true;
        let cached_ocr = if mode == PinOverlayMode::Translation {
            ov.data.clone()
        } else {
            None
        };
        service::run_pin_overlay_process(
            app,
            self.pin_id.clone(),
            self.capture_id.clone(),
            mode,
            cached_ocr,
        );
    }

    fn build_rendered_blocks(&self, painter: &egui::Painter, screen: Rect) -> Vec<RenderedBlock> {
        let Some(ov) = &self.text_overlay else {
            return Vec::new();
        };
        let Some(data) = &ov.data else {
            return Vec::new();
        };
        build_rendered_blocks_for_data(
            painter,
            screen,
            self.base_width,
            self.base_height,
            data,
            ov.mode,
        )
    }

    fn begin_window_drag(&mut self, window: &winit::window::Window) {
        let cursor = native::cursor_pos();
        let win_pos = window
            .outer_position()
            .map(|p| (p.x, p.y))
            .unwrap_or((0, 0));
        self.drag_origin = Some((cursor, win_pos));
    }

    fn continue_window_drag(&self, hwnd: Option<isize>) -> bool {
        if let (Some((c0, w0)), Some(h)) = (self.drag_origin, hwnd) {
            let c1 = native::cursor_pos();
            native::move_window_physical(h, w0.0 + (c1.0 - c0.0), w0.1 + (c1.1 - c0.1));
            return true;
        }
        false
    }

    /// Target physical outer window size (image size at current zoom + shadow padding on all sides).
    pub fn target_window_size_physical(&self, scale_factor: f64) -> (i32, i32) {
        let pad_px = pin_shadow_pad_physical(scale_factor);
        let img_w = ((self.base_width as i64 * self.scale) / 100).max(40) as i32;
        let img_h = ((self.base_height as i64 * self.scale) / 100).max(40) as i32;
        (img_w + pad_px * 2, img_h + pad_px * 2)
    }

    pub fn sync_window_size_for_scale(&self, hwnd: Option<isize>, scale_factor: f64) {
        if let Some(h) = hwnd {
            let (nw, nh) = self.target_window_size_physical(scale_factor);
            native::resize_window_physical(h, nw, nh);
        }
    }

    fn apply_zoom(
        &mut self,
        app: &App,
        hwnd: Option<isize>,
        next_scale: i64,
        scale_factor: f64,
    ) -> bool {
        if next_scale == self.scale {
            return false;
        }
        self.scale = next_scale;
        self.sync_window_size_for_scale(hwnd, scale_factor);
        if let Some(ps) = app.core.lock().unwrap().pins.get_mut(&self.pin_id) {
            ps.scale = next_scale;
        }
        true
    }
}

fn build_rendered_blocks_for_data(
    painter: &egui::Painter,
    screen: Rect,
    base_width: u32,
    base_height: u32,
    data: &OcrOverlayData,
    mode: PinOverlayMode,
) -> Vec<RenderedBlock> {
    let is_ocr = mode == PinOverlayMode::Ocr;
    let sx = screen.width() / (base_width.max(1) as f32);
    let sy = screen.height() / (base_height.max(1) as f32);

    // Collect non-empty entries and resolve pairwise base-box overlaps first.
    let mut active_indices: Vec<usize> = Vec::with_capacity(data.blocks.len());
    let mut base_rects: Vec<Rect> = Vec::with_capacity(data.blocks.len());
    let mut rows: Vec<usize> = Vec::with_capacity(data.blocks.len());

    for (idx, block) in data.blocks.iter().enumerate() {
        let text = match mode {
            PinOverlayMode::Ocr => block.text.as_str(),
            PinOverlayMode::Translation => block.translated.as_deref().unwrap_or(&block.text),
        };
        if text.is_empty() {
            continue;
        }
        active_indices.push(idx);
        base_rects.push(Rect::from_min_max(
            Pos2::new(
                screen.min.x + block.rect[0] * sx,
                screen.min.y + block.rect[1] * sy,
            ),
            Pos2::new(
                screen.min.x + block.rect[2] * sx,
                screen.min.y + block.rect[3] * sy,
            ),
        ));
        rows.push(block.row);
    }

    resolve_base_rect_overlaps(&mut base_rects, &rows, screen);

    let mut out = Vec::with_capacity(active_indices.len());
    for (local_i, &idx) in active_indices.iter().enumerate() {
        let block = &data.blocks[idx];
        let text = match mode {
            PinOverlayMode::Ocr => block.text.as_str(),
            PinOverlayMode::Translation => block.translated.as_deref().unwrap_or(&block.text),
        };
        let orig_rect = base_rects[local_i];
        let bg_color = Color32::from_rgb(block.bg_rgb[0], block.bg_rgb[1], block.bg_rgb[2]);
        let fg_color = Color32::from_rgb(block.fg_rgb[0], block.fg_rgb[1], block.fg_rgb[2]);
        let char_len = text.chars().count();

        if is_ocr {
            let font_size = (orig_rect.height() * 0.72).clamp(8.0, 96.0);
            let galley = painter.layout_no_wrap(
                text.to_string(),
                FontId::proportional(font_size),
                Color32::TRANSPARENT,
            );
            let x_scale = if galley.size().x > 1.0 {
                orig_rect.width() / galley.size().x
            } else {
                1.0
            };
            let text_pos = Pos2::new(
                orig_rect.min.x,
                orig_rect.center().y - galley.size().y * 0.5,
            );
            out.push(RenderedBlock {
                idx,
                row: block.row,
                orig_rect,
                hit_rect: orig_rect,
                patch_rect: orig_rect,
                text_pos,
                galley,
                x_scale,
                bg_color,
                fg_color,
                char_len,
                full_text: text.to_string(),
                truncated: false,
            });
        } else {
            // Compute neighbor-aware horizontal & vertical available corridor so translated text
            // expands only into genuinely empty space and never overlaps neighboring blocks.
            let mut right_limit = screen.max.x - 2.0;
            for (j, other) in base_rects.iter().enumerate() {
                if j == local_i {
                    continue;
                }
                let v_overlap =
                    orig_rect.max.y.min(other.max.y) - orig_rect.min.y.max(other.min.y);
                if v_overlap > 1.0 && other.min.x >= orig_rect.max.x - 1.0 {
                    right_limit = right_limit.min(other.min.x - 2.0);
                }
            }
            let avail_right_w = (right_limit - orig_rect.min.x).max(orig_rect.width());
            let max_text_w = avail_right_w
                .min((orig_rect.width() * 1.65).max(orig_rect.width() + 64.0))
                .max(orig_rect.width().min(avail_right_w))
                .max(12.0);

            let h_span_min = orig_rect.min.x;
            let h_span_max = (orig_rect.min.x + max_text_w).min(right_limit);
            let mut top_limit = screen.min.y + 1.0;
            let mut bottom_limit = screen.max.y - 1.0;
            for (j, other) in base_rects.iter().enumerate() {
                if j == local_i {
                    continue;
                }
                let h_overlap = h_span_max.min(other.max.x) - h_span_min.max(other.min.x);
                if h_overlap <= 1.0 {
                    continue;
                }
                if other.center().y < orig_rect.center().y {
                    top_limit = top_limit.max(other.max.y + 1.5);
                } else if other.center().y > orig_rect.center().y {
                    bottom_limit = bottom_limit.min(other.min.y - 1.5);
                }
            }
            let slot_min_y = top_limit.min(orig_rect.min.y);
            let slot_max_y = bottom_limit.max(orig_rect.max.y);
            let avail_h = (slot_max_y - slot_min_y).max(orig_rect.height()).max(10.0);

            // Fit text cleanly within (max_text_w, avail_h), accounting for actual CJK glyph line height.
            let target_font = (orig_rect.height() * 0.68).clamp(7.5, 64.0);
            let mut chosen: Option<(Arc<Galley>, bool)> = None;

            // 1. Try single line at target_font (scaled down if glyph height exceeds avail_h).
            let mut single_font = target_font;
            let mut single = painter.layout_no_wrap(
                text.to_string(),
                FontId::proportional(single_font),
                fg_color,
            );
            if single.size().y > avail_h && single.size().y > 1.0 {
                single_font =
                    (single_font * (avail_h / single.size().y) * 0.96).clamp(7.0, single_font);
                single = painter.layout_no_wrap(
                    text.to_string(),
                    FontId::proportional(single_font),
                    fg_color,
                );
            }
            if single.size().x <= max_text_w && single.size().y <= avail_h + 0.5 {
                chosen = Some((single, false));
            } else {
                // 2. Try gentle horizontal single-line shrink before wrapping.
                let min_single_font = (target_font * 0.62).max(8.0);
                if single.size().x > 1.0 {
                    let fitted = single_font * (max_text_w / single.size().x) * 0.98;
                    if fitted >= min_single_font {
                        let g = painter.layout_no_wrap(
                            text.to_string(),
                            FontId::proportional(fitted),
                            fg_color,
                        );
                        if g.size().x <= max_text_w + 1.0 && g.size().y <= avail_h + 0.5 {
                            chosen = Some((g, false));
                        }
                    }
                }
            }

            // 3. Try wrapped multi-line layout if there is vertical space.
            if chosen.is_none() && avail_h >= 16.0 {
                for factor in [0.82_f32, 0.72, 0.62, 0.54] {
                    let wf = (target_font * factor).clamp(7.5, target_font);
                    let g = painter.layout(
                        text.to_string(),
                        FontId::proportional(wf),
                        fg_color,
                        max_text_w,
                    );
                    if g.size().y <= avail_h + 0.5 && g.size().x <= max_text_w + 1.0 {
                        chosen = Some((g, false));
                        break;
                    }
                }
            }

            // 4. Guaranteed non-overflowing clamped layout with ellipsis (full text in hover tooltip & right panel).
            let (galley, truncated) = chosen.unwrap_or_else(|| {
                let f = (target_font * 0.70).clamp(7.5, 14.0);
                let probe = painter.layout_no_wrap("Ag中".to_string(), FontId::proportional(f), fg_color);
                let line_h = probe.size().y.max(f * 1.25).max(8.0);
                let max_rows = ((avail_h / line_h).floor() as usize).max(1);
                layout_wrapped_clamped(painter, text, f, fg_color, max_text_w, max_rows)
            });

            let gsize = galley.size();
            let raw_x = if gsize.x <= orig_rect.width() {
                orig_rect.center().x - gsize.x * 0.5
            } else {
                orig_rect.min.x
            };
            let text_x = raw_x.clamp(
                orig_rect.min.x.max(screen.min.x + 2.0),
                (right_limit - gsize.x).max(orig_rect.min.x).max(screen.min.x + 2.0),
            );
            let text_y = (orig_rect.center().y - gsize.y * 0.5).clamp(
                slot_min_y,
                (slot_max_y - gsize.y).max(slot_min_y),
            );
            let text_pos = Pos2::new(text_x, text_y);
            let text_rect = Rect::from_min_size(text_pos, gsize);

            // Build patch covering the original text plus the rendered translation, clamped strictly inside the slot.
            let mut patch_rect = orig_rect
                .expand2(Vec2::new(1.5, 1.0))
                .union(text_rect.expand2(Vec2::new(2.0, 1.0)));
            patch_rect.min.x = patch_rect.min.x.max(screen.min.x + 1.0);
            patch_rect.max.x = patch_rect.max.x.min(right_limit + 1.0).min(screen.max.x - 1.0);
            patch_rect.min.y = patch_rect.min.y.max(slot_min_y);
            patch_rect.max.y = patch_rect.max.y.min(slot_max_y);
            if patch_rect.max.x <= patch_rect.min.x + 2.0 {
                patch_rect = orig_rect;
            }
            if patch_rect.max.y <= patch_rect.min.y + 2.0 {
                patch_rect = orig_rect;
            }

            out.push(RenderedBlock {
                idx,
                row: block.row,
                orig_rect,
                hit_rect: patch_rect,
                patch_rect,
                text_pos,
                galley,
                x_scale: 1.0,
                bg_color,
                fg_color,
                char_len,
                full_text: text.to_string(),
                truncated,
            });
        }
    }

    if !is_ocr {
        resolve_patch_overlaps(&mut out, screen);
    }

    out
}

pub fn current_lang_key(settings: &serde_json::Value) -> String {
    let src = crate::translate::normalize_lang_code(
        settings["providers"]["translation"]["source"]
            .as_str()
            .unwrap_or("auto"),
    );
    let tgt = crate::translate::normalize_lang_code(
        settings["providers"]["translation"]["target"]
            .as_str()
            .unwrap_or("en"),
    );
    format!("{}->{}", src, tgt)
}

/// Paint a soft, multi-layer elevation drop shadow extending slightly outward on all four sides
/// around `image_rect`, plus a subtle 1px outer hairline so the pinned image clearly separates
/// from both light and pure-dark underlying screens without covering any pixels of the image.
fn paint_pin_shadow(painter: &egui::Painter, image_rect: Rect) {
    let layers = [
        // Wide ambient shadow (left/right: 10pt, top: 9pt, bottom: 11pt — fades cleanly to alpha=0
        // before the 12pt window edge so there is never a visible rectangular cutoff).
        egui::Shadow {
            offset: [0, 1],
            blur: 12,
            spread: 4,
            color: Color32::from_black_alpha(38),
        },
        // Mid soft shadow (left/right: 6pt, top: 5pt, bottom: 7pt).
        egui::Shadow {
            offset: [0, 1],
            blur: 10,
            spread: 1,
            color: Color32::from_black_alpha(54),
        },
        // Crisp contact shadow right around all four edges of the pinned image.
        egui::Shadow {
            offset: [0, 0],
            blur: 4,
            spread: 0,
            color: Color32::from_black_alpha(84),
        },
    ];
    for shadow in layers {
        painter.add(shadow.as_shape(image_rect, CornerRadius::same(1)));
    }
    // Subtle 1px outer rim strictly outside `image_rect` so the pin boundary stays legible over
    // pure-black backgrounds as well.
    painter.rect_stroke(
        image_rect,
        CornerRadius::ZERO,
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 42)),
        egui::StrokeKind::Outside,
    );
}

fn rect_distance_sq(rect: Rect, pos: Pos2) -> f32 {
    let dx = if pos.x < rect.min.x {
        rect.min.x - pos.x
    } else if pos.x > rect.max.x {
        pos.x - rect.max.x
    } else {
        0.0
    };
    let dy = if pos.y < rect.min.y {
        rect.min.y - pos.y
    } else if pos.y > rect.max.y {
        pos.y - rect.max.y
    } else {
        0.0
    };
    dx * dx + dy * dy
}

/// Edge-friendly text block hit-testing:
/// 1. Exact hits inside a block's `hit_rect` always win (even flush against the image edge).
/// 2. Otherwise, within a small 5pt horizontal / 2.5pt vertical assist margin (clamped inside
///    `image_rect.expand(5.0)` so the outer portion of the shadow halo stays a pure drag handle),
///    select the closest text block so users can easily start selecting edge characters from
///    slightly outside the image boundary.
fn hit_text_block<'a>(
    rendered: &'a [RenderedBlock],
    image_rect: Rect,
    pos: Pos2,
) -> Option<&'a RenderedBlock> {
    if let Some(exact) = rendered.iter().find(|b| b.hit_rect.contains(pos)) {
        return Some(exact);
    }
    const ASSIST_X: f32 = 5.0;
    const ASSIST_Y: f32 = 2.5;
    if !image_rect.expand(ASSIST_X).contains(pos) {
        return None;
    }
    rendered
        .iter()
        .filter(|b| b.hit_rect.expand2(Vec2::new(ASSIST_X, ASSIST_Y)).contains(pos))
        .min_by(|a, b| rect_distance_sq(a.hit_rect, pos).total_cmp(&rect_distance_sq(b.hit_rect, pos)))
}

fn nearest_text_cursor(
    rendered: &[RenderedBlock],
    pos: Pos2,
    is_ocr: bool,
) -> Option<(usize, usize)> {
    if rendered.is_empty() {
        return None;
    }
    if let Some(hit) = rendered.iter().find(|b| b.hit_rect.contains(pos)) {
        return Some((hit.idx, hit.char_index_at(pos, is_ocr)));
    }
    let best = rendered.iter().min_by(|a, b| {
        let dy_a = if pos.y < a.hit_rect.min.y {
            a.hit_rect.min.y - pos.y
        } else if pos.y > a.hit_rect.max.y {
            pos.y - a.hit_rect.max.y
        } else {
            0.0
        };
        let dy_b = if pos.y < b.hit_rect.min.y {
            b.hit_rect.min.y - pos.y
        } else if pos.y > b.hit_rect.max.y {
            pos.y - b.hit_rect.max.y
        } else {
            0.0
        };
        let dx_a = if pos.x < a.hit_rect.min.x {
            a.hit_rect.min.x - pos.x
        } else if pos.x > a.hit_rect.max.x {
            pos.x - a.hit_rect.max.x
        } else {
            0.0
        };
        let dx_b = if pos.x < b.hit_rect.min.x {
            b.hit_rect.min.x - pos.x
        } else if pos.x > b.hit_rect.max.x {
            pos.x - b.hit_rect.max.x
        } else {
            0.0
        };
        dy_a.total_cmp(&dy_b).then_with(|| dx_a.total_cmp(&dx_b))
    })?;
    if pos.y < best.hit_rect.min.y {
        return Some((best.idx, 0));
    }
    if pos.y > best.hit_rect.max.y {
        return Some((best.idx, best.char_len));
    }
    let clamped = Pos2::new(
        pos.x.clamp(best.hit_rect.min.x, best.hit_rect.max.x),
        pos.y.clamp(best.hit_rect.min.y, best.hit_rect.max.y),
    );
    Some((best.idx, best.char_index_at(clamped, is_ocr)))
}

fn toolbar_icon_button(ui: &mut egui::Ui, icon: Icon, tip: &str, active: bool) -> egui::Response {
    icons::icon_button_sized(ui, icon, Vec2::splat(26.0), 14.5, active, false).on_hover_text(tip)
}

fn toolbar_lang_button(ui: &mut egui::Ui, label: &str, open: bool) -> egui::Response {
    let font_id = FontId::proportional(11.5);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Color32::WHITE);
    let w = (galley.size().x + 22.0).max(38.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let palette = ui::ctx_palette(ui.ctx());
        let visuals = ui.visuals();
        let (fill, stroke, fg) = if open {
            (
                if resp.hovered() {
                    palette.accent_hover
                } else {
                    palette.accent
                },
                Stroke::new(1.0_f32, palette.accent_hover),
                Color32::WHITE,
            )
        } else if resp.hovered() {
            (
                visuals.widgets.hovered.bg_fill,
                visuals.widgets.hovered.bg_stroke,
                visuals.strong_text_color(),
            )
        } else {
            (
                visuals.widgets.inactive.bg_fill,
                visuals.widgets.inactive.bg_stroke,
                visuals.text_color(),
            )
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(5),
            fill,
            stroke,
            egui::StrokeKind::Inside,
        );
        let text_pos = Pos2::new(rect.min.x + 6.0, rect.center().y - galley.size().y * 0.5);
        ui.painter()
            .galley_with_override_text_color(text_pos, galley, fg);
        let arrow_rect = Rect::from_center_size(
            Pos2::new(rect.max.x - 8.5, rect.center().y),
            Vec2::splat(9.5),
        );
        icons::paint(
            ui.painter(),
            arrow_rect,
            if open {
                Icon::ChevronUp
            } else {
                Icon::ChevronDown
            },
            fg,
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_selection_orders_forward_and_backward_ranges() {
        let sel = TextSelection {
            anchor: (2, 4),
            focus: (0, 2),
        };
        assert!(!sel.is_empty());
        assert_eq!(sel.ordered(), ((0, 2), (2, 4)));
        assert_eq!(sel.block_range(0, 10), Some((2, 10)));
        assert_eq!(sel.block_range(1, 5), Some((0, 5)));
        assert_eq!(sel.block_range(2, 8), Some((0, 4)));
        assert_eq!(sel.block_range(3, 8), None);
    }

    #[test]
    fn overlay_selected_text_joins_same_row_with_space_and_rows_with_newline() {
        let mut ov = PinTextOverlay::new(PinOverlayMode::Ocr, "auto->zh".into());
        ov.busy = false;
        ov.data = Some(OcrOverlayData {
            blocks: vec![
                OcrTextBlock {
                    rect: [0.0, 0.0, 50.0, 20.0],
                    row: 0,
                    text: "Hello".into(),
                    translated: Some("你好".into()),
                    bg_rgb: [255, 255, 255],
                    fg_rgb: [20, 24, 28],
                },
                OcrTextBlock {
                    rect: [120.0, 0.0, 180.0, 20.0],
                    row: 0,
                    text: "World".into(),
                    translated: Some("世界".into()),
                    bg_rgb: [255, 255, 255],
                    fg_rgb: [20, 24, 28],
                },
                OcrTextBlock {
                    rect: [0.0, 30.0, 100.0, 50.0],
                    row: 1,
                    text: "Line 2".into(),
                    translated: Some("第二行".into()),
                    bg_rgb: [255, 255, 255],
                    fg_rgb: [20, 24, 28],
                },
            ],
            full_text: "Hello World\nLine 2".into(),
            full_translated: Some("你好 世界\n第二行".into()),
        });
        ov.sync_plain_bufs();
        ov.select_all();
        assert_eq!(ov.selected_text().as_deref(), Some("Hello World\nLine 2"));

        ov.mode = PinOverlayMode::Translation;
        ov.select_all();
        assert_eq!(ov.selected_text().as_deref(), Some("你好 世界\n第二行"));

        // Partial cross-block selection.
        ov.selection = Some(TextSelection {
            anchor: (0, 1),
            focus: (2, 2),
        });
        assert_eq!(ov.selected_text().as_deref(), Some("好 世界\n第二"));
    }

    #[test]
    fn pin_initial_rect_preserves_exact_capture_origin_at_100_percent() {
        let rect = compute_pin_initial_rect(
            640,
            400,
            100,
            Some((320, 180)),
            (910, 620), // cursor at toolbar button far away from capture
            (0, 0, 1920, 1040),
        );
        assert_eq!(rect, (320, 180, 640, 400));
    }

    #[test]
    fn pin_initial_rect_centers_on_cursor_when_no_origin() {
        let rect = compute_pin_initial_rect(400, 300, 100, None, (800, 500), (0, 0, 1920, 1040));
        assert_eq!(rect, (600, 350, 400, 300));
    }

    #[test]
    fn toolbar_places_below_pin_by_default_and_flips_above_near_bottom() {
        let work = (0, 0, 1920, 1040);
        // Plenty of room below: sits directly below pin with no overlap.
        let (tx, ty, tw, th, picker_above) =
            compute_toolbar_rect((300, 200, 400, 300), (340, 38), 0, 8, work);
        assert_eq!(ty, 200 + 300 + 8);
        assert_eq!(tw, 340);
        assert_eq!(th, 38);
        assert!(!picker_above);
        assert!(tx >= 4);

        // Near bottom of screen: flips above the pin without covering the image.
        let (_, ty2, _, th2, _) =
            compute_toolbar_rect((300, 720, 400, 300), (340, 38), 0, 8, work);
        assert_eq!(ty2 + th2 + 8, 720);
    }

    #[test]
    fn text_panel_places_to_right_of_pin_and_falls_back_to_left_near_edge() {
        let work = (0, 0, 1920, 1040);
        // Normal case: directly to the right of pin.
        let (px, py, pw, ph) = compute_text_panel_rect((200, 150, 500, 400), 320, 220, 220, 8, work, (0, 0));
        assert_eq!(px, 200 + 500 + 8);
        assert_eq!(py, 150);
        assert_eq!(pw, 320);
        assert_eq!(ph, 400);

        // Near right edge of monitor: flips cleanly to left side without shifting pin.
        let (lx, ly, lw, _) = compute_text_panel_rect((1450, 150, 450, 400), 320, 220, 220, 8, work, (0, 0));
        assert_eq!(lx + lw + 8, 1450);
        assert_eq!(ly, 150);
    }

    #[test]
    fn rendered_blocks_eliminate_overlaps_in_dense_ocr_and_translation() {
        let ctx = egui::Context::default();
        let data = OcrOverlayData {
            blocks: vec![
                // Two tightly stacked lines whose raw OCR boxes overlap vertically by 4px,
                // plus a second column on row 0 and a long English translation that wants to wrap.
                OcrTextBlock {
                    rect: [10.0, 10.0, 140.0, 28.0],
                    row: 0,
                    text: "第一行左列".into(),
                    translated: Some("A much longer translated line for the left column".into()),
                    bg_rgb: [255, 255, 255],
                    fg_rgb: [20, 24, 28],
                },
                OcrTextBlock {
                    rect: [148.0, 10.0, 260.0, 28.0],
                    row: 0,
                    text: "第一行右列".into(),
                    translated: Some("Right column label text".into()),
                    bg_rgb: [255, 255, 255],
                    fg_rgb: [20, 24, 28],
                },
                OcrTextBlock {
                    rect: [10.0, 24.0, 240.0, 42.0],
                    row: 1,
                    text: "第二行紧邻第一行".into(),
                    translated: Some("Second tightly packed line right underneath the first line".into()),
                    bg_rgb: [255, 255, 255],
                    fg_rgb: [20, 24, 28],
                },
                OcrTextBlock {
                    rect: [10.0, 39.0, 240.0, 56.0],
                    row: 2,
                    text: "第三行紧邻第二行".into(),
                    translated: Some("Third tightly packed line right underneath the second line".into()),
                    bg_rgb: [255, 255, 255],
                    fg_rgb: [20, 24, 28],
                },
            ],
            full_text: "第一行左列 第一行右列\n第二行紧邻第一行\n第三行紧邻第二行".into(),
            full_translated: Some("translated".into()),
        };

        let screen = Rect::from_min_max(Pos2::ZERO, Pos2::new(300.0, 120.0));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // Verify OCR mode has zero pairwise box overlap.
                let ocr_blocks = build_rendered_blocks_for_data(
                    ui.painter(),
                    screen,
                    300,
                    120,
                    &data,
                    PinOverlayMode::Ocr,
                );
                assert_eq!(ocr_blocks.len(), 4);
                for i in 0..ocr_blocks.len() {
                    for j in (i + 1)..ocr_blocks.len() {
                        let a = ocr_blocks[i].orig_rect;
                        let b = ocr_blocks[j].orig_rect;
                        let ox = a.max.x.min(b.max.x) - a.min.x.max(b.min.x);
                        let oy = a.max.y.min(b.max.y) - a.min.y.max(b.min.y);
                        assert!(
                            ox <= 0.01 || oy <= 0.01,
                            "OCR blocks {} ({:?}) and {} ({:?}) overlapped",
                            i,
                            a,
                            j,
                            b
                        );
                    }
                }

                // Verify Translation mode has zero pairwise patch overlap.
                let tr_blocks = build_rendered_blocks_for_data(
                    ui.painter(),
                    screen,
                    300,
                    120,
                    &data,
                    PinOverlayMode::Translation,
                );
                assert_eq!(tr_blocks.len(), 4);
                for i in 0..tr_blocks.len() {
                    for j in (i + 1)..tr_blocks.len() {
                        let a = tr_blocks[i].patch_rect;
                        let b = tr_blocks[j].patch_rect;
                        let ox = a.max.x.min(b.max.x) - a.min.x.max(b.min.x);
                        let oy = a.max.y.min(b.max.y) - a.min.y.max(b.min.y);
                        assert!(
                            ox <= 0.01 || oy <= 0.01,
                            "Translation patches {} ({:?}) and {} ({:?}) overlapped",
                            i,
                            a,
                            j,
                            b
                        );
                    }
                }
            });
        });
    }

    #[test]
    fn pin_shadow_padding_preserves_exact_image_origin_and_size() {
        let pad_1x = pin_shadow_pad_physical(1.0);
        assert_eq!(pad_1x, 12);
        let win = pin_window_rect_from_image_rect((320, 180, 640, 400), pad_1x);
        assert_eq!(win, (308, 168, 664, 424));
        let recovered =
            pin_image_rect_from_window_rect((win.0, win.1, win.2 as i32, win.3 as i32), pad_1x);
        assert_eq!(recovered, (320, 180, 640, 400));

        // 150% DPI scaling snaps to integer physical pixels (18px).
        let pad_150 = pin_shadow_pad_physical(1.5);
        assert_eq!(pad_150, 18);
        let screen = Rect::from_min_max(Pos2::ZERO, Pos2::new(424.0, 224.0));
        let img = pin_image_rect_logical(screen, 1.5);
        assert!((img.min.x - 12.0).abs() < 0.001);
        assert!((img.max.x - 412.0).abs() < 0.001);
    }

    #[test]
    fn edge_text_blocks_are_selectable_from_edge_and_inner_shadow_while_outer_rim_drags() {
        let ctx = egui::Context::default();
        let data = OcrOverlayData {
            blocks: vec![OcrTextBlock {
                // Flush against top-left and right edges of a 200x60 image.
                rect: [0.0, 0.0, 200.0, 20.0],
                row: 0,
                text: "EdgeText".into(),
                translated: None,
                bg_rgb: [255, 255, 255],
                fg_rgb: [20, 24, 28],
            }],
            full_text: "EdgeText".into(),
            full_translated: None,
        };
        let screen = Rect::from_min_max(Pos2::ZERO, Pos2::new(224.0, 84.0));
        let image_rect = pin_image_rect_logical(screen, 1.0);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let blocks = build_rendered_blocks_for_data(
                    ui.painter(),
                    image_rect,
                    200,
                    60,
                    &data,
                    PinOverlayMode::Ocr,
                );
                assert_eq!(blocks.len(), 1);

                // Clicking right on the image's top-left pixel selects the edge block.
                let edge_hit = hit_text_block(&blocks, image_rect, Pos2::new(12.0, 12.0));
                assert!(edge_hit.is_some());

                // Starting 3pt into the left shadow margin selects from character 0.
                let left_assist = hit_text_block(&blocks, image_rect, Pos2::new(9.0, 20.0));
                assert!(left_assist.is_some());
                assert_eq!(left_assist.unwrap().char_index_at(Pos2::new(9.0, 20.0), true), 0);

                // Starting 3pt into the right shadow margin selects from the last character.
                let right_assist = hit_text_block(&blocks, image_rect, Pos2::new(215.0, 20.0));
                assert!(right_assist.is_some());
                assert_eq!(
                    right_assist.unwrap().char_index_at(Pos2::new(215.0, 20.0), true),
                    "EdgeText".chars().count()
                );

                // Outer shadow rim (8pt outside image_rect) does NOT steal window-drag.
                assert!(hit_text_block(&blocks, image_rect, Pos2::new(4.0, 20.0)).is_none());
                assert!(hit_text_block(&blocks, image_rect, Pos2::new(220.0, 20.0)).is_none());
            });
        });
    }

    #[test]
    fn dismiss_overlay_error_closes_blank_page_panel_and_requests_image_redraw() {
        let mut state = PinWindowState {
            pin_id: "pin-test".into(),
            capture_id: "cap-test".into(),
            base_width: 300,
            base_height: 200,
            scale: 100,
            opacity: 100,
            passthrough: false,
            double_click_close: false,
            texture: None,
            image_dirty: false,
            gif_player: None,
            drag_origin: None,
            panel_drag_origin: None,
            text_panel_offset: (0, 0),
            message: None,
            text_overlay: Some(PinTextOverlay::new(PinOverlayMode::Ocr, "auto->en".into())),
        };
        state.apply_overlay_result(
            PinOverlayMode::Ocr,
            "auto->en".into(),
            Err("No text detected in the image".into()),
        );
        assert!(state.wants_text_panel());

        let mut fb = PinUiFeedback::default();
        assert!(state.dismiss_overlay_error(&mut fb));
        assert!(state.text_overlay.is_none());
        assert!(!state.wants_text_panel());
        assert_eq!(fb.text_panel_visible, Some(false));
        assert!(fb.redraw_image);
        assert!(fb.redraw_toolbar);
    }
}
