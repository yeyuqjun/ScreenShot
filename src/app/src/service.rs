//! Application service orchestration: capture workflow, in-memory sessions, export,
//! preferences, encrypted cloud sync, and OCR/translation.

use crate::annotations::{self, Stroke};
use crate::clipboard;
use crate::dxgi;
use crate::export::{self, SaveOptions};
use crate::i18n;
use crate::images::{self, PixelRect, Rgba};
use crate::native::{self, NativeWindow};
use crate::network;
use crate::settings;
use crate::state::{
    Account, ActiveCapture, App, AppEvent, CaptureItem, Core, OverlayEntry, PinOverlayMode,
    SessionCapture, TextTarget,
};
use crate::vault;
use base64::Engine;
use serde_json::{json, Value};
use std::io::Write as _;
use std::sync::Arc;
use uuid::Uuid;

pub struct AddOptions {
    pub title: Option<String>,
    pub annotations: Vec<Stroke>,
    pub source_rgba: Option<Rgba>,
    pub skip_auto_save: bool,
    pub skip_auto_copy: bool,
    pub saved_path: Option<String>,
    pub override_buffer: Option<Vec<u8>>,
    pub gif_frames: Option<Arc<Vec<(Rgba, u32)>>>,
    pub capture_origin: Option<(i32, i32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectAction {
    /// Copy to clipboard and finish (default confirm action for region capture).
    Copy,
    /// Open the capture editor window for annotating and saving.
    Save,
    /// Open the capture editor window and immediately run OCR.
    Ocr,
    Pin,
    Long,
    Gif,
}

// ============================================================================
// Capture orchestration
// ============================================================================

/// Start a capture session (`region`, `long`, `gif`, `pin`, or `clipboard`).
pub fn start_capture(app: &Arc<App>, mode: &str, from_tray: bool) -> bool {
    if mode == "clipboard" {
        return pin_clipboard(app, from_tray);
    }
    if !matches!(mode, "region" | "long" | "gif" | "pin") {
        app.notify(i18n::t("Unknown capture mode"), true);
        return false;
    }
    let (delay, detect_windows) = {
        let mut core = app.core.lock().unwrap();
        if core.active_capture.is_some() || core.recording.is_some() {
            app.notify(
                i18n::t("Please finish the current capture or recording first"),
                true,
            );
            return false;
        }
        let delay = core.settings["capture"]["delay"].as_i64().unwrap_or(0);
        let detect = core.settings["capture"]["detectWindows"]
            .as_bool()
            .unwrap_or(true);
        core.active_capture = Some(ActiveCapture {
            mode: mode.to_string(),
            entries: Vec::new(),
            windows: Vec::new(),
        });
        (delay, detect)
    };
    let app_bg = app.clone();
    let perf = std::env::var_os("SNAPDOCK_PERF_DEBUG").is_some();
    let t0 = std::time::Instant::now();
    app.runtime.spawn(async move {
        let app = app_bg;
        // Only wait for the system tray menu to dismiss when triggered from the tray.
        let pre_delay_ms = if from_tray { 60i64 } else { 0 };
        let wait_ms = pre_delay_ms + delay.max(0) * 1000;
        if wait_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms as u64)).await;
        }
        let t_grab = std::time::Instant::now();
        let (windows, frames_res) = tokio::join!(
            async {
                if detect_windows {
                    tokio::task::spawn_blocking(native::list_windows)
                        .await
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            },
            tokio::task::spawn_blocking(dxgi::grab_all_screens),
        );
        let frames = match frames_res {
            Ok(Ok(frames)) => frames,
            Ok(Err(e)) => {
                cancel_capture(&app);
                app.notify(e, true);
                return;
            }
            Err(e) => {
                cancel_capture(&app);
                app.notify(e.to_string(), true);
                return;
            }
        };
        if perf {
            eprintln!(
                "[perf] capture grab+windows: {:?} (total since start_capture: {:?}, monitors={})",
                t_grab.elapsed(),
                t0.elapsed(),
                frames.len()
            );
        }
        let mut entries = Vec::with_capacity(frames.len());
        for (index, frame) in frames.into_iter().enumerate() {
            let image = Arc::new(Rgba::new(frame.width, frame.height, frame.pixels));
            entries.push(OverlayEntry {
                index,
                monitor_x: frame.rect.left,
                monitor_y: frame.rect.top,
                monitor_width: frame.width,
                monitor_height: frame.height,
                image,
            });
        }
        {
            let mut core = app.core.lock().unwrap();
            let Some(active) = core.active_capture.as_mut() else {
                return;
            };
            active.entries = entries;
            active.windows = windows;
        }
        app.emit(AppEvent::OpenOverlays);
    });
    true
}

/// Cancel active capture and close all overlay windows.
pub fn cancel_capture(app: &Arc<App>) {
    app.core.lock().unwrap().active_capture = None;
    app.emit(AppEvent::CancelCapture);
}

/// Hit-test hovered window or child UI control (returns monitor-local physical rect and title).
pub fn hover_window(
    app: &App,
    entry_index: usize,
    local_px_x: i32,
    local_px_y: i32,
) -> Option<(f32, f32, f32, f32, String)> {
    let (mx, my, mw, mh, px, py, target, detect_controls) = {
        let core = app.core.lock().unwrap();
        let active = core.active_capture.as_ref()?;
        let entry = active.entries.get(entry_index)?;
        let mx = entry.monitor_x;
        let my = entry.monitor_y;
        let px = mx + local_px_x;
        let py = my + local_px_y;
        let target = active
            .windows
            .iter()
            .find(|w| px >= w.x && py >= w.y && px < w.x + w.width && py < w.y + w.height)?
            .clone();
        (
            mx,
            my,
            entry.monitor_width as i32,
            entry.monitor_height as i32,
            px,
            py,
            target,
            core.settings["capture"]["detectControls"]
                .as_bool()
                .unwrap_or(false),
        )
    };
    let candidate = if detect_controls {
        native::inspect_control(&target, px, py)
            .map(|value| NativeWindow {
                hwnd: value
                    .get("hwnd")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(target.hwnd as i64) as isize,
                title: value
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| i18n::t("UI Control"))
                    .to_string(),
                x: value.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                y: value.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                width: value.get("width").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                height: value.get("height").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            })
            .filter(|c| c.width > 0 && c.height > 0)
            .or_else(|| native::child_at(&target, px, py))
            .unwrap_or_else(|| target.clone())
    } else {
        target
    };
    let x0 = (candidate.x - mx).clamp(0, mw);
    let y0 = (candidate.y - my).clamp(0, mh);
    let x1 = (candidate.x + candidate.width - mx).clamp(0, mw);
    let y1 = (candidate.y + candidate.height - my).clamp(0, mh);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((
        x0 as f32,
        y0 as f32,
        (x1 - x0) as f32,
        (y1 - y0) as f32,
        candidate.title,
    ))
}

/// Commit a capture selection (physical pixel rect, inline strokes, and action).
pub fn select_capture(
    app: &Arc<App>,
    entry_index: usize,
    rect: PixelRect,
    strokes: Vec<Stroke>,
    action: SelectAction,
    scroll_mode: &str,
) -> Result<(), String> {
    if rect.width < 3 || rect.height < 3 {
        return Err(i18n::t("Please select a valid capture area").into());
    }
    let (monitor, source_rgba) = {
        let core = app.core.lock().unwrap();
        let active = core
            .active_capture
            .as_ref()
            .ok_or_else(|| i18n::t("Capture overlay has closed"))?;
        let entry = active
            .entries
            .get(entry_index)
            .ok_or_else(|| i18n::t("Capture overlay has closed"))?;
        let cropped = images::crop_pixels(&entry.image, rect)?;
        (
            (
                entry.monitor_x,
                entry.monitor_y,
                entry.monitor_width,
                entry.monitor_height,
            ),
            cropped,
        )
    };

    // Close all selection overlays.
    app.core.lock().unwrap().active_capture = None;
    app.emit(AppEvent::CancelCapture);

    // Hand off to scrolling capture or GIF recording.
    if matches!(action, SelectAction::Long | SelectAction::Gif) {
        let mode = if action == SelectAction::Long { "long" } else { "gif" };
        let cx = monitor.0 + rect.x as i32 + rect.width as i32 / 2;
        let cy = monitor.1 + rect.y as i32 + rect.height as i32 / 2;
        let target_hwnd = native::window_at(cx, cy).map(|w| w.hwnd).unwrap_or(0);
        return crate::recording::begin_recording(
            app,
            mode,
            rect,
            monitor,
            scroll_mode,
            target_hwnd,
        );
    }

    let composed = annotations::rasterize_annotations(&source_rgba, &strokes);
    let item = add_capture(
        app,
        &composed,
        "image",
        AddOptions {
            title: None,
            annotations: strokes,
            source_rgba: Some(source_rgba),
            // Skip auto-save when opening the editor so the user saves manually.
            skip_auto_save: matches!(action, SelectAction::Save | SelectAction::Ocr),
            // Skip auto-copy for explicit Copy action to avoid double clipboard lock contention.
            skip_auto_copy: action == SelectAction::Copy,
            saved_path: None,
            override_buffer: None,
            gif_frames: None,
            capture_origin: Some((monitor.0 + rect.x as i32, monitor.1 + rect.y as i32)),
        },
    )?;

    match action {
        SelectAction::Pin => {
            app.emit(AppEvent::CreatePin {
                id: item.id.clone(),
            });
        }
        SelectAction::Copy => {
            match clipboard::write_image(&composed) {
                Ok(()) => app.notify(i18n::t("Screenshot copied to clipboard"), false),
                Err(e) => app.notify(e, true),
            }
            release_capture(app, &item.id);
        }
        SelectAction::Ocr => open_result_with_ocr(app, &item.id)?,
        _ => open_result(app, &item.id)?,
    }
    Ok(())
}

/// Pin clipboard image directly, or enter pin-capture mode if the clipboard has no image.
pub fn pin_clipboard(app: &Arc<App>, from_tray: bool) -> bool {
    match clipboard::read_image() {
        Ok(image) => {
            match add_capture(
                app,
                &image,
                "image",
                AddOptions {
                    title: Some(i18n::t("Pin Clipboard").into()),
                    annotations: Vec::new(),
                    source_rgba: Some(image.clone()),
                    skip_auto_save: false,
                    skip_auto_copy: true,
                    saved_path: None,
                    override_buffer: None,
                    gif_frames: None,
                    capture_origin: None,
                },
            ) {
                Ok(item) => app.emit(AppEvent::CreatePin { id: item.id }),
                Err(e) => app.notify(e, true),
            }
            false
        }
        Err(_) => start_capture(app, "pin", from_tray),
    }
}

// ============================================================================
// In-memory capture session management
// ============================================================================

pub fn add_capture(
    app: &Arc<App>,
    image: &Rgba,
    kind: &str,
    options: AddOptions,
) -> Result<CaptureItem, String> {
    let id = Uuid::new_v4().to_string();
    let file = format!("{}.{}", id, if kind == "gif" { "gif" } else { "png" });
    let title = options.title.unwrap_or_else(|| {
        match kind {
            "long" => i18n::t("Scrolling Capture"),
            "gif" => i18n::t("Animated Capture (GIF)"),
            _ => i18n::t("Region Capture"),
        }
        .to_string()
    });
    let created_at = vault::now_millis();
    let item = CaptureItem {
        id: id.clone(),
        file,
        kind: kind.to_string(),
        title,
        created_at,
        updated_at: None,
        saved_path: options.saved_path.clone(),
        width: image.width,
        height: image.height,
        poster: None,
    };
    let png_bytes = images::encode_png(image)?;
    let (buffer, poster) = match options.override_buffer {
        Some(buf) => (buf, Some(png_bytes)),
        None => (png_bytes, None),
    };

    let (copy_on_capture, history_limit, auto_save, directory, format, quality) = {
        let core = app.core.lock().unwrap();
        let s = &core.settings;
        (
            s["capture"]["copyOnCapture"].as_bool().unwrap_or(false),
            s["capture"]["historyLimit"].as_i64().unwrap_or(100),
            s["storage"]["autoSave"].as_bool().unwrap_or(false),
            s["storage"]["directory"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            s["storage"]["format"].as_str().unwrap_or("png").to_string(),
            s["storage"]["quality"].as_i64().unwrap_or(90),
        )
    };

    {
        let mut core = app.core.lock().unwrap();
        core.captures.insert(
            id.clone(),
            SessionCapture {
                item: item.clone(),
                buffer: buffer.clone(),
                poster,
                source_rgba: options.source_rgba,
                composed_rgba: Some(image.clone()),
                annotations: options.annotations,
                gif_frames: options.gif_frames,
                capture_origin: options.capture_origin,
            },
        );
        core.captures_order.push(id.clone());
        let overflow: Vec<String> = core
            .captures_order
            .iter()
            .rev()
            .skip(history_limit.max(0) as usize)
            .cloned()
            .collect();
        for older in overflow {
            if core.result_id.as_deref() != Some(older.as_str())
                && !core.pins.values().any(|p| p.capture_id == older)
            {
                core.captures.remove(&older);
            }
            core.captures_order.retain(|x| x != &older);
        }
    }

    if copy_on_capture && !options.skip_auto_copy && kind != "gif" {
        if clipboard::write_image(image).is_err() {
            app.notify(
                i18n::t("Screenshot captured, but the clipboard is currently unavailable"),
                true,
            );
        }
    }

    if auto_save && !options.skip_auto_save {
        let dir = if directory.is_empty() {
            app.picture_dir().to_string_lossy().to_string()
        } else {
            directory
        };
        let stamp = chrono_like_stamp(created_at);
        let ext = if kind == "gif" { "gif" } else { &format };
        let destination = std::path::Path::new(&dir).join(format!(
            "SnapDock_{}_{}.{}",
            stamp,
            &id[..8],
            ext
        ));
        let write_result = std::fs::create_dir_all(&dir)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                let bytes_to_write = if kind == "gif" {
                    buffer
                } else if format == "jpg" {
                    images::encode_jpeg(image, quality as u8)?
                } else {
                    images::encode_png(image)?
                };
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .and_then(|mut f| f.write_all(&bytes_to_write))
                    .map_err(|e| e.to_string())
            });
        match write_result {
            Ok(()) => {
                let mut core = app.core.lock().unwrap();
                if let Some(session) = core.captures.get_mut(&id) {
                    session.item.saved_path = Some(destination.to_string_lossy().to_string());
                }
            }
            Err(error) => {
                app.notify(
                    i18n::tf(
                        "Auto-save failed; you can save manually in the editor: {err}",
                        &[("err", &error)],
                    ),
                    true,
                );
            }
        }
    }

    let final_item = app
        .core
        .lock()
        .unwrap()
        .captures
        .get(&id)
        .map(|s| s.item.clone())
        .unwrap_or(item);
    Ok(final_item)
}

pub fn open_result(app: &Arc<App>, id: &str) -> Result<(), String> {
    set_active_result(app, id)?;
    app.emit(AppEvent::OpenResult { id: id.to_string() });
    Ok(())
}

pub fn open_result_with_ocr(app: &Arc<App>, id: &str) -> Result<(), String> {
    set_active_result(app, id)?;
    app.emit(AppEvent::OpenResultOcr { id: id.to_string() });
    Ok(())
}

fn set_active_result(app: &Arc<App>, id: &str) -> Result<(), String> {
    let mut core = app.core.lock().unwrap();
    let _ = find_image(&core, app, id)?;
    let previous = core.result_id.clone();
    core.result_id = Some(id.to_string());
    if let Some(prev) = previous {
        if prev != id {
            drop(core);
            release_capture(app, &prev);
        }
    }
    Ok(())
}

pub fn close_result(app: &Arc<App>, id: Option<&str>) {
    let target = {
        let mut core = app.core.lock().unwrap();
        let target = id
            .map(str::to_string)
            .or_else(|| core.result_id.clone());
        if let Some(ref t) = target {
            if core.result_id.as_deref() == Some(t.as_str()) {
                core.result_id = None;
                Some(t.clone())
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(t) = target {
        app.emit(AppEvent::ResultClosed { id: t.clone() });
        release_capture(app, &t);
    }
}

pub fn release_capture(app: &Arc<App>, id: &str) {
    let mut core = app.core.lock().unwrap();
    if core.result_id.as_deref() == Some(id)
        || core.pins.values().any(|pin| pin.capture_id == id)
    {
        return;
    }
    core.captures.remove(id);
    core.captures_order.retain(|item| item != id);
}

pub fn find_image(core: &Core, app: &App, id: &str) -> Result<CaptureItem, String> {
    if let Some(session) = core.captures.get(id) {
        return Ok(session.item.clone());
    }
    let item = core
        .history
        .iter()
        .find(|item| item.id == id)
        .ok_or_else(|| i18n::t("Capture does not exist or has been removed"))?;
    if !app.capture_dir.join(&item.file).exists() {
        return Err(i18n::t("Capture does not exist or has been removed").into());
    }
    Ok(item.clone())
}

pub fn static_rgba(app: &App, id: &str) -> Result<Rgba, String> {
    {
        let core = app.core.lock().unwrap();
        if let Some(session) = core.captures.get(id) {
            if let Some(rgba) = &session.composed_rgba {
                return Ok(rgba.clone());
            }
            let bytes = session.poster.as_deref().unwrap_or(&session.buffer);
            return images::decode_png(bytes);
        }
        if let Some(item) = core.history.iter().find(|i| i.id == id) {
            let bytes = std::fs::read(app.capture_dir.join(&item.file))
                .map_err(|_| i18n::t("Capture does not exist or has been removed"))?;
            return images::decode_any(&bytes);
        }
    }
    Err(i18n::t("Capture does not exist or has been removed").into())
}

pub fn image_buffer(app: &App, id: &str) -> Result<Vec<u8>, String> {
    let core = app.core.lock().unwrap();
    if let Some(session) = core.captures.get(id) {
        return Ok(session.buffer.clone());
    }
    if let Some(item) = core.history.iter().find(|i| i.id == id) {
        return std::fs::read(app.capture_dir.join(&item.file))
            .map_err(|_| i18n::t("Capture does not exist or has been removed").to_string());
    }
    Err(i18n::t("Capture does not exist or has been removed").into())
}

/// Commit editor annotations to the session buffer and notify linked pin windows.
pub fn update_image_annotations(
    app: &Arc<App>,
    id: &str,
    strokes: Vec<Stroke>,
) -> Result<(), String> {
    let composed = {
        let mut core = app.core.lock().unwrap();
        let session = core
            .captures
            .get_mut(id)
            .ok_or_else(|| i18n::t("Capture does not exist or has been removed"))?;
        if session.item.kind == "gif" {
            return Ok(());
        }
        let source = session
            .source_rgba
            .clone()
            .or_else(|| session.composed_rgba.clone())
            .ok_or_else(|| i18n::t("Missing source image data"))?;
        if session.source_rgba.is_none() {
            session.source_rgba = Some(source.clone());
        }
        let composed = annotations::rasterize_annotations(&source, &strokes);
        let png = images::encode_png(&composed)?;
        session.buffer = png;
        session.composed_rgba = Some(composed.clone());
        session.annotations = strokes;
        session.item.updated_at = Some(vault::now_millis());
        composed
    };
    let _ = composed;
    app.emit(AppEvent::ImageUpdated { id: id.to_string() });
    Ok(())
}

pub fn copy_image(app: &Arc<App>, id: &str) -> Result<(), String> {
    let rgba = static_rgba(app, id)?;
    clipboard::write_image(&rgba)
}

pub fn save_image_with_options(
    app: &Arc<App>,
    id: &str,
    options: &SaveOptions,
    parent: Option<&winit::window::Window>,
) -> Result<bool, String> {
    let animated = {
        let core = app.core.lock().unwrap();
        find_image(&core, app, id)?.kind == "gif"
    };
    let buffer = if animated {
        image_buffer(app, id)?
    } else {
        let rgba = static_rgba(app, id)?;
        match options.format.as_str() {
            "png" => images::encode_png(&rgba)?,
            _ => images::encode_jpeg(&rgba, options.quality as u8)?,
        }
    };
    if !export::write_export(&buffer, options, parent)? {
        return Ok(false);
    }
    remember_save_defaults(app, options);
    {
        let mut core = app.core.lock().unwrap();
        if let Some(session) = core.captures.get_mut(id) {
            session.item.saved_path = Some(options.file_path.clone());
        } else if let Some(item) = core.history.iter_mut().find(|i| i.id == id) {
            item.saved_path = Some(options.file_path.clone());
        }
    }
    app.emit(AppEvent::ImageUpdated { id: id.to_string() });
    Ok(true)
}

fn remember_save_defaults(app: &Arc<App>, options: &SaveOptions) {
    let next = {
        let mut core = app.core.lock().unwrap();
        if let Some(storage) = core.settings.get_mut("storage").and_then(|v| v.as_object_mut()) {
            storage.insert("directory".into(), json!(options.directory));
            if options.format == "png" || options.format == "jpg" {
                storage.insert("format".into(), json!(options.format));
            }
            storage.insert("quality".into(), json!(options.quality));
        }
        core.settings.clone()
    };
    let _ = next;
    let app_clone = app.clone();
    app.runtime.spawn(async move {
        let _ = vault::save_vault(&app_clone).await;
    });
}

pub fn save_defaults(app: &App) -> (String, String, i64) {
    let core = app.core.lock().unwrap();
    let dir = core.settings["storage"]["directory"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    let directory = if dir.is_empty() {
        app.picture_dir().to_string_lossy().to_string()
    } else {
        dir
    };
    let format = core.settings["storage"]["format"]
        .as_str()
        .unwrap_or("png")
        .to_string();
    let quality = core.settings["storage"]["quality"].as_i64().unwrap_or(90);
    (directory, format, quality)
}

/// Open a local image file for annotating or pinning.
pub fn import_image(app: &Arc<App>) {
    let app = app.clone();
    std::thread::spawn(move || {
        let picked = rfd::FileDialog::new()
            .set_title(i18n::t("Open Image"))
            .add_filter(
                i18n::t("Image Files"),
                &["png", "jpg", "jpeg", "gif", "bmp", "webp"],
            )
            .pick_file();
        let Some(path) = picked else { return };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                app.notify(
                    i18n::tf("Unable to read file: {err}", &[("err", &e.to_string())]),
                    true,
                );
                return;
            }
        };
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| i18n::t("Imported Image").into());
        let is_gif = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("gif"));
        let res = if is_gif {
            images::decode_gif_frames(&bytes).and_then(|frames| {
                let first = frames
                    .first()
                    .map(|(rgba, _)| rgba.clone())
                    .ok_or_else(|| i18n::t("GIF contains no valid frames").to_string())?;
                add_capture(
                    &app,
                    &first,
                    "gif",
                    AddOptions {
                        title: Some(title),
                        annotations: Vec::new(),
                        source_rgba: None,
                        skip_auto_save: true,
                        skip_auto_copy: true,
                        saved_path: Some(path.to_string_lossy().to_string()),
                        override_buffer: Some(bytes),
                        gif_frames: Some(Arc::new(frames)),
                        capture_origin: None,
                    },
                )
            })
        } else {
            images::decode_any(&bytes).and_then(|rgba| {
                add_capture(
                    &app,
                    &rgba,
                    "image",
                    AddOptions {
                        title: Some(title),
                        annotations: Vec::new(),
                        source_rgba: Some(rgba.clone()),
                        skip_auto_save: true,
                        skip_auto_copy: true,
                        saved_path: Some(path.to_string_lossy().to_string()),
                        override_buffer: None,
                        gif_frames: None,
                        capture_origin: None,
                    },
                )
            })
        };
        match res {
            Ok(item) => {
                let _ = open_result(&app, &item.id);
            }
            Err(e) => app.notify(e, true),
        }
    });
}

// ============================================================================
// Settings, autostart, and configuration import/export
// ============================================================================

pub fn update_settings(app: &Arc<App>, input: &Value) -> Result<(), String> {
    let next = settings::normalize(Some(input));
    // Validate sync server URL when set.
    let service_url = next["service"]["url"].as_str().unwrap_or("").trim();
    if !service_url.is_empty() {
        network::endpoint(service_url, false)?;
    }
    let launch_at_login = next["general"]["launchAtLogin"].as_bool().unwrap_or(false);
    if app.packaged {
        let _ = apply_autostart(launch_at_login);
    }
    i18n::apply_settings(&next);
    {
        let mut core = app.core.lock().unwrap();
        core.settings = next;
    }
    app.emit(AppEvent::ReregisterHotkeys);
    app.emit(AppEvent::SettingsChanged);
    let app_clone = app.clone();
    app.runtime.spawn(async move {
        if let Err(e) = vault::save_vault(&app_clone).await {
            app_clone.notify(
                i18n::tf("Failed to save local settings: {err}", &[("err", &e)]),
                true,
            );
        }
    });
    Ok(())
}

pub fn update_annotation_preference(
    app: &Arc<App>,
    tool: &str,
    patch: Value,
) {
    {
        let mut core = app.core.lock().unwrap();
        if let Some(annotations_obj) = core
            .settings
            .get_mut("annotations")
            .and_then(|v| v.as_object_mut())
        {
            if let Some(tool_obj) = annotations_obj
                .get_mut(tool)
                .and_then(|v| v.as_object_mut())
            {
                if let Value::Object(patch_map) = patch {
                    for (k, v) in patch_map {
                        tool_obj.insert(k, v);
                    }
                }
            }
        }
        core.settings = settings::normalize(Some(&core.settings));
    }
    let app_clone = app.clone();
    app.runtime.spawn(async move {
        let _ = vault::save_vault(&app_clone).await;
    });
}

fn apply_autostart(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_str = exe.to_string_lossy().to_string();
    let auto = auto_launch::AutoLaunchBuilder::new()
        .set_app_name("SnapDock")
        .set_app_path(&exe_str)
        .build()
        .map_err(|e| e.to_string())?;
    if enabled {
        auto.enable().map_err(|e| e.to_string())
    } else {
        if auto.is_enabled().unwrap_or(false) {
            auto.disable().map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    }
}

pub fn reset_settings(app: &Arc<App>) {
    let next = {
        let core = app.core.lock().unwrap();
        let mut fresh = settings::defaults().clone();
        if let Some(service) = core.settings.get("service") {
            fresh["service"] = service.clone();
        }
        if let Some(storage) = core.settings.get("storage") {
            fresh["storage"] = storage.clone();
        }
        settings::normalize(Some(&fresh))
    };
    match update_settings(app, &next) {
        Ok(()) => app.notify(i18n::t("Default settings restored"), false),
        Err(e) => app.notify(e, true),
    }
}

pub fn export_settings(app: &Arc<App>) {
    let mut data = settings::portable(&app.core.lock().unwrap().settings);
    // Strip API keys from exported files.
    if let Some(providers) = data.get_mut("providers").and_then(|v| v.as_object_mut()) {
        for provider in providers.values_mut() {
            for mode in ["api", "ai"] {
                if let Some(m) = provider.get_mut(mode).and_then(|v| v.as_object_mut()) {
                    m.insert("key".into(), json!(""));
                }
            }
        }
    }
    let text = serde_json::to_string_pretty(&data).unwrap_or_default();
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .set_title(i18n::t("Export Settings"))
            .set_file_name("snapdock-settings.json")
            .add_filter("JSON", &["json"])
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, text) {
            Ok(()) => app.notify(
                i18n::t("Settings exported (API keys and credentials excluded)"),
                false,
            ),
            Err(e) => app.notify(
                i18n::tf("Export failed: {err}", &[("err", &e.to_string())]),
                true,
            ),
        }
    });
}

pub fn import_settings(app: &Arc<App>) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .set_title(i18n::t("Import Settings"))
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                app.notify(
                    i18n::tf("Failed to read settings file: {err}", &[("err", &e.to_string())]),
                    true,
                );
                return;
            }
        };
        let remote: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                app.notify(i18n::t("Settings file is not valid JSON"), true);
                return;
            }
        };
        let local = app.core.lock().unwrap().settings.clone();
        match settings::apply_portable(&local, &remote) {
            Ok(mut applied) => {
                // Keep existing local API keys when imported fields are empty.
                for kind in ["ocr", "translation"] {
                    for mode in ["api", "ai"] {
                        let imported_key = applied["providers"][kind][mode]["key"]
                            .as_str()
                            .unwrap_or("");
                        if imported_key.is_empty() {
                            applied["providers"][kind][mode]["key"] =
                                local["providers"][kind][mode]["key"].clone();
                        }
                    }
                }
                match update_settings(&app, &applied) {
                    Ok(()) => app.notify(i18n::t("Settings imported"), false),
                    Err(e) => app.notify(e, true),
                }
            }
            Err(e) => app.notify(e, true),
        }
    });
}

// ============================================================================
// Account sync and background auto-sync timer
// ============================================================================

pub fn check_service_meta(app: &Arc<App>, url: String, ca_pem: String) {
    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        let mut fake = app.core.lock().unwrap().settings.clone();
        fake["service"]["url"] = json!(url);
        fake["service"]["caPem"] = json!(ca_pem);
        let result = network::service_request(&fake, "meta", "GET", None, None)
            .await
            .map(|v| {
                v.get("registration_enabled")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false)
            });
        match &result {
            Ok(_) => app.notify(i18n::t("Server connected and certificate verified"), false),
            Err(e) => app.notify(e.clone(), true),
        }
        app.emit(AppEvent::AccountMetaResult(result));
    });
}

pub fn login_or_register(
    app: &Arc<App>,
    url: String,
    ca_pem: String,
    username: String,
    password: String,
    register: bool,
) {
    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        let username = username.trim().to_string();
        if !(3..=40).contains(&username.chars().count()) {
            app.notify(i18n::t("Username must be between 3 and 40 characters"), true);
            return;
        }
        if password.chars().count() < 10 {
            app.notify(i18n::t("Password must be at least 10 characters"), true);
            return;
        }
        if let Err(e) = network::endpoint(&url, false) {
            app.notify(e, true);
            return;
        }
        let next = {
            let mut merged = app.core.lock().unwrap().settings.clone();
            merged["service"]["url"] = json!(url);
            merged["service"]["username"] = json!(username);
            merged["service"]["caPem"] = json!(ca_pem);
            settings::normalize(Some(&merged))
        };
        let route = if register { "auth/register" } else { "auth/login" };
        let response = match network::service_request(
            &next,
            route,
            "POST",
            None,
            Some(&json!({ "username": username, "password": password })),
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                app.notify(e, true);
                return;
            }
        };
        let Some(vault_salt) = response["vault_salt"].as_str().map(str::to_string) else {
            app.notify(i18n::t("Server response is missing encryption salt"), true);
            return;
        };
        let key = match network::derive_key(&password, &vault_salt) {
            Ok(k) => k,
            Err(e) => {
                app.notify(e, true);
                return;
            }
        };
        let key_base64 = base64::engine::general_purpose::STANDARD.encode(key);
        let token = response["token"].as_str().unwrap_or("").to_string();
        let expires_at = response["expires_at"].as_i64().unwrap_or(0);
        let resp_username = response["username"]
            .as_str()
            .unwrap_or(&username)
            .to_string();
        let service_url = next["service"]["url"].as_str().unwrap_or("").to_string();
        {
            let mut core = app.core.lock().unwrap();
            let (revision, fingerprint) = match core.account.as_ref() {
                Some(old) if old.username == resp_username && old.url == service_url => {
                    (old.revision, old.fingerprint.clone())
                }
                _ => (0, String::new()),
            };
            core.settings = next;
            core.account = Some(Account {
                username: resp_username,
                token,
                expires_at,
                vault_salt,
                key: key_base64,
                revision,
                fingerprint,
                url: service_url,
            });
        }
        let _ = vault::save_vault(&app).await;
        app.emit(AppEvent::SettingsChanged);
        app.emit(AppEvent::AccountChanged);
        app.notify(
            if register {
                i18n::t("Account created and signed in")
            } else {
                i18n::t("Signed in successfully; ready to sync settings")
            },
            false,
        );
    });
}

pub fn logout(app: &Arc<App>) {
    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        let (token, settings_val) = {
            let core = app.core.lock().unwrap();
            if core.sync_flight {
                app.notify(
                    i18n::t("Please wait for sync to finish before signing out"),
                    true,
                );
                return;
            }
            (
                core.account.as_ref().map(|a| a.token.clone()),
                core.settings.clone(),
            )
        };
        let mut revoked = true;
        if let Some(token) = token {
            if network::service_request(&settings_val, "auth/logout", "POST", Some(&token), None)
                .await
                .is_err()
            {
                revoked = false;
            }
        }
        app.core.lock().unwrap().account = None;
        let _ = vault::save_vault(&app).await;
        app.emit(AppEvent::AccountChanged);
        app.notify(
            if revoked {
                i18n::t("Signed out")
            } else {
                i18n::t("Signed out locally; server session will expire automatically")
            },
            false,
        );
    });
}

pub fn sync_config(app: &Arc<App>, direction: &'static str, quiet_auto: bool) {
    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        {
            let mut core = app.core.lock().unwrap();
            if core.account.is_none() {
                if !quiet_auto {
                    app.notify(i18n::t("Please sign in to the sync server first"), true);
                }
                return;
            }
            if core.sync_flight {
                if !quiet_auto {
                    app.notify(i18n::t("Sync already in progress, please wait"), true);
                }
                return;
            }
            core.sync_flight = true;
        }
        let res = sync_inner(&app, direction).await;
        app.core.lock().unwrap().sync_flight = false;
        match res {
            Ok(actual) => {
                app.emit(AppEvent::AccountChanged);
                if !quiet_auto {
                    match actual.as_str() {
                        "upload" => app.notify(i18n::t("Local settings encrypted and uploaded"), false),
                        "download" => app.notify(i18n::t("Cloud settings downloaded and applied"), false),
                        _ => app.notify(i18n::t("Settings are already up to date"), false),
                    }
                }
            }
            Err(e) => {
                app.notify(e, true);
            }
        }
    });
}

async fn sync_inner(app: &Arc<App>, direction: &str) -> Result<String, String> {
    let local_fingerprint = {
        let core = app.core.lock().unwrap();
        settings::fingerprint(&core.settings)
    };
    let mut dir = direction.to_string();
    let remote = if dir != "upload" {
        let (settings_val, token) = {
            let core = app.core.lock().unwrap();
            (
                core.settings.clone(),
                core.account.as_ref().map(|a| a.token.clone()),
            )
        };
        Some(
            network::service_request(&settings_val, "config", "GET", token.as_deref(), None)
                .await?,
        )
    } else {
        None
    };

    if dir == "auto" {
        let (account_rev, account_fp) = {
            let core = app.core.lock().unwrap();
            let acc = core
                .account
                .as_ref()
                .ok_or_else(|| i18n::t("Please sign in to the sync server first"))?;
            (acc.revision, acc.fingerprint.clone())
        };
        let remote_rev = remote
            .as_ref()
            .and_then(|r| r.get("revision"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let dirty = account_fp != local_fingerprint;
        if remote_rev != account_rev && dirty && remote_rev != 0 {
            return Err(i18n::t(
                "Both local and cloud settings changed; download cloud settings manually before syncing",
            )
            .into());
        }
        dir = if remote_rev > account_rev {
            "download".into()
        } else if dirty || remote_rev == 0 {
            "upload".into()
        } else {
            "none".into()
        };
    }

    let account = app
        .core
        .lock()
        .unwrap()
        .account
        .clone()
        .ok_or_else(|| i18n::t("Please sign in to the sync server first"))?;
    let key = base64::engine::general_purpose::STANDARD
        .decode(&account.key)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| i18n::t("Local encryption key is invalid; please sign in again"))?;

    if dir == "download" {
        let remote = remote.ok_or_else(|| i18n::t("No cloud settings available to download"))?;
        let remote_rev = remote
            .get("revision")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if remote_rev == 0 {
            return Err(i18n::t("No cloud settings found; please upload local settings first").into());
        }
        let restored = network::decrypt_config(&remote, &key)?;
        let local = app.core.lock().unwrap().settings.clone();
        let applied = settings::apply_portable(&local, &restored)?;
        update_settings(app, &applied)?;
        {
            let mut core = app.core.lock().unwrap();
            let fp = settings::fingerprint(&core.settings);
            if let Some(acc) = core.account.as_mut() {
                acc.revision = remote_rev;
                acc.fingerprint = fp;
            }
        }
    } else if dir == "upload" {
        let (portable, expected_rev, settings_val, token) = {
            let core = app.core.lock().unwrap();
            (
                settings::portable(&core.settings),
                core.account.as_ref().map(|a| a.revision).unwrap_or(0),
                core.settings.clone(),
                core.account.as_ref().map(|a| a.token.clone()),
            )
        };
        let mut body = network::encrypt_config(&portable, &key)?;
        if let Value::Object(map) = &mut body {
            map.insert("expected_revision".into(), json!(expected_rev));
        }
        let resp = network::service_request(
            &settings_val,
            "config",
            "PUT",
            token.as_deref(),
            Some(&body),
        )
        .await?;
        let rev = resp
            .get("revision")
            .and_then(|v| v.as_u64())
            .unwrap_or(expected_rev);
        {
            let mut core = app.core.lock().unwrap();
            let fp = settings::fingerprint(&core.settings);
            if let Some(acc) = core.account.as_mut() {
                acc.revision = rev;
                acc.fingerprint = fp;
            }
        }
    }

    app.core.lock().unwrap().last_sync = network::utc_iso(vault::now_millis());
    vault::save_vault(app).await?;
    Ok(dir)
}

/// Background auto-sync scheduler (checks every 120 seconds).
pub fn start_auto_sync_loop(app: &Arc<App>) {
    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(120));
        interval.tick().await;
        loop {
            interval.tick().await;
            let should_sync = {
                let core = app.core.lock().unwrap();
                core.settings["service"]["autoSync"]
                    .as_bool()
                    .unwrap_or(false)
                    && core.account.is_some()
                    && !core.sync_flight
            };
            if should_sync {
                sync_config(&app, "auto", true);
            }
        }
    });
}

// ============================================================================
// OCR and translation
// ============================================================================

async fn run_ocr_for_rgba(
    app: &Arc<App>,
    settings_val: &Value,
    rgba: Rgba,
) -> Result<String, String> {
    let mode = settings_val["providers"]["ocr"]["mode"]
        .as_str()
        .unwrap_or("local");
    if mode == "local" {
        let data_dir = app.data_dir.clone();
        let settings_clone = settings_val.clone();
        return tokio::task::spawn_blocking(move || {
            crate::ocr::recognize_rgba(&rgba, &data_dir, &settings_clone)
        })
        .await
        .map_err(|e| {
            i18n::tf(
                "Local OCR execution failed: {err}",
                &[("err", &e.to_string())],
            )
        })?;
    }
    let png = images::encode_png(&rgba)?;
    let data_url = images::data_url_png(&png);
    network::process_text(settings_val, "ocr", Some(&data_url), None).await
}

pub fn run_text_process(
    app: &Arc<App>,
    target: TextTarget,
    kind: String,
    capture_id: Option<String>,
    source_text: Option<String>,
) {
    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        let settings_val = app.core.lock().unwrap().settings.clone();
        let result = if kind == "ocr" {
            let rgba_res = match capture_id.as_deref() {
                Some(id) => static_rgba(&app, id),
                None if target == TextTarget::SettingsTest => Ok(crate::ocr::build_test_image()),
                None => Err(i18n::t("Please select a capture first").into()),
            };
            match rgba_res {
                Ok(rgba) => run_ocr_for_rgba(&app, &settings_val, rgba).await,
                Err(e) => Err(e),
            }
        } else if kind == "translation" {
            let text_to_translate = match source_text {
                Some(t) if !t.trim().is_empty() => Ok(t),
                _ => match capture_id.as_deref() {
                    Some(id) => match static_rgba(&app, id) {
                        Ok(rgba) => run_ocr_for_rgba(&app, &settings_val, rgba).await,
                        Err(e) => Err(e),
                    },
                    None => Err(i18n::t("No text available to translate").into()),
                },
            };
            match text_to_translate {
                Ok(t) => {
                    let trans_mode = settings_val["providers"]["translation"]["mode"]
                        .as_str()
                        .unwrap_or("local");
                    if trans_mode == "local" {
                        let data_dir = app.data_dir.clone();
                        let settings_clone = settings_val.clone();
                        match tokio::task::spawn_blocking(move || {
                            crate::translate::translate_text(&t, &data_dir, &settings_clone)
                        })
                        .await
                        {
                            Ok(res) => res,
                            Err(e) => Err(i18n::tf(
                                "Local translation execution failed: {err}",
                                &[("err", &e.to_string())],
                            )),
                        }
                    } else {
                        network::process_text(&settings_val, "translation", None, Some(&t)).await
                    }
                }
                Err(e) => Err(e),
            }
        } else {
            Err(i18n::t("Unknown processing type").into())
        };
        app.emit(AppEvent::TextResult {
            target,
            kind,
            result,
        });
    });
}

async fn run_ocr_overlay_for_rgba(
    app: &Arc<App>,
    settings_val: &Value,
    rgba: Rgba,
) -> Result<crate::ocr::OcrOverlayData, String> {
    let mode = settings_val["providers"]["ocr"]["mode"]
        .as_str()
        .unwrap_or("local");
    if mode == "local" {
        let data_dir = app.data_dir.clone();
        let settings_clone = settings_val.clone();
        return tokio::task::spawn_blocking(move || {
            crate::ocr::recognize_rgba_with_blocks(&rgba, &data_dir, &settings_clone)
        })
        .await
        .map_err(|e| {
            i18n::tf(
                "Local OCR execution failed: {err}",
                &[("err", &e.to_string())],
            )
        })?;
    }
    let png = images::encode_png(&rgba)?;
    let data_url = images::data_url_png(&png);
    let full_text = network::process_text(settings_val, "ocr", Some(&data_url), None).await?;
    if full_text.trim().is_empty() {
        return Err(i18n::t("No text detected in the image").into());
    }
    Ok(crate::ocr::OcrOverlayData {
        blocks: Vec::new(),
        full_text,
        full_translated: None,
    })
}

fn join_blocks_text(blocks: &[crate::ocr::OcrTextBlock], use_translated: bool) -> String {
    let mut rows: Vec<String> = Vec::new();
    let mut last_row: Option<usize> = None;
    for block in blocks {
        let piece = if use_translated {
            block.translated.as_deref().unwrap_or(&block.text)
        } else {
            &block.text
        };
        if piece.trim().is_empty() {
            continue;
        }
        if last_row == Some(block.row) {
            if let Some(last) = rows.last_mut() {
                last.push(' ');
                last.push_str(piece);
            }
        } else {
            rows.push(piece.to_string());
            last_row = Some(block.row);
        }
    }
    rows.join("\n")
}

/// Run OCR or per-block translation for a text-overlay pin window.
pub fn run_pin_overlay_process(
    app: &Arc<App>,
    pin_id: String,
    capture_id: String,
    mode: PinOverlayMode,
    cached_ocr: Option<crate::ocr::OcrOverlayData>,
) {
    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        let settings_val = app.core.lock().unwrap().settings.clone();

        let ocr_res = match cached_ocr {
            Some(existing) if !existing.full_text.trim().is_empty() => Ok(existing),
            _ => match static_rgba(&app, &capture_id) {
                Ok(rgba) => run_ocr_overlay_for_rgba(&app, &settings_val, rgba).await,
                Err(e) => Err(e),
            },
        };

        let result = match (mode, ocr_res) {
            (PinOverlayMode::Ocr, res) => res,
            (PinOverlayMode::Translation, Err(e)) => Err(e),
            (PinOverlayMode::Translation, Ok(mut data)) => {
                let trans_mode = settings_val["providers"]["translation"]["mode"]
                    .as_str()
                    .unwrap_or("local");

                if !data.blocks.is_empty() {
                    let block_lines: Vec<String> = data
                        .blocks
                        .iter()
                        .map(|b| b.text.replace(['\r', '\n'], " "))
                        .collect();

                    if trans_mode == "local" {
                        let data_dir = app.data_dir.clone();
                        let settings_clone = settings_val.clone();
                        match tokio::task::spawn_blocking(move || {
                            crate::translate::translate_lines(
                                &block_lines,
                                &data_dir,
                                &settings_clone,
                            )
                        })
                        .await
                        {
                            Ok(Ok(translated_lines)) => {
                                for (block, tr) in
                                    data.blocks.iter_mut().zip(translated_lines.into_iter())
                                {
                                    block.translated = Some(tr);
                                }
                                data.full_translated = Some(join_blocks_text(&data.blocks, true));
                                Ok(data)
                            }
                            Ok(Err(e)) => Err(e),
                            Err(e) => Err(i18n::tf(
                                "Local translation execution failed: {err}",
                                &[("err", &e.to_string())],
                            )),
                        }
                    } else {
                        let joined = block_lines.join("\n");
                        match network::process_text(
                            &settings_val,
                            "translation",
                            None,
                            Some(&joined),
                        )
                        .await
                        {
                            Ok(resp) => {
                                let trimmed = resp.trim().to_string();
                                let raw_lines: Vec<&str> = trimmed.lines().collect();
                                let non_empty_lines: Vec<&str> = trimmed
                                    .lines()
                                    .map(str::trim)
                                    .filter(|l| !l.is_empty())
                                    .collect();
                                if data.blocks.len() == 1 {
                                    data.blocks[0].translated = Some(trimmed.clone());
                                } else if raw_lines.len() == data.blocks.len() {
                                    for (block, tr) in
                                        data.blocks.iter_mut().zip(raw_lines.into_iter())
                                    {
                                        block.translated = Some(tr.trim().to_string());
                                    }
                                } else if non_empty_lines.len() == data.blocks.len() {
                                    for (block, tr) in
                                        data.blocks.iter_mut().zip(non_empty_lines.into_iter())
                                    {
                                        block.translated = Some(tr.to_string());
                                    }
                                } else {
                                    for block in &mut data.blocks {
                                        block.translated = None;
                                    }
                                }
                                data.full_translated = Some(trimmed);
                                Ok(data)
                            }
                            Err(e) => Err(e),
                        }
                    }
                } else {
                    // Remote OCR returned plain text without bounding boxes.
                    let source = data.full_text.clone();
                    let trans_res = if trans_mode == "local" {
                        let data_dir = app.data_dir.clone();
                        let settings_clone = settings_val.clone();
                        match tokio::task::spawn_blocking(move || {
                            crate::translate::translate_text(&source, &data_dir, &settings_clone)
                        })
                        .await
                        {
                            Ok(res) => res,
                            Err(e) => Err(i18n::tf(
                                "Local translation execution failed: {err}",
                                &[("err", &e.to_string())],
                            )),
                        }
                    } else {
                        network::process_text(&settings_val, "translation", None, Some(&source))
                            .await
                    };
                    match trans_res {
                        Ok(translated) => {
                            data.full_translated = Some(translated);
                            Ok(data)
                        }
                        Err(e) => Err(e),
                    }
                }
            }
        };

        app.emit(AppEvent::PinOverlayResult {
            pin_id,
            mode,
            result,
        });
    });
}

/// Asynchronously download a local OCR model pack with live progress and cancellation.
pub fn download_ocr_model(
    app: &Arc<App>,
    model_id: String,
    base_dir: std::path::PathBuf,
    mirror: String,
) {
    let Some(pack) = crate::ocr::find_model_pack(&model_id) else {
        app.notify(i18n::t("OCR model pack not found"), true);
        return;
    };
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let label = pack.display_label();
    {
        let mut core = app.core.lock().unwrap();
        if core.ocr_downloads.contains_key(&model_id) {
            app.notify(
                i18n::tf("OCR model [{label}] is already downloading", &[("label", label)]),
                false,
            );
            return;
        }
        let approx_total = crate::ocr::approx_pack_bytes(pack);
        core.ocr_downloads.insert(
            model_id.clone(),
            crate::state::OcrDownloadTask {
                progress: crate::ocr::OcrDownloadProgress {
                    model_id: model_id.clone(),
                    file_index: 0,
                    file_count: pack.download_assets().len(),
                    current_file: i18n::t("Connecting...").into(),
                    downloaded_bytes: 0,
                    total_bytes: approx_total,
                },
                cancel: cancel.clone(),
            },
        );
    }
    app.emit(AppEvent::OcrModelsChanged);

    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        let app_progress = app.clone();
        let mid_progress = model_id.clone();
        let result = crate::ocr::download_model_pack(
            &model_id,
            &base_dir,
            &mirror,
            cancel,
            move |progress| {
                if let Some(task) = app_progress
                    .core
                    .lock()
                    .unwrap()
                    .ocr_downloads
                    .get_mut(&mid_progress)
                {
                    task.progress = progress;
                }
                app_progress.emit(AppEvent::OcrModelsChanged);
            },
        )
        .await;

        app.core.lock().unwrap().ocr_downloads.remove(&model_id);
        app.emit(AppEvent::OcrModelsChanged);

        let cur_label = pack.display_label();
        match result {
            Ok(dir) => {
                app.notify(
                    i18n::tf(
                        "OCR model [{label}] downloaded ({path})",
                        &[
                            ("label", cur_label),
                            ("path", &dir.display().to_string()),
                        ],
                    ),
                    false,
                );
            }
            Err(e) => {
                let cancelled = e == i18n::t("Download cancelled");
                app.notify(
                    if cancelled {
                        i18n::tf("Download cancelled for [{label}]", &[("label", cur_label)])
                    } else {
                        i18n::tf(
                            "Failed to download [{label}]: {err}",
                            &[("label", cur_label), ("err", &e)],
                        )
                    },
                    !cancelled,
                );
            }
        }
    });
}

/// Cancel an in-progress OCR model download.
pub fn cancel_ocr_model_download(app: &Arc<App>, model_id: &str) {
    if let Some(task) = app.core.lock().unwrap().ocr_downloads.get(model_id) {
        task.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Delete a downloaded local OCR model pack.
pub fn delete_ocr_model(app: &Arc<App>, model_id: &str, base_dir: &std::path::Path) {
    if app.core.lock().unwrap().ocr_downloads.contains_key(model_id) {
        app.notify(
            i18n::t("Please cancel the active download before deleting"),
            true,
        );
        return;
    }
    let label = crate::ocr::find_model_pack(model_id)
        .map(|p| p.display_label().to_string())
        .unwrap_or_else(|| model_id.to_string());
    match crate::ocr::remove_model_pack(model_id, base_dir) {
        Ok(()) => {
            app.emit(AppEvent::OcrModelsChanged);
            app.notify(
                i18n::tf("Deleted OCR model [{label}]", &[("label", &label)]),
                false,
            );
        }
        Err(e) => app.notify(e, true),
    }
}

/// Asynchronously download a local translation language pack with live progress and cancellation.
pub fn download_translation_pack(
    app: &Arc<App>,
    pack_id: String,
    base_dir: std::path::PathBuf,
    mirror: String,
) {
    let Some(spec) = crate::translate::find_pack_spec(&pack_id) else {
        app.notify(i18n::t("Translation language pack not found"), true);
        return;
    };
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let label = spec.display_label();
    {
        let mut core = app.core.lock().unwrap();
        if core.pack_downloads.contains_key(&pack_id) {
            app.notify(
                i18n::tf("Language pack [{label}] is already downloading", &[("label", &label)]),
                false,
            );
            return;
        }
        let approx_total: u64 = spec.files.iter().map(|f| f.approx_bytes).sum();
        core.pack_downloads.insert(
            pack_id.clone(),
            crate::state::PackDownloadTask {
                progress: crate::translate::PackDownloadProgress {
                    pack_id: pack_id.clone(),
                    file_index: 0,
                    file_count: spec.files.len(),
                    current_file: i18n::t("Connecting...").into(),
                    downloaded_bytes: 0,
                    total_bytes: approx_total,
                },
                cancel: cancel.clone(),
            },
        );
    }
    app.emit(AppEvent::TranslationPacksChanged);

    let app_bg = app.clone();
    app.runtime.spawn(async move {
        let app = app_bg;
        let app_progress = app.clone();
        let pid_progress = pack_id.clone();
        let result = crate::translate::download_pack(
            &pack_id,
            &base_dir,
            &mirror,
            cancel,
            move |progress| {
                if let Some(task) = app_progress
                    .core
                    .lock()
                    .unwrap()
                    .pack_downloads
                    .get_mut(&pid_progress)
                {
                    task.progress = progress;
                }
                app_progress.emit(AppEvent::TranslationPacksChanged);
            },
        )
        .await;

        app.core.lock().unwrap().pack_downloads.remove(&pack_id);
        app.emit(AppEvent::TranslationPacksChanged);

        let cur_label = spec.display_label();
        match result {
            Ok(dir) => {
                app.notify(
                    i18n::tf(
                        "Language pack [{label}] downloaded ({path})",
                        &[
                            ("label", &cur_label),
                            ("path", &dir.display().to_string()),
                        ],
                    ),
                    false,
                );
            }
            Err(e) => {
                let cancelled = e == i18n::t("Download cancelled");
                app.notify(
                    if cancelled {
                        i18n::tf("Download cancelled for [{label}]", &[("label", &cur_label)])
                    } else {
                        i18n::tf(
                            "Failed to download [{label}]: {err}",
                            &[("label", &cur_label), ("err", &e)],
                        )
                    },
                    !cancelled,
                );
            }
        }
    });
}

/// Cancel an in-progress translation pack download.
pub fn cancel_translation_pack_download(app: &Arc<App>, pack_id: &str) {
    if let Some(task) = app.core.lock().unwrap().pack_downloads.get(pack_id) {
        task.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Delete a downloaded local translation language pack.
pub fn delete_translation_pack(app: &Arc<App>, pack_id: &str, base_dir: &std::path::Path) {
    if app.core.lock().unwrap().pack_downloads.contains_key(pack_id) {
        app.notify(
            i18n::t("Please cancel the active download before deleting"),
            true,
        );
        return;
    }
    let label = crate::translate::find_pack_spec(pack_id)
        .map(|s| s.display_label())
        .unwrap_or_else(|| pack_id.to_string());
    match crate::translate::remove_pack(pack_id, base_dir) {
        Ok(()) => {
            app.emit(AppEvent::TranslationPacksChanged);
            app.notify(
                i18n::tf("Deleted language pack [{label}]", &[("label", &label)]),
                false,
            );
        }
        Err(e) => app.notify(e, true),
    }
}

/// Open a directory in File Explorer (creating it if needed).
pub fn open_directory(app: &Arc<App>, dir: &std::path::Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        app.notify(
            i18n::tf("Unable to create directory: {err}", &[("err", &e.to_string())]),
            true,
        );
        return;
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
    }
}

fn chrono_like_stamp(millis: i64) -> String {
    let secs = millis / 1000;
    let millis_part = millis % 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}-{:03}",
        year, month, day, hour, minute, second, millis_part
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
