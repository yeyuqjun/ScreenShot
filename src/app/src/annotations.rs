//! Annotation data structures, strict validation, and pure-Rust high-precision rasterization engine (tiny-skia + ab_glyph).

use crate::i18n;
use crate::images::Rgba;
use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tiny_skia::{
    BlendMode, Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapPaint, Rect,
    Stroke as SkiaStroke, Transform,
};

pub const FONTS: &[(&str, &str)] = &[
    ("Microsoft YaHei", "Microsoft YaHei"),
    ("SimSun", "SimSun"),
    ("SimHei", "SimHei"),
    ("Arial", "Arial"),
    ("Consolas", "Consolas"),
];

pub const PRESET_COLORS: &[&str] = &[
    "#ef6656", "#f6ca45", "#00aa66", "#2789e8", "#9163db", "#ffffff", "#263b35",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Select,
    Rect,
    Ellipse,
    Arrow,
    Line,
    Pen,
    Highlighter,
    Text,
    Watermark,
    Number,
    Mosaic,
    Eraser,
}

impl Tool {
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Rect => "rect",
            Tool::Ellipse => "ellipse",
            Tool::Arrow => "arrow",
            Tool::Line => "line",
            Tool::Pen => "pen",
            Tool::Highlighter => "highlighter",
            Tool::Text => "text",
            Tool::Watermark => "watermark",
            Tool::Number => "number",
            Tool::Mosaic => "mosaic",
            Tool::Eraser => "eraser",
        }
    }

    pub fn is_text(self) -> bool {
        matches!(self, Tool::Text | Tool::Watermark)
    }

    pub fn is_freehand(self) -> bool {
        matches!(self, Tool::Pen | Tool::Eraser)
    }

    pub fn supports_fill(self) -> bool {
        matches!(self, Tool::Rect | Tool::Ellipse)
    }

    pub fn is_layer_editable(self) -> bool {
        !matches!(self, Tool::Select | Tool::Eraser)
    }

    pub fn is_box_shape(self) -> bool {
        matches!(
            self,
            Tool::Rect | Tool::Ellipse | Tool::Mosaic | Tool::Highlighter
        )
    }

    pub fn is_segment(self) -> bool {
        matches!(self, Tool::Line | Tool::Arrow)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    pub id: String,
    pub tool: Tool,
    pub points: Vec<Point>,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_color: Option<String>,
    pub width: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f32>,
}

impl Stroke {
    pub fn effective_font_size(&self) -> f32 {
        self.font_size.unwrap_or_else(|| (self.width * 7.0).max(18.0))
    }

    pub fn effective_opacity(&self) -> f32 {
        let default_pct = match self.tool {
            Tool::Watermark | Tool::Highlighter => 30.0,
            _ => 100.0,
        };
        (self.opacity.unwrap_or(default_pct) / 100.0).clamp(0.05, 1.0)
    }

    pub fn has_fill(&self) -> bool {
        self.tool.supports_fill()
            && self
                .fill_color
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty())
    }

    /// Axis-aligned bounding box `(min_x, min_y, max_x, max_y)` in image coordinates.
    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        if self.points.is_empty() {
            return None;
        }
        match self.tool {
            Tool::Text | Tool::Watermark => {
                let (bx, by, bw, bh) = text_bounds(self);
                let rot = self.rotation.unwrap_or(0.0);
                let corners = [
                    Point { x: 0.0, y: 0.0 },
                    Point { x: bw, y: 0.0 },
                    Point { x: bw, y: bh },
                    Point { x: 0.0, y: bh },
                ];
                let mut min_x = f32::INFINITY;
                let mut min_y = f32::INFINITY;
                let mut max_x = f32::NEG_INFINITY;
                let mut max_y = f32::NEG_INFINITY;
                for c in corners {
                    let r = rotate_point(c, rot);
                    let wx = bx + r.x;
                    let wy = by + r.y;
                    min_x = min_x.min(wx);
                    min_y = min_y.min(wy);
                    max_x = max_x.max(wx);
                    max_y = max_y.max(wy);
                }
                Some((min_x, min_y, max_x, max_y))
            }
            Tool::Number => {
                let p = self.points[0];
                let r = (self.width * 4.0).max(14.0);
                Some((p.x - r, p.y - r, p.x + r, p.y + r))
            }
            _ => {
                let mut min_x = f32::INFINITY;
                let mut min_y = f32::INFINITY;
                let mut max_x = f32::NEG_INFINITY;
                let mut max_y = f32::NEG_INFINITY;
                for p in &self.points {
                    min_x = min_x.min(p.x);
                    min_y = min_y.min(p.y);
                    max_x = max_x.max(p.x);
                    max_y = max_y.max(p.y);
                }
                Some((min_x, min_y, max_x, max_y))
            }
        }
    }

    /// Translate all control points by `(dx, dy)` while keeping them within image bounds
    /// without distorting multi-point geometry.
    pub fn translate(&mut self, dx: f32, dy: f32, img_w: f32, img_h: f32) {
        if self.points.is_empty() {
            return;
        }
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for p in &self.points {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        let eff_dx = dx.clamp(-min_x, (img_w - max_x).max(-min_x));
        let eff_dy = dy.clamp(-min_y, (img_h - max_y).max(-min_y));
        for p in &mut self.points {
            p.x = (p.x + eff_dx).clamp(0.0, img_w);
            p.y = (p.y + eff_dy).clamp(0.0, img_h);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolOptions {
    pub color: String,
    pub fill_color: Option<String>,
    pub width: f32,
    pub font_size: f32,
    pub font_family: String,
    pub opacity: f32,
}

impl Default for ToolOptions {
    fn default() -> Self {
        Self {
            color: "#ef6656".into(),
            fill_color: None,
            width: 3.0,
            font_size: 24.0,
            font_family: "Microsoft YaHei".into(),
            opacity: 100.0,
        }
    }
}

impl ToolOptions {
    pub fn from_settings(settings: &Value, tool: Tool) -> Self {
        let key = tool.as_str();
        let node = &settings["annotations"][key];
        let def_opacity = if matches!(tool, Tool::Watermark | Tool::Highlighter) {
            30.0
        } else {
            100.0
        };
        Self {
            color: node["color"]
                .as_str()
                .unwrap_or(if tool == Tool::Highlighter { "#f6ca45" } else { "#ef6656" })
                .to_string(),
            fill_color: if tool.supports_fill() {
                node["fillColor"]
                    .as_str()
                    .filter(|c| !c.is_empty())
                    .map(str::to_string)
            } else {
                None
            },
            width: node["width"].as_f64().unwrap_or(3.0) as f32,
            font_size: node["fontSize"].as_f64().unwrap_or(24.0) as f32,
            font_family: node["fontFamily"]
                .as_str()
                .unwrap_or("Microsoft YaHei")
                .to_string(),
            opacity: node["opacity"].as_f64().unwrap_or(def_opacity) as f32,
        }
    }
}

pub fn next_number(strokes: &[Stroke]) -> i64 {
    strokes
        .iter()
        .filter(|s| s.tool == Tool::Number)
        .filter_map(|s| s.number)
        .max()
        .unwrap_or(0)
        + 1
}

pub fn parse_hex_rgb(hex: &str) -> (u8, u8, u8) {
    let clean = hex.trim_start_matches('#');
    if clean.len() == 6 {
        let r = u8::from_str_radix(&clean[0..2], 16).unwrap_or(239);
        let g = u8::from_str_radix(&clean[2..4], 16).unwrap_or(102);
        let b = u8::from_str_radix(&clean[4..6], 16).unwrap_or(86);
        return (r, g, b);
    }
    (239, 102, 86)
}

pub fn format_hex_rgb(r: u8, g: u8, b: u8) -> String {
    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

pub fn ink_contrast(hex: &str) -> (u8, u8, u8) {
    let (r, g, b) = parse_hex_rgb(hex);
    if (r as f32) * 0.299 + (g as f32) * 0.587 + (b as f32) * 0.114 > 170.0 {
        (0x18, 0x3b, 0x29)
    } else {
        (255, 255, 255)
    }
}

pub fn rotate_point(p: Point, degrees: f32) -> Point {
    let rad = degrees.to_radians();
    let (s, c) = rad.sin_cos();
    Point {
        x: p.x * c - p.y * s,
        y: p.x * s + p.y * c,
    }
}

// ============================================================================
// System font loading (used for text measurement and rasterized export)
// ============================================================================

fn font_cache() -> &'static Mutex<HashMap<String, Arc<FontVec>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<FontVec>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn candidate_font_paths(family: &str) -> Vec<&'static str> {
    let mut paths = match family {
        "SimSun" => vec!["C:\\Windows\\Fonts\\simsun.ttc", "C:\\Windows\\Fonts\\simsunb.ttf"],
        "SimHei" => vec!["C:\\Windows\\Fonts\\simhei.ttf"],
        "Arial" => vec!["C:\\Windows\\Fonts\\arialbd.ttf", "C:\\Windows\\Fonts\\arial.ttf"],
        "Consolas" => vec!["C:\\Windows\\Fonts\\consolab.ttf", "C:\\Windows\\Fonts\\consola.ttf"],
        _ => vec!["C:\\Windows\\Fonts\\msyhbd.ttc", "C:\\Windows\\Fonts\\msyh.ttc"],
    };
    paths.extend_from_slice(&[
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
    ]);
    paths
}

pub fn load_font(family: &str) -> Option<Arc<FontVec>> {
    let mut cache = font_cache().lock().unwrap();
    if let Some(found) = cache.get(family) {
        return Some(found.clone());
    }
    for path in candidate_font_paths(family) {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = FontVec::try_from_vec_and_index(bytes, 0) {
                let arc = Arc::new(font);
                cache.insert(family.to_string(), arc.clone());
                return Some(arc);
            }
        }
    }
    None
}

/// Measure the unscaled bounding box `(width, height)` of text.
pub fn measure_text_unscaled(text: &str, font_family: &str, font_size: f32) -> (f32, f32) {
    let lines: Vec<&str> = text.split('\n').collect();
    let line_height = font_size * 1.3;
    let total_height = (lines.len().max(1) as f32) * line_height;
    let Some(font) = load_font(font_family) else {
        let max_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1);
        return ((max_chars as f32 * font_size * 0.65).max(24.0), total_height);
    };
    let scaled = font.as_scaled(PxScale::from(font_size));
    let mut max_w: f32 = 24.0;
    for line in &lines {
        let mut w = 0.0f32;
        let mut prev = None;
        for ch in line.chars() {
            let gid = scaled.glyph_id(ch);
            if let Some(p) = prev {
                w += scaled.kern(p, gid);
            }
            w += scaled.h_advance(gid);
            prev = Some(gid);
        }
        max_w = max_w.max(w);
    }
    (max_w, total_height)
}

pub fn text_bounds(stroke: &Stroke) -> (f32, f32, f32, f32) {
    let p = stroke.points.first().copied().unwrap_or(Point { x: 0.0, y: 0.0 });
    let family = stroke.font_family.as_deref().unwrap_or("Microsoft YaHei");
    let (w, h) = measure_text_unscaled(
        stroke.text.as_deref().unwrap_or(""),
        family,
        stroke.effective_font_size(),
    );
    let scale = stroke.scale.unwrap_or(1.0);
    (p.x, p.y, w * scale, h * scale)
}

// ============================================================================
// Layered element hit-testing
// ============================================================================

/// Shortest Euclidean distance from point `p` to line segment `a`--`b`.
pub fn dist_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let vx = b.x - a.x;
    let vy = b.y - a.y;
    let len_sq = vx * vx + vy * vy;
    if len_sq <= 1e-6 {
        return (p.x - a.x).hypot(p.y - a.y);
    }
    let t = (((p.x - a.x) * vx + (p.y - a.y) * vy) / len_sq).clamp(0.0, 1.0);
    let proj_x = a.x + t * vx;
    let proj_y = a.y + t * vy;
    (p.x - proj_x).hypot(p.y - proj_y)
}

fn point_in_triangle(p: Point, a: Point, b: Point, c: Point) -> bool {
    let sign = |p1: Point, p2: Point, p3: Point| -> f32 {
        (p1.x - p3.x) * (p2.y - p3.y) - (p2.x - p3.x) * (p1.y - p3.y)
    };
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = (d1 < 0.0) || (d2 < 0.0) || (d3 < 0.0);
    let has_pos = (d1 > 0.0) || (d2 > 0.0) || (d3 > 0.0);
    !(has_neg && has_pos)
}

/// Hit-test a single annotation layer at image point `p` with hit tolerance `tol` (in image px).
///
/// - Hollow `Rect` / `Ellipse` (without `fill_color`) only hit their border outline.
/// - Filled `Rect` / `Ellipse` (with `fill_color`) hit anywhere inside or on the border.
/// - `Line`, `Arrow`, and `Pen` hit their stroke path (plus arrowhead for `Arrow`).
/// - `Text`, `Watermark`, `Number`, `Mosaic`, and `Highlighter` hit their visual area.
pub fn hit_test_stroke(stroke: &Stroke, p: Point, tol: f32) -> bool {
    if stroke.points.is_empty() || !stroke.tool.is_layer_editable() {
        return false;
    }
    let tol = tol.max(2.0);
    let start = stroke.points[0];
    let end = *stroke.points.last().unwrap();
    let min_x = start.x.min(end.x);
    let max_x = start.x.max(end.x);
    let min_y = start.y.min(end.y);
    let max_y = start.y.max(end.y);

    match stroke.tool {
        Tool::Select | Tool::Eraser => false,
        Tool::Rect => {
            let edge_tol = (stroke.width * 0.5).max(tol);
            if stroke.has_fill() {
                p.x >= min_x - edge_tol
                    && p.x <= max_x + edge_tol
                    && p.y >= min_y - edge_tol
                    && p.y <= max_y + edge_tol
            } else {
                let tl = Point { x: min_x, y: min_y };
                let tr = Point { x: max_x, y: min_y };
                let br = Point { x: max_x, y: max_y };
                let bl = Point { x: min_x, y: max_y };
                dist_to_segment(p, tl, tr) <= edge_tol
                    || dist_to_segment(p, tr, br) <= edge_tol
                    || dist_to_segment(p, br, bl) <= edge_tol
                    || dist_to_segment(p, bl, tl) <= edge_tol
            }
        }
        Tool::Ellipse => {
            let cx = (min_x + max_x) * 0.5;
            let cy = (min_y + max_y) * 0.5;
            let rx = ((max_x - min_x) * 0.5).max(0.5);
            let ry = ((max_y - min_y) * 0.5).max(0.5);
            let edge_tol = (stroke.width * 0.5).max(tol);
            let outer_x = (p.x - cx) / (rx + edge_tol);
            let outer_y = (p.y - cy) / (ry + edge_tol);
            if outer_x * outer_x + outer_y * outer_y > 1.0 {
                return false;
            }
            if stroke.has_fill() {
                return true;
            }
            if rx <= edge_tol || ry <= edge_tol {
                return true;
            }
            let inner_x = (p.x - cx) / (rx - edge_tol);
            let inner_y = (p.y - cy) / (ry - edge_tol);
            inner_x * inner_x + inner_y * inner_y >= 1.0
        }
        Tool::Line => {
            let hit_tol = (stroke.width * 0.5).max(tol);
            dist_to_segment(p, start, end) <= hit_tol
        }
        Tool::Arrow => {
            let hit_tol = (stroke.width * 0.5).max(tol);
            if dist_to_segment(p, start, end) <= hit_tol {
                return true;
            }
            let dx = end.x - start.x;
            let dy = end.y - start.y;
            let dist = dx.hypot(dy);
            let head_len = dist.min((stroke.width * 4.0).max(12.0));
            if head_len > 0.5 {
                let angle = dy.atan2(dx);
                let a1 = angle - std::f32::consts::PI / 6.0;
                let a2 = angle + std::f32::consts::PI / 6.0;
                let h1 = Point {
                    x: end.x - head_len * a1.cos(),
                    y: end.y - head_len * a1.sin(),
                };
                let h2 = Point {
                    x: end.x - head_len * a2.cos(),
                    y: end.y - head_len * a2.sin(),
                };
                let head_tol = tol * 0.75;
                if point_in_triangle(p, end, h1, h2)
                    || dist_to_segment(p, end, h1) <= head_tol
                    || dist_to_segment(p, end, h2) <= head_tol
                    || dist_to_segment(p, h1, h2) <= head_tol
                {
                    return true;
                }
            }
            false
        }
        Tool::Pen => {
            let hit_tol = (stroke.width * 0.5).max(tol);
            if stroke.points.len() == 1 {
                return (p.x - start.x).hypot(p.y - start.y) <= hit_tol;
            }
            stroke
                .points
                .windows(2)
                .any(|w| dist_to_segment(p, w[0], w[1]) <= hit_tol)
        }
        Tool::Text | Tool::Watermark => {
            let (bx, by, bw, bh) = text_bounds(stroke);
            let rel = Point {
                x: p.x - bx,
                y: p.y - by,
            };
            let unrot = rotate_point(rel, -stroke.rotation.unwrap_or(0.0));
            unrot.x >= -tol && unrot.y >= -tol && unrot.x <= bw + tol && unrot.y <= bh + tol
        }
        Tool::Number => {
            let radius = (stroke.width * 4.0).max(14.0);
            (p.x - start.x).hypot(p.y - start.y) <= radius + tol * 0.5
        }
        Tool::Mosaic | Tool::Highlighter => {
            let pad = tol * 0.5;
            p.x >= min_x - pad && p.x <= max_x + pad && p.y >= min_y - pad && p.y <= max_y + pad
        }
    }
}

// ============================================================================
// Pure-Rust annotation rasterization (matches legacy Canvas2D paint semantics)
// ============================================================================

fn rgba_to_pixmap(img: &Rgba) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(img.width.max(1), img.height.max(1))?;
    let dst = pixmap.data_mut();
    for (src_px, dst_px) in img.pixels.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        let a = src_px[3] as u32;
        dst_px[0] = ((src_px[0] as u32 * a + 127) / 255) as u8;
        dst_px[1] = ((src_px[1] as u32 * a + 127) / 255) as u8;
        dst_px[2] = ((src_px[2] as u32 * a + 127) / 255) as u8;
        dst_px[3] = src_px[3];
    }
    Some(pixmap)
}

fn pixmap_to_rgba(pixmap: &Pixmap) -> Rgba {
    let w = pixmap.width();
    let h = pixmap.height();
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for (src_px, dst_px) in pixmap.data().chunks_exact(4).zip(pixels.chunks_exact_mut(4)) {
        let a = src_px[3];
        if a == 0 {
            dst_px.copy_from_slice(&[0, 0, 0, 0]);
        } else if a == 255 {
            dst_px.copy_from_slice(src_px);
        } else {
            let a32 = a as u32;
            dst_px[0] = ((src_px[0] as u32 * 255 + a32 / 2) / a32).min(255) as u8;
            dst_px[1] = ((src_px[1] as u32 * 255 + a32 / 2) / a32).min(255) as u8;
            dst_px[2] = ((src_px[2] as u32 * 255 + a32 / 2) / a32).min(255) as u8;
            dst_px[3] = a;
        }
    }
    Rgba::new(w, h, pixels)
}

fn draw_text_onto_pixmap(
    target: &mut Pixmap,
    text: &str,
    font_family: &str,
    font_size: f32,
    color_rgb: (u8, u8, u8),
    opacity: f32,
    origin: Point,
    scale: f32,
    rotation_deg: f32,
    centered: bool,
) {
    let Some(font) = load_font(font_family) else {
        return;
    };
    let (raw_w, raw_h) = measure_text_unscaled(text, font_family, font_size);
    let pad = (font_size * 0.4).ceil() as u32 + 4;
    let buf_w = (raw_w.ceil() as u32 + pad * 2).max(4);
    let buf_h = (raw_h.ceil() as u32 + pad * 2).max(4);
    let Some(mut text_pm) = Pixmap::new(buf_w, buf_h) else {
        return;
    };

    let scaled = font.as_scaled(PxScale::from(font_size));
    let ascent = scaled.ascent();
    let line_height = font_size * 1.3;
    let (cr, cg, cb) = color_rgb;

    for (line_idx, line) in text.split('\n').enumerate() {
        let mut cursor_x = pad as f32;
        let baseline_y = pad as f32 + line_idx as f32 * line_height + ascent;
        let mut prev = None;
        for ch in line.chars() {
            let gid = scaled.glyph_id(ch);
            if let Some(p) = prev {
                cursor_x += scaled.kern(p, gid);
            }
            let glyph = gid.with_scale_and_position(
                PxScale::from(font_size),
                ab_glyph::point(cursor_x, baseline_y),
            );
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                let data = text_pm.data_mut();
                outlined.draw(|gx, gy, cov| {
                    let px = bounds.min.x as i32 + gx as i32;
                    let py = bounds.min.y as i32 + gy as i32;
                    if px >= 0 && py >= 0 && (px as u32) < buf_w && (py as u32) < buf_h {
                        let cov = (cov * opacity).clamp(0.0, 1.0);
                        let alpha = (cov * 255.0).round() as u8;
                        if alpha > 0 {
                            let idx = ((py as u32 * buf_w + px as u32) * 4) as usize;
                            let old_a = data[idx + 3] as u32;
                            let new_a = alpha as u32;
                            let out_a = (new_a + old_a * (255 - new_a) / 255).min(255) as u8;
                            data[idx] = ((cr as u32 * out_a as u32 + 127) / 255) as u8;
                            data[idx + 1] = ((cg as u32 * out_a as u32 + 127) / 255) as u8;
                            data[idx + 2] = ((cb as u32 * out_a as u32 + 127) / 255) as u8;
                            data[idx + 3] = out_a;
                        }
                    }
                });
            }
            cursor_x += scaled.h_advance(gid);
            prev = Some(gid);
        }
    }

    let offset_x = if centered {
        -(pad as f32 + raw_w * 0.5)
    } else {
        -(pad as f32)
    };
    let offset_y = if centered {
        -(pad as f32 + raw_h * 0.5)
    } else {
        -(pad as f32)
    };
    let transform = Transform::from_translate(offset_x, offset_y)
        .post_scale(scale, scale)
        .post_rotate(rotation_deg)
        .post_translate(origin.x, origin.y);
    let paint = PixmapPaint {
        opacity: 1.0,
        blend_mode: BlendMode::SourceOver,
        quality: tiny_skia::FilterQuality::Bilinear,
    };
    target.draw_pixmap(0, 0, text_pm.as_ref(), &paint, transform, None);
}

/// Rasterize and composite the annotation list onto the base image, returning the final RGBA pixels.
pub fn rasterize_annotations(base: &Rgba, strokes: &[Stroke]) -> Rgba {
    if strokes.is_empty() {
        return base.clone();
    }
    let Some(mut output) = rgba_to_pixmap(base) else {
        return base.clone();
    };
    let w = output.width();
    let h = output.height();
    let Some(mut layer) = Pixmap::new(w, h) else {
        return base.clone();
    };
    let mut highlights: Option<Pixmap> = None;

    for stroke in strokes {
        if stroke.points.is_empty() {
            continue;
        }
        let start = stroke.points[0];
        let end = *stroke.points.last().unwrap();
        let x = start.x.min(end.x);
        let y = start.y.min(end.y);
        let rw = (end.x - start.x).abs();
        let rh = (end.y - start.y).abs();
        let (r, g, b) = parse_hex_rgb(&stroke.color);
        let opacity = stroke.effective_opacity();
        let alpha_u8 = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;

        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(r, g, b, alpha_u8));
        paint.anti_alias = true;

        let mut skia_stroke = SkiaStroke {
            width: stroke.width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };

        match stroke.tool {
            Tool::Select => {}
            Tool::Highlighter => {
                if rw > 0.0 && rh > 0.0 {
                    let shade = highlights.get_or_insert_with(|| {
                        let mut pm = Pixmap::new(w, h).unwrap();
                        pm.fill(Color::from_rgba8(7, 30, 27, 0x65));
                        pm
                    });
                    if let Some(rect) = Rect::from_xywh(x, y, rw, rh) {
                        let mut clear_paint = Paint::default();
                        clear_paint.blend_mode = BlendMode::Clear;
                        shade.fill_rect(rect, &clear_paint, Transform::identity(), None);
                    }
                }
            }
            Tool::Rect => {
                if let Some(rect) = Rect::from_xywh(x, y, rw.max(0.5), rh.max(0.5)) {
                    let path = PathBuilder::from_rect(rect);
                    if let Some(fill_hex) = stroke.fill_color.as_deref().filter(|c| !c.is_empty()) {
                        let (fr, fg, fb) = parse_hex_rgb(fill_hex);
                        let mut fill_paint = Paint::default();
                        fill_paint.set_color(Color::from_rgba8(fr, fg, fb, alpha_u8));
                        fill_paint.anti_alias = true;
                        layer.fill_path(
                            &path,
                            &fill_paint,
                            FillRule::Winding,
                            Transform::identity(),
                            None,
                        );
                    }
                    layer.stroke_path(&path, &paint, &skia_stroke, Transform::identity(), None);
                }
            }
            Tool::Ellipse => {
                if let Some(rect) = Rect::from_xywh(x, y, rw.max(0.5), rh.max(0.5)) {
                    if let Some(path) = PathBuilder::from_oval(rect) {
                        if let Some(fill_hex) =
                            stroke.fill_color.as_deref().filter(|c| !c.is_empty())
                        {
                            let (fr, fg, fb) = parse_hex_rgb(fill_hex);
                            let mut fill_paint = Paint::default();
                            fill_paint.set_color(Color::from_rgba8(fr, fg, fb, alpha_u8));
                            fill_paint.anti_alias = true;
                            layer.fill_path(
                                &path,
                                &fill_paint,
                                FillRule::Winding,
                                Transform::identity(),
                                None,
                            );
                        }
                        layer.stroke_path(
                            &path,
                            &paint,
                            &skia_stroke,
                            Transform::identity(),
                            None,
                        );
                    }
                }
            }
            Tool::Pen | Tool::Eraser => {
                let is_eraser = stroke.tool == Tool::Eraser;
                if is_eraser {
                    paint.set_color(Color::from_rgba8(0, 0, 0, 255));
                    paint.blend_mode = BlendMode::DestinationOut;
                    skia_stroke.width = stroke.width * 8.0;
                }
                let all_same = stroke
                    .points
                    .iter()
                    .all(|p| (p.x - start.x).abs() < 0.01 && (p.y - start.y).abs() < 0.01);
                if all_same {
                    if let Some(path) =
                        PathBuilder::from_circle(start.x, start.y, skia_stroke.width * 0.5)
                    {
                        layer.fill_path(
                            &path,
                            &paint,
                            FillRule::Winding,
                            Transform::identity(),
                            None,
                        );
                        if is_eraser {
                            if let Some(shade) = highlights.as_mut() {
                                shade.fill_path(
                                    &path,
                                    &paint,
                                    FillRule::Winding,
                                    Transform::identity(),
                                    None,
                                );
                            }
                        }
                    }
                } else {
                    let mut pb = PathBuilder::new();
                    pb.move_to(start.x, start.y);
                    for p in &stroke.points[1..] {
                        pb.line_to(p.x, p.y);
                    }
                    if let Some(path) = pb.finish() {
                        layer.stroke_path(
                            &path,
                            &paint,
                            &skia_stroke,
                            Transform::identity(),
                            None,
                        );
                        if is_eraser {
                            if let Some(shade) = highlights.as_mut() {
                                shade.stroke_path(
                                    &path,
                                    &paint,
                                    &skia_stroke,
                                    Transform::identity(),
                                    None,
                                );
                            }
                        }
                    }
                }
            }
            Tool::Line | Tool::Arrow => {
                let dx = end.x - start.x;
                let dy = end.y - start.y;
                let dist = dx.hypot(dy);
                let angle = dy.atan2(dx);
                let head_len = dist.min((stroke.width * 4.0).max(12.0));
                let inset = if stroke.tool == Tool::Arrow {
                    (head_len * (std::f32::consts::PI / 6.0).cos() * 2.0) / 3.0
                } else {
                    0.0
                };
                if stroke.tool == Tool::Arrow {
                    skia_stroke.line_cap = LineCap::Butt;
                    skia_stroke.width = stroke.width.min((head_len * 2.0) / 3.0);
                }
                let mut pb = PathBuilder::new();
                pb.move_to(start.x, start.y);
                pb.line_to(end.x - inset * angle.cos(), end.y - inset * angle.sin());
                if let Some(path) = pb.finish() {
                    layer.stroke_path(&path, &paint, &skia_stroke, Transform::identity(), None);
                }
                if stroke.tool == Tool::Arrow && head_len > 0.5 {
                    let a1 = angle - std::f32::consts::PI / 6.0;
                    let a2 = angle + std::f32::consts::PI / 6.0;
                    let mut hb = PathBuilder::new();
                    hb.move_to(end.x, end.y);
                    hb.line_to(end.x - head_len * a1.cos(), end.y - head_len * a1.sin());
                    hb.line_to(end.x - head_len * a2.cos(), end.y - head_len * a2.sin());
                    hb.close();
                    if let Some(head) = hb.finish() {
                        layer.fill_path(
                            &head,
                            &paint,
                            FillRule::Winding,
                            Transform::identity(),
                            None,
                        );
                    }
                }
            }
            Tool::Number => {
                let radius = (stroke.width * 4.0).max(14.0);
                if let Some(circle) = PathBuilder::from_circle(start.x, start.y, radius) {
                    layer.fill_path(
                        &circle,
                        &paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
                let num_text = stroke.number.unwrap_or(1).to_string();
                let contrast = ink_contrast(&stroke.color);
                draw_text_onto_pixmap(
                    &mut layer,
                    &num_text,
                    "Microsoft YaHei",
                    radius * 1.1,
                    contrast,
                    opacity,
                    start,
                    1.0,
                    0.0,
                    true,
                );
            }
            Tool::Text | Tool::Watermark => {
                let text = stroke.text.as_deref().unwrap_or("");
                if !text.is_empty() {
                    let family = stroke.font_family.as_deref().unwrap_or("Microsoft YaHei");
                    draw_text_onto_pixmap(
                        &mut layer,
                        text,
                        family,
                        stroke.effective_font_size(),
                        (r, g, b),
                        opacity,
                        start,
                        stroke.scale.unwrap_or(1.0),
                        stroke.rotation.unwrap_or(0.0),
                        false,
                    );
                }
            }
            Tool::Mosaic => {
                if rw > 0.0 && rh > 0.0 {
                    let ix = (x.floor() as i32).clamp(0, w as i32 - 1) as u32;
                    let iy = (y.floor() as i32).clamp(0, h as i32 - 1) as u32;
                    let iw = (rw.ceil() as u32).min(w - ix);
                    let ih = (rh.ceil() as u32).min(h - iy);
                    let block = ((stroke.width * 4.0).round() as u32).max(6);
                    // Composite the current image state for mosaic sampling
                    let mut snapshot = output.clone();
                    let over = PixmapPaint::default();
                    if let Some(shade) = highlights.as_ref() {
                        snapshot.draw_pixmap(0, 0, shade.as_ref(), &over, Transform::identity(), None);
                    }
                    snapshot.draw_pixmap(0, 0, layer.as_ref(), &over, Transform::identity(), None);
                    let snap_data = snapshot.data();
                    let mut by = iy;
                    while by < iy + ih {
                        let bh = block.min(iy + ih - by);
                        let mut bx = ix;
                        while bx < ix + iw {
                            let bw = block.min(ix + iw - bx);
                            let mut sum_r = 0u64;
                            let mut sum_g = 0u64;
                            let mut sum_b = 0u64;
                            let mut sum_a = 0u64;
                            let mut count = 0u64;
                            for py in by..by + bh {
                                for px in bx..bx + bw {
                                    let idx = ((py * w + px) * 4) as usize;
                                    sum_r += snap_data[idx] as u64;
                                    sum_g += snap_data[idx + 1] as u64;
                                    sum_b += snap_data[idx + 2] as u64;
                                    sum_a += snap_data[idx + 3] as u64;
                                    count += 1;
                                }
                            }
                            if count > 0 {
                                let avg_a = (sum_a / count) as u8;
                                let (ur, ug, ub) = if avg_a > 0 {
                                    let a64 = avg_a as u64;
                                    (
                                        ((sum_r / count) * 255 / a64).min(255) as u8,
                                        ((sum_g / count) * 255 / a64).min(255) as u8,
                                        ((sum_b / count) * 255 / a64).min(255) as u8,
                                    )
                                } else {
                                    (0, 0, 0)
                                };
                                let mut mp = Paint::default();
                                mp.set_color(Color::from_rgba8(ur, ug, ub, avg_a));
                                mp.anti_alias = false;
                                if let Some(rect) =
                                    Rect::from_xywh(bx as f32, by as f32, bw as f32, bh as f32)
                                {
                                    layer.fill_rect(rect, &mp, Transform::identity(), None);
                                }
                            }
                            bx += bw;
                        }
                        by += bh;
                    }
                }
            }
        }
    }

    let over = PixmapPaint::default();
    if let Some(shade) = highlights.as_ref() {
        output.draw_pixmap(0, 0, shade.as_ref(), &over, Transform::identity(), None);
    }
    output.draw_pixmap(0, 0, layer.as_ref(), &over, Transform::identity(), None);
    pixmap_to_rgba(&output)
}

// ============================================================================
// Strict schema validation
// ============================================================================

fn valid_id(id: &Value) -> bool {
    let Some(text) = id.as_str() else {
        return false;
    };
    (1..=80).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn hex_color(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    text.len() == 7
        && text.starts_with('#')
        && text[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn number_in(value: Option<&Value>, min: f64, max: f64) -> bool {
    value
        .and_then(|v| v.as_f64())
        .is_some_and(|n| n.is_finite() && n >= min && n <= max)
}

/// Validate and normalize annotations; `size` is the physical size of the composite image.
#[allow(dead_code)]
pub fn validate_annotations(input: &Value, size: (u32, u32)) -> Result<Vec<Value>, String> {
    let strokes = input
        .as_array()
        .ok_or_else(|| i18n::t("Annotations are invalid or exceed the limit").to_string())?;
    if strokes.len() > 5000 {
        return Err(i18n::t("Annotations are invalid or exceed the limit").into());
    }
    let mut point_count: usize = 0;
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(strokes.len());
    for stroke in strokes {
        let map = stroke
            .as_object()
            .ok_or_else(|| i18n::t("Invalid annotation format").to_string())?;
        let id = map.get("id").cloned().unwrap_or(Value::Null);
        if !valid_id(&id) {
            return Err(i18n::t("Invalid annotation format").into());
        }
        let id_text = id.as_str().unwrap().to_string();
        if !ids.insert(id_text) {
            return Err(i18n::t("Invalid annotation format").into());
        }
        let tool = map
            .get("tool")
            .and_then(|t| t.as_str())
            .ok_or_else(|| i18n::t("Invalid annotation format").to_string())?;
        if crate::settings::defaults()["annotations"]
            .as_object()
            .is_none_or(|a| !a.contains_key(tool))
        {
            return Err(i18n::t("Invalid annotation format").into());
        }
        let Some(points) = map.get("points").and_then(|p| p.as_array()) else {
            return Err(i18n::t("Invalid annotation format").into());
        };
        if points.is_empty() {
            return Err(i18n::t("Invalid annotation format").into());
        }
        if !hex_color(map.get("color").unwrap_or(&Value::Null)) {
            return Err(i18n::t("Invalid annotation format").into());
        }
        if !number_in(map.get("width"), 1.0, 24.0) {
            return Err(i18n::t("Invalid annotation format").into());
        }
        point_count += points.len();
        if point_count > 200_000 {
            return Err(i18n::t("Too many annotation points").into());
        }
        let mut normalized_points = Vec::with_capacity(points.len());
        for point in points {
            let map = point
                .as_object()
                .ok_or_else(|| i18n::t("Annotation coordinates are outside image bounds").to_string())?;
            let x = map.get("x").and_then(|v| v.as_f64());
            let y = map.get("y").and_then(|v| v.as_f64());
            let (Some(x), Some(y)) = (x, y) else {
                return Err(i18n::t("Annotation coordinates are outside image bounds").into());
            };
            if !x.is_finite()
                || !y.is_finite()
                || x < 0.0
                || y < 0.0
                || x > size.0 as f64
                || y > size.1 as f64
            {
                return Err(i18n::t("Annotation coordinates are outside image bounds").into());
            }
            normalized_points.push(json!({ "x": x, "y": y }));
        }
        let mut result_stroke = Map::new();
        result_stroke.insert("id".into(), id);
        result_stroke.insert("tool".into(), json!(tool));
        result_stroke.insert("points".into(), Value::Array(normalized_points));
        result_stroke.insert("color".into(), map["color"].clone());
        if let Some(fill_color) = map.get("fillColor") {
            if !fill_color.is_null() {
                if !hex_color(fill_color) {
                    return Err(i18n::t("Invalid annotation format").into());
                }
                result_stroke.insert("fillColor".into(), fill_color.clone());
            }
        }
        result_stroke.insert("width".into(), map["width"].clone());
        if tool == "text" || tool == "watermark" {
            let text = map.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if text.len() > 2000
                || !number_in(map.get("fontSize"), 12.0, 96.0)
                || !map
                    .get("fontFamily")
                    .and_then(|f| f.as_str())
                    .is_some_and(|f| FONTS.iter().any(|(name, _)| *name == f))
            {
                return Err(i18n::t("Invalid text annotation parameters").into());
            }
            result_stroke.insert("text".into(), json!(text));
            result_stroke.insert("fontSize".into(), map["fontSize"].clone());
            result_stroke.insert("fontFamily".into(), map["fontFamily"].clone());
            for (key, min, max) in [("scale", 0.1, 20.0), ("rotation", -180.0, 180.0)] {
                if let Some(value) = map.get(key) {
                    if !number_in(Some(value), min, max) {
                        return Err(i18n::t("Invalid text transform parameters").into());
                    }
                    result_stroke.insert(key.into(), value.clone());
                }
            }
        }
        if tool == "number" {
            let number = map.get("number").and_then(|n| n.as_i64());
            if number.is_none_or(|n| !(1..=5000).contains(&n)) {
                return Err(i18n::t("Invalid sequence number").into());
            }
            result_stroke.insert("number".into(), json!(number.unwrap()));
        }
        if let Some(opacity) = map.get("opacity") {
            if !number_in(Some(opacity), 5.0, 100.0) {
                return Err(i18n::t("Invalid annotation opacity").into());
            }
            result_stroke.insert("opacity".into(), opacity.clone());
        }
        result.push(Value::Object(result_stroke));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke_json(tool: &str, extra: Value) -> Value {
        let base = serde_json::json!({
            "id": "stroke-1",
            "tool": tool,
            "points": [{ "x": 1, "y": 2 }],
            "color": "#ff0000",
            "width": 4,
        });
        let mut map = base.as_object().cloned().unwrap_or_default();
        if let Value::Object(patch) = extra {
            for (key, value) in patch {
                map.insert(key, value);
            }
        }
        Value::Object(map)
    }

    #[test]
    fn accepts_valid_rect_stroke() {
        let result = validate_annotations(
            &serde_json::json!([stroke_json("rect", Value::Null)]),
            (100, 100),
        )
        .unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn rejects_unknown_tool_and_bad_color() {
        assert!(validate_annotations(
            &serde_json::json!([stroke_json("nope", Value::Null)]),
            (100, 100)
        )
        .is_err());
        assert!(validate_annotations(
            &serde_json::json!([stroke_json("rect", serde_json::json!({ "color": "red" }))]),
            (100, 100)
        )
        .is_err());
    }

    #[test]
    fn rejects_out_of_bounds_points() {
        assert!(validate_annotations(
            &serde_json::json!([stroke_json(
                "rect",
                serde_json::json!({ "points": [{ "x": 1, "y": 999 }] })
            )]),
            (100, 100)
        )
        .is_err());
    }

    #[test]
    fn text_requires_font_parameters() {
        assert!(validate_annotations(
            &serde_json::json!([stroke_json(
                "text",
                serde_json::json!({ "text": "hi", "fontSize": 20, "fontFamily": "SimHei" })
            )]),
            (100, 100)
        )
        .is_ok());
        assert!(validate_annotations(
            &serde_json::json!([stroke_json(
                "text",
                serde_json::json!({ "text": "hi", "fontSize": 20 })
            )]),
            (100, 100)
        )
        .is_err());
    }

    #[test]
    fn number_requires_integer_in_range() {
        assert!(validate_annotations(
            &serde_json::json!([stroke_json("number", serde_json::json!({ "number": 3 }))]),
            (100, 100)
        )
        .is_ok());
        assert!(validate_annotations(
            &serde_json::json!([stroke_json(
                "number",
                serde_json::json!({ "number": 9999 })
            )]),
            (100, 100)
        )
        .is_err());
    }

    #[test]
    fn rasterizer_draws_rect_and_eraser_preserves_transparent_base() {
        // Transparent base + rectangle + eraser stroke: base alpha channel should remain transparent
        let base = Rgba::new(40, 40, vec![0u8; 40 * 40 * 4]);
        let rect = Stroke {
            id: "s1".into(),
            tool: Tool::Rect,
            points: vec![Point { x: 10.0, y: 10.0 }, Point { x: 30.0, y: 30.0 }],
            color: "#ff0000".into(),
            fill_color: None,
            width: 4.0,
            text: None,
            number: None,
            font_size: None,
            font_family: None,
            opacity: Some(100.0),
            scale: None,
            rotation: None,
        };
        let drawn = rasterize_annotations(&base, &[rect.clone()]);
        let top_edge_idx = (10 * 40 + 20) * 4;
        assert!(drawn.pixels[top_edge_idx] > 200);
        assert!(drawn.pixels[top_edge_idx + 3] > 200);

        let eraser = Stroke {
            id: "s2".into(),
            tool: Tool::Eraser,
            points: vec![Point { x: 10.0, y: 10.0 }, Point { x: 30.0, y: 10.0 }],
            color: "#ff0000".into(),
            fill_color: None,
            width: 3.0,
            text: None,
            number: None,
            font_size: None,
            font_family: None,
            opacity: Some(100.0),
            scale: None,
            rotation: None,
        };
        let erased = rasterize_annotations(&base, &[rect, eraser]);
        assert_eq!(erased.pixels[top_edge_idx + 3], 0);
    }

    #[test]
    fn rasterizer_highlighter_shades_outside_and_preserves_inside() {
        let mut pixels = vec![0u8; 40 * 40 * 4];
        for px in pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[240, 240, 240, 255]);
        }
        let base = Rgba::new(40, 40, pixels);
        let hl = Stroke {
            id: "h1".into(),
            tool: Tool::Highlighter,
            points: vec![Point { x: 10.0, y: 10.0 }, Point { x: 30.0, y: 30.0 }],
            color: "#f6ca45".into(),
            fill_color: None,
            width: 3.0,
            text: None,
            number: None,
            font_size: None,
            font_family: None,
            opacity: Some(30.0),
            scale: None,
            rotation: None,
        };
        let out = rasterize_annotations(&base, &[hl]);
        let inside = (20 * 40 + 20) * 4;
        let outside = (2 * 40 + 2) * 4;
        assert_eq!(out.pixels[inside], 240);
        assert!(out.pixels[outside] < 180);
    }

    #[test]
    fn rasterizer_draws_filled_rect_and_ellipse() {
        let base = Rgba::new(60, 60, vec![0u8; 60 * 60 * 4]);
        let rect = Stroke {
            id: "r1".into(),
            tool: Tool::Rect,
            points: vec![Point { x: 10.0, y: 10.0 }, Point { x: 50.0, y: 50.0 }],
            color: "#ff0000".into(),
            fill_color: Some("#00ff00".into()),
            width: 4.0,
            text: None,
            number: None,
            font_size: None,
            font_family: None,
            opacity: Some(100.0),
            scale: None,
            rotation: None,
        };
        let out = rasterize_annotations(&base, &[rect]);
        let center = (30 * 60 + 30) * 4;
        assert!(out.pixels[center + 1] > 200);
        assert_eq!(out.pixels[center], 0);
        let edge = (10 * 60 + 30) * 4;
        assert!(out.pixels[edge] > 200);

        let ellipse = Stroke {
            id: "e1".into(),
            tool: Tool::Ellipse,
            points: vec![Point { x: 10.0, y: 10.0 }, Point { x: 50.0, y: 50.0 }],
            color: "#0000ff".into(),
            fill_color: Some("#00ff00".into()),
            width: 4.0,
            text: None,
            number: None,
            font_size: None,
            font_family: None,
            opacity: Some(100.0),
            scale: None,
            rotation: None,
        };
        let out_e = rasterize_annotations(&base, &[ellipse]);
        assert!(out_e.pixels[center + 1] > 200);
        let corner = (11 * 60 + 11) * 4;
        assert_eq!(out_e.pixels[corner + 3], 0);
    }

    #[test]
    fn hit_test_distinguishes_hollow_border_vs_filled_area() {
        let make_shape = |tool: Tool, fill: Option<&str>| Stroke {
            id: "s".into(),
            tool,
            points: vec![Point { x: 100.0, y: 100.0 }, Point { x: 200.0, y: 200.0 }],
            color: "#ef6656".into(),
            fill_color: fill.map(str::to_string),
            width: 3.0,
            text: None,
            number: None,
            font_size: None,
            font_family: None,
            opacity: Some(100.0),
            scale: None,
            rotation: None,
        };

        // Hollow rectangle: border hits, interior misses.
        let hollow_rect = make_shape(Tool::Rect, None);
        assert!(hit_test_stroke(&hollow_rect, Point { x: 150.0, y: 100.0 }, 6.0));
        assert!(hit_test_stroke(&hollow_rect, Point { x: 100.0, y: 150.0 }, 6.0));
        assert!(!hit_test_stroke(&hollow_rect, Point { x: 150.0, y: 150.0 }, 6.0));
        assert!(!hit_test_stroke(&hollow_rect, Point { x: 50.0, y: 50.0 }, 6.0));

        // Filled rectangle: interior AND border hit.
        let filled_rect = make_shape(Tool::Rect, Some("#f6ca45"));
        assert!(hit_test_stroke(&filled_rect, Point { x: 150.0, y: 150.0 }, 6.0));
        assert!(hit_test_stroke(&filled_rect, Point { x: 100.0, y: 150.0 }, 6.0));
        assert!(!hit_test_stroke(&filled_rect, Point { x: 50.0, y: 50.0 }, 6.0));

        // Hollow ellipse: perimeter hits, center misses, outside corner misses.
        let hollow_ellipse = make_shape(Tool::Ellipse, None);
        assert!(hit_test_stroke(&hollow_ellipse, Point { x: 150.0, y: 100.0 }, 6.0));
        assert!(!hit_test_stroke(&hollow_ellipse, Point { x: 150.0, y: 150.0 }, 6.0));
        assert!(!hit_test_stroke(&hollow_ellipse, Point { x: 102.0, y: 102.0 }, 6.0));

        // Filled ellipse: center hits, outside bounding-box corner still misses.
        let filled_ellipse = make_shape(Tool::Ellipse, Some("#2789e8"));
        assert!(hit_test_stroke(&filled_ellipse, Point { x: 150.0, y: 150.0 }, 6.0));
        assert!(hit_test_stroke(&filled_ellipse, Point { x: 150.0, y: 100.0 }, 6.0));
        assert!(!hit_test_stroke(&filled_ellipse, Point { x: 102.0, y: 102.0 }, 4.0));
    }

    #[test]
    fn hit_test_arrow_mosaic_and_validation_of_fill_color() {
        let arrow = Stroke {
            id: "a1".into(),
            tool: Tool::Arrow,
            points: vec![Point { x: 10.0, y: 10.0 }, Point { x: 110.0, y: 10.0 }],
            color: "#ef6656".into(),
            fill_color: None,
            width: 3.0,
            text: None,
            number: None,
            font_size: None,
            font_family: None,
            opacity: Some(100.0),
            scale: None,
            rotation: None,
        };
        assert!(hit_test_stroke(&arrow, Point { x: 60.0, y: 12.0 }, 6.0));
        assert!(!hit_test_stroke(&arrow, Point { x: 60.0, y: 40.0 }, 6.0));

        let mosaic = Stroke {
            id: "m1".into(),
            tool: Tool::Mosaic,
            points: vec![Point { x: 20.0, y: 20.0 }, Point { x: 80.0, y: 80.0 }],
            color: "#ef6656".into(),
            fill_color: None,
            width: 4.0,
            text: None,
            number: None,
            font_size: None,
            font_family: None,
            opacity: Some(100.0),
            scale: None,
            rotation: None,
        };
        assert!(hit_test_stroke(&mosaic, Point { x: 50.0, y: 50.0 }, 6.0));
        assert!(!hit_test_stroke(&mosaic, Point { x: 5.0, y: 5.0 }, 6.0));

        assert!(validate_annotations(
            &serde_json::json!([stroke_json("rect", serde_json::json!({ "fillColor": "#00aa66" }))]),
            (100, 100)
        )
        .is_ok());
        assert!(validate_annotations(
            &serde_json::json!([stroke_json("rect", serde_json::json!({ "fillColor": "invalid" }))]),
            (100, 100)
        )
        .is_err());
    }
}
