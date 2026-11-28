//! Pure vector icon drawing and button wrappers (egui::Painter on a 24x24 logical grid).
//!
//! Avoids missing Emoji/symbol glyphs on Windows and stays crisp at any DPI scaling.

use crate::ui;
use egui::{
    Color32, CornerRadius, Id, PopupCloseBehavior, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2,
};

/// Application-wide vector icon catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    // Brand / selection
    Brand,
    Select,
    // Annotation tools
    Rect,
    Line,
    Ellipse,
    Pen,
    Arrow,
    Text,
    Watermark,
    Number,
    Mosaic,
    Highlighter,
    Eraser,
    Layers,
    Undo,
    // Capture & actions
    RecordGif,
    LongCapture,
    Pin,
    Save,
    Copy,
    Check,
    Close,
    Reselect,
    Ocr,
    Translate,
    ZoomIn,
    ZoomOut,
    Passthrough,
    Play,
    Stop,
    Back,
    ScrollManual,
    Folder,
    Upload,
    Trash,
    Alert,
    ChevronDown,
    ChevronUp,
    Eye,
    EyeOff,
    TextPanel,
    // Settings sections & account actions
    SettingsGeneral,
    Keyboard,
    Cloud,
    Swap,
    Shield,
    LogIn,
    LogOut,
    UserPlus,
}

/// Paint a centered vector icon inside `rect`.
pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let side = rect.width().min(rect.height()).max(1.0);
    let r = Rect::from_center_size(rect.center(), Vec2::splat(side));
    let p = |x: f32, y: f32| -> Pos2 {
        Pos2::new(r.min.x + (x / 24.0) * r.width(), r.min.y + (y / 24.0) * r.height())
    };
    let s = |v: f32| -> f32 { (v / 24.0) * side };
    let sw = (side / 16.0 * 1.55).clamp(1.25, 2.4);
    let stroke = Stroke::new(sw, color);
    let thin = Stroke::new((sw * 0.82).max(1.1), color);

    let poly = |pts: &[(f32, f32)], closed: bool, st: Stroke| {
        let mapped: Vec<Pos2> = pts.iter().map(|&(x, y)| p(x, y)).collect();
        if closed {
            painter.add(egui::Shape::closed_line(mapped, st));
        } else {
            painter.add(egui::Shape::line(mapped, st));
        }
    };

    match icon {
        Icon::Brand => {
            poly(&[(4.0, 9.0), (4.0, 4.0), (9.0, 4.0)], false, stroke);
            poly(&[(15.0, 4.0), (20.0, 4.0), (20.0, 9.0)], false, stroke);
            poly(&[(20.0, 15.0), (20.0, 20.0), (15.0, 20.0)], false, stroke);
            poly(&[(9.0, 20.0), (4.0, 20.0), (4.0, 15.0)], false, stroke);
            painter.circle_filled(p(12.0, 12.0), s(1.8), color);
        }
        Icon::Select => {
            poly(
                &[
                    (5.5, 3.5),
                    (5.5, 19.0),
                    (9.5, 14.8),
                    (12.6, 20.4),
                    (14.8, 19.3),
                    (11.8, 13.7),
                    (17.5, 13.7),
                ],
                true,
                thin,
            );
        }
        Icon::Rect => {
            painter.rect_stroke(
                Rect::from_min_max(p(4.0, 5.0), p(20.0, 19.0)),
                CornerRadius::same(s(2.4).round() as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        Icon::Line => {
            painter.line_segment([p(5.0, 19.0), p(19.0, 5.0)], stroke);
            painter.circle_filled(p(5.0, 19.0), s(1.7), color);
            painter.circle_filled(p(19.0, 5.0), s(1.7), color);
        }
        Icon::Ellipse => {
            painter.circle_stroke(p(12.0, 12.0), s(7.5), stroke);
        }
        Icon::Pen => {
            poly(
                &[
                    (16.0, 4.0),
                    (20.0, 8.0),
                    (8.8, 19.2),
                    (4.2, 19.8),
                    (4.8, 15.2),
                ],
                true,
                thin,
            );
            painter.line_segment([p(13.5, 6.5), p(17.5, 10.5)], thin);
        }
        Icon::Arrow => {
            painter.line_segment([p(5.5, 18.5), p(18.5, 5.5)], stroke);
            poly(&[(10.0, 5.5), (18.5, 5.5), (18.5, 14.0)], false, stroke);
        }
        Icon::Text => {
            poly(&[(5.0, 8.0), (5.0, 5.5), (19.0, 5.5), (19.0, 8.0)], false, stroke);
            painter.line_segment([p(12.0, 5.5), p(12.0, 19.0)], stroke);
            painter.line_segment([p(8.5, 19.0), p(15.5, 19.0)], stroke);
        }
        Icon::Watermark => {
            painter.rect_stroke(
                Rect::from_min_max(p(3.5, 5.5), p(20.5, 18.5)),
                CornerRadius::same(s(2.0).round() as u8),
                thin,
                egui::StrokeKind::Middle,
            );
            painter.line_segment([p(6.5, 15.5), p(17.5, 8.5)], stroke);
            painter.line_segment([p(7.5, 10.0), p(11.5, 10.0)], thin);
            painter.line_segment([p(12.5, 14.5), p(16.5, 14.5)], thin);
        }
        Icon::Number => {
            painter.circle_stroke(p(12.0, 12.0), s(8.0), stroke);
            poly(&[(10.2, 9.6), (12.2, 7.8), (12.2, 16.2)], false, thin);
            painter.line_segment([p(9.8, 16.2), p(14.6, 16.2)], thin);
        }
        Icon::Mosaic => {
            painter.rect_stroke(
                Rect::from_min_max(p(4.0, 4.0), p(20.0, 20.0)),
                CornerRadius::same(s(2.0).round() as u8),
                thin,
                egui::StrokeKind::Middle,
            );
            let cell = 14.0 / 3.0;
            for (gx, gy) in [(0, 0), (2, 0), (1, 1), (0, 2), (2, 2)] {
                let x0 = 5.0 + gx as f32 * cell;
                let y0 = 5.0 + gy as f32 * cell;
                painter.rect_filled(
                    Rect::from_min_max(p(x0, y0), p(x0 + cell, y0 + cell)),
                    CornerRadius::ZERO,
                    color,
                );
            }
        }
        Icon::Highlighter => {
            poly(&[(4.0, 8.5), (4.0, 4.0), (8.5, 4.0)], false, thin);
            poly(&[(15.5, 4.0), (20.0, 4.0), (20.0, 8.5)], false, thin);
            poly(&[(20.0, 15.5), (20.0, 20.0), (15.5, 20.0)], false, thin);
            poly(&[(8.5, 20.0), (4.0, 20.0), (4.0, 15.5)], false, thin);
            painter.rect_filled(
                Rect::from_min_max(p(8.2, 8.2), p(15.8, 15.8)),
                CornerRadius::same(s(1.6).round() as u8),
                color,
            );
        }
        Icon::Eraser => {
            poly(
                &[
                    (14.0, 4.5),
                    (19.8, 10.3),
                    (11.3, 18.8),
                    (6.5, 18.8),
                    (4.2, 16.5),
                ],
                true,
                thin,
            );
            painter.line_segment([p(9.2, 9.3), p(15.0, 15.1)], thin);
            painter.line_segment([p(11.3, 18.8), p(20.0, 18.8)], stroke);
        }
        Icon::Layers => {
            poly(
                &[(12.0, 4.0), (20.0, 8.2), (12.0, 12.4), (4.0, 8.2)],
                true,
                thin,
            );
            poly(&[(4.0, 12.2), (12.0, 16.4), (20.0, 12.2)], false, thin);
            poly(&[(4.0, 16.0), (12.0, 20.2), (20.0, 16.0)], false, thin);
        }
        Icon::Undo => {
            poly(&[(9.2, 13.0), (4.5, 8.5), (9.2, 4.0)], false, stroke);
            let mut arc_pts = vec![p(4.8, 8.5), p(14.5, 8.5)];
            for i in 1..=8 {
                let t = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (i as f32 / 8.0);
                arc_pts.push(p(14.5 + 4.8 * t.cos(), 13.3 + 4.8 * t.sin()));
            }
            arc_pts.push(p(9.5, 18.1));
            painter.add(egui::Shape::line(arc_pts, stroke));
        }
        Icon::RecordGif => {
            painter.rect_stroke(
                Rect::from_min_max(p(3.2, 6.5), p(15.2, 17.5)),
                CornerRadius::same(s(2.4).round() as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
            poly(
                &[(15.4, 10.2), (20.8, 7.2), (20.8, 16.8), (15.4, 13.8)],
                true,
                thin,
            );
            painter.circle_filled(p(7.6, 12.0), s(1.7), color);
        }
        Icon::LongCapture => {
            painter.line_segment([p(7.0, 4.0), p(17.0, 4.0)], thin);
            painter.line_segment([p(5.5, 7.0), p(18.5, 7.0)], thin);
            painter.rect_stroke(
                Rect::from_min_max(p(4.0, 10.0), p(20.0, 20.0)),
                CornerRadius::same(s(2.2).round() as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.line_segment([p(12.0, 12.2), p(12.0, 17.6)], thin);
            poly(&[(9.5, 15.2), (12.0, 17.7), (14.5, 15.2)], false, thin);
        }
        Icon::Pin => {
            painter.line_segment([p(14.2, 4.2), p(19.8, 9.8)], stroke);
            painter.line_segment([p(15.6, 5.6), p(10.8, 10.4)], thin);
            painter.line_segment([p(18.4, 8.4), p(13.6, 13.2)], thin);
            painter.line_segment([p(8.0, 9.6), p(14.4, 16.0)], stroke);
            painter.line_segment([p(11.0, 13.0), p(4.5, 19.5)], stroke);
        }
        Icon::Save => {
            painter.line_segment([p(12.0, 4.5), p(12.0, 15.0)], stroke);
            poly(&[(7.8, 11.0), (12.0, 15.2), (16.2, 11.0)], false, stroke);
            poly(
                &[(4.2, 15.2), (4.2, 19.5), (19.8, 19.5), (19.8, 15.2)],
                false,
                stroke,
            );
        }
        Icon::Copy => {
            painter.rect_stroke(
                Rect::from_min_max(p(8.5, 8.5), p(19.5, 19.5)),
                CornerRadius::same(s(2.0).round() as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
            poly(
                &[
                    (15.5, 8.5),
                    (15.5, 4.5),
                    (4.5, 4.5),
                    (4.5, 15.5),
                    (8.5, 15.5),
                ],
                false,
                stroke,
            );
        }
        Icon::Check => {
            poly(&[(4.8, 12.5), (9.8, 17.5), (19.5, 6.5)], false, stroke);
        }
        Icon::Close => {
            painter.line_segment([p(6.0, 6.0), p(18.0, 18.0)], stroke);
            painter.line_segment([p(18.0, 6.0), p(6.0, 18.0)], stroke);
        }
        Icon::Reselect => {
            let mut pts = Vec::with_capacity(14);
            let start = -2.35_f32;
            let end = 2.65_f32;
            for i in 0..=12 {
                let a = start + (end - start) * (i as f32 / 12.0);
                pts.push(p(12.0 + 7.2 * a.cos(), 12.0 + 7.2 * a.sin()));
            }
            painter.add(egui::Shape::line(pts, stroke));
            poly(&[(4.5, 4.8), (6.2, 9.2), (10.6, 8.0)], false, stroke);
        }
        Icon::Ocr => {
            poly(&[(4.0, 8.5), (4.0, 4.0), (8.5, 4.0)], false, thin);
            poly(&[(15.5, 4.0), (20.0, 4.0), (20.0, 8.5)], false, thin);
            poly(&[(20.0, 15.5), (20.0, 20.0), (15.5, 20.0)], false, thin);
            poly(&[(8.5, 20.0), (4.0, 20.0), (4.0, 15.5)], false, thin);
            painter.line_segment([p(7.8, 9.0), p(16.2, 9.0)], thin);
            painter.line_segment([p(7.8, 12.2), p(16.2, 12.2)], thin);
            painter.line_segment([p(7.8, 15.4), p(13.2, 15.4)], thin);
        }
        Icon::Translate => {
            // Left CJK glyph strokes
            painter.line_segment([p(4.0, 7.2), p(12.2, 7.2)], thin);
            painter.line_segment([p(8.1, 4.5), p(8.1, 7.2)], thin);
            painter.line_segment([p(10.6, 7.2), p(5.0, 14.0)], thin);
            painter.line_segment([p(6.0, 8.8), p(11.4, 14.0)], thin);
            // Bottom-right "A" glyph strokes
            poly(&[(12.5, 19.5), (16.2, 11.0), (20.0, 19.5)], false, thin);
            painter.line_segment([p(13.9, 16.8), p(18.6, 16.8)], thin);
        }
        Icon::ZoomIn => {
            painter.line_segment([p(12.0, 5.5), p(12.0, 18.5)], stroke);
            painter.line_segment([p(5.5, 12.0), p(18.5, 12.0)], stroke);
        }
        Icon::ZoomOut => {
            painter.line_segment([p(5.5, 12.0), p(18.5, 12.0)], stroke);
        }
        Icon::Passthrough => {
            // Dashed frame + pass-through arrow (click-through indicator)
            painter.line_segment([p(4.5, 5.5), p(10.5, 5.5)], thin);
            painter.line_segment([p(14.5, 5.5), p(19.5, 5.5)], thin);
            painter.line_segment([p(19.5, 5.5), p(19.5, 11.0)], thin);
            painter.line_segment([p(19.5, 14.5), p(19.5, 18.5)], thin);
            painter.line_segment([p(4.5, 18.5), p(10.0, 18.5)], thin);
            painter.line_segment([p(14.0, 18.5), p(19.5, 18.5)], thin);
            painter.line_segment([p(4.5, 5.5), p(4.5, 10.5)], thin);
            painter.line_segment([p(4.5, 14.0), p(4.5, 18.5)], thin);
            painter.line_segment([p(7.5, 16.5), p(16.5, 7.5)], stroke);
            poly(&[(11.8, 7.5), (16.5, 7.5), (16.5, 12.2)], false, thin);
        }
        Icon::Play => {
            painter.add(egui::Shape::convex_polygon(
                vec![p(7.5, 5.0), p(19.0, 12.0), p(7.5, 19.0)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Stop => {
            painter.rect_filled(
                Rect::from_min_max(p(6.0, 6.0), p(18.0, 18.0)),
                CornerRadius::same(s(2.0).round() as u8),
                color,
            );
        }
        Icon::Back => {
            painter.line_segment([p(19.0, 12.0), p(5.5, 12.0)], stroke);
            poly(&[(11.0, 6.5), (5.5, 12.0), (11.0, 17.5)], false, stroke);
        }
        Icon::ScrollManual => {
            painter.line_segment([p(12.0, 4.8), p(12.0, 19.2)], stroke);
            poly(&[(8.2, 8.5), (12.0, 4.8), (15.8, 8.5)], false, stroke);
            poly(&[(8.2, 15.5), (12.0, 19.2), (15.8, 15.5)], false, stroke);
        }
        Icon::Folder => {
            poly(
                &[
                    (3.5, 6.5),
                    (9.5, 6.5),
                    (11.5, 9.0),
                    (20.5, 9.0),
                    (20.5, 18.5),
                    (3.5, 18.5),
                ],
                true,
                thin,
            );
        }
        Icon::Upload => {
            painter.line_segment([p(12.0, 15.0), p(12.0, 4.5)], stroke);
            poly(&[(7.8, 8.5), (12.0, 4.3), (16.2, 8.5)], false, stroke);
            poly(
                &[(4.2, 15.2), (4.2, 19.5), (19.8, 19.5), (19.8, 15.2)],
                false,
                stroke,
            );
        }
        Icon::Trash => {
            painter.line_segment([p(4.5, 7.2), p(19.5, 7.2)], stroke);
            poly(&[(9.2, 7.2), (9.2, 4.5), (14.8, 4.5), (14.8, 7.2)], false, thin);
            poly(&[(6.5, 7.2), (7.5, 19.5), (16.5, 19.5), (17.5, 7.2)], false, thin);
            painter.line_segment([p(10.2, 10.5), p(10.2, 16.5)], thin);
            painter.line_segment([p(13.8, 10.5), p(13.8, 16.5)], thin);
        }
        Icon::Alert => {
            poly(&[(12.0, 4.2), (20.2, 18.8), (3.8, 18.8)], true, thin);
            painter.line_segment([p(12.0, 9.5), p(12.0, 13.8)], stroke);
            painter.circle_filled(p(12.0, 16.3), s(1.1), color);
        }
        Icon::ChevronDown => {
            poly(&[(6.5, 9.2), (12.0, 14.8), (17.5, 9.2)], false, stroke);
        }
        Icon::ChevronUp => {
            poly(&[(6.5, 14.8), (12.0, 9.2), (17.5, 14.8)], false, stroke);
        }
        Icon::Eye => {
            poly(
                &[
                    (3.5, 12.0),
                    (7.2, 7.5),
                    (12.0, 6.2),
                    (16.8, 7.5),
                    (20.5, 12.0),
                    (16.8, 16.5),
                    (12.0, 17.8),
                    (7.2, 16.5),
                ],
                true,
                thin,
            );
            painter.circle_stroke(p(12.0, 12.0), s(2.7), thin);
        }
        Icon::EyeOff => {
            poly(
                &[(4.5, 12.0), (8.2, 8.2), (12.0, 7.2), (15.8, 8.2), (19.5, 12.0)],
                false,
                thin,
            );
            poly(
                &[(4.5, 12.0), (8.2, 15.8), (12.0, 16.8), (15.8, 15.8), (19.5, 12.0)],
                false,
                thin,
            );
            painter.line_segment([p(4.5, 4.5), p(19.5, 19.5)], stroke);
        }
        Icon::TextPanel => {
            // Side plain-text panel icon: split frame with text lines on the right pane.
            painter.rect_stroke(
                Rect::from_min_max(p(3.5, 4.5), p(20.5, 19.5)),
                CornerRadius::same(s(2.0).round() as u8),
                thin,
                egui::StrokeKind::Middle,
            );
            painter.line_segment([p(10.5, 4.5), p(10.5, 19.5)], thin);
            painter.line_segment([p(13.0, 9.0), p(18.0, 9.0)], thin);
            painter.line_segment([p(13.0, 12.2), p(18.0, 12.2)], thin);
            painter.line_segment([p(13.0, 15.4), p(16.5, 15.4)], thin);
        }
        Icon::SettingsGeneral => {
            painter.line_segment([p(4.0, 7.0), p(20.0, 7.0)], thin);
            painter.line_segment([p(4.0, 12.0), p(20.0, 12.0)], thin);
            painter.line_segment([p(4.0, 17.0), p(20.0, 17.0)], thin);
            painter.circle_filled(p(9.0, 7.0), s(2.0), color);
            painter.circle_filled(p(15.5, 12.0), s(2.0), color);
            painter.circle_filled(p(10.5, 17.0), s(2.0), color);
        }
        Icon::Keyboard => {
            painter.rect_stroke(
                Rect::from_min_max(p(3.2, 6.2), p(20.8, 17.8)),
                CornerRadius::same(s(2.4).round() as u8),
                thin,
                egui::StrokeKind::Middle,
            );
            for x in [6.8, 10.2, 13.8, 17.2] {
                painter.circle_filled(p(x, 10.0), s(1.0), color);
            }
            painter.line_segment([p(8.0, 14.4), p(16.0, 14.4)], stroke);
        }
        Icon::Cloud => {
            poly(
                &[
                    (7.0, 17.5),
                    (17.5, 17.5),
                    (20.0, 15.5),
                    (19.8, 12.5),
                    (17.2, 11.2),
                    (15.6, 7.6),
                    (11.6, 6.8),
                    (8.4, 9.2),
                    (7.4, 11.2),
                    (4.5, 12.4),
                    (4.3, 15.5),
                ],
                true,
                thin,
            );
        }
        Icon::Swap => {
            painter.line_segment([p(4.5, 8.2), p(19.5, 8.2)], stroke);
            poly(&[(15.8, 4.8), (19.5, 8.2), (15.8, 11.6)], false, stroke);
            painter.line_segment([p(19.5, 15.8), p(4.5, 15.8)], stroke);
            poly(&[(8.2, 12.4), (4.5, 15.8), (8.2, 19.2)], false, stroke);
        }
        Icon::Shield => {
            poly(
                &[
                    (12.0, 3.8),
                    (19.2, 6.8),
                    (19.2, 12.2),
                    (16.5, 17.2),
                    (12.0, 20.2),
                    (7.5, 17.2),
                    (4.8, 12.2),
                    (4.8, 6.8),
                ],
                true,
                thin,
            );
            poly(&[(9.0, 12.2), (11.2, 14.4), (15.4, 9.8)], false, thin);
        }
        Icon::LogIn => {
            poly(&[(13.5, 4.5), (19.2, 4.5), (19.2, 19.5), (13.5, 19.5)], false, stroke);
            painter.line_segment([p(4.5, 12.0), p(14.5, 12.0)], stroke);
            poly(&[(10.8, 8.3), (14.5, 12.0), (10.8, 15.7)], false, stroke);
        }
        Icon::LogOut => {
            poly(&[(10.5, 4.5), (4.8, 4.5), (4.8, 19.5), (10.5, 19.5)], false, stroke);
            painter.line_segment([p(9.5, 12.0), p(19.5, 12.0)], stroke);
            poly(&[(15.8, 8.3), (19.5, 12.0), (15.8, 15.7)], false, stroke);
        }
        Icon::UserPlus => {
            painter.circle_stroke(p(9.5, 8.0), s(3.2), thin);
            poly(
                &[
                    (3.8, 19.2),
                    (4.6, 15.6),
                    (7.2, 14.0),
                    (11.8, 14.0),
                    (14.4, 15.6),
                    (15.2, 19.2),
                ],
                false,
                thin,
            );
            painter.line_segment([p(16.0, 11.0), p(21.0, 11.0)], thin);
            painter.line_segment([p(18.5, 8.5), p(18.5, 13.5)], thin);
        }
    }
}

fn button_colors(
    ui: &Ui,
    hovered: bool,
    active: bool,
    primary: bool,
    enabled: bool,
    danger: bool,
) -> (Color32, Stroke, Color32) {
    let visuals = ui.visuals();
    if !enabled {
        // `ui.disable()` / `add_enabled_ui(false, ...)` already tints all painted shapes toward
        // `visuals.fade_out_to_color()`. Keep disabled colors opaque (no `gamma_multiply`) so
        // `ecolor::tint_color_towards` performs a single clean fade instead of producing
        // super-luminous premultiplied colors (`RGB > A`) that render as a blank white block.
        return (
            visuals.widgets.inactive.bg_fill,
            visuals.widgets.inactive.bg_stroke,
            visuals.text_color(),
        );
    }
    if primary || active {
        let palette = ui::ctx_palette(ui.ctx());
        let fill = if hovered {
            palette.accent_hover
        } else {
            palette.accent
        };
        return (fill, Stroke::new(1.0_f32, palette.accent_hover), Color32::WHITE);
    }
    if danger {
        let fill = if hovered {
            ui::DANGER.gamma_multiply(0.18)
        } else {
            visuals.widgets.inactive.bg_fill
        };
        let border = if hovered {
            Stroke::new(1.0_f32, ui::DANGER)
        } else {
            visuals.widgets.inactive.bg_stroke
        };
        return (fill, border, ui::DANGER);
    }
    if hovered {
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
    }
}

/// Icon-only button (used in the floating capture toolbar).
/// When `has_submenu` is true, draws a tiny corner triangle indicating expandable options.
pub fn icon_button(
    ui: &mut Ui,
    icon: Icon,
    active: bool,
    primary: bool,
    has_submenu: bool,
) -> Response {
    let size = Vec2::splat(30.0);
    let enabled = ui.is_enabled();
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let (fill, stroke, fg) = button_colors(ui, resp.hovered(), active, primary, enabled, false);
        ui.painter()
            .rect(rect, CornerRadius::same(6), fill, stroke, egui::StrokeKind::Inside);
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(16.0));
        paint(ui.painter(), icon_rect, icon, fg);
        if has_submenu {
            // Minimal corner triangle indicating expandable sub-options.
            let c = if enabled { fg.gamma_multiply(0.8) } else { fg };
            let p1 = Pos2::new(rect.max.x - 3.5, rect.max.y - 7.0);
            let p2 = Pos2::new(rect.max.x - 3.5, rect.max.y - 3.5);
            let p3 = Pos2::new(rect.max.x - 7.0, rect.max.y - 3.5);
            ui.painter()
                .add(egui::Shape::convex_polygon(vec![p1, p2, p3], c, Stroke::NONE));
        }
    }
    resp
}

/// Button with icon and label (optional trailing chevron).
pub fn icon_text_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    active: bool,
    primary: bool,
    trailing: Option<Icon>,
) -> Response {
    icon_text_button_styled(ui, icon, label, active, primary, false, trailing)
}

/// Danger-styled button with icon and label.
pub fn icon_text_button_danger(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    icon_text_button_styled(ui, icon, label, false, false, true, None)
}

fn icon_text_button_styled(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    active: bool,
    primary: bool,
    danger: bool,
    trailing: Option<Icon>,
) -> Response {
    let enabled = ui.is_enabled();
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let icon_size = 15.0_f32;
    let trail_size = if trailing.is_some() { 11.0_f32 } else { 0.0 };
    let gap = 5.0_f32;
    let pad_x = 9.0_f32;
    let pad_y = 5.0_f32;

    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Color32::WHITE);
    let total_w = pad_x * 2.0
        + icon_size
        + if label.is_empty() { 0.0 } else { gap + galley.size().x }
        + if trailing.is_some() { 4.0 + trail_size } else { 0.0 };
    let total_h = (galley.size().y.max(icon_size) + pad_y * 2.0).max(28.0);

    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, total_h), sense);
    if ui.is_rect_visible(rect) {
        let (fill, stroke, fg) = button_colors(ui, resp.hovered(), active, primary, enabled, danger);
        ui.painter()
            .rect(rect, CornerRadius::same(7), fill, stroke, egui::StrokeKind::Inside);

        let mut x = rect.min.x + pad_x;
        let icon_rect = Rect::from_center_size(
            Pos2::new(x + icon_size * 0.5, rect.center().y),
            Vec2::splat(icon_size),
        );
        paint(ui.painter(), icon_rect, icon, fg);
        x += icon_size;

        if !label.is_empty() {
            x += gap;
            let text_pos = Pos2::new(x, rect.center().y - galley.size().y * 0.5);
            ui.painter()
                .galley_with_override_text_color(text_pos, galley, fg);
            x = rect.max.x - pad_x - trail_size;
        }

        if let Some(trail_icon) = trailing {
            let trail_rect = Rect::from_center_size(
                Pos2::new(x + trail_size * 0.5, rect.center().y),
                Vec2::splat(trail_size),
            );
            paint(ui.painter(), trail_rect, trail_icon, fg);
        }
    }
    resp
}

/// Sidebar navigation button with icon and label (settings window).
pub fn icon_nav_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    active: bool,
    width: f32,
) -> Response {
    let size = Vec2::new(width, 32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let (fill, fg) = if active {
            (ui::ctx_palette(ui.ctx()).accent, Color32::WHITE)
        } else if resp.hovered() {
            (visuals.widgets.hovered.bg_fill, visuals.strong_text_color())
        } else {
            (Color32::TRANSPARENT, visuals.text_color())
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), fill);

        let icon_rect = Rect::from_center_size(
            Pos2::new(rect.min.x + 17.0, rect.center().y),
            Vec2::splat(15.0),
        );
        paint(ui.painter(), icon_rect, icon, fg);

        let font_id = egui::TextStyle::Body.resolve(ui.style());
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_string(), font_id, fg);
        let text_pos = Pos2::new(rect.min.x + 31.0, rect.center().y - galley.size().y * 0.5);
        ui.painter().galley(text_pos, galley, fg);
    }
    resp
}

/// Compact icon button for dark floating bars (pin hover bar, recorder bar).
#[allow(dead_code)]
pub fn dark_icon_button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    dark_icon_button_active(ui, icon, tip, false)
}

/// Compact icon button for dark floating bars with optional active accent highlight.
#[allow(dead_code)]
pub fn dark_icon_button_active(ui: &mut Ui, icon: Icon, tip: &str, active: bool) -> Response {
    let size = Vec2::splat(26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let palette = ui::ctx_palette(ui.ctx());
        let fill = if active {
            if resp.hovered() {
                palette.accent_hover
            } else {
                palette.accent
            }
        } else if resp.hovered() {
            Color32::from_rgba_unmultiplied(255, 255, 255, 48)
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 20)
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(5), fill);
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(14.5));
        paint(ui.painter(), icon_rect, icon, Color32::WHITE);
    }
    resp.on_hover_text(tip)
}

/// Icon+text button for dark floating bars (recorder control bar).
pub fn dark_icon_text_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    fill: Color32,
    hover_fill: Color32,
) -> Response {
    let enabled = ui.is_enabled();
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let icon_size = 14.0_f32;
    let gap = 5.0_f32;
    let pad_x = 9.0_f32;
    let pad_y = 5.0_f32;
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Color32::WHITE);
    let total_w = pad_x * 2.0 + icon_size + gap + galley.size().x;
    let total_h = (galley.size().y.max(icon_size) + pad_y * 2.0).max(26.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, total_h), sense);
    if ui.is_rect_visible(rect) {
        let bg = if enabled && resp.hovered() {
            hover_fill
        } else {
            fill
        };
        // When `!enabled`, `ui.disable()` already fades opaque colors toward `fade_out_to_color()`.
        let fg = Color32::WHITE;
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), bg);
        let icon_rect = Rect::from_center_size(
            Pos2::new(rect.min.x + pad_x + icon_size * 0.5, rect.center().y),
            Vec2::splat(icon_size),
        );
        paint(ui.painter(), icon_rect, icon, fg);
        let text_pos = Pos2::new(
            rect.min.x + pad_x + icon_size + gap,
            rect.center().y - galley.size().y * 0.5,
        );
        ui.painter()
            .galley_with_override_text_color(text_pos, galley, fg);
    }
    resp
}

/// Small icon-only button (panel close buttons, folder pickers).
pub fn small_icon_button(ui: &mut Ui, icon: Icon) -> Response {
    icon_button_sized(ui, icon, Vec2::splat(24.0), 13.5, false, false)
}

/// Custom-sized icon-only button (e.g. 28x28 swap button aligned with language pickers).
pub fn icon_button_sized(
    ui: &mut Ui,
    icon: Icon,
    size: Vec2,
    icon_size: f32,
    active: bool,
    primary: bool,
) -> Response {
    let enabled = ui.is_enabled();
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let (fill, stroke, fg) = button_colors(ui, resp.hovered(), active, primary, enabled, false);
        let radius = if size.y >= 28.0 { 7 } else { 5 };
        ui.painter()
            .rect(rect, CornerRadius::same(radius), fill, stroke, egui::StrokeKind::Inside);
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(icon_size));
        paint(ui.painter(), icon_rect, icon, fg);
    }
    resp
}

/// Compact button with icon and label (for inline status banners, list row actions, and narrow tabs).
pub fn small_icon_text_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    active: bool,
    primary: bool,
) -> Response {
    let enabled = ui.is_enabled();
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let icon_size = 13.0_f32;
    let gap = 4.0_f32;
    let pad_x = 7.0_f32;
    let pad_y = 3.0_f32;

    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Color32::WHITE);
    let total_w = pad_x * 2.0
        + icon_size
        + if label.is_empty() { 0.0 } else { gap + galley.size().x };
    let total_h = (galley.size().y.max(icon_size) + pad_y * 2.0).max(22.0);

    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, total_h), sense);
    if ui.is_rect_visible(rect) {
        let (fill, stroke, fg) = button_colors(ui, resp.hovered(), active, primary, enabled, false);
        ui.painter()
            .rect(rect, CornerRadius::same(6), fill, stroke, egui::StrokeKind::Inside);

        let x = rect.min.x + pad_x;
        let icon_rect = Rect::from_center_size(
            Pos2::new(x + icon_size * 0.5, rect.center().y),
            Vec2::splat(icon_size),
        );
        paint(ui.painter(), icon_rect, icon, fg);

        if !label.is_empty() {
            let text_pos = Pos2::new(x + icon_size + gap, rect.center().y - galley.size().y * 0.5);
            ui.painter()
                .galley_with_override_text_color(text_pos, galley, fg);
        }
    }
    resp
}

/// Paint an inline vector icon directly in layout (brand badge, toast icon).
pub fn paint_inline(ui: &mut Ui, icon: Icon, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint(ui.painter(), rect, icon, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ICONS: &[Icon] = &[
        Icon::Brand,
        Icon::Select,
        Icon::Rect,
        Icon::Line,
        Icon::Ellipse,
        Icon::Pen,
        Icon::Arrow,
        Icon::Text,
        Icon::Watermark,
        Icon::Number,
        Icon::Mosaic,
        Icon::Highlighter,
        Icon::Eraser,
        Icon::Layers,
        Icon::Undo,
        Icon::RecordGif,
        Icon::LongCapture,
        Icon::Pin,
        Icon::Save,
        Icon::Copy,
        Icon::Check,
        Icon::Close,
        Icon::Reselect,
        Icon::Ocr,
        Icon::Translate,
        Icon::ZoomIn,
        Icon::ZoomOut,
        Icon::Passthrough,
        Icon::Play,
        Icon::Stop,
        Icon::Back,
        Icon::ScrollManual,
        Icon::Folder,
        Icon::Upload,
        Icon::Trash,
        Icon::Alert,
        Icon::ChevronDown,
        Icon::ChevronUp,
        Icon::Eye,
        Icon::EyeOff,
        Icon::TextPanel,
        Icon::SettingsGeneral,
        Icon::Keyboard,
        Icon::Cloud,
        Icon::Swap,
        Icon::Shield,
        Icon::LogIn,
        Icon::LogOut,
        Icon::UserPlus,
    ];

    #[test]
    fn all_icons_render_shapes_without_panicking() {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for &icon in ALL_ICONS {
                        let _ = icon_button(ui, icon, false, false, false);
                    }
                });
            });
        });
        assert!(!out.shapes.is_empty());
    }

    fn assert_valid_premultiplied(color: Color32, context: &str) {
        assert!(
            color.r() <= color.a() && color.g() <= color.a() && color.b() <= color.a(),
            "super-luminous premultiplied color {:?} in {}",
            color,
            context
        );
    }

    fn check_shape_colors(shape: &egui::Shape, context: &str) {
        match shape {
            egui::Shape::Rect(r) => {
                assert_valid_premultiplied(r.fill, context);
                assert_valid_premultiplied(r.stroke.color, context);
            }
            egui::Shape::LineSegment { stroke, .. } => {
                assert_valid_premultiplied(stroke.color, context);
            }
            egui::Shape::Path(p) => {
                assert_valid_premultiplied(p.fill, context);
                if let egui::epaint::ColorMode::Solid(c) = p.stroke.color {
                    assert_valid_premultiplied(c, context);
                }
            }
            egui::Shape::Circle(c) => {
                assert_valid_premultiplied(c.fill, context);
                assert_valid_premultiplied(c.stroke.color, context);
            }
            egui::Shape::Text(t) => {
                if let Some(c) = t.override_text_color {
                    assert_valid_premultiplied(c, context);
                }
            }
            egui::Shape::Vec(shapes) => {
                for s in shapes {
                    check_shape_colors(s, context);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn disabled_buttons_stay_visible_without_white_blowout() {
        for &accent in ui::ThemeAccent::ALL {
            for dark in [false, true] {
                let ctx = egui::Context::default();
                ui::apply_theme(&ctx, dark, accent, ui::TextColorSetting::Auto);
                let out = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.add_enabled_ui(false, |ui| {
                            let _ = icon_text_button(ui, Icon::Save, "Save", false, false, None);
                            let _ = small_icon_text_button(ui, Icon::Check, "Use", false, false);
                            let _ = icon_button(ui, Icon::Undo, false, false, true);
                            let _ = icon_button_sized(
                                ui,
                                Icon::Swap,
                                Vec2::splat(28.0),
                                15.0,
                                false,
                                false,
                            );
                            let _ = small_icon_button(ui, Icon::Close);
                            if dark {
                                let _ = dark_icon_text_button(
                                    ui,
                                    Icon::Stop,
                                    "Done",
                                    accent.palette().accent,
                                    accent.palette().accent_hover,
                                );
                            }
                        });
                    });
                });

                let mut saw_text = false;
                let mut saw_button_rect = false;
                for clipped in &out.shapes {
                    check_shape_colors(&clipped.shape, "disabled_buttons");
                    match &clipped.shape {
                        egui::Shape::Rect(r) if r.corner_radius != CornerRadius::ZERO => {
                            saw_button_rect = true;
                            if !dark {
                                assert_ne!(
                                    r.fill,
                                    Color32::WHITE,
                                    "disabled button fill blew out to solid white in light mode"
                                );
                            }
                        }
                        egui::Shape::Text(t) => {
                            saw_text = true;
                            if let Some(fg) = t.override_text_color {
                                if !dark {
                                    // In light mode, disabled text must stay visibly darker than white.
                                    assert!(
                                        fg.r() < 200 && fg.g() < 200 && fg.b() < 200,
                                        "disabled text washed out in light mode: {:?}",
                                        fg
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
                assert!(saw_button_rect, "expected disabled button rect shape");
                assert!(saw_text, "expected disabled button text shape");
            }
        }
    }
}

/// Dropdown menu button with icon and label (used for the editor's Text menu).
pub fn icon_menu_button(
    ui: &mut Ui,
    id_salt: &str,
    icon: Icon,
    label: &str,
    add_contents: impl FnOnce(&mut Ui),
) {
    let popup_id = ui.make_persistent_id(Id::new(id_salt));
    let is_open = ui.memory(|m| m.is_popup_open(popup_id));
    let resp = icon_text_button(
        ui,
        icon,
        label,
        is_open,
        false,
        Some(if is_open {
            Icon::ChevronUp
        } else {
            Icon::ChevronDown
        }),
    );
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    egui::popup::popup_below_widget(ui, popup_id, &resp, PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(136.0);
        add_contents(ui);
    });
}
