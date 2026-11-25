//! Pure-Rust native UI layer (egui + wgpu + winit).
//!
//! Two persistent windows: standalone settings window (`settings_window`) and capture editor
//! (`editor_window`); other windows are transient (selection overlay, recording bar, pins, toast).

pub mod canvas;
pub mod editor_window;
pub mod gpu;
pub mod icons;
pub mod overlay;
pub mod pin_window;
pub mod recorder;
pub mod settings_window;
pub mod toast;

use crate::i18n;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke, TextureHandle, Vec2, Visuals};
use serde_json::Value;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

pub const DANGER: Color32 = Color32::from_rgb(0xd9, 0x48, 0x48);

const THEME_ACCENT_KEY: &str = "snapdock_theme_accent";

/// Accent color presets that combine freely with light, dark, or system theme mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ThemeAccent {
    Mint = 0,
    Ocean = 1,
    Indigo = 2,
    Violet = 3,
    Rose = 4,
    Coral = 5,
    Amber = 6,
    Slate = 7,
}

impl ThemeAccent {
    pub const ALL: &'static [ThemeAccent] = &[
        ThemeAccent::Mint,
        ThemeAccent::Ocean,
        ThemeAccent::Indigo,
        ThemeAccent::Violet,
        ThemeAccent::Rose,
        ThemeAccent::Coral,
        ThemeAccent::Amber,
        ThemeAccent::Slate,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mint => "mint",
            Self::Ocean => "ocean",
            Self::Indigo => "indigo",
            Self::Violet => "violet",
            Self::Rose => "rose",
            Self::Coral => "coral",
            Self::Amber => "amber",
            Self::Slate => "slate",
        }
    }

    pub fn label(self) -> &'static str {
        let key = match self {
            Self::Mint => "Mint",
            Self::Ocean => "Ocean",
            Self::Indigo => "Indigo",
            Self::Violet => "Violet",
            Self::Rose => "Rose",
            Self::Coral => "Coral",
            Self::Amber => "Amber",
            Self::Slate => "Slate",
        };
        i18n::t(key)
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "mint" => Some(Self::Mint),
            "ocean" => Some(Self::Ocean),
            "indigo" => Some(Self::Indigo),
            "violet" => Some(Self::Violet),
            "rose" => Some(Self::Rose),
            "coral" => Some(Self::Coral),
            "amber" => Some(Self::Amber),
            "slate" => Some(Self::Slate),
            _ => None,
        }
    }

    pub fn from_str_or_default(key: &str) -> Self {
        Self::from_key(key).unwrap_or(Self::Mint)
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Ocean,
            2 => Self::Indigo,
            3 => Self::Violet,
            4 => Self::Rose,
            5 => Self::Coral,
            6 => Self::Amber,
            7 => Self::Slate,
            _ => Self::Mint,
        }
    }

    pub const fn palette(self) -> ThemePalette {
        match self {
            // Preserve SnapDock's default hand-tuned Mint palette.
            Self::Mint => ThemePalette {
                accent: Color32::from_rgb(0x00, 0xaa, 0x66),
                accent_hover: Color32::from_rgb(0x0d, 0x96, 0x60),
                accent_bright: Color32::from_rgb(0x40, 0xd9, 0x9b),
                accent_hint: Color32::from_rgb(0x7c, 0xe0, 0xb8),
                toast_bg: Color32::from_rgb(0x12, 0x5c, 0x41),
                floating_bg: Color32::from_rgb(0x12, 0x1a, 0x18),
                floating_btn: Color32::from_rgb(0x22, 0x32, 0x2e),
                floating_btn_hover: Color32::from_rgb(0x2d, 0x43, 0x3d),
                dark_panel: Color32::from_rgb(0x11, 0x19, 0x17),
                dark_window: Color32::from_rgb(0x18, 0x23, 0x20),
                dark_extreme: Color32::from_rgb(0x0d, 0x14, 0x12),
                dark_faint: Color32::from_rgb(0x1c, 0x29, 0x26),
                dark_stroke: Color32::from_rgb(0x28, 0x3b, 0x35),
                dark_inactive: Color32::from_rgb(0x1e, 0x2c, 0x28),
                dark_inactive_stroke: Color32::from_rgb(0x2d, 0x42, 0x3b),
                dark_hovered: Color32::from_rgb(0x24, 0x36, 0x31),
                light_panel: Color32::from_rgb(0xf4, 0xf7, 0xf6),
                light_faint: Color32::from_rgb(0xeb, 0xf2, 0xef),
                light_stroke: Color32::from_rgb(0xdc, 0xe6, 0xe2),
                light_inactive: Color32::from_rgb(0xee, 0xf4, 0xf1),
                light_inactive_stroke: Color32::from_rgb(0xd4, 0xe1, 0xdc),
                light_hovered: Color32::from_rgb(0xe2, 0xee, 0xe9),
                shade_rgb: (7, 30, 27),
                border_bgr: rgb_to_bgr(0x00, 0xaa, 0x66),
            },
            Self::Ocean => ThemePalette::tinted(
                (0x25, 0x77, 0xe8),
                (0x1b, 0x66, 0xcc),
                (0x5c, 0xa3, 0xff),
                (0x91, 0xc3, 0xff),
                (0x17, 0x4b, 0x8f),
            ),
            Self::Indigo => ThemePalette::tinted(
                (0x55, 0x62, 0xe8),
                (0x44, 0x51, 0xd1),
                (0x85, 0x90, 0xff),
                (0xae, 0xb6, 0xff),
                (0x31, 0x3a, 0x94),
            ),
            Self::Violet => ThemePalette::tinted(
                (0x83, 0x52, 0xec),
                (0x70, 0x3f, 0xd9),
                (0xae, 0x87, 0xff),
                (0xcb, 0xb2, 0xff),
                (0x4c, 0x2b, 0x94),
            ),
            Self::Rose => ThemePalette::tinted(
                (0xdc, 0x48, 0x69),
                (0xc4, 0x38, 0x57),
                (0xff, 0x7c, 0x98),
                (0xff, 0xa6, 0xba),
                (0x8c, 0x25, 0x3d),
            ),
            Self::Coral => ThemePalette::tinted(
                (0xe0, 0x65, 0x2e),
                (0xc9, 0x55, 0x20),
                (0xff, 0x95, 0x66),
                (0xff, 0xb9, 0x99),
                (0x8a, 0x3c, 0x18),
            ),
            Self::Amber => ThemePalette::tinted(
                (0xcc, 0x82, 0x12),
                (0xb5, 0x71, 0x0b),
                (0xf2, 0xb4, 0x49),
                (0xfa, 0xd0, 0x82),
                (0x7d, 0x4e, 0x0b),
            ),
            Self::Slate => ThemePalette::tinted(
                (0x5c, 0x76, 0x8c),
                (0x4b, 0x62, 0x75),
                (0x8f, 0xab, 0xc2),
                (0xb6, 0xcb, 0xdc),
                (0x35, 0x46, 0x54),
            ),
        }
    }
}

const fn rgb_to_bgr(r: u8, g: u8, b: u8) -> u32 {
    ((b as u32) << 16) | ((g as u32) << 8) | (r as u32)
}

const fn mix_u8(base: u8, tint: u8, pct: u16) -> u8 {
    let inv = 100 - pct;
    ((base as u16 * inv + tint as u16 * pct + 50) / 100) as u8
}

const fn mix_rgb(base: (u8, u8, u8), tint: (u8, u8, u8), pct: u16) -> Color32 {
    Color32::from_rgb(
        mix_u8(base.0, tint.0, pct),
        mix_u8(base.1, tint.1, pct),
        mix_u8(base.2, tint.2, pct),
    )
}

/// Full theme palette (accent, floating bars, dark/light surfaces, and Win32 BGR border color).
#[derive(Clone, Copy, Debug)]
pub struct ThemePalette {
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_bright: Color32,
    pub accent_hint: Color32,
    pub toast_bg: Color32,
    pub floating_bg: Color32,
    pub floating_btn: Color32,
    pub floating_btn_hover: Color32,
    pub dark_panel: Color32,
    pub dark_window: Color32,
    pub dark_extreme: Color32,
    pub dark_faint: Color32,
    pub dark_stroke: Color32,
    pub dark_inactive: Color32,
    pub dark_inactive_stroke: Color32,
    pub dark_hovered: Color32,
    pub light_panel: Color32,
    pub light_faint: Color32,
    pub light_stroke: Color32,
    pub light_inactive: Color32,
    pub light_inactive_stroke: Color32,
    pub light_hovered: Color32,
    shade_rgb: (u8, u8, u8),
    pub border_bgr: u32,
}

impl ThemePalette {
    const fn tinted(
        accent: (u8, u8, u8),
        hover: (u8, u8, u8),
        bright: (u8, u8, u8),
        hint: (u8, u8, u8),
        toast: (u8, u8, u8),
    ) -> Self {
        let shade_r = mix_u8(12, accent.0, 12);
        let shade_g = mix_u8(14, accent.1, 12);
        let shade_b = mix_u8(18, accent.2, 12);
        Self {
            accent: Color32::from_rgb(accent.0, accent.1, accent.2),
            accent_hover: Color32::from_rgb(hover.0, hover.1, hover.2),
            accent_bright: Color32::from_rgb(bright.0, bright.1, bright.2),
            accent_hint: Color32::from_rgb(hint.0, hint.1, hint.2),
            toast_bg: Color32::from_rgb(toast.0, toast.1, toast.2),
            floating_bg: mix_rgb((18, 20, 23), accent, 6),
            floating_btn: mix_rgb((34, 38, 43), accent, 10),
            floating_btn_hover: mix_rgb((46, 51, 57), accent, 13),
            dark_panel: mix_rgb((17, 19, 22), accent, 6),
            dark_window: mix_rgb((24, 27, 31), accent, 7),
            dark_extreme: mix_rgb((13, 15, 18), accent, 5),
            dark_faint: mix_rgb((28, 32, 36), accent, 8),
            dark_stroke: mix_rgb((40, 46, 52), accent, 12),
            dark_inactive: mix_rgb((31, 35, 40), accent, 8),
            dark_inactive_stroke: mix_rgb((46, 52, 59), accent, 13),
            dark_hovered: mix_rgb((38, 43, 49), accent, 10),
            light_panel: mix_rgb((245, 246, 248), accent, 3),
            light_faint: mix_rgb((236, 239, 242), accent, 5),
            light_stroke: mix_rgb((221, 226, 231), accent, 7),
            light_inactive: mix_rgb((239, 242, 245), accent, 4),
            light_inactive_stroke: mix_rgb((213, 219, 225), accent, 9),
            light_hovered: mix_rgb((227, 232, 238), accent, 7),
            shade_rgb: (shade_r, shade_g, shade_b),
            border_bgr: rgb_to_bgr(accent.0, accent.1, accent.2),
        }
    }

    /// Dimmed overlay shade color outside the selection region.
    pub fn overlay_shade_color(&self) -> Color32 {
        Color32::from_rgba_unmultiplied(self.shade_rgb.0, self.shade_rgb.1, self.shade_rgb.2, 0x65)
    }

    /// Translucent dark floating toolbar background.
    pub fn floating_bg_alpha(&self, alpha: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(
            self.floating_bg.r(),
            self.floating_bg.g(),
            self.floating_bg.b(),
            alpha,
        )
    }

    /// Translucent toast notification background.
    pub fn toast_bg_alpha(&self, alpha: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(
            self.toast_bg.r(),
            self.toast_bg.g(),
            self.toast_bg.b(),
            alpha,
        )
    }

    /// High-contrast accent text color for both light and dark surfaces.
    pub fn accent_text(&self, dark: bool) -> Color32 {
        if dark {
            mix_rgb(
                (self.accent.r(), self.accent.g(), self.accent.b()),
                (255, 255, 255),
                26,
            )
        } else {
            mix_rgb(
                (self.accent.r(), self.accent.g(), self.accent.b()),
                (0, 0, 0),
                18,
            )
        }
    }

    /// Window clear color for the wgpu render pass.
    pub fn clear_color(&self, dark: bool) -> wgpu::Color {
        let c = if dark { self.dark_panel } else { self.light_panel };
        wgpu::Color {
            r: c.r() as f64 / 255.0,
            g: c.g() as f64 / 255.0,
            b: c.b() as f64 / 255.0,
            a: 1.0,
        }
    }
}

static ACTIVE_ACCENT: AtomicU8 = AtomicU8::new(ThemeAccent::Mint as u8);

pub fn set_active_accent(accent: ThemeAccent) {
    ACTIVE_ACCENT.store(accent as u8, Ordering::Relaxed);
}

pub fn active_accent() -> ThemeAccent {
    ThemeAccent::from_u8(ACTIVE_ACCENT.load(Ordering::Relaxed))
}

pub fn ctx_accent(ctx: &egui::Context) -> ThemeAccent {
    ctx.data(|d| d.get_temp::<ThemeAccent>(egui::Id::new(THEME_ACCENT_KEY)))
        .unwrap_or_else(active_accent)
}

pub fn ctx_palette(ctx: &egui::Context) -> ThemePalette {
    ctx_accent(ctx).palette()
}

/// Frame-by-frame GIF player shared by the editor and pin windows.
pub struct GifPlayer {
    pub frames: Arc<Vec<(crate::images::Rgba, u32)>>,
    pub index: usize,
    pub last_switch: Instant,
    pub texture: Option<TextureHandle>,
}

impl GifPlayer {
    pub fn new(frames: Arc<Vec<(crate::images::Rgba, u32)>>) -> Option<Self> {
        if frames.is_empty() {
            return None;
        }
        Some(Self {
            frames,
            index: 0,
            last_switch: Instant::now(),
            texture: None,
        })
    }

    /// Advance animation timing and return the current frame texture.
    pub fn current_texture(&mut self, ctx: &egui::Context, name: &str) -> Option<TextureHandle> {
        let max_side = ctx.input(|i| i.max_texture_side);
        let delay = std::time::Duration::from_millis(self.frames[self.index].1.max(20) as u64);
        let advance = self.last_switch.elapsed() >= delay;
        if advance {
            self.index = (self.index + 1) % self.frames.len();
            self.last_switch = Instant::now();
        }
        if advance || self.texture.is_none() {
            let img = rgba_to_color_image_fit(&self.frames[self.index].0, max_side);
            match self.texture.as_mut() {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture(name.to_string(), img, egui::TextureOptions::LINEAR))
                }
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
        self.texture.clone()
    }
}

/// Cache Windows CJK font definitions once with shared `Arc<FontData>` to avoid re-reading disk or copying ~20MB per window.
fn cached_font_definitions() -> &'static FontDefinitions {
    static FONTS: OnceLock<FontDefinitions> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = FontDefinitions::default();
        for path in [
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\simhei.ttf",
            "C:\\Windows\\Fonts\\simsun.ttc",
            "C:\\Windows\\Fonts\\Deng.ttf",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts.font_data.insert(
                    "cjk".to_owned(),
                    Arc::new(FontData::from_owned(bytes)),
                );
                fonts
                    .families
                    .entry(FontFamily::Proportional)
                    .or_default()
                    .insert(0, "cjk".to_owned());
                fonts
                    .families
                    .entry(FontFamily::Monospace)
                    .or_default()
                    .push("cjk".to_owned());
                break;
            }
        }
        fonts
    })
}

/// Warm the cached system CJK font file on a background thread so the first window opens without disk I/O.
pub fn preload_fonts() {
    let _ = cached_font_definitions();
}

/// Install Windows CJK fallback fonts on each `egui::Context` (zero-copy `Arc<FontData>` clone).
pub fn configure_egui(ctx: &egui::Context) {
    ctx.set_fonts(cached_font_definitions().clone());
}

/// Common UI text color presets (paired with Auto mode).
pub const TEXT_COLOR_PRESETS: &[(&str, &str)] = &[
    ("#141a18", "Deep Ink"),
    ("#334155", "Slate Charcoal"),
    ("#44342a", "Warm Espresso"),
    ("#1e3a5f", "Deep Navy"),
    ("#cbd5e1", "Moonlight Gray"),
    ("#ffffff", "Pure White"),
];

/// UI text color preference: `Auto` (dark on light / light on dark) or a custom HEX color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextColorSetting {
    Auto,
    Custom(Color32),
}

impl TextColorSetting {
    pub fn from_setting_str(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
            return Self::Auto;
        }
        let clean = trimmed.strip_prefix('#').unwrap_or("");
        if clean.len() == 6 && clean.chars().all(|c| c.is_ascii_hexdigit()) {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&clean[0..2], 16),
                u8::from_str_radix(&clean[2..4], 16),
                u8::from_str_radix(&clean[4..6], 16),
            ) {
                return Self::Custom(Color32::from_rgb(r, g, b));
            }
        }
        Self::Auto
    }

    pub fn as_hex_or_auto(self) -> String {
        match self {
            Self::Auto => "auto".into(),
            Self::Custom(c) => format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b()),
        }
    }

    pub fn display_label(self, dark: bool) -> String {
        match self {
            Self::Auto => {
                if dark {
                    i18n::t("Auto (Current: Light)").into()
                } else {
                    i18n::t("Auto (Current: Dark)").into()
                }
            }
            Self::Custom(c) => {
                let hex = format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
                if let Some((_, name)) = TEXT_COLOR_PRESETS
                    .iter()
                    .find(|(p, _)| p.eq_ignore_ascii_case(&hex))
                {
                    format!("{} ({})", i18n::t(name), hex.to_ascii_uppercase())
                } else {
                    format!("{} ({})", i18n::t("Custom"), hex.to_ascii_uppercase())
                }
            }
        }
    }

    /// Resolve into `(body_color, strong_color)`.
    pub fn resolve(self, dark: bool) -> (Color32, Color32) {
        match self {
            Self::Auto => {
                if dark {
                    (Color32::from_rgb(0xe6, 0xec, 0xea), Color32::WHITE)
                } else {
                    (
                        Color32::from_rgb(0x1f, 0x29, 0x26),
                        Color32::from_rgb(0x0d, 0x13, 0x11),
                    )
                }
            }
            Self::Custom(c) => (c, c),
        }
    }

    /// Check whether a custom text color has low contrast (< 3.0) against the current theme background.
    pub fn has_low_contrast(self, dark: bool, palette: ThemePalette) -> bool {
        let Self::Custom(fg) = self else {
            return false;
        };
        let bg = if dark {
            palette.dark_window
        } else {
            Color32::WHITE
        };
        contrast_ratio(fg, bg) < 3.0
    }
}

fn relative_luminance(c: Color32) -> f32 {
    fn channel(v: u8) -> f32 {
        let s = v as f32 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

pub fn is_dark_theme(settings: &Value) -> bool {
    match settings["general"]["theme"].as_str() {
        Some("dark") => true,
        Some("light") => false,
        _ => crate::native::system_prefers_dark(),
    }
}

pub fn theme_accent(settings: &Value) -> ThemeAccent {
    settings["general"]["accent"]
        .as_str()
        .map(ThemeAccent::from_str_or_default)
        .unwrap_or(ThemeAccent::Mint)
}

pub fn theme_text_color(settings: &Value) -> TextColorSetting {
    settings["general"]["textColor"]
        .as_str()
        .map(TextColorSetting::from_setting_str)
        .unwrap_or(TextColorSetting::Auto)
}

/// Apply SnapDock light/dark mode, accent palette, and text color to an `egui::Context`.
pub fn apply_theme(
    ctx: &egui::Context,
    dark: bool,
    accent: ThemeAccent,
    text_color: TextColorSetting,
) {
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(THEME_ACCENT_KEY), accent));
    let p = accent.palette();
    let (body_fg, strong_fg) = text_color.resolve(dark);
    let mut visuals = if dark {
        let mut v = Visuals::dark();
        v.panel_fill = p.dark_panel;
        v.window_fill = p.dark_window;
        v.extreme_bg_color = p.dark_extreme;
        v.faint_bg_color = p.dark_faint;
        v.override_text_color = Some(body_fg);
        v.widgets.noninteractive.bg_fill = p.dark_window;
        v.widgets.noninteractive.weak_bg_fill = p.dark_panel;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.dark_stroke);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, body_fg);
        v.widgets.inactive.bg_fill = p.dark_inactive;
        v.widgets.inactive.weak_bg_fill = p.dark_inactive;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.dark_inactive_stroke);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, body_fg);
        v.widgets.hovered.bg_fill = p.dark_hovered;
        v.widgets.hovered.weak_bg_fill = p.dark_hovered;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
        v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, strong_fg);
        v.widgets.active.bg_fill = p.accent;
        v.widgets.active.weak_bg_fill = p.accent;
        v.widgets.active.fg_stroke = Stroke::new(1.5_f32, strong_fg);
        v
    } else {
        let mut v = Visuals::light();
        let active_tint = mix_rgb((255, 255, 255), (p.accent.r(), p.accent.g(), p.accent.b()), 22);
        v.panel_fill = p.light_panel;
        v.window_fill = Color32::from_rgb(0xff, 0xff, 0xff);
        v.extreme_bg_color = Color32::from_rgb(0xff, 0xff, 0xff);
        v.faint_bg_color = p.light_faint;
        v.override_text_color = Some(body_fg);
        v.widgets.noninteractive.bg_fill = Color32::from_rgb(0xff, 0xff, 0xff);
        v.widgets.noninteractive.weak_bg_fill = p.light_panel;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.light_stroke);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, body_fg);
        v.widgets.inactive.bg_fill = p.light_inactive;
        v.widgets.inactive.weak_bg_fill = p.light_inactive;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.light_inactive_stroke);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, body_fg);
        v.widgets.hovered.bg_fill = p.light_hovered;
        v.widgets.hovered.weak_bg_fill = p.light_hovered;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
        v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, strong_fg);
        v.widgets.active.bg_fill = active_tint;
        v.widgets.active.weak_bg_fill = active_tint;
        v.widgets.active.fg_stroke = Stroke::new(1.5_f32, strong_fg);
        v
    };
    visuals.selection.bg_fill = p.accent.gamma_multiply(if dark { 0.30 } else { 0.16 });
    visuals.selection.stroke = Stroke::new(1.5_f32, p.accent_text(dark));
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(7);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(7);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(7);
    visuals.widgets.active.corner_radius = CornerRadius::same(7);
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 7.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
    });
}

/// Searchable language dropdown supporting filtering across all 42 languages by English, localized, native names, or codes.
/// Returns `Some(selected_code)` when the user picks a language (`"auto"` if `include_auto` is true).
pub fn searchable_language_picker(
    ui: &mut egui::Ui,
    id_salt: &str,
    current_code: &str,
    fallback_label: &str,
    include_auto: bool,
    width: f32,
    search_query: &mut String,
) -> Option<String> {
    let popup_id = ui.make_persistent_id(egui::Id::new(id_salt));
    let is_open = ui.memory(|m| m.is_popup_open(popup_id));

    let display_text = if include_auto && current_code == "auto" {
        i18n::t("Auto Detect").to_string()
    } else if let Some(lang) = crate::translate::find_language(current_code) {
        lang.option_label()
    } else if !fallback_label.trim().is_empty() {
        fallback_label.to_string()
    } else {
        current_code.to_string()
    };

    let button_size = Vec2::new(width, 28.0);
    let (rect, resp) = ui.allocate_exact_size(button_size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let (fill, stroke, fg) = if is_open || resp.hovered() {
            (
                visuals.widgets.hovered.bg_fill,
                Stroke::new(1.0_f32, ctx_palette(ui.ctx()).accent),
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
            CornerRadius::same(7),
            fill,
            stroke,
            egui::StrokeKind::Inside,
        );

        let arrow_size = 11.0_f32;
        let pad_x = 9.0_f32;
        let max_text_w = (rect.width() - pad_x * 2.0 - arrow_size - 4.0).max(20.0);
        let font_id = egui::TextStyle::Body.resolve(ui.style());
        let mut job = egui::text::LayoutJob::single_section(
            display_text,
            egui::TextFormat::simple(font_id, fg),
        );
        job.wrap = egui::text::TextWrapping {
            max_width: max_text_w,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = ui.painter().layout_job(job);
        let text_pos = egui::Pos2::new(rect.min.x + pad_x, rect.center().y - galley.size().y * 0.5);
        ui.painter().galley(text_pos, galley, fg);

        let arrow_rect = egui::Rect::from_center_size(
            egui::Pos2::new(rect.max.x - pad_x - arrow_size * 0.5, rect.center().y),
            Vec2::splat(arrow_size),
        );
        icons::paint(
            ui.painter(),
            arrow_rect,
            if is_open {
                icons::Icon::ChevronUp
            } else {
                icons::Icon::ChevronDown
            },
            fg,
        );
    }

    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
        if !is_open {
            search_query.clear();
        }
    }

    let mut picked: Option<String> = None;
    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &resp,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            let popup_w = width.max(230.0);
            ui.set_min_width(popup_w);
            ui.set_max_width(popup_w + 30.0);

            ui.horizontal(|ui| {
                let clear_w = if search_query.is_empty() { 0.0 } else { 26.0 };
                let edit_resp = ui.add(
                    egui::TextEdit::singleline(search_query)
                        .desired_width((popup_w - clear_w - 6.0).max(120.0))
                        .hint_text(i18n::t("Search language (name / code)...")),
                );
                if !is_open {
                    edit_resp.request_focus();
                }
                if !search_query.is_empty()
                    && icons::small_icon_button(ui, icons::Icon::Close)
                        .on_hover_text(i18n::t("Clear search"))
                        .clicked()
                {
                    search_query.clear();
                }
            });
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(2.0);

            let mut any_shown = false;
            egui::ScrollArea::vertical()
                .id_salt(format!("{}_scroll", id_salt))
                .max_height(240.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_min_width(popup_w - 8.0);
                    if include_auto && crate::translate::matches_auto_query(search_query) {
                        any_shown = true;
                        let selected = current_code == "auto";
                        if ui
                            .selectable_label(selected, i18n::t("Auto Detect (auto)"))
                            .clicked()
                        {
                            picked = Some("auto".to_string());
                        }
                    }
                    for lang in crate::translate::LANGUAGES {
                        if !crate::translate::matches_language_query(lang, search_query) {
                            continue;
                        }
                        any_shown = true;
                        let selected = current_code == lang.code;
                        let label = format!("{}  [{}]", lang.option_label(), lang.code);
                        if ui.selectable_label(selected, label).clicked() {
                            picked = Some(lang.code.to_string());
                        }
                    }
                    if !any_shown {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(i18n::t("No matching languages found"))
                                .weak()
                                .small(),
                        );
                        ui.add_space(6.0);
                    }
                });

            if picked.is_some() {
                search_query.clear();
                ui.memory_mut(|m| m.close_popup());
            }
        },
    );

    picked
}

/// Pill-style toggle switch.
pub fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = Vec2::new(38.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let radius = rect.height() * 0.5;
        let bg = if *on {
            ctx_palette(ui.ctx()).accent
        } else {
            ui.visuals().widgets.inactive.bg_stroke.color
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius as u8), bg);
        let cx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        ui.painter().circle_filled(
            egui::pos2(cx, rect.center().y),
            radius - 2.5,
            Color32::WHITE,
        );
    }
    response
}

/// Convert `Rgba` to an `egui::ColorImage` at its original resolution.
#[allow(dead_code)]
pub fn rgba_to_color_image(rgba: &crate::images::Rgba) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width as usize, rgba.height as usize],
        &rgba.pixels,
    )
}

/// Fast path for opaque RGBA buffers (alpha == 255 for all pixels, e.g. screen captures).
/// Skips per-pixel alpha premultiplication math since unmultiplied == premultiplied at alpha 255.
pub fn rgba_opaque_to_color_image_fit(
    rgba: &crate::images::Rgba,
    max_side: usize,
) -> egui::ColorImage {
    let limit = max_side.max(64) as u32;
    if rgba.width <= limit && rgba.height <= limit {
        return egui::ColorImage::from_rgba_premultiplied(
            [rgba.width as usize, rgba.height as usize],
            &rgba.pixels,
        );
    }
    rgba_to_color_image_fit(rgba, max_side)
}

/// Convert `Rgba` to an `egui::ColorImage` clamped within the GPU's max texture dimension.
/// Standard resolutions pass through 1:1; only oversized captures scale proportionally to prevent upload crashes.
pub fn rgba_to_color_image_fit(rgba: &crate::images::Rgba, max_side: usize) -> egui::ColorImage {
    let limit = max_side.max(64) as u32;
    if rgba.width <= limit && rgba.height <= limit {
        return egui::ColorImage::from_rgba_unmultiplied(
            [rgba.width as usize, rgba.height as usize],
            &rgba.pixels,
        );
    }
    let scale = (limit as f64 / rgba.width.max(1) as f64)
        .min(limit as f64 / rgba.height.max(1) as f64);
    let nw = ((rgba.width as f64 * scale).floor() as u32).clamp(1, limit);
    let nh = ((rgba.height as f64 * scale).floor() as u32).clamp(1, limit);
    let resized = image::DynamicImage::ImageRgba8(rgba.buffer())
        .resize_exact(nw, nh, image::imageops::FilterType::Triangle)
        .to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied([nw as usize, nh as usize], &resized.into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::Rgba;

    #[test]
    fn color_image_fit_preserves_resolution_within_limit() {
        let img = Rgba::new(2560, 1440, vec![120; 2560 * 1440 * 4]);
        let color = rgba_to_color_image_fit(&img, 16384);
        assert_eq!(color.size, [2560, 1440]);
    }

    #[test]
    fn opaque_color_image_fit_matches_unmultiplied_for_opaque_pixels() {
        let pixels = vec![10, 20, 30, 255, 240, 180, 90, 255, 0, 0, 0, 255, 255, 255, 255, 255];
        let img = Rgba::new(2, 2, pixels);
        let fast = rgba_opaque_to_color_image_fit(&img, 16384);
        let ref_img = rgba_to_color_image_fit(&img, 16384);
        assert_eq!(fast.size, ref_img.size);
        assert_eq!(fast.pixels, ref_img.pixels);
    }

    #[test]
    fn dropped_texture_handle_appears_in_tex_manager_free_delta() {
        let ctx = egui::Context::default();
        let img = egui::ColorImage::new([4, 4], Color32::WHITE);
        let handle = ctx.load_texture("pool_test_tex", img, egui::TextureOptions::NEAREST);
        let tex_id = handle.id();
        let _ = ctx.tex_manager().write().take_delta();
        drop(handle);
        let delta = ctx.tex_manager().write().take_delta();
        assert!(delta.free.contains(&tex_id));
    }

    #[test]
    fn color_image_fit_clamps_oversized_texture_preserving_aspect() {
        let img = Rgba::new(2560, 1440, vec![80; 2560 * 1440 * 4]);
        let color = rgba_to_color_image_fit(&img, 2048);
        assert_eq!(color.size[0], 2048);
        assert_eq!(color.size[1], 1152);
    }

    #[test]
    fn all_theme_accents_roundtrip_and_apply_to_context() {
        assert_eq!(ThemeAccent::ALL.len(), 8);
        assert_eq!(ThemeAccent::Mint.palette().border_bgr, 0x0066AA00);
        let ctx = egui::Context::default();
        for &accent in ThemeAccent::ALL {
            assert_eq!(ThemeAccent::from_key(accent.as_str()), Some(accent));
            assert!(!accent.label().is_empty());
            for dark in [false, true] {
                apply_theme(&ctx, dark, accent, TextColorSetting::Auto);
                assert_eq!(ctx_accent(&ctx), accent);
                assert_eq!(ctx.style().visuals.dark_mode, dark);
                let strong = ctx.style().visuals.strong_text_color();
                let body = ctx.style().visuals.text_color();
                if dark {
                    assert_eq!(strong, Color32::WHITE);
                    assert!(body.r() > 200);
                } else {
                    assert!(strong.r() < 40 && strong.g() < 40 && strong.b() < 40);
                    assert!(body.r() < 60 && body.g() < 60 && body.b() < 60);
                }
            }
        }
        let custom = TextColorSetting::from_setting_str("#1e3a5f");
        assert_eq!(custom.as_hex_or_auto(), "#1e3a5f");
        apply_theme(&ctx, false, ThemeAccent::Mint, custom);
        assert_eq!(ctx.style().visuals.text_color(), Color32::from_rgb(0x1e, 0x3a, 0x5f));
        assert_eq!(ctx.style().visuals.strong_text_color(), Color32::from_rgb(0x1e, 0x3a, 0x5f));
        assert!(TextColorSetting::Custom(Color32::WHITE).has_low_contrast(false, ThemeAccent::Mint.palette()));
        assert!(!TextColorSetting::Custom(Color32::WHITE).has_low_contrast(true, ThemeAccent::Mint.palette()));
        assert_eq!(ThemeAccent::from_str_or_default("unknown"), ThemeAccent::Mint);
    }
}
