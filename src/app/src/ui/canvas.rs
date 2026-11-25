//! Shared annotation editor and floating toolbar (reused by both the editor window and capture overlay).

use crate::annotations::{
    self, Point, Stroke, Tool, ToolOptions, FONTS, PRESET_COLORS,
};
use crate::i18n;
use crate::images::Rgba;
use crate::service;
use crate::state::App;
use crate::ui;
use crate::ui::icons::{self, Icon};
use egui::{Color32, CornerRadius, Pos2, Rect, RichText, Sense, TextureHandle, TextureOptions, Vec2};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolbarAction {
    Confirm,
    Ocr,
    Gif,
    Long,
    Pin,
    Save,
    Close,
    Reselect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolGroup {
    Geometry,
    Arrow,
    Text,
    Mosaic,
    Eraser,
}

pub fn tool_icon(tool: Tool) -> Icon {
    match tool {
        Tool::Select => Icon::Select,
        Tool::Rect => Icon::Rect,
        Tool::Line => Icon::Line,
        Tool::Ellipse => Icon::Ellipse,
        Tool::Pen => Icon::Pen,
        Tool::Arrow => Icon::Arrow,
        Tool::Text => Icon::Text,
        Tool::Watermark => Icon::Watermark,
        Tool::Number => Icon::Number,
        Tool::Mosaic => Icon::Mosaic,
        Tool::Highlighter => Icon::Highlighter,
        Tool::Eraser => Icon::Eraser,
    }
}

impl ToolGroup {
    fn label(self) -> &'static str {
        let key = match self {
            ToolGroup::Geometry => "Shapes",
            ToolGroup::Arrow => "Arrow",
            ToolGroup::Text => "Text",
            ToolGroup::Mosaic => "Mosaic",
            ToolGroup::Eraser => "Eraser",
        };
        i18n::t(key)
    }

    fn tooltip(self) -> &'static str {
        let key = match self {
            ToolGroup::Geometry => "Shapes (Rectangle / Line / Ellipse / Pen)",
            ToolGroup::Arrow => "Arrow",
            ToolGroup::Text => "Text (Text / Watermark / Step Number)",
            ToolGroup::Mosaic => "Mosaic / Spotlight",
            ToolGroup::Eraser => "Eraser",
        };
        i18n::t(key)
    }

    fn icon(self, active_tool: Tool) -> Icon {
        if self.contains(active_tool) {
            return tool_icon(active_tool);
        }
        match self {
            ToolGroup::Geometry => Icon::Rect,
            ToolGroup::Arrow => Icon::Arrow,
            ToolGroup::Text => Icon::Text,
            ToolGroup::Mosaic => Icon::Mosaic,
            ToolGroup::Eraser => Icon::Eraser,
        }
    }

    fn options(self) -> &'static [(Tool, &'static str)] {
        match self {
            ToolGroup::Geometry => &[
                (Tool::Rect, "Rectangle"),
                (Tool::Line, "Line"),
                (Tool::Ellipse, "Ellipse"),
                (Tool::Pen, "Pen"),
            ],
            ToolGroup::Arrow => &[(Tool::Arrow, "Arrow")],
            ToolGroup::Text => &[
                (Tool::Text, "Text"),
                (Tool::Watermark, "Watermark"),
                (Tool::Number, "Number"),
            ],
            ToolGroup::Mosaic => &[(Tool::Mosaic, "Mosaic"), (Tool::Highlighter, "Spotlight")],
            ToolGroup::Eraser => &[(Tool::Eraser, "Eraser")],
        }
    }

    fn contains(self, tool: Tool) -> bool {
        self.options().iter().any(|(t, _)| *t == tool)
    }

    fn of_tool(tool: Tool) -> Option<ToolGroup> {
        [
            ToolGroup::Geometry,
            ToolGroup::Arrow,
            ToolGroup::Text,
            ToolGroup::Mosaic,
            ToolGroup::Eraser,
        ]
        .into_iter()
        .find(|g| g.contains(tool))
    }
}

#[derive(Clone, Debug)]
pub struct EditingText {
    pub stroke: Stroke,
    pub existing: bool,
    pub drag_mode: Option<TextDragMode>,
}

#[derive(Clone, Copy, Debug)]
pub enum TextDragMode {
    Move { start_img: Point, orig_pos: Point },
    Scale { center_img: Point, orig_dist: f32, orig_scale: f32 },
    Rotate { center_img: Point, start_angle: f32, orig_rot: f32 },
}

pub struct AnnotationState {
    pub tool: Tool,
    open_group: Option<ToolGroup>,
    pub strokes: Vec<Stroke>,
    pub draft: Option<Stroke>,
    pub editing: Option<EditingText>,
    pub per_tool: HashMap<Tool, ToolOptions>,
    pub dirty: bool,
    pub texture_dirty: bool,
    pub preview_tex: Option<TextureHandle>,
    pub last_base_dims: (u32, u32),
}

impl AnnotationState {
    pub fn new(app: &App) -> Self {
        let settings = app.core.lock().unwrap().settings.clone();
        let mut per_tool = HashMap::new();
        for tool in [
            Tool::Rect,
            Tool::Line,
            Tool::Ellipse,
            Tool::Arrow,
            Tool::Pen,
            Tool::Highlighter,
            Tool::Text,
            Tool::Watermark,
            Tool::Number,
            Tool::Mosaic,
            Tool::Eraser,
        ] {
            per_tool.insert(tool, ToolOptions::from_settings(&settings, tool));
        }
        Self {
            tool: Tool::Select,
            open_group: None,
            strokes: Vec::new(),
            draft: None,
            editing: None,
            per_tool,
            dirty: false,
            texture_dirty: true,
            preview_tex: None,
            last_base_dims: (0, 0),
        }
    }

    pub fn reset(&mut self) {
        self.strokes.clear();
        self.draft = None;
        self.editing = None;
        self.dirty = false;
        self.texture_dirty = true;
    }

    pub fn load(&mut self, strokes: Vec<Stroke>) {
        self.strokes = strokes;
        self.draft = None;
        self.editing = None;
        self.dirty = false;
        self.texture_dirty = true;
    }

    pub fn current_options(&self) -> ToolOptions {
        if let Some(editing) = &self.editing {
            return ToolOptions {
                color: editing.stroke.color.clone(),
                width: editing.stroke.width,
                font_size: editing.stroke.effective_font_size(),
                font_family: editing
                    .stroke
                    .font_family
                    .clone()
                    .unwrap_or_else(|| "Microsoft YaHei".into()),
                opacity: editing.stroke.opacity.unwrap_or(100.0),
            };
        }
        self.per_tool
            .get(&self.tool)
            .cloned()
            .unwrap_or_default()
    }

    pub fn configure(&mut self, app: &Arc<App>, next: ToolOptions) {
        let target_tool = if let Some(editing) = self.editing.as_mut() {
            editing.stroke.color = next.color.clone();
            editing.stroke.width = next.width;
            editing.stroke.font_size = Some(next.font_size);
            editing.stroke.font_family = Some(next.font_family.clone());
            editing.stroke.opacity = Some(next.opacity);
            self.texture_dirty = true;
            self.dirty = true;
            editing.stroke.tool
        } else {
            self.tool
        };
        if target_tool != Tool::Select {
            self.per_tool.insert(target_tool, next.clone());
            service::update_annotation_preference(
                app,
                target_tool.as_str(),
                json!({
                    "color": next.color,
                    "width": next.width.round() as i64,
                    "fontSize": next.font_size.round() as i64,
                    "fontFamily": next.font_family,
                    "opacity": next.opacity.round() as i64,
                }),
            );
        }
    }

    pub fn finish_text(&mut self) {
        if let Some(editing) = self.editing.take() {
            let text = editing.stroke.text.as_deref().unwrap_or("").trim();
            if !text.is_empty() {
                if editing.existing {
                    if let Some(slot) = self
                        .strokes
                        .iter_mut()
                        .find(|s| s.id == editing.stroke.id)
                    {
                        *slot = editing.stroke;
                    } else {
                        self.strokes.push(editing.stroke);
                    }
                } else {
                    self.strokes.push(editing.stroke);
                }
                self.dirty = true;
            } else if editing.existing {
                self.strokes.retain(|s| s.id != editing.stroke.id);
                self.dirty = true;
            }
            self.texture_dirty = true;
        }
    }

    pub fn undo(&mut self) {
        if self.editing.is_some() {
            self.editing = None;
            self.texture_dirty = true;
            return;
        }
        if self.strokes.pop().is_some() {
            self.dirty = true;
            self.texture_dirty = true;
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.strokes.is_empty() || self.editing.is_some()
    }

    pub fn all_strokes_snapshot(&self) -> Vec<Stroke> {
        let mut all = self.strokes.clone();
        if let Some(editing) = &self.editing {
            if !editing.stroke.text.as_deref().unwrap_or("").trim().is_empty() {
                if let Some(slot) = all.iter_mut().find(|s| s.id == editing.stroke.id) {
                    *slot = editing.stroke.clone();
                } else {
                    all.push(editing.stroke.clone());
                }
            }
        }
        all
    }

    /// Render composite preview texture with dirty-flag caching to avoid redundant rasterization.
    pub fn ensure_preview_texture(&mut self, ctx: &egui::Context, base: &Rgba) -> Option<TextureHandle> {
        let dims = (base.width, base.height);
        if self.last_base_dims != dims {
            self.last_base_dims = dims;
            self.texture_dirty = true;
        }
        if self.texture_dirty || self.preview_tex.is_none() {
            let mut render_list = Vec::with_capacity(self.strokes.len() + 2);
            for s in &self.strokes {
                if self
                    .editing
                    .as_ref()
                    .is_some_and(|e| e.existing && e.stroke.id == s.id)
                {
                    continue;
                }
                render_list.push(s.clone());
            }
            if let Some(editing) = &self.editing {
                if !editing.stroke.text.as_deref().unwrap_or("").is_empty() {
                    render_list.push(editing.stroke.clone());
                }
            }
            if let Some(draft) = &self.draft {
                render_list.push(draft.clone());
            }
            let composed = annotations::rasterize_annotations(base, &render_list);
            let max_side = ctx.input(|i| i.max_texture_side);
            let color_img = ui::rgba_to_color_image_fit(&composed, max_side);
            if let Some(tex) = self.preview_tex.as_mut() {
                tex.set(color_img, TextureOptions::LINEAR);
            } else {
                self.preview_tex = Some(ctx.load_texture(
                    "annotation_preview",
                    color_img,
                    TextureOptions::LINEAR,
                ));
            }
            self.texture_dirty = false;
        }
        self.preview_tex.clone()
    }
}

/// Render annotation toolbar (used by the capture overlay; supports icon-only or icon+text mode plus options popup).
#[allow(clippy::too_many_arguments)]
pub fn show_annotation_toolbar(
    ui: &mut egui::Ui,
    app: &Arc<App>,
    state: &mut AnnotationState,
    disabled: bool,
    actions: &[(ToolbarAction, Icon, &str, &str)],
    show_reselect: bool,
    confirm: Option<(Icon, &str, &str)>,
    icon_only: bool,
) -> Option<ToolbarAction> {
    let mut triggered = None;
    ui.vertical(|ui| {
        ui.add_enabled_ui(!disabled, |ui| {
            ui.horizontal(|ui| {
                if icon_only {
                    ui.spacing_mut().item_spacing.x = 5.0;
                }
                show_tool_buttons(ui, state, icon_only);
                ui.separator();

                for (act, icon, label, tip) in actions {
                    let resp = if icon_only {
                        icons::icon_button(ui, *icon, false, false, false).on_hover_text(*tip)
                    } else {
                        icons::icon_text_button(ui, *icon, label, false, false, None)
                            .on_hover_text(*tip)
                    };
                    if resp.clicked() {
                        state.finish_text();
                        state.open_group = None;
                        triggered = Some(*act);
                    }
                }

                if show_reselect {
                    let resp = if icon_only {
                        icons::icon_button(ui, Icon::Reselect, false, false, false)
                            .on_hover_text(i18n::t("Reselect region"))
                    } else {
                        icons::icon_text_button(
                            ui,
                            Icon::Reselect,
                            i18n::t("Reselect"),
                            false,
                            false,
                            None,
                        )
                        .on_hover_text(i18n::t("Reselect region"))
                    };
                    if resp.clicked() {
                        state.finish_text();
                        state.open_group = None;
                        triggered = Some(ToolbarAction::Reselect);
                    }
                }

                if let Some((icon, label, tip)) = confirm {
                    let resp = if icon_only {
                        icons::icon_button(ui, icon, false, true, false).on_hover_text(tip)
                    } else {
                        icons::icon_text_button(ui, icon, label, false, true, None)
                            .on_hover_text(tip)
                    };
                    if resp.clicked() {
                        state.finish_text();
                        state.open_group = None;
                        triggered = Some(ToolbarAction::Confirm);
                    }
                }
            });
        });
        show_tool_options(ui, app, state);
    });
    triggered
}

/// Tool button row (Select, tool groups, Undo); layout and icon-only mode are controlled by the caller.
pub fn show_tool_buttons(ui: &mut egui::Ui, state: &mut AnnotationState, icon_only: bool) {
    let groups = [
        ToolGroup::Geometry,
        ToolGroup::Arrow,
        ToolGroup::Text,
        ToolGroup::Mosaic,
        ToolGroup::Eraser,
    ];
    let sel_active = state.tool == Tool::Select;
    let sel_resp = if icon_only {
        icons::icon_button(ui, Icon::Select, sel_active, false, false)
            .on_hover_text(i18n::t("Select / Move selection"))
    } else {
        icons::icon_text_button(ui, Icon::Select, i18n::t("Select"), sel_active, false, None)
            .on_hover_text(i18n::t("View / Move"))
    };
    if sel_resp.clicked() {
        state.finish_text();
        state.tool = Tool::Select;
        state.open_group = None;
    }

    for group in groups {
        let active = group.contains(state.tool);
        let expanded = state.open_group == Some(group);
        let group_icon = group.icon(state.tool);
        let resp = if icon_only {
            icons::icon_button(ui, group_icon, active, false, true).on_hover_text(group.tooltip())
        } else {
            icons::icon_text_button(
                ui,
                group_icon,
                group.label(),
                active,
                false,
                Some(if expanded {
                    Icon::ChevronUp
                } else {
                    Icon::ChevronDown
                }),
            )
            .on_hover_text(group.tooltip())
        };
        if resp.clicked() {
            if state.open_group == Some(group) {
                state.open_group = None;
            } else {
                state.finish_text();
                state.tool = group.options()[0].0;
                state.open_group = Some(group);
            }
        }
    }

    ui.add_enabled_ui(state.can_undo(), |ui| {
        let undo_resp = if icon_only {
            icons::icon_button(ui, Icon::Undo, false, false, false)
                .on_hover_text(i18n::t("Undo (Ctrl+Z)"))
        } else {
            icons::icon_text_button(ui, Icon::Undo, i18n::t("Undo"), false, false, None)
                .on_hover_text(i18n::t("Undo Ctrl+Z"))
        };
        if undo_resp.clicked() {
            state.undo();
        }
    });
}

/// Expanded tool options row (color, stroke width, font, opacity, etc.).
pub fn show_tool_options(ui: &mut egui::Ui, app: &Arc<App>, state: &mut AnnotationState) {
    {
        if let Some(group) = state.open_group {
            ui.add_space(4.0);
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(10, 6))
                .corner_radius(CornerRadius::same(8))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(group.label()).strong());

                        // Sub-tool picker.
                        let opts = group.options();
                        if opts.len() > 1 {
                            ui.separator();
                            for (tool, name_key) in opts {
                                if icons::icon_text_button(
                                    ui,
                                    tool_icon(*tool),
                                    i18n::t(name_key),
                                    state.tool == *tool,
                                    false,
                                    None,
                                )
                                .clicked()
                                {
                                    state.finish_text();
                                    state.tool = *tool;
                                }
                            }
                        }

                        let mut cur = state.current_options();
                        let mut changed = false;

                        // Color.
                        if !matches!(state.tool, Tool::Mosaic | Tool::Eraser | Tool::Highlighter) {
                            ui.separator();
                            ui.label(i18n::t("Color"));
                            let (r, g, b) = annotations::parse_hex_rgb(&cur.color);
                            let mut rgb = [r, g, b];
                            if ui.color_edit_button_srgb(&mut rgb).changed() {
                                cur.color = annotations::format_hex_rgb(rgb[0], rgb[1], rgb[2]);
                                changed = true;
                            }
                            for hex in PRESET_COLORS {
                                let (sr, sg, sb) = annotations::parse_hex_rgb(hex);
                                let swatch_color = Color32::from_rgb(sr, sg, sb);
                                let selected = cur.color.eq_ignore_ascii_case(hex);
                                let (rect, resp) =
                                    ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                                ui.painter().circle_filled(rect.center(), 7.5, swatch_color);
                                if selected {
                                    ui.painter().circle_stroke(
                                        rect.center(),
                                        8.5,
                                        egui::Stroke::new(2.0_f32, ui::ctx_palette(ui.ctx()).accent),
                                    );
                                } else {
                                    ui.painter().circle_stroke(
                                        rect.center(),
                                        7.5,
                                        egui::Stroke::new(1.0_f32, Color32::from_gray(140)),
                                    );
                                }
                                if resp.clicked() {
                                    cur.color = (*hex).to_string();
                                    changed = true;
                                }
                            }
                        }

                        // Text options vs stroke width.
                        if state.tool.is_text() {
                            ui.separator();
                            ui.label(i18n::t("Font"));
                            let cur_label = FONTS
                                .iter()
                                .find(|(v, _)| *v == cur.font_family)
                                .map(|(_, l)| i18n::t(l))
                                .unwrap_or(i18n::t("Microsoft YaHei"));
                            egui::ComboBox::from_id_salt("ann_font_combo")
                                .selected_text(cur_label)
                                .width(110.0)
                                .show_ui(ui, |ui| {
                                    for (val, name_key) in FONTS {
                                        if ui
                                            .selectable_value(
                                                &mut cur.font_family,
                                                (*val).to_string(),
                                                i18n::t(name_key),
                                            )
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                            ui.label(i18n::t("Size"));
                            let mut fs = cur.font_size.round() as i32;
                            if ui
                                .add(egui::DragValue::new(&mut fs).range(12..=96).suffix(" px"))
                                .changed()
                            {
                                cur.font_size = fs as f32;
                                changed = true;
                            }
                            if state.tool == Tool::Watermark {
                                ui.label(i18n::t("Opacity"));
                                let mut op = cur.opacity.round() as i32;
                                if ui
                                    .add(egui::Slider::new(&mut op, 5..=100).suffix("%"))
                                    .changed()
                                {
                                    cur.opacity = op as f32;
                                    changed = true;
                                }
                            }
                        } else if state.tool != Tool::Highlighter {
                            ui.separator();
                            let wlabel = match state.tool {
                                Tool::Mosaic => i18n::t("Block"),
                                Tool::Eraser => i18n::t("Size"),
                                _ => i18n::t("Width"),
                            };
                            ui.label(wlabel);
                            let mut w = cur.width.round() as i32;
                            if ui
                                .add(egui::Slider::new(&mut w, 1..=24).suffix(" px"))
                                .changed()
                            {
                                cur.width = w as f32;
                                changed = true;
                            }
                        } else {
                            ui.separator();
                            ui.label(
                                RichText::new(i18n::t(
                                    "Drag to highlight region(s) while dimming the surroundings.",
                                ))
                                .weak()
                                .small(),
                            );
                        }

                        if changed {
                            state.configure(app, cur);
                        }
                    });
                });
        }
    }
}

/// Render interactive annotation canvas inside `target_rect` (image coordinates 0..base.width x 0..base.height).
pub fn show_annotation_canvas(
    ui: &mut egui::Ui,
    state: &mut AnnotationState,
    base: &Rgba,
    target_rect: Rect,
    disabled: bool,
) {
    let ctx = ui.ctx().clone();
    let tex = state.ensure_preview_texture(&ctx, base);

    // Draw composite image.
    if let Some(tex) = &tex {
        ui.painter().image(
            tex.id(),
            target_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    if disabled {
        return;
    }

    let img_w = base.width.max(1) as f32;
    let img_h = base.height.max(1) as f32;
    let sx = target_rect.width() / img_w;
    let sy = target_rect.height() / img_h;

    let to_img = |p: Pos2| -> Point {
        Point {
            x: ((p.x - target_rect.min.x) / sx).clamp(0.0, img_w),
            y: ((p.y - target_rect.min.y) / sy).clamp(0.0, img_h),
        }
    };
    let to_screen = |p: Point| -> Pos2 {
        Pos2::new(target_rect.min.x + p.x * sx, target_rect.min.y + p.y * sy)
    };

    // Interactive region.
    let sense = if state.tool == Tool::Select && state.editing.is_none() {
        Sense::click()
    } else {
        Sense::click_and_drag()
    };
    let resp = ui.allocate_rect(target_rect, sense);

    // Interactive bounding box, scale corner, and rotate handle for text/watermark.
    let mut handled_by_text_gizmo = false;
    if let Some(editing) = state.editing.as_mut() {
        let (bx, by, bw, bh) = annotations::text_bounds(&editing.stroke);
        let rot = editing.stroke.rotation.unwrap_or(0.0);
        let origin = Point { x: bx, y: by };
        let to_world = |local: Point| -> Pos2 {
            let r = annotations::rotate_point(local, rot);
            to_screen(Point {
                x: origin.x + r.x,
                y: origin.y + r.y,
            })
        };
        let p_tl = to_world(Point { x: 0.0, y: 0.0 });
        let p_tr = to_world(Point { x: bw, y: 0.0 });
        let p_br = to_world(Point { x: bw, y: bh });
        let p_bl = to_world(Point { x: 0.0, y: bh });
        let p_center_img = {
            let r = annotations::rotate_point(Point { x: bw * 0.5, y: bh * 0.5 }, rot);
            Point {
                x: origin.x + r.x,
                y: origin.y + r.y,
            }
        };
        let p_top_mid = to_world(Point { x: bw * 0.5, y: 0.0 });
        let p_rot_handle = to_world(Point {
            x: bw * 0.5,
            y: -26.0 / sy.max(0.1),
        });

        let accent = ui::ctx_palette(ui.ctx()).accent;
        let painter = ui.painter();
        let box_stroke = egui::Stroke::new(1.5_f32, accent);
        painter.line_segment([p_tl, p_tr], box_stroke);
        painter.line_segment([p_tr, p_br], box_stroke);
        painter.line_segment([p_br, p_bl], box_stroke);
        painter.line_segment([p_bl, p_tl], box_stroke);
        painter.line_segment([p_top_mid, p_rot_handle], box_stroke);
        painter.circle_filled(p_rot_handle, 6.0, accent);
        painter.circle_stroke(p_rot_handle, 6.0, egui::Stroke::new(1.5_f32, Color32::WHITE));
        painter.circle_filled(p_br, 6.0, Color32::WHITE);
        painter.circle_stroke(p_br, 6.0, egui::Stroke::new(2.0_f32, accent));

        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let shift_down = ui.input(|i| i.modifiers.shift);

        if resp.drag_started() {
            if let Some(pos) = pointer_pos {
                if pos.distance(p_rot_handle) <= 14.0 {
                    let img_p = to_img(pos);
                    let start_angle =
                        (img_p.y - p_center_img.y).atan2(img_p.x - p_center_img.x).to_degrees();
                    editing.drag_mode = Some(TextDragMode::Rotate {
                        center_img: p_center_img,
                        start_angle,
                        orig_rot: rot,
                    });
                    handled_by_text_gizmo = true;
                } else if pos.distance(p_br) <= 14.0 {
                    let img_p = to_img(pos);
                    let orig_dist = (img_p.x - p_center_img.x)
                        .hypot(img_p.y - p_center_img.y)
                        .max(8.0);
                    editing.drag_mode = Some(TextDragMode::Scale {
                        center_img: p_center_img,
                        orig_dist,
                        orig_scale: editing.stroke.scale.unwrap_or(1.0),
                    });
                    handled_by_text_gizmo = true;
                } else {
                    let hit_rect = Rect::from_points(&[p_tl, p_tr, p_br, p_bl]).expand(8.0);
                    if hit_rect.contains(pos) {
                        editing.drag_mode = Some(TextDragMode::Move {
                            start_img: to_img(pos),
                            orig_pos: origin,
                        });
                        handled_by_text_gizmo = true;
                    }
                }
            }
        }
        if let Some(mode) = editing.drag_mode {
            handled_by_text_gizmo = true;
            if let Some(pos) = pointer_pos {
                let img_p = to_img(pos);
                match mode {
                    TextDragMode::Move {
                        start_img,
                        orig_pos,
                    } => {
                        editing.stroke.points[0] = Point {
                            x: (orig_pos.x + img_p.x - start_img.x).clamp(0.0, img_w),
                            y: (orig_pos.y + img_p.y - start_img.y).clamp(0.0, img_h),
                        };
                        state.texture_dirty = true;
                        state.dirty = true;
                    }
                    TextDragMode::Scale {
                        center_img,
                        orig_dist,
                        orig_scale,
                    } => {
                        let dist = (img_p.x - center_img.x)
                            .hypot(img_p.y - center_img.y)
                            .max(4.0);
                        let next_scale = (orig_scale * (dist / orig_dist)).clamp(0.2, 10.0);
                        editing.stroke.scale = Some(next_scale);
                        state.texture_dirty = true;
                        state.dirty = true;
                    }
                    TextDragMode::Rotate {
                        center_img,
                        start_angle,
                        orig_rot,
                    } => {
                        let cur_angle = (img_p.y - center_img.y)
                            .atan2(img_p.x - center_img.x)
                            .to_degrees();
                        let mut deg = orig_rot + (cur_angle - start_angle);
                        while deg > 180.0 {
                            deg -= 360.0;
                        }
                        while deg < -180.0 {
                            deg += 360.0;
                        }
                        if shift_down {
                            deg = (deg / 15.0).round() * 15.0;
                        }
                        editing.stroke.rotation = Some(deg);
                        state.texture_dirty = true;
                        state.dirty = true;
                    }
                }
            }
            if resp.drag_stopped() {
                editing.drag_mode = None;
            }
        }

        // Inline text input popup below the text box.
        let input_pos = Pos2::new(
            p_bl.x.clamp(target_rect.left() + 6.0, (target_rect.right() - 220.0).max(target_rect.left() + 6.0)),
            (p_bl.y + 10.0).min((target_rect.bottom() - 80.0).max(target_rect.top() + 6.0)),
        );
        let mut finish_clicked = false;
        egui::Area::new(egui::Id::new("inline_text_editor_popup"))
            .fixed_pos(input_pos)
            .order(egui::Order::Foreground)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(220.0);
                    let buf = editing.stroke.text.get_or_insert_with(String::new);
                    if ui
                        .add(
                            egui::TextEdit::multiline(buf)
                                .desired_rows(2)
                                .desired_width(210.0)
                                .hint_text(i18n::t("Enter text; drag corner to scale, top handle to rotate")),
                        )
                        .changed()
                    {
                        state.texture_dirty = true;
                        state.dirty = true;
                    }
                    ui.horizontal(|ui| {
                        if icons::icon_text_button(
                            ui,
                            Icon::Check,
                            i18n::t("Done"),
                            false,
                            true,
                            None,
                        )
                        .clicked()
                        {
                            finish_clicked = true;
                        }
                        if icons::icon_text_button_danger(ui, Icon::Trash, i18n::t("Delete"))
                            .clicked()
                        {
                            editing.stroke.text = Some(String::new());
                            finish_clicked = true;
                        }
                    });
                });
            });
        if finish_clicked {
            state.finish_text();
            return;
        }
    }

    if handled_by_text_gizmo {
        return;
    }

    // Click an existing text/watermark item to re-edit it.
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let img_p = to_img(pos);
            let hit_idx = state.strokes.iter().rposition(|s| {
                if !s.tool.is_text() {
                    return false;
                }
                let (bx, by, bw, bh) = annotations::text_bounds(s);
                let rel = Point {
                    x: img_p.x - bx,
                    y: img_p.y - by,
                };
                let unrot = annotations::rotate_point(rel, -s.rotation.unwrap_or(0.0));
                unrot.x >= -6.0 && unrot.y >= -6.0 && unrot.x <= bw + 6.0 && unrot.y <= bh + 6.0
            });
            if let Some(idx) = hit_idx {
                state.finish_text();
                let found = state.strokes[idx].clone();
                state.tool = found.tool;
                state.open_group = ToolGroup::of_tool(found.tool);
                state.editing = Some(EditingText {
                    stroke: found,
                    existing: true,
                    drag_mode: None,
                });
                state.texture_dirty = true;
                return;
            }
            if state.editing.is_some() {
                state.finish_text();
                return;
            }
            // Create a new step number or text/watermark item.
            if state.tool == Tool::Number {
                let opts = state.current_options();
                let num = annotations::next_number(&state.strokes);
                state.strokes.push(Stroke {
                    id: Uuid::new_v4().to_string(),
                    tool: Tool::Number,
                    points: vec![img_p],
                    color: opts.color,
                    width: opts.width,
                    text: None,
                    number: Some(num),
                    font_size: None,
                    font_family: None,
                    opacity: Some(100.0),
                    scale: None,
                    rotation: None,
                });
                state.dirty = true;
                state.texture_dirty = true;
                return;
            }
            if state.tool.is_text() {
                let opts = state.current_options();
                state.editing = Some(EditingText {
                    stroke: Stroke {
                        id: Uuid::new_v4().to_string(),
                        tool: state.tool,
                        points: vec![img_p],
                        color: opts.color,
                        width: opts.width,
                        text: Some(String::new()),
                        number: None,
                        font_size: Some(opts.font_size),
                        font_family: Some(opts.font_family),
                        opacity: Some(opts.opacity),
                        scale: Some(1.0),
                        rotation: Some(0.0),
                    },
                    existing: false,
                    drag_mode: None,
                });
                state.texture_dirty = true;
                return;
            }
        }
    }

    // Drag to draw shapes, arrows, freehand pen, mosaic, spotlight, or eraser.
    if state.tool != Tool::Select && !state.tool.is_text() && state.tool != Tool::Number {
        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                state.finish_text();
                let p = to_img(pos);
                let opts = state.current_options();
                state.draft = Some(Stroke {
                    id: Uuid::new_v4().to_string(),
                    tool: state.tool,
                    points: vec![p, p],
                    color: opts.color,
                    width: opts.width,
                    text: None,
                    number: None,
                    font_size: None,
                    font_family: None,
                    opacity: Some(opts.opacity),
                    scale: None,
                    rotation: None,
                });
                state.texture_dirty = true;
            }
        } else if resp.dragged() {
            if let (Some(pos), Some(draft)) = (resp.interact_pointer_pos(), state.draft.as_mut()) {
                let p = to_img(pos);
                if draft.tool.is_freehand() {
                    let push = draft
                        .points
                        .last()
                        .is_none_or(|last| (last.x - p.x).hypot(last.y - p.y) >= 1.5);
                    if push {
                        draft.points.push(p);
                        state.texture_dirty = true;
                    }
                } else if let Some(last) = draft.points.last_mut() {
                    *last = p;
                    state.texture_dirty = true;
                }
            }
        } else if resp.drag_stopped() {
            if let Some(draft) = state.draft.take() {
                state.strokes.push(draft);
                state.dirty = true;
                state.texture_dirty = true;
            }
        }
    }
}
