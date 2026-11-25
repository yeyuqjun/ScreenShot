//! Recording orchestration: scrolling capture stitching loop and GIF frame acquisition.

use crate::dxgi::ScreenGrabber;
use crate::gif;
use crate::i18n;
use crate::images::{self, Rgba};
use crate::native::{self, LongBorderWindow};
use crate::service::{self, AddOptions};
use crate::state::{App, AppEvent, CaptureItem, RecordingProgress};
use crate::stitch;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::RECT;

/// Scrolling capture stitching state.
pub struct LongState {
    pub max_frames: u32,
    pub interval_ms: i64,
    pub sticky_top: u32,
    pub sticky_bottom: u32,
    pub scroll_mode: String,
    pub count: u32,
    pub stalled: u32,
    pub hint: String,
    pub assembled: Option<Rgba>,
    pub previous: Option<Rgba>,
    pub sample: Option<Rgba>,
}

impl LongState {
    fn new(
        max_frames: u32,
        interval_ms: i64,
        sticky_top: u32,
        sticky_bottom: u32,
        scroll_mode: &str,
    ) -> Self {
        let mut state = Self {
            max_frames,
            interval_ms,
            sticky_top,
            sticky_bottom,
            scroll_mode: "auto".into(),
            count: 0,
            stalled: 0,
            hint: i18n::t("Auto-scrolling inside selection; switch to manual anytime").into(),
            assembled: None,
            previous: None,
            sample: None,
        };
        if scroll_mode == "manual" {
            state.manual(i18n::t(
                "Scroll down slowly inside the selection, then click Done",
            ));
        }
        state
    }

    pub fn manual(&mut self, hint: &str) {
        self.scroll_mode = "manual".into();
        self.hint = hint.to_string();
    }

    pub fn accept(&mut self) -> Result<bool, String> {
        let Some(sample) = self.sample.clone() else {
            return Ok(false);
        };
        let Some(previous) = self.previous.clone() else {
            self.previous = Some(sample.clone());
            self.assembled = Some(sample);
            self.count = 1;
            return Ok(true);
        };
        let assembled = self.assembled.clone().expect("missing scrolling capture image");
        match stitch::append_frame(
            &assembled,
            &previous,
            &sample,
            self.sticky_top,
            self.sticky_bottom,
        ) {
            Ok((image, shift)) => {
                if shift == 0 {
                    if self.scroll_mode == "auto" {
                        self.stalled += 1;
                        if self.stalled >= 2 {
                            self.manual(i18n::t(
                                "Auto-scroll stopped advancing; scroll down manually or click Done if at the bottom",
                            ));
                        }
                    }
                    return Ok(false);
                }
                self.assembled = Some(image);
                self.previous = Some(sample);
                self.count += 1;
                self.stalled = 0;
                if self.scroll_mode == "manual" {
                    self.hint = i18n::t(
                        "Scroll down slowly inside the selection, then click Done",
                    )
                    .into();
                }
                Ok(true)
            }
            Err(error) => {
                if error == i18n::t("Scrolling capture reached the maximum size limit")
                    || error == i18n::t("Selection area size changed")
                {
                    return Err(error);
                }
                self.manual(&error);
                Ok(false)
            }
        }
    }
}

/// Shared recording session state.
pub struct Recording {
    pub mode: String,
    pub rect: images::PixelRect,
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub monitor_rect: RECT,
    pub target_hwnd: isize,
    pub start: i64,
    pub stopping: bool,
    pub cancelled: bool,
    pub stopped: bool,
    pub encoding: bool,
    pub progress: f64,
    pub notice: Option<String>,
    pub error: Option<String>,
    pub long: Option<LongState>,
    pub frames: Vec<Vec<u8>>,
    pub delays: Vec<u32>,
    pub frame_bytes: u64,
    pub last_frame_at: Option<i64>,
    pub control_origin: (i32, i32),
    pub control_size: (i32, i32),
    pub border: Option<LongBorderWindow>,
}

pub type SharedRecording = Arc<Mutex<Recording>>;

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Control bar placement: prefer open space outside the selection (physical coordinates).
pub fn control_bounds(
    selection: (i32, i32, i32, i32),
    size: (i32, i32),
    work: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (sx, sy, sw, sh) = selection;
    let (wx, wy, ww, wh) = work;
    let clamp_x = |x: i32| x.clamp(wx, (wx + ww - size.0).max(wx));
    let clamp_y = |y: i32| y.clamp(wy, (wy + wh - size.1).max(wy));
    let below = sy + sh + 14;
    if below + size.1 <= wy + wh {
        return (clamp_x(sx), below);
    }
    let above = sy - size.1 - 14;
    if above >= wy {
        return (clamp_x(sx), above);
    }
    let right = sx + sw + 14;
    if right + size.0 <= wx + ww {
        return (right, clamp_y(sy));
    }
    let left = sx - size.0 - 14;
    if left >= wx {
        return (left, clamp_y(sy));
    }
    (clamp_x(sx + sw - size.0), clamp_y(below))
}

/// Start a scrolling or GIF recording session.
pub fn begin_recording(
    app: &Arc<App>,
    mode: &str,
    rect: images::PixelRect,
    monitor: (i32, i32, u32, u32),
    scroll_mode: &str,
    target_hwnd: isize,
) -> Result<(), String> {
    let settings = app.core.lock().unwrap().settings.clone();
    let (max_frames, interval_ms, sticky_top, sticky_bottom) = (
        settings["long"]["maxFrames"].as_i64().unwrap_or(30) as u32,
        settings["long"]["intervalMs"].as_i64().unwrap_or(250),
        settings["long"]["stickyTop"].as_i64().unwrap_or(0) as u32,
        settings["long"]["stickyBottom"].as_i64().unwrap_or(0) as u32,
    );
    let monitor_rect = RECT {
        left: monitor.0,
        top: monitor.1,
        right: monitor.0 + monitor.2 as i32,
        bottom: monitor.1 + monitor.3 as i32,
    };

    let selection = (
        monitor.0 + rect.x as i32,
        monitor.1 + rect.y as i32,
        rect.width as i32,
        rect.height as i32,
    );
    let control_size = if mode == "long" { (560, 96) } else { (400, 68) };
    let work = native::work_area_at_point(selection.0, selection.1);
    let control_origin = control_bounds(selection, control_size, work);

    // Hollow high-contrast click-through border for scrolling capture (pure Win32, excluded from capture).
    let border = if mode == "long" {
        let edge = 3;
        let bx = (selection.0 - edge).max(monitor.0);
        let by = (selection.1 - edge).max(monitor.1);
        let br = (selection.0 + selection.2 + edge).min(monitor.0 + monitor.2 as i32);
        let bb = (selection.1 + selection.3 + edge).min(monitor.1 + monitor.3 as i32);
        let border_bgr = crate::ui::theme_accent(&app.core.lock().unwrap().settings)
            .palette()
            .border_bgr;
        Some(LongBorderWindow::spawn(
            bx,
            by,
            (br - bx).max(8),
            (bb - by).max(8),
            border_bgr,
        ))
    } else {
        None
    };

    let recording = Arc::new(Mutex::new(Recording {
        mode: mode.to_string(),
        rect,
        monitor_x: monitor.0,
        monitor_y: monitor.1,
        monitor_rect,
        target_hwnd,
        start: now_millis(),
        stopping: false,
        cancelled: false,
        stopped: false,
        encoding: false,
        progress: 0.0,
        notice: None,
        error: None,
        long: if mode == "long" {
            Some(LongState::new(
                max_frames,
                interval_ms,
                sticky_top,
                sticky_bottom,
                scroll_mode,
            ))
        } else {
            None
        },
        frames: Vec::new(),
        delays: Vec::new(),
        frame_bytes: 0,
        last_frame_at: None,
        control_origin,
        control_size,
        border,
    }));

    if mode == "long" && scroll_mode == "auto" {
        let center_x = selection.0 + selection.2 / 2;
        let center_y = selection.1 + selection.3 / 2;
        if native::scroll_at(center_x, center_y, 0, target_hwnd).is_err() {
            if let Some(long) = recording.lock().unwrap().long.as_mut() {
                long.manual(i18n::t(
                    "Auto-scroll unavailable; please scroll down manually inside the selection",
                ));
            }
        }
    }

    app.core.lock().unwrap().recording = Some(recording.clone());
    app.emit(AppEvent::OpenRecorder);
    app.emit(AppEvent::RecordingProgress(progress_view(
        &recording.lock().unwrap(),
    )));

    let app_for_thread = app.clone();
    std::thread::spawn(move || {
        if let Err(message) = record_loop(&app_for_thread, &recording) {
            recording.lock().unwrap().error = Some(message);
        }
        if let Err(message) = finish_recording(&app_for_thread, &recording) {
            app_for_thread.notify(message, true);
        }
    });
    Ok(())
}

static GRABBER: std::sync::Mutex<Option<ScreenGrabber>> = std::sync::Mutex::new(None);

fn capture_frame(recording: &Recording) -> Result<Rgba, String> {
    let mut holder = GRABBER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let need_new = match holder.as_ref() {
        Some(grabber) => grabber.monitor_origin() != (recording.monitor_x, recording.monitor_y),
        None => true,
    };
    if need_new {
        *holder = Some(ScreenGrabber::new(RECT {
            left: recording.monitor_x,
            top: recording.monitor_y,
            right: recording.monitor_rect.right,
            bottom: recording.monitor_rect.bottom,
        })?);
    }
    let full = holder.as_mut().expect("missing screen grabber").grab()?;
    let image = Rgba::new(full.width, full.height, full.pixels);
    images::crop_pixels(&image, recording.rect)
}

fn gif_loop(app: &Arc<App>, recording: &SharedRecording) -> Result<(), String> {
    let (fps, max_seconds, max_width) = {
        let settings = app.core.lock().unwrap().settings.clone();
        (
            settings["record"]["fps"].as_i64().unwrap_or(8),
            settings["record"]["maxSeconds"].as_i64().unwrap_or(15),
            settings["record"]["maxWidth"].as_i64().unwrap_or(1280) as u32,
        )
    };
    loop {
        let frame_start = now_millis();
        let frame = capture_frame(&recording.lock().unwrap())?;
        let scale = 1f64
            .min(max_width as f64 / frame.width as f64)
            .min((1_500_000f64 / (frame.width as f64 * frame.height as f64)).sqrt());
        let scaled = if scale < 1.0 {
            let new_width = ((frame.width as f64 * scale).round() as u32).max(1);
            let new_height = ((frame.height as f64 * scale).round() as u32).max(1);
            let buffer = image::ImageBuffer::from_raw(
                frame.width,
                frame.height,
                frame.pixels.clone(),
            )
            .expect("frame buffer size mismatch");
            let resized = image::DynamicImage::ImageRgba8(buffer)
                .resize_exact(
                    new_width,
                    new_height,
                    image::imageops::FilterType::Triangle,
                )
                .to_rgba8();
            Rgba::new(new_width, new_height, resized.into_raw())
        } else {
            frame
        };
        let buffer = images::encode_png(&scaled)?;
        let timestamp = now_millis();
        {
            let mut guard = recording.lock().unwrap();
            guard.frame_bytes += buffer.len() as u64;
            if let Some(last) = guard.last_frame_at {
                if let Some(last_delay) = guard.delays.last_mut() {
                    *last_delay = (timestamp - last) as u32;
                }
            }
            guard.last_frame_at = Some(timestamp);
            guard.frames.push(buffer);
            guard.delays.push((1000 / fps.max(1)) as u32);
            if timestamp - guard.start >= max_seconds * 1000
                || guard.frames.len() >= 300
                || guard.frame_bytes >= 128 * 1024 * 1024
            {
                guard.stopping = true;
            }
        }
        app.emit(AppEvent::RecordingProgress(progress_view(
            &recording.lock().unwrap(),
        )));
        if recording.lock().unwrap().stopping {
            break;
        }
        let elapsed = now_millis() - frame_start;
        let target = 1000 / fps.max(1);
        if target - elapsed > 0 {
            std::thread::sleep(std::time::Duration::from_millis((target - elapsed) as u64));
        }
    }
    Ok(())
}

fn long_loop(app: &Arc<App>, recording: &SharedRecording) -> Result<(), String> {
    loop {
        let iteration_start = now_millis();
        if recording.lock().unwrap().cancelled {
            break;
        }
        let mut stable = false;
        {
            let frame = capture_frame(&recording.lock().unwrap())?;
            let mut guard = recording.lock().unwrap();
            if let Some(long) = guard.long.as_mut() {
                let stable_now = long
                    .sample
                    .as_ref()
                    .map(|sample| stitch::difference(sample, &frame) < 0.15)
                    .unwrap_or(false);
                long.sample = Some(frame);
                stable = stable_now;
            }
        }
        for _ in 0..12 {
            if recording.lock().unwrap().cancelled {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
            let frame = capture_frame(&recording.lock().unwrap())?;
            let mut guard = recording.lock().unwrap();
            if let Some(long) = guard.long.as_mut() {
                let stable_now = long
                    .sample
                    .as_ref()
                    .map(|sample| stitch::difference(sample, &frame) < 0.15)
                    .unwrap_or(false);
                long.sample = Some(frame);
                stable = stable_now;
            }
            if stable {
                break;
            }
        }
        let (cancelled, count, stopping) = {
            let guard = recording.lock().unwrap();
            (
                guard.cancelled,
                guard.long.as_ref().map(|l| l.count).unwrap_or(0),
                guard.stopping,
            )
        };
        if cancelled {
            break;
        }
        if !stable && count > 0 {
            if stopping {
                break;
            }
            continue;
        }
        {
            let mut guard = recording.lock().unwrap();
            if let Some(long) = guard.long.as_mut() {
                if let Err(error) = long.accept() {
                    guard.notice = Some(error);
                    guard.stopping = true;
                }
            }
        }
        let (count, max_frames, scroll_mode) = {
            let guard = recording.lock().unwrap();
            (
                guard.long.as_ref().map(|l| l.count).unwrap_or(0),
                guard
                    .long
                    .as_ref()
                    .map(|l| l.max_frames)
                    .unwrap_or(u32::MAX),
                guard
                    .long
                    .as_ref()
                    .map(|l| l.scroll_mode.clone())
                    .unwrap_or_default(),
            )
        };
        if count >= max_frames {
            recording.lock().unwrap().stopping = true;
        }
        app.emit(AppEvent::RecordingProgress(progress_view(
            &recording.lock().unwrap(),
        )));
        if recording.lock().unwrap().stopping {
            break;
        }
        if !stable {
            if let Some(long) = recording.lock().unwrap().long.as_mut() {
                long.manual(i18n::t(
                    "Screen is still moving; wait for scrolling to settle before continuing (first frame kept)",
                ));
            }
            app.emit(AppEvent::RecordingProgress(progress_view(
                &recording.lock().unwrap(),
            )));
            continue;
        }
        if scroll_mode == "auto" {
            let (center_x, center_y, target_hwnd) = {
                let guard = recording.lock().unwrap();
                (
                    guard.monitor_x + guard.rect.x as i32 + guard.rect.width as i32 / 2,
                    guard.monitor_y + guard.rect.y as i32 + guard.rect.height as i32 / 2,
                    guard.target_hwnd,
                )
            };
            if native::scroll_at(center_x, center_y, -120, target_hwnd).is_err() {
                if let Some(long) = recording.lock().unwrap().long.as_mut() {
                    long.manual(i18n::t(
                        "Auto-scroll unavailable; please scroll down manually inside the selection",
                    ));
                }
                app.emit(AppEvent::RecordingProgress(progress_view(
                    &recording.lock().unwrap(),
                )));
            }
        }
        let interval = if scroll_mode == "manual" {
            120
        } else {
            recording
                .lock()
                .unwrap()
                .long
                .as_ref()
                .map(|l| l.interval_ms)
                .unwrap_or(250)
        };
        let remaining = interval - (now_millis() - iteration_start);
        if remaining > 0 {
            std::thread::sleep(std::time::Duration::from_millis(remaining as u64));
        }
    }
    Ok(())
}

fn record_loop(app: &Arc<App>, recording: &SharedRecording) -> Result<(), String> {
    let mode = recording.lock().unwrap().mode.clone();
    if mode == "long" {
        long_loop(app, recording)
    } else {
        gif_loop(app, recording)
    }
}

fn finish_recording(app: &Arc<App>, recording: &SharedRecording) -> Result<(), String> {
    {
        let mut guard = recording.lock().unwrap();
        if guard.stopped {
            return Ok(());
        }
        guard.stopped = true;
    }
    let outcome: Result<Option<CaptureItem>, String> = (|| {
        let (cancelled, error, mode) = {
            let guard = recording.lock().unwrap();
            (guard.cancelled, guard.error.clone(), guard.mode.clone())
        };
        if cancelled {
            return Ok(None);
        }
        if let Some(error) = error {
            return Err(error);
        }
        if mode == "gif" {
            let (frames, delays) = {
                let guard = recording.lock().unwrap();
                (guard.frames.clone(), guard.delays.clone())
            };
            recording.lock().unwrap().encoding = true;
            app.emit(AppEvent::RecordingProgress(progress_view(
                &recording.lock().unwrap(),
            )));
            let poster = frames.first().cloned();
            let buffer = gif::encode(&frames, &delays)?;
            if recording.lock().unwrap().cancelled {
                return Ok(None);
            }
            let poster_rgba = images::decode_png(poster.as_deref().unwrap_or_default())
                .unwrap_or_else(|_| Rgba::new(1, 1, vec![0, 0, 0, 255]));
            let gif_frames = images::decode_gif_frames(&buffer).ok().map(Arc::new);
            let item = service::add_capture(
                app,
                &poster_rgba,
                "gif",
                AddOptions {
                    title: None,
                    annotations: Vec::new(),
                    source_rgba: None,
                    skip_auto_save: false,
                    skip_auto_copy: false,
                    saved_path: None,
                    override_buffer: Some(buffer),
                    gif_frames,
                    capture_origin: None,
                },
            )?;
            Ok(Some(item))
        } else {
            let (assembled, count) = {
                let guard = recording.lock().unwrap();
                (
                    guard.long.as_ref().and_then(|l| l.assembled.clone()),
                    guard.long.as_ref().map(|l| l.count).unwrap_or(0),
                )
            };
            let Some(assembled) = assembled else {
                return Ok(None);
            };
            if count == 0 {
                return Ok(None);
            }
            let item = service::add_capture(
                app,
                &assembled,
                "long",
                AddOptions {
                    title: None,
                    annotations: Vec::new(),
                    source_rgba: Some(assembled.clone()),
                    skip_auto_save: false,
                    skip_auto_copy: false,
                    saved_path: None,
                    override_buffer: None,
                    gif_frames: None,
                    capture_origin: None,
                },
            )?;
            Ok(Some(item))
        }
    })();

    let notice = recording.lock().unwrap().notice.clone();
    if let Some(notice) = notice {
        app.notify(notice, true);
    }
    match outcome {
        Ok(Some(item)) => {
            cleanup_recording(app, recording);
            service::open_result(app, &item.id)?;
        }
        Ok(None) => {
            cleanup_recording(app, recording);
        }
        Err(error) => {
            cleanup_recording(app, recording);
            if !recording.lock().unwrap().cancelled {
                app.notify(error, true);
            }
        }
    }
    Ok(())
}

fn cleanup_recording(app: &Arc<App>, recording: &SharedRecording) {
    if let Some(border) = recording.lock().unwrap().border.take() {
        border.close();
    }
    {
        let mut core = app.core.lock().unwrap();
        if let Some(current) = core.recording.as_ref() {
            if Arc::ptr_eq(current, recording) {
                core.recording = None;
            }
        }
    }
    app.emit(AppEvent::CloseRecording);
}

pub fn progress_view(recording: &Recording) -> RecordingProgress {
    let elapsed = (now_millis() - recording.start) / 1000;
    RecordingProgress {
        mode: recording.mode.clone(),
        frames: recording
            .frames
            .len()
            .max(recording.long.as_ref().map(|l| l.count).unwrap_or(0) as usize),
        elapsed,
        encoding: recording.encoding,
        progress: recording.progress,
        scroll_mode: recording.long.as_ref().map(|l| l.scroll_mode.clone()),
        hint: recording.long.as_ref().map(|l| l.hint.clone()),
    }
}

pub fn stop(app: &App) -> bool {
    let Some(recording) = app.core.lock().unwrap().recording.clone() else {
        return false;
    };
    recording.lock().unwrap().stopping = true;
    true
}

pub fn abort(app: &App) -> bool {
    let Some(recording) = app.core.lock().unwrap().recording.clone() else {
        return false;
    };
    let mut guard = recording.lock().unwrap();
    guard.cancelled = true;
    guard.stopping = true;
    true
}

pub fn set_manual(app: &App, hint: &str) -> bool {
    let Some(recording) = app.core.lock().unwrap().recording.clone() else {
        return false;
    };
    if let Some(long) = recording.lock().unwrap().long.as_mut() {
        long.manual(hint);
    }
    app.emit(AppEvent::RecordingProgress(progress_view(
        &recording.lock().unwrap(),
    )));
    true
}
