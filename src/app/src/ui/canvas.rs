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
use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, TextureHandle, TextureOptions,
    Vec2,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

const MAX_UNDO_STEPS: usize = 60;
const MIN_DRAG_CREATE_PX: f32 = 4.0;
const HANDLE_RADIUS: f32 = 5.0;
const HANDLE_HIT_PX: f32 = 12.0;

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

fn tool_name(tool: Tool) -> &'static str {
    let key = match tool {
        Tool::Select => "Select",
        Tool::Rect => "Rectangle",
        Tool::Line => "Line",
        Tool::Ellipse => "Ellipse",
        Tool::Pen => "Pen",
        Tool::Arrow => "Arrow",
        Tool::Text => "Text",
        Tool::Watermark => "Watermark",
        Tool::Number => "Number",
        Tool::Mosaic => "Mosaic",
        Tool::Highlighter => "Spotlight",
        Tool::Eraser => "Eraser",
    };
    i18n::t(key)
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
enum EditDrag {
    Move {
        stroke_id: String,
        start_img: Point,
        orig_points: Vec<Point>,
        snapshot_before: Vec<Stroke>,
    },
    ResizeBox {
        stroke_id: String,
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
        orig_min: Point,
        orig_max: Point,
        snapshot_before: Vec<Stroke>,
    },
    MoveEndpoint {
        stroke_id: String,
        endpoint_idx: usize,
        snapshot_before: Vec<Stroke>,
    },
    TextScale {
        stroke_id: String,
        center_img: Point,
        orig_dist: f32,
        orig_scale: f32,
        snapshot_before: Vec<Stroke>,
    },
    TextRotate {
        stroke_id: String,
        center_img: Point,
        start_angle: f32,
        orig_rot: f32,
        snapshot_before: Vec<Stroke>,
    },
}

pub struct AnnotationState {
    pub tool: Tool,
    open_group: Option<ToolGroup>,
    layers_open: bool,
    pub strokes: Vec<Stroke>,
    pub draft: Option<Stroke>,
    draft_start_screen: Option<Pos2>,
    draft_moved: bool,
    pub selected_id: Option<String>,
    selected_is_new_text: bool,
    edit_drag: Option<EditDrag>,
    undo_stack: Vec<Vec<Stroke>>,
    style_undo_key: Option<(String, &'static str)>,
    pub per_tool: HashMap<Tool, ToolOptions>,
    pub dirty: bool,
    pub texture_dirty: bool,
    pub preview_tex: Option<TextureHandle>,
    pub last_base_dims: (u32, u32),
    pub last_cursor: Option<CursorIcon>,
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
            layers_open: false,
            strokes: Vec::new(),
            draft: None,
            draft_start_screen: None,
            draft_moved: false,
            selected_id: None,
            selected_is_new_text: false,
            edit_drag: None,
            undo_stack: Vec::new(),
            style_undo_key: None,
            per_tool,
            dirty: false,
            texture_dirty: true,
            preview_tex: None,
            last_base_dims: (0, 0),
            last_cursor: None,
        }
    }

    pub fn reset(&mut self) {
        self.strokes.clear();
        self.draft = None;
        self.draft_start_screen = None;
        self.draft_moved = false;
        self.selected_id = None;
        self.selected_is_new_text = false;
        self.edit_drag = None;
        self.undo_stack.clear();
        self.style_undo_key = None;
        self.layers_open = false;
        self.dirty = false;
        self.texture_dirty = true;
        self.last_cursor = None;
    }

    pub fn load(&mut self, strokes: Vec<Stroke>) {
        self.strokes = strokes;
        self.draft = None;
        self.draft_start_screen = None;
        self.draft_moved = false;
        self.selected_id = None;
        self.selected_is_new_text = false;
        self.edit_drag = None;
        self.undo_stack.clear();
        self.style_undo_key = None;
        self.dirty = false;
        self.texture_dirty = true;
        self.last_cursor = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selected_id
            .as_deref()
            .is_some_and(|id| self.strokes.iter().any(|s| s.id == id))
    }

    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected_id.as_deref()?;
        self.strokes.iter().position(|s| s.id == id)
    }

    pub fn selected_stroke(&self) -> Option<&Stroke> {
        self.selected_index().and_then(|i| self.strokes.get(i))
    }

    fn push_undo_snapshot(&mut self, snapshot: Vec<Stroke>) {
        if self.undo_stack.last().is_some_and(|top| top == &snapshot) {
            return;
        }
        self.undo_stack.push(snapshot);
        if self.undo_stack.len() > MAX_UNDO_STEPS {
            self.undo_stack.remove(0);
        }
    }

    fn push_undo(&mut self) {
        let snap = self.strokes.clone();
        self.push_undo_snapshot(snap);
    }

    fn cleanup_empty_selected_text(&mut self) {
        let Some(idx) = self.selected_index() else {
            return;
        };
        let is_empty_text = self.strokes[idx].tool.is_text()
            && self.strokes[idx]
                .text
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty();
        if is_empty_text {
            self.strokes.remove(idx);
            if !self.selected_is_new_text {
                self.dirty = true;
            }
            self.texture_dirty = true;
        }
    }

    pub fn select_layer(&mut self, id: String, sync_tool: bool) {
        if self.selected_id.as_deref() == Some(id.as_str()) {
            return;
        }
        self.cleanup_empty_selected_text();
        let Some(stroke) = self.strokes.iter().find(|s| s.id == id) else {
            return;
        };
        let stroke_tool = stroke.tool;
        self.selected_id = Some(id);
        self.selected_is_new_text = false;
        self.edit_drag = None;
        self.style_undo_key = None;
        if sync_tool || self.tool != Tool::Select {
            self.tool = stroke_tool;
        }
        self.open_group = ToolGroup::of_tool(stroke_tool);
    }

    pub fn active_options_tool(&self) -> Tool {
        self.selected_stroke()
            .map(|s| s.tool)
            .unwrap_or(self.tool)
    }

    pub fn current_options(&self) -> ToolOptions {
        if let Some(stroke) = self.selected_stroke() {
            return ToolOptions {
                color: stroke.color.clone(),
                fill_color: stroke.fill_color.clone(),
                width: stroke.width,
                font_size: stroke.effective_font_size(),
                font_family: stroke
                    .font_family
                    .clone()
                    .unwrap_or_else(|| "Microsoft YaHei".into()),
                opacity: stroke.opacity.unwrap_or(100.0),
            };
        }
        self.per_tool
            .get(&self.tool)
            .cloned()
            .unwrap_or_default()
    }

    pub fn configure(&mut self, app: &Arc<App>, next: ToolOptions, prop_key: &'static str) {
        let target_tool = if let Some(idx) = self.selected_index() {
            let stroke_id = self.strokes[idx].id.clone();
            let key = (stroke_id, prop_key);
            if self.style_undo_key.as_ref() != Some(&key) {
                self.push_undo();
                self.style_undo_key = Some(key);
            }
            let stroke = &mut self.strokes[idx];
            stroke.color = next.color.clone();
            stroke.fill_color = if stroke.tool.supports_fill() {
                next.fill_color.clone()
            } else {
                None
            };
            stroke.width = next.width;
            if stroke.tool.is_text() {
                stroke.font_size = Some(next.font_size);
                stroke.font_family = Some(next.font_family.clone());
                stroke.opacity = Some(next.opacity);
            }
            self.texture_dirty = true;
            self.dirty = true;
            stroke.tool
        } else {
            self.tool
        };
        if target_tool != Tool::Select {
            self.per_tool.insert(target_tool, next.clone());
            let mut patch = json!({
                "color": next.color,
                "width": next.width.round() as i64,
                "fontSize": next.font_size.round() as i64,
                "fontFamily": next.font_family,
                "opacity": next.opacity.round() as i64,
            });
            if target_tool.supports_fill() {
                patch["fillColor"] = match &next.fill_color {
                    Some(c) if !c.is_empty() => json!(c),
                    _ => serde_json::Value::Null,
                };
            }
            service::update_annotation_preference(app, target_tool.as_str(), patch);
        }
    }

    /// Commit or clear any active element / text editing selection.
    pub fn finish_text(&mut self) {
        self.cleanup_empty_selected_text();
        self.selected_id = None;
        self.selected_is_new_text = false;
        self.edit_drag = None;
        self.style_undo_key = None;
    }

    pub fn delete_selected(&mut self) -> bool {
        let Some(id) = self.selected_id.clone() else {
            return false;
        };
        self.delete_layer(&id)
    }

    pub fn delete_layer(&mut self, id: &str) -> bool {
        let Some(idx) = self.strokes.iter().position(|s| s.id == id) else {
            return false;
        };
        let skip_undo = self.selected_id.as_deref() == Some(id)
            && self.selected_is_new_text
            && self.strokes[idx]
                .text
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty();
        if !skip_undo {
            self.push_undo();
        }
        self.strokes.remove(idx);
        if self.selected_id.as_deref() == Some(id) {
            self.selected_id = None;
            self.selected_is_new_text = false;
            self.edit_drag = None;
            self.style_undo_key = None;
        }
        self.dirty = true;
        self.texture_dirty = true;
        true
    }

    pub fn move_layer_up(&mut self, id: &str) -> bool {
        let Some(idx) = self.strokes.iter().position(|s| s.id == id) else {
            return false;
        };
        if idx + 1 >= self.strokes.len() {
            return false;
        }
        self.push_undo();
        self.strokes.swap(idx, idx + 1);
        self.style_undo_key = None;
        self.dirty = true;
        self.texture_dirty = true;
        true
    }

    pub fn move_layer_down(&mut self, id: &str) -> bool {
        let Some(idx) = self.strokes.iter().position(|s| s.id == id) else {
            return false;
        };
        if idx == 0 {
            return false;
        }
        self.push_undo();
        self.strokes.swap(idx, idx - 1);
        self.style_undo_key = None;
        self.dirty = true;
        self.texture_dirty = true;
        true
    }

    pub fn nudge_selected(&mut self, dx: f32, dy: f32) -> bool {
        let Some(idx) = self.selected_index() else {
            return false;
        };
        let (img_w, img_h) = (
            self.last_base_dims.0.max(1) as f32,
            self.last_base_dims.1.max(1) as f32,
        );
        let key = (self.strokes[idx].id.clone(), "nudge");
        if self.style_undo_key.as_ref() != Some(&key) {
            self.push_undo();
            self.style_undo_key = Some(key);
        }
        self.strokes[idx].translate(dx, dy, img_w, img_h);
        self.dirty = true;
        self.texture_dirty = true;
        true
    }

    pub fn undo(&mut self) {
        if self.selected_is_new_text {
            if let Some(idx) = self.selected_index() {
                if self.strokes[idx]
                    .text
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
                {
                    self.strokes.remove(idx);
                    self.selected_id = None;
                    self.selected_is_new_text = false;
                    self.edit_drag = None;
                    self.texture_dirty = true;
                    return;
                }
            }
        }
        if let Some(prev) = self.undo_stack.pop() {
            self.strokes = prev;
            if let Some(id) = self.selected_id.as_deref() {
                if !self.strokes.iter().any(|s| s.id == id) {
                    self.selected_id = None;
                    self.selected_is_new_text = false;
                }
            }
            self.edit_drag = None;
            self.style_undo_key = None;
            self.dirty = true;
            self.texture_dirty = true;
            return;
        }
        if let Some(removed) = self.strokes.pop() {
            if self.selected_id.as_deref() == Some(removed.id.as_str()) {
                self.selected_id = None;
                self.selected_is_new_text = false;
            }
            self.edit_drag = None;
            self.style_undo_key = None;
            self.dirty = true;
            self.texture_dirty = true;
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty() || !self.strokes.is_empty()
    }

    pub fn all_strokes_snapshot(&self) -> Vec<Stroke> {
        self.strokes
            .iter()
            .filter(|s| {
                !s.tool.is_text() || !s.text.as_deref().unwrap_or("").trim().is_empty()
            })
            .cloned()
            .collect()
    }

    /// Render composite preview texture with dirty-flag caching to avoid redundant rasterization.
    pub fn ensure_preview_texture(
        &mut self,
        ctx: &egui::Context,
        base: &Rgba,
    ) -> Option<TextureHandle> {
        let dims = (base.width, base.height);
        if self.last_base_dims != dims {
            self.last_base_dims = dims;
            self.texture_dirty = true;
        }
        if self.texture_dirty || self.preview_tex.is_none() {
            let mut render_list = Vec::with_capacity(self.strokes.len() + 1);
            for s in &self.strokes {
                if s.tool.is_text() && s.text.as_deref().unwrap_or("").is_empty() {
                    continue;
                }
                render_list.push(s.clone());
            }
            if self.draft_moved {
                if let Some(draft) = &self.draft {
                    render_list.push(draft.clone());
                }
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
                        state.layers_open = false;
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
                        state.layers_open = false;
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
                        state.layers_open = false;
                        triggered = Some(ToolbarAction::Confirm);
                    }
                }
            });
        });
        show_tool_options(ui, app, state);
    });
    triggered
}

/// Tool button row (Select, tool groups, Layers, Undo); layout and icon-only mode are controlled by the caller.
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
            .on_hover_text(i18n::t("Select / Edit elements"))
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

    let editable_count = state
        .strokes
        .iter()
        .filter(|s| s.tool.is_layer_editable())
        .count();
    if editable_count == 0 {
        state.layers_open = false;
    }
    ui.add_enabled_ui(editable_count > 0, |ui| {
        let layers_tip = i18n::tf(
            "Layers ({count})",
            &[("count", &editable_count.to_string())],
        );
        let layers_resp = if icon_only {
            icons::icon_button(ui, Icon::Layers, state.layers_open, false, false)
                .on_hover_text(layers_tip)
        } else {
            let label = if editable_count > 0 {
                format!("{} ({})", i18n::t("Layers"), editable_count)
            } else {
                i18n::t("Layers").to_string()
            };
            icons::icon_text_button(
                ui,
                Icon::Layers,
                &label,
                state.layers_open,
                false,
                Some(if state.layers_open {
                    Icon::ChevronUp
                } else {
                    Icon::ChevronDown
                }),
            )
            .on_hover_text(layers_tip)
        };
        if layers_resp.clicked() {
            state.layers_open = !state.layers_open;
        }
    });

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

/// Draw a "No Fill" circular swatch with a diagonal slash.
fn no_fill_swatch(ui: &mut egui::Ui, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
    let center = rect.center();
    let painter = ui.painter();
    painter.circle_filled(center, 7.5, Color32::from_gray(245));
    painter.line_segment(
        [
            Pos2::new(center.x - 5.0, center.y + 5.0),
            Pos2::new(center.x + 5.0, center.y - 5.0),
        ],
        egui::Stroke::new(1.8_f32, Color32::from_rgb(0xd9, 0x3b, 0x3b)),
    );
    if selected {
        painter.circle_stroke(
            center,
            8.5,
            egui::Stroke::new(2.0_f32, ui::ctx_palette(ui.ctx()).accent),
        );
    } else {
        painter.circle_stroke(center, 7.5, egui::Stroke::new(1.0_f32, Color32::from_gray(140)));
    }
    resp.on_hover_text(i18n::t("No fill"))
}

/// Expanded tool options row (color, fill color, stroke width, font, opacity, and selected-layer controls)
/// plus optional collapsible layer list.
pub fn show_tool_options(ui: &mut egui::Ui, app: &Arc<App>, state: &mut AnnotationState) {
    let selected_info = state
        .selected_index()
        .map(|idx| (idx, state.strokes.len(), state.strokes[idx].id.clone(), state.strokes[idx].tool));

    let active_group = state
        .open_group
        .or_else(|| selected_info.as_ref().and_then(|(_, _, _, t)| ToolGroup::of_tool(*t)));

    if let Some(group) = active_group {
        let active_tool = state.active_options_tool();
        ui.add_space(4.0);
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 6))
            .corner_radius(CornerRadius::same(8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(group.label()).strong());

                    // Sub-tool picker (only when not editing a locked element type, or clicking switches & deselects).
                    let opts = group.options();
                    if opts.len() > 1 {
                        ui.separator();
                        for (tool, name_key) in opts {
                            if icons::icon_text_button(
                                ui,
                                tool_icon(*tool),
                                i18n::t(name_key),
                                active_tool == *tool,
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
                    let mut changed_prop: Option<&'static str> = None;

                    // Stroke / foreground color.
                    if !matches!(active_tool, Tool::Mosaic | Tool::Eraser | Tool::Highlighter) {
                        ui.separator();
                        ui.label(i18n::t("Color"));
                        let (r, g, b) = annotations::parse_hex_rgb(&cur.color);
                        let mut rgb = [r, g, b];
                        if ui.color_edit_button_srgb(&mut rgb).changed() {
                            cur.color = annotations::format_hex_rgb(rgb[0], rgb[1], rgb[2]);
                            changed_prop = Some("color");
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
                                changed_prop = Some("color");
                            }
                        }
                    }

                    // Fill color for closed shapes (Rectangle / Ellipse).
                    if active_tool.supports_fill() {
                        ui.separator();
                        ui.label(i18n::t("Fill"));
                        if no_fill_swatch(ui, cur.fill_color.is_none()).clicked() {
                            cur.fill_color = None;
                            changed_prop = Some("fill");
                        }
                        let preview_hex = cur.fill_color.as_deref().unwrap_or(&cur.color);
                        let (fr, fg, fb) = annotations::parse_hex_rgb(preview_hex);
                        let mut frgb = [fr, fg, fb];
                        if ui
                            .color_edit_button_srgb(&mut frgb)
                            .on_hover_text(i18n::t("Fill Color"))
                            .changed()
                        {
                            cur.fill_color =
                                Some(annotations::format_hex_rgb(frgb[0], frgb[1], frgb[2]));
                            changed_prop = Some("fill");
                        }
                        for hex in PRESET_COLORS {
                            let (sr, sg, sb) = annotations::parse_hex_rgb(hex);
                            let swatch_color = Color32::from_rgb(sr, sg, sb);
                            let selected = cur
                                .fill_color
                                .as_deref()
                                .is_some_and(|c| c.eq_ignore_ascii_case(hex));
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
                                cur.fill_color = Some((*hex).to_string());
                                changed_prop = Some("fill");
                            }
                        }
                    }

                    // Text options vs stroke width.
                    if active_tool.is_text() {
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
                                        changed_prop = Some("font");
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
                            changed_prop = Some("fontSize");
                        }
                        if active_tool == Tool::Watermark {
                            ui.label(i18n::t("Opacity"));
                            let mut op = cur.opacity.round() as i32;
                            if ui
                                .add(egui::Slider::new(&mut op, 5..=100).suffix("%"))
                                .changed()
                            {
                                cur.opacity = op as f32;
                                changed_prop = Some("opacity");
                            }
                        }
                    } else if active_tool != Tool::Highlighter {
                        ui.separator();
                        let wlabel = match active_tool {
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
                            changed_prop = Some("width");
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

                    if let Some(prop) = changed_prop {
                        state.configure(app, cur, prop);
                    }

                    // Selected layer quick controls (Bring Forward / Send Backward / Delete).
                    if let Some((idx, total, id, _)) = &selected_info {
                        ui.separator();
                        ui.label(
                            RichText::new(i18n::tf(
                                "Layer {idx}/{total}",
                                &[
                                    ("idx", &(idx + 1).to_string()),
                                    ("total", &total.to_string()),
                                ],
                            ))
                            .small()
                            .weak(),
                        );
                        ui.add_enabled_ui(*idx + 1 < *total, |ui| {
                            if icons::small_icon_button(ui, Icon::ChevronUp)
                                .on_hover_text(i18n::t("Bring Forward"))
                                .clicked()
                            {
                                state.move_layer_up(id);
                            }
                        });
                        ui.add_enabled_ui(*idx > 0, |ui| {
                            if icons::small_icon_button(ui, Icon::ChevronDown)
                                .on_hover_text(i18n::t("Send Backward"))
                                .clicked()
                            {
                                state.move_layer_down(id);
                            }
                        });
                        if icons::small_icon_button(ui, Icon::Trash)
                            .on_hover_text(i18n::t("Delete (Del)"))
                            .clicked()
                        {
                            state.delete_layer(id);
                        }
                    }
                });
            });
    }

    // Collapsible Layers list (top layer first).
    if state.layers_open && !state.strokes.is_empty() {
        ui.add_space(4.0);
        let mut action: Option<(&'static str, String)> = None;
        let palette = ui::ctx_palette(ui.ctx());
        let total = state.strokes.len();
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(10, 6))
            .corner_radius(CornerRadius::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(i18n::t("Layers")).strong());
                    ui.label(
                        RichText::new(i18n::t("Top to bottom · Click a layer to edit"))
                            .weak()
                            .small(),
                    );
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("annotation_layers_scroll")
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for idx in (0..total).rev() {
                            let s = &state.strokes[idx];
                            if !s.tool.is_layer_editable() {
                                continue;
                            }
                            let is_sel = state.selected_id.as_deref() == Some(s.id.as_str());
                            let row_fill = if is_sel {
                                palette.accent.gamma_multiply(0.18)
                            } else {
                                Color32::TRANSPARENT
                            };
                            let row_stroke = if is_sel {
                                egui::Stroke::new(1.0_f32, palette.accent.gamma_multiply(0.7))
                            } else {
                                egui::Stroke::NONE
                            };
                            egui::Frame::NONE
                                .fill(row_fill)
                                .stroke(row_stroke)
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(6, 3))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let label_text = layer_summary(s, idx + 1);
                                        let (dot_rect, _) = ui
                                            .allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                                        let (r, g, b) = annotations::parse_hex_rgb(&s.color);
                                        let stroke_col = Color32::from_rgb(r, g, b);
                                        if let Some(fill_hex) =
                                            s.fill_color.as_deref().filter(|c| !c.is_empty())
                                        {
                                            let (fr, fg, fb) = annotations::parse_hex_rgb(fill_hex);
                                            ui.painter().circle_filled(
                                                dot_rect.center(),
                                                5.5,
                                                Color32::from_rgb(fr, fg, fb),
                                            );
                                            ui.painter().circle_stroke(
                                                dot_rect.center(),
                                                5.5,
                                                egui::Stroke::new(1.6_f32, stroke_col),
                                            );
                                        } else {
                                            ui.painter().circle_filled(
                                                dot_rect.center(),
                                                5.0,
                                                stroke_col,
                                            );
                                        }

                                        let btn = icons::small_icon_text_button(
                                            ui,
                                            tool_icon(s.tool),
                                            &label_text,
                                            is_sel,
                                            false,
                                        );
                                        if btn.clicked() {
                                            action = Some(("select", s.id.clone()));
                                        }

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if icons::small_icon_button(ui, Icon::Trash)
                                                    .on_hover_text(i18n::t("Delete"))
                                                    .clicked()
                                                {
                                                    action = Some(("delete", s.id.clone()));
                                                }
                                                ui.add_enabled_ui(idx > 0, |ui| {
                                                    if icons::small_icon_button(
                                                        ui,
                                                        Icon::ChevronDown,
                                                    )
                                                    .on_hover_text(i18n::t("Send Backward"))
                                                    .clicked()
                                                    {
                                                        action = Some(("down", s.id.clone()));
                                                    }
                                                });
                                                ui.add_enabled_ui(idx + 1 < total, |ui| {
                                                    if icons::small_icon_button(
                                                        ui,
                                                        Icon::ChevronUp,
                                                    )
                                                    .on_hover_text(i18n::t("Bring Forward"))
                                                    .clicked()
                                                    {
                                                        action = Some(("up", s.id.clone()));
                                                    }
                                                });
                                            },
                                        );
                                    });
                                });
                        }
                    });
            });

        if let Some((kind, id)) = action {
            match kind {
                "select" => state.select_layer(id, true),
                "up" => {
                    state.move_layer_up(&id);
                }
                "down" => {
                    state.move_layer_down(&id);
                }
                "delete" => {
                    state.delete_layer(&id);
                }
                _ => {}
            }
        }
    }
}

fn layer_summary(stroke: &Stroke, layer_num: usize) -> String {
    let base = tool_name(stroke.tool);
    match stroke.tool {
        Tool::Text | Tool::Watermark => {
            let txt = stroke.text.as_deref().unwrap_or("").trim();
            if txt.is_empty() {
                format!("#{layer_num} {base}")
            } else {
                let preview: String = txt.chars().take(12).collect();
                format!("#{layer_num} {base}: {preview}")
            }
        }
        Tool::Number => {
            format!("#{layer_num} {base} {}", stroke.number.unwrap_or(1))
        }
        Tool::Rect | Tool::Ellipse if stroke.has_fill() => {
            format!("#{layer_num} {base} ({})", i18n::t("Fill"))
        }
        _ => format!("#{layer_num} {base}"),
    }
}

/// Top-to-bottom layered hit-test filtered by active tool (or all layers for `Tool::Select`).
fn hit_test_layers(
    strokes: &[Stroke],
    img_p: Point,
    tol: f32,
    active_tool: Tool,
    group_fallback: bool,
) -> Option<usize> {
    if active_tool == Tool::Eraser {
        return None;
    }
    if active_tool == Tool::Select {
        return strokes
            .iter()
            .rposition(|s| annotations::hit_test_stroke(s, img_p, tol));
    }
    if let Some(exact) = strokes
        .iter()
        .rposition(|s| s.tool == active_tool && annotations::hit_test_stroke(s, img_p, tol))
    {
        return Some(exact);
    }
    if group_fallback {
        if let Some(group) = ToolGroup::of_tool(active_tool) {
            return strokes.iter().rposition(|s| {
                group.contains(s.tool) && annotations::hit_test_stroke(s, img_p, tol)
            });
        }
    }
    None
}

#[derive(Clone, Copy, Debug)]
enum GizmoHit {
    BoxHandle {
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
    },
    Endpoint(usize),
    TextRotate,
    TextScale,
    Body,
}

fn cursor_for_gizmo_hit(hit: GizmoHit, dragging: bool) -> CursorIcon {
    match hit {
        GizmoHit::BoxHandle {
            left,
            right,
            top,
            bottom,
        } => match (left, right, top, bottom) {
            (true, false, true, false) | (false, true, false, true) => CursorIcon::ResizeNwSe,
            (false, true, true, false) | (true, false, false, true) => CursorIcon::ResizeNeSw,
            (true, false, false, false) | (false, true, false, false) => {
                CursorIcon::ResizeHorizontal
            }
            _ => CursorIcon::ResizeVertical,
        },
        GizmoHit::Endpoint(_) | GizmoHit::TextRotate | GizmoHit::TextScale => {
            if dragging {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            }
        }
        GizmoHit::Body => {
            if dragging {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            }
        }
    }
}

fn box_handles(rect: Rect) -> [(Pos2, bool, bool, bool, bool); 8] {
    [
        (rect.left_top(), true, false, true, false),
        (rect.center_top(), false, false, true, false),
        (rect.right_top(), false, true, true, false),
        (rect.right_center(), false, true, false, false),
        (rect.right_bottom(), false, true, false, true),
        (rect.center_bottom(), false, false, false, true),
        (rect.left_bottom(), true, false, false, true),
        (rect.left_center(), true, false, false, false),
    ]
}

fn hit_test_selected_gizmo(
    stroke: &Stroke,
    pos_screen: Pos2,
    img_p: Point,
    tol_img: f32,
    sy: f32,
    to_screen: &impl Fn(Point) -> Pos2,
) -> Option<GizmoHit> {
    if stroke.points.is_empty() {
        return None;
    }
    if stroke.tool.is_text() {
        let (bx, by, bw, bh) = annotations::text_bounds(stroke);
        let rot = stroke.rotation.unwrap_or(0.0);
        let to_world = |local: Point| -> Pos2 {
            let r = annotations::rotate_point(local, rot);
            to_screen(Point {
                x: bx + r.x,
                y: by + r.y,
            })
        };
        let p_br = to_world(Point { x: bw, y: bh });
        let p_rot = to_world(Point {
            x: bw * 0.5,
            y: -26.0 / sy.max(0.1),
        });
        if pos_screen.distance(p_rot) <= 14.0 {
            return Some(GizmoHit::TextRotate);
        }
        if pos_screen.distance(p_br) <= 14.0 {
            return Some(GizmoHit::TextScale);
        }
        if annotations::hit_test_stroke(stroke, img_p, tol_img) {
            return Some(GizmoHit::Body);
        }
        return None;
    }

    if stroke.tool.is_box_shape() {
        let start = stroke.points[0];
        let end = *stroke.points.last().unwrap();
        let min_p = Point {
            x: start.x.min(end.x),
            y: start.y.min(end.y),
        };
        let max_p = Point {
            x: start.x.max(end.x),
            y: start.y.max(end.y),
        };
        let srect = Rect::from_min_max(to_screen(min_p), to_screen(max_p));
        for (hp, left, right, top, bottom) in box_handles(srect) {
            if pos_screen.distance(hp) <= HANDLE_HIT_PX {
                return Some(GizmoHit::BoxHandle {
                    left,
                    right,
                    top,
                    bottom,
                });
            }
        }
        if annotations::hit_test_stroke(stroke, img_p, tol_img) {
            return Some(GizmoHit::Body);
        }
        return None;
    }

    if stroke.tool.is_segment() {
        let p0 = to_screen(stroke.points[0]);
        let p1 = to_screen(*stroke.points.last().unwrap());
        if pos_screen.distance(p1) <= HANDLE_HIT_PX {
            return Some(GizmoHit::Endpoint(1));
        }
        if pos_screen.distance(p0) <= HANDLE_HIT_PX {
            return Some(GizmoHit::Endpoint(0));
        }
        if annotations::hit_test_stroke(stroke, img_p, tol_img) {
            return Some(GizmoHit::Body);
        }
        return None;
    }

    if annotations::hit_test_stroke(stroke, img_p, tol_img) {
        return Some(GizmoHit::Body);
    }
    None
}

fn draw_hover_outline(
    painter: &egui::Painter,
    stroke: &Stroke,
    accent: Color32,
    sx: f32,
    sy: f32,
    to_screen: &impl Fn(Point) -> Pos2,
) {
    let hover_stroke = egui::Stroke::new(1.5_f32, accent.gamma_multiply(0.65));
    if stroke.tool.is_text() {
        let (bx, by, bw, bh) = annotations::text_bounds(stroke);
        let rot = stroke.rotation.unwrap_or(0.0);
        let to_world = |local: Point| -> Pos2 {
            let r = annotations::rotate_point(local, rot);
            to_screen(Point {
                x: bx + r.x,
                y: by + r.y,
            })
        };
        let pts = [
            to_world(Point { x: 0.0, y: 0.0 }),
            to_world(Point { x: bw, y: 0.0 }),
            to_world(Point { x: bw, y: bh }),
            to_world(Point { x: 0.0, y: bh }),
        ];
        for i in 0..4 {
            painter.line_segment([pts[i], pts[(i + 1) % 4]], hover_stroke);
        }
    } else if stroke.tool.is_segment() && stroke.points.len() >= 2 {
        let p0 = to_screen(stroke.points[0]);
        let p1 = to_screen(*stroke.points.last().unwrap());
        painter.circle_stroke(p0, 4.0, hover_stroke);
        painter.circle_stroke(p1, 4.0, hover_stroke);
    } else if stroke.tool == Tool::Number && !stroke.points.is_empty() {
        let c = to_screen(stroke.points[0]);
        let r = (stroke.width * 4.0).max(14.0) * sx.min(sy) + 3.0;
        painter.circle_stroke(c, r, hover_stroke);
    } else if let Some((min_x, min_y, max_x, max_y)) = stroke.bounds() {
        let srect = Rect::from_min_max(
            to_screen(Point { x: min_x, y: min_y }),
            to_screen(Point { x: max_x, y: max_y }),
        );
        painter.rect_stroke(srect, 0.0, hover_stroke, egui::StrokeKind::Outside);
    }
}

fn draw_selected_gizmo(
    painter: &egui::Painter,
    stroke: &Stroke,
    accent: Color32,
    sx: f32,
    sy: f32,
    to_screen: &impl Fn(Point) -> Pos2,
) {
    if stroke.points.is_empty() {
        return;
    }
    let box_stroke = egui::Stroke::new(1.5_f32, accent);
    let handle_stroke = egui::Stroke::new(1.8_f32, accent);

    if stroke.tool.is_text() {
        let (bx, by, bw, bh) = annotations::text_bounds(stroke);
        let rot = stroke.rotation.unwrap_or(0.0);
        let to_world = |local: Point| -> Pos2 {
            let r = annotations::rotate_point(local, rot);
            to_screen(Point {
                x: bx + r.x,
                y: by + r.y,
            })
        };
        let p_tl = to_world(Point { x: 0.0, y: 0.0 });
        let p_tr = to_world(Point { x: bw, y: 0.0 });
        let p_br = to_world(Point { x: bw, y: bh });
        let p_bl = to_world(Point { x: 0.0, y: bh });
        let p_top_mid = to_world(Point { x: bw * 0.5, y: 0.0 });
        let p_rot_handle = to_world(Point {
            x: bw * 0.5,
            y: -26.0 / sy.max(0.1),
        });
        painter.line_segment([p_tl, p_tr], box_stroke);
        painter.line_segment([p_tr, p_br], box_stroke);
        painter.line_segment([p_br, p_bl], box_stroke);
        painter.line_segment([p_bl, p_tl], box_stroke);
        painter.line_segment([p_top_mid, p_rot_handle], box_stroke);
        painter.circle_filled(p_rot_handle, 6.0, accent);
        painter.circle_stroke(p_rot_handle, 6.0, egui::Stroke::new(1.5_f32, Color32::WHITE));
        painter.circle_filled(p_br, 6.0, Color32::WHITE);
        painter.circle_stroke(p_br, 6.0, egui::Stroke::new(2.0_f32, accent));
        return;
    }

    if stroke.tool.is_box_shape() {
        let start = stroke.points[0];
        let end = *stroke.points.last().unwrap();
        let srect = Rect::from_min_max(
            to_screen(Point {
                x: start.x.min(end.x),
                y: start.y.min(end.y),
            }),
            to_screen(Point {
                x: start.x.max(end.x),
                y: start.y.max(end.y),
            }),
        );
        painter.rect_stroke(srect, 0.0, box_stroke, egui::StrokeKind::Outside);
        for (pt, _, _, _, _) in box_handles(srect) {
            painter.circle_filled(pt, HANDLE_RADIUS, Color32::WHITE);
            painter.circle_stroke(pt, HANDLE_RADIUS, handle_stroke);
        }
        return;
    }

    if stroke.tool.is_segment() {
        let p0 = to_screen(stroke.points[0]);
        let p1 = to_screen(*stroke.points.last().unwrap());
        painter.line_segment([p0, p1], egui::Stroke::new(1.0_f32, accent.gamma_multiply(0.55)));
        for pt in [p0, p1] {
            painter.circle_filled(pt, HANDLE_RADIUS + 0.5, Color32::WHITE);
            painter.circle_stroke(pt, HANDLE_RADIUS + 0.5, handle_stroke);
        }
        return;
    }

    if stroke.tool == Tool::Number {
        let c = to_screen(stroke.points[0]);
        let r = (stroke.width * 4.0).max(14.0) * sx.min(sy) + 4.0;
        painter.circle_stroke(c, r, box_stroke);
        return;
    }

    if let Some((min_x, min_y, max_x, max_y)) = stroke.bounds() {
        let srect = Rect::from_min_max(
            to_screen(Point { x: min_x, y: min_y }),
            to_screen(Point { x: max_x, y: max_y }),
        )
        .expand(4.0);
        painter.rect_stroke(srect, 0.0, box_stroke, egui::StrokeKind::Outside);
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
        state.last_cursor = None;
        return;
    }

    let img_w = base.width.max(1) as f32;
    let img_h = base.height.max(1) as f32;
    let sx = target_rect.width() / img_w;
    let sy = target_rect.height() / img_h;
    let tol_img = (8.0 / sx.min(sy).max(0.1)).max(4.0);

    let to_img = |p: Pos2| -> Point {
        Point {
            x: ((p.x - target_rect.min.x) / sx).clamp(0.0, img_w),
            y: ((p.y - target_rect.min.y) / sy).clamp(0.0, img_h),
        }
    };
    let to_screen = |p: Point| -> Pos2 {
        Pos2::new(target_rect.min.x + p.x * sx, target_rect.min.y + p.y * sy)
    };

    let resp = ui.allocate_rect(target_rect, Sense::click_and_drag());
    let accent = ui::ctx_palette(&ctx).accent;
    let pointer_pos = ui.input(|i| i.pointer.interact_pos());
    let hover_pos = ui.input(|i| i.pointer.hover_pos());
    let shift_down = ui.input(|i| i.modifiers.shift);

    // 1. Handle active element drag (move / resize / endpoint / scale / rotate).
    if resp.drag_started_by(egui::PointerButton::Primary) {
        if let Some(pos) = pointer_pos {
            let img_p = to_img(pos);
            let mut started_edit = false;

            // 1a. Check currently selected element's handles or hit region first.
            if let Some(sel_idx) = state.selected_index() {
                let hit = hit_test_selected_gizmo(
                    &state.strokes[sel_idx],
                    pos,
                    img_p,
                    tol_img,
                    sy,
                    &to_screen,
                );
                if let Some(gizmo_hit) = hit {
                    let s = &state.strokes[sel_idx];
                    let snapshot_before = state.strokes.clone();
                    match gizmo_hit {
                        GizmoHit::BoxHandle {
                            left,
                            right,
                            top,
                            bottom,
                        } => {
                            let p0 = s.points[0];
                            let p1 = *s.points.last().unwrap();
                            state.edit_drag = Some(EditDrag::ResizeBox {
                                stroke_id: s.id.clone(),
                                left,
                                right,
                                top,
                                bottom,
                                orig_min: Point {
                                    x: p0.x.min(p1.x),
                                    y: p0.y.min(p1.y),
                                },
                                orig_max: Point {
                                    x: p0.x.max(p1.x),
                                    y: p0.y.max(p1.y),
                                },
                                snapshot_before,
                            });
                            started_edit = true;
                        }
                        GizmoHit::Endpoint(endpoint_idx) => {
                            state.edit_drag = Some(EditDrag::MoveEndpoint {
                                stroke_id: s.id.clone(),
                                endpoint_idx,
                                snapshot_before,
                            });
                            started_edit = true;
                        }
                        GizmoHit::TextRotate => {
                            let (bx, by, bw, bh) = annotations::text_bounds(s);
                            let rot = s.rotation.unwrap_or(0.0);
                            let r = annotations::rotate_point(
                                Point {
                                    x: bw * 0.5,
                                    y: bh * 0.5,
                                },
                                rot,
                            );
                            let center_img = Point {
                                x: bx + r.x,
                                y: by + r.y,
                            };
                            let start_angle = (img_p.y - center_img.y)
                                .atan2(img_p.x - center_img.x)
                                .to_degrees();
                            state.edit_drag = Some(EditDrag::TextRotate {
                                stroke_id: s.id.clone(),
                                center_img,
                                start_angle,
                                orig_rot: rot,
                                snapshot_before,
                            });
                            started_edit = true;
                        }
                        GizmoHit::TextScale => {
                            let (bx, by, bw, bh) = annotations::text_bounds(s);
                            let rot = s.rotation.unwrap_or(0.0);
                            let r = annotations::rotate_point(
                                Point {
                                    x: bw * 0.5,
                                    y: bh * 0.5,
                                },
                                rot,
                            );
                            let center_img = Point {
                                x: bx + r.x,
                                y: by + r.y,
                            };
                            let orig_dist = (img_p.x - center_img.x)
                                .hypot(img_p.y - center_img.y)
                                .max(8.0);
                            state.edit_drag = Some(EditDrag::TextScale {
                                stroke_id: s.id.clone(),
                                center_img,
                                orig_dist,
                                orig_scale: s.scale.unwrap_or(1.0),
                                snapshot_before,
                            });
                            started_edit = true;
                        }
                        GizmoHit::Body => {
                            state.edit_drag = Some(EditDrag::Move {
                                stroke_id: s.id.clone(),
                                start_img: img_p,
                                orig_points: s.points.clone(),
                                snapshot_before,
                            });
                            started_edit = true;
                        }
                    }
                }
            }

            // 1b. If not on currently selected gizmo, check layered hit-test for matching tool (or any tool in Select mode).
            if !started_edit {
                if let Some(hit_idx) =
                    hit_test_layers(&state.strokes, img_p, tol_img, state.tool, false)
                {
                    let id = state.strokes[hit_idx].id.clone();
                    state.select_layer(id.clone(), false);
                    if let Some(idx) = state.selected_index() {
                        let snapshot_before = state.strokes.clone();
                        let orig_points = state.strokes[idx].points.clone();
                        state.edit_drag = Some(EditDrag::Move {
                            stroke_id: id,
                            start_img: img_p,
                            orig_points,
                            snapshot_before,
                        });
                        started_edit = true;
                    }
                }
            }

            // 1c. Otherwise start a new creation draft for drag-based drawing tools.
            if !started_edit
                && state.tool != Tool::Select
                && !state.tool.is_text()
                && state.tool != Tool::Number
            {
                let opts = state.per_tool.get(&state.tool).cloned().unwrap_or_default();
                let is_eraser = state.tool == Tool::Eraser;
                if is_eraser {
                    state.finish_text();
                }
                state.draft = Some(Stroke {
                    id: Uuid::new_v4().to_string(),
                    tool: state.tool,
                    points: vec![img_p, img_p],
                    color: opts.color,
                    fill_color: if state.tool.supports_fill() {
                        opts.fill_color
                    } else {
                        None
                    },
                    width: opts.width,
                    text: None,
                    number: None,
                    font_size: None,
                    font_family: None,
                    opacity: Some(opts.opacity),
                    scale: None,
                    rotation: None,
                });
                state.draft_start_screen = Some(pos);
                state.draft_moved = is_eraser;
                if is_eraser {
                    state.texture_dirty = true;
                }
            }
        }
    }

    // 2. Continue active edit drag or drawing draft.
    if let Some(drag) = state.edit_drag.clone() {
        if let Some(pos) = pointer_pos {
            let img_p = to_img(pos);
            let target_id = match &drag {
                EditDrag::Move { stroke_id, .. }
                | EditDrag::ResizeBox { stroke_id, .. }
                | EditDrag::MoveEndpoint { stroke_id, .. }
                | EditDrag::TextScale { stroke_id, .. }
                | EditDrag::TextRotate { stroke_id, .. } => stroke_id.as_str(),
            };
            if let Some(stroke) = state.strokes.iter_mut().find(|s| s.id == target_id) {
                match &drag {
                    EditDrag::Move {
                        start_img,
                        orig_points,
                        ..
                    } => {
                        let dx = img_p.x - start_img.x;
                        let dy = img_p.y - start_img.y;
                        if dx.hypot(dy) > 0.25 {
                            stroke.points = orig_points.clone();
                            stroke.translate(dx, dy, img_w, img_h);
                            state.texture_dirty = true;
                            state.dirty = true;
                        }
                    }
                    EditDrag::ResizeBox {
                        left,
                        right,
                        top,
                        bottom,
                        orig_min,
                        orig_max,
                        ..
                    } => {
                        let mut x0 = orig_min.x;
                        let mut y0 = orig_min.y;
                        let mut x1 = orig_max.x;
                        let mut y1 = orig_max.y;
                        if *left {
                            x0 = img_p.x.clamp(0.0, img_w);
                        }
                        if *right {
                            x1 = img_p.x.clamp(0.0, img_w);
                        }
                        if *top {
                            y0 = img_p.y.clamp(0.0, img_h);
                        }
                        if *bottom {
                            y1 = img_p.y.clamp(0.0, img_h);
                        }
                        if (x1 - x0).abs() < 2.0 {
                            if *left {
                                x0 = (x1 - 2.0).max(0.0);
                            } else {
                                x1 = (x0 + 2.0).min(img_w);
                            }
                        }
                        if (y1 - y0).abs() < 2.0 {
                            if *top {
                                y0 = (y1 - 2.0).max(0.0);
                            } else {
                                y1 = (y0 + 2.0).min(img_h);
                            }
                        }
                        stroke.points = vec![
                            Point {
                                x: x0.min(x1),
                                y: y0.min(y1),
                            },
                            Point {
                                x: x0.max(x1),
                                y: y0.max(y1),
                            },
                        ];
                        state.texture_dirty = true;
                        state.dirty = true;
                    }
                    EditDrag::MoveEndpoint { endpoint_idx, .. } => {
                        if stroke.points.len() >= 2 {
                            if *endpoint_idx == 0 {
                                stroke.points[0] = img_p;
                            } else if let Some(last) = stroke.points.last_mut() {
                                *last = img_p;
                            }
                            state.texture_dirty = true;
                            state.dirty = true;
                        }
                    }
                    EditDrag::TextScale {
                        center_img,
                        orig_dist,
                        orig_scale,
                        ..
                    } => {
                        let dist = (img_p.x - center_img.x)
                            .hypot(img_p.y - center_img.y)
                            .max(4.0);
                        let next_scale = (*orig_scale * (dist / *orig_dist)).clamp(0.2, 10.0);
                        stroke.scale = Some(next_scale);
                        state.texture_dirty = true;
                        state.dirty = true;
                    }
                    EditDrag::TextRotate {
                        center_img,
                        start_angle,
                        orig_rot,
                        ..
                    } => {
                        let cur_angle = (img_p.y - center_img.y)
                            .atan2(img_p.x - center_img.x)
                            .to_degrees();
                        let mut deg = *orig_rot + (cur_angle - *start_angle);
                        while deg > 180.0 {
                            deg -= 360.0;
                        }
                        while deg < -180.0 {
                            deg += 360.0;
                        }
                        if shift_down {
                            deg = (deg / 15.0).round() * 15.0;
                        }
                        stroke.rotation = Some(deg);
                        state.texture_dirty = true;
                        state.dirty = true;
                    }
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(done) = state.edit_drag.take() {
                let snapshot_before = match done {
                    EditDrag::Move {
                        snapshot_before, ..
                    }
                    | EditDrag::ResizeBox {
                        snapshot_before, ..
                    }
                    | EditDrag::MoveEndpoint {
                        snapshot_before, ..
                    }
                    | EditDrag::TextScale {
                        snapshot_before, ..
                    }
                    | EditDrag::TextRotate {
                        snapshot_before, ..
                    } => snapshot_before,
                };
                if snapshot_before != state.strokes {
                    state.push_undo_snapshot(snapshot_before);
                }
            }
        }
    } else if state.draft.is_some() {
        if resp.dragged() {
            if let Some(pos) = pointer_pos {
                let moved_enough = state
                    .draft_start_screen
                    .is_some_and(|start| pos.distance(start) >= MIN_DRAG_CREATE_PX);
                if moved_enough && !state.draft_moved {
                    state.draft_moved = true;
                    state.finish_text();
                }
                if state.draft_moved {
                    if let Some(draft) = state.draft.as_mut() {
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
                }
            }
        }
        if resp.drag_stopped() {
            let moved = state.draft_moved;
            state.draft_start_screen = None;
            state.draft_moved = false;
            if let Some(draft) = state.draft.take() {
                if moved {
                    state.push_undo();
                    state.strokes.push(draft);
                    state.dirty = true;
                    state.texture_dirty = true;
                } else if let Some(pos) = pointer_pos {
                    // Single click (no meaningful drag): check same-group layer hit or deselect.
                    let img_p = to_img(pos);
                    if let Some(hit_idx) =
                        hit_test_layers(&state.strokes, img_p, tol_img, state.tool, true)
                    {
                        let id = state.strokes[hit_idx].id.clone();
                        state.select_layer(id, true);
                    } else if state.has_selection() {
                        state.finish_text();
                    }
                }
            }
        }
    } else if resp.clicked() {
        // Click for Select / Text / Watermark / Number (no drag draft created).
        if let Some(pos) = resp.interact_pointer_pos() {
            let img_p = to_img(pos);
            let clicked_selected = state.selected_index().is_some_and(|idx| {
                hit_test_selected_gizmo(
                    &state.strokes[idx],
                    pos,
                    img_p,
                    tol_img,
                    sy,
                    &to_screen,
                )
                .is_some()
            });
            if !clicked_selected {
                if let Some(hit_idx) =
                    hit_test_layers(&state.strokes, img_p, tol_img, state.tool, true)
                {
                    let id = state.strokes[hit_idx].id.clone();
                    state.select_layer(id, state.tool != Tool::Select);
                } else if state.has_selection() {
                    state.finish_text();
                } else if state.tool == Tool::Number {
                    let opts = state.current_options();
                    let num = annotations::next_number(&state.strokes);
                    state.push_undo();
                    state.strokes.push(Stroke {
                        id: Uuid::new_v4().to_string(),
                        tool: Tool::Number,
                        points: vec![img_p],
                        color: opts.color,
                        fill_color: None,
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
                } else if state.tool.is_text() {
                    let opts = state.current_options();
                    let id = Uuid::new_v4().to_string();
                    state.push_undo();
                    state.strokes.push(Stroke {
                        id: id.clone(),
                        tool: state.tool,
                        points: vec![img_p],
                        color: opts.color,
                        fill_color: None,
                        width: opts.width,
                        text: Some(String::new()),
                        number: None,
                        font_size: Some(opts.font_size),
                        font_family: Some(opts.font_family),
                        opacity: Some(opts.opacity),
                        scale: Some(1.0),
                        rotation: Some(0.0),
                    });
                    state.selected_id = Some(id);
                    state.selected_is_new_text = true;
                    state.open_group = ToolGroup::of_tool(state.tool);
                    state.texture_dirty = true;
                }
            }
        }
    }

    // 3. Draw hover outline (when hovering an unselected hittable layer) and compute cursor icon.
    let mut desired_cursor = if state.tool == Tool::Select {
        CursorIcon::Default
    } else if state.tool.is_text() {
        CursorIcon::Text
    } else {
        CursorIcon::Crosshair
    };

    if let Some(drag) = &state.edit_drag {
        desired_cursor = match drag {
            EditDrag::ResizeBox {
                left,
                right,
                top,
                bottom,
                ..
            } => cursor_for_gizmo_hit(
                GizmoHit::BoxHandle {
                    left: *left,
                    right: *right,
                    top: *top,
                    bottom: *bottom,
                },
                true,
            ),
            _ => CursorIcon::Grabbing,
        };
    } else if state.draft.is_none() {
        if let Some(pos) = hover_pos.filter(|p| target_rect.contains(*p)) {
            if !ctx.is_pointer_over_area() || resp.hovered() {
                let img_p = to_img(pos);
                if let Some(sel_idx) = state.selected_index() {
                    if let Some(gh) = hit_test_selected_gizmo(
                        &state.strokes[sel_idx],
                        pos,
                        img_p,
                        tol_img,
                        sy,
                        &to_screen,
                    ) {
                        desired_cursor = cursor_for_gizmo_hit(gh, false);
                    } else if let Some(h_idx) =
                        hit_test_layers(&state.strokes, img_p, tol_img, state.tool, false)
                    {
                        if h_idx != sel_idx {
                            draw_hover_outline(
                                ui.painter(),
                                &state.strokes[h_idx],
                                accent,
                                sx,
                                sy,
                                &to_screen,
                            );
                            desired_cursor = CursorIcon::Grab;
                        }
                    }
                } else if let Some(h_idx) =
                    hit_test_layers(&state.strokes, img_p, tol_img, state.tool, false)
                {
                    draw_hover_outline(
                        ui.painter(),
                        &state.strokes[h_idx],
                        accent,
                        sx,
                        sy,
                        &to_screen,
                    );
                    desired_cursor = CursorIcon::Grab;
                }
            }
        }
    }

    state.last_cursor = Some(desired_cursor);
    if resp.hovered() || state.edit_drag.is_some() || state.draft.is_some() {
        ctx.set_cursor_icon(desired_cursor);
    }

    // 4. Draw selected element's gizmo + inline text editor popup when editing text/watermark.
    if let Some(sel_idx) = state.selected_index() {
        let selected_clone = state.strokes[sel_idx].clone();
        draw_selected_gizmo(ui.painter(), &selected_clone, accent, sx, sy, &to_screen);

        if selected_clone.tool.is_text() {
            let (bx, by, bw, bh) = annotations::text_bounds(&selected_clone);
            let rot = selected_clone.rotation.unwrap_or(0.0);
            let r_bl = annotations::rotate_point(Point { x: 0.0, y: bh }, rot);
            let p_bl = to_screen(Point {
                x: bx + r_bl.x,
                y: by + r_bl.y,
            });
            let _ = bw;
            let input_pos = Pos2::new(
                p_bl.x.clamp(
                    target_rect.left() + 6.0,
                    (target_rect.right() - 220.0).max(target_rect.left() + 6.0),
                ),
                (p_bl.y + 10.0).min((target_rect.bottom() - 80.0).max(target_rect.top() + 6.0)),
            );
            let mut finish_clicked = false;
            let mut delete_clicked = false;
            egui::Area::new(egui::Id::new("inline_text_editor_popup"))
                .fixed_pos(input_pos)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_width(220.0);
                        let stroke = &mut state.strokes[sel_idx];
                        let buf = stroke.text.get_or_insert_with(String::new);
                        if ui
                            .add(
                                egui::TextEdit::multiline(buf)
                                    .desired_rows(2)
                                    .desired_width(210.0)
                                    .hint_text(i18n::t(
                                        "Enter text; drag corner to scale, top handle to rotate",
                                    )),
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
                                delete_clicked = true;
                            }
                        });
                    });
                });
            if delete_clicked {
                state.delete_selected();
            } else if finish_clicked {
                state.finish_text();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_stroke(id: &str, tool: Tool, p0: (f32, f32), p1: (f32, f32), fill: Option<&str>) -> Stroke {
        Stroke {
            id: id.into(),
            tool,
            points: vec![Point { x: p0.0, y: p0.1 }, Point { x: p1.0, y: p1.1 }],
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
        }
    }

    #[test]
    fn layered_hit_test_matches_active_tool_and_z_order() {
        let strokes = vec![
            mk_stroke("filled-rect", Tool::Rect, (10.0, 10.0), (200.0, 200.0), Some("#f6ca45")),
            mk_stroke("hollow-rect", Tool::Rect, (20.0, 20.0), (180.0, 180.0), None),
            mk_stroke("arrow", Tool::Arrow, (50.0, 100.0), (150.0, 100.0), None),
            mk_stroke("mosaic", Tool::Mosaic, (120.0, 120.0), (170.0, 170.0), None),
        ];

        // On Rect tool: clicking border of top hollow rect hits hollow-rect (index 1).
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 100.0, y: 20.0 }, 6.0, Tool::Rect, false),
            Some(1)
        );
        // Clicking inside hollow-rect passes through to filled-rect underneath (index 0).
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 100.0, y: 100.0 }, 6.0, Tool::Rect, false),
            Some(0)
        );
        // On Arrow tool: hits arrow (index 2), ignores underlying filled-rect.
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 100.0, y: 100.0 }, 6.0, Tool::Arrow, false),
            Some(2)
        );
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 30.0, y: 30.0 }, 6.0, Tool::Arrow, false),
            None
        );
        // On Mosaic tool: hits mosaic (index 3).
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 145.0, y: 145.0 }, 6.0, Tool::Mosaic, false),
            Some(3)
        );
        // On Select tool: hits topmost element across all layers (mosaic at 145,145; arrow at 100,100).
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 145.0, y: 145.0 }, 6.0, Tool::Select, true),
            Some(3)
        );
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 100.0, y: 100.0 }, 6.0, Tool::Select, true),
            Some(2)
        );
    }

    #[test]
    fn group_fallback_allows_click_select_without_hijacking_different_subtool_drag() {
        let strokes = vec![mk_stroke(
            "ellipse",
            Tool::Ellipse,
            (40.0, 40.0),
            (140.0, 140.0),
            Some("#2789e8"),
        )];
        // Dragging on Rect tool (group_fallback = false) does not hijack the Ellipse.
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 90.0, y: 90.0 }, 6.0, Tool::Rect, false),
            None
        );
        // Clicking on Rect tool (group_fallback = true) selects the Ellipse in the same Geometry group.
        assert_eq!(
            hit_test_layers(&strokes, Point { x: 90.0, y: 90.0 }, 6.0, Tool::Rect, true),
            Some(0)
        );
    }
}
