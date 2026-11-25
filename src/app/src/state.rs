//! Core application state and event bus: pure Rust without any WebView or Tauri dependencies.

use crate::annotations::Stroke;
use crate::images::Rgba;
use crate::native::NativeWindow;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use winit::event_loop::EventLoopProxy;

/// Destination for text-processing (OCR / translation) results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextTarget {
    Editor { capture_id: String },
    #[allow(dead_code)]
    Pin { pin_id: String },
    SettingsTest,
}

/// Overlay display mode for a text-overlay pin window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinOverlayMode {
    Ocr,
    Translation,
}

/// Snapshot of recording / scrolling capture progress.
#[derive(Clone, Debug)]
pub struct RecordingProgress {
    pub mode: String,
    pub frames: usize,
    pub elapsed: i64,
    pub encoding: bool,
    pub progress: f64,
    pub scroll_mode: Option<String>,
    pub hint: Option<String>,
}

/// Application events sent across threads to the main winit event loop.
#[derive(Clone, Debug)]
pub enum AppEvent {
    /// Open the standalone settings window (optionally targeting a section such as "providers").
    OpenSettings { section: Option<String> },
    /// Second instance launch or tray double-click: restore an active session or open settings.
    RestoreSession,
    /// Pre-initialize the shared GPU context and hidden overlay pool after tray startup.
    WarmupGraphics,
    StartCapture { mode: String, from_tray: bool },
    OpenOverlays,
    CancelCapture,
    OpenResult { id: String },
    OpenResultOcr { id: String },
    ResultClosed { id: String },
    ImageUpdated { id: String },
    SettingsChanged,
    AccountChanged,
    OcrModelsChanged,
    TranslationPacksChanged,
    CreatePin { id: String },
    ClosePin { pin_id: String },
    TogglePins,
    OpenRecorder,
    RecordingProgress(RecordingProgress),
    CloseRecording,
    Toast { message: String, error: bool },
    TextResult {
        target: TextTarget,
        kind: String,
        result: Result<String, String>,
    },
    PinOverlayResult {
        pin_id: String,
        mode: PinOverlayMode,
        result: Result<crate::ocr::OcrOverlayData, String>,
    },
    AccountMetaResult(Result<bool, String>),
    ReregisterHotkeys,
    Hotkey(global_hotkey::GlobalHotKeyEvent),
    ImportImage,
    Quit,
}

/// Session capture record; history index entries share the same schema (kept backward-compatible).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureItem {
    pub id: String,
    pub file: String,
    pub kind: String,
    pub title: String,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
}

/// Sync account (field names kept compatible with stored vault payloads).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Account {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub vault_salt: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub url: String,
}

/// UI display view of the signed-in sync account.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct AccountView {
    pub username: String,
    pub expires_at: i64,
    pub revision: u64,
    pub last_sync: String,
    pub dirty: bool,
}

/// In-memory session capture buffer; released on close if neither saved nor pinned.
#[derive(Clone)]
pub struct SessionCapture {
    pub item: CaptureItem,
    /// Composited output PNG (or original animated file for GIF).
    pub buffer: Vec<u8>,
    /// Thumbnail poster frame PNG; provides the first frame for GIFs.
    pub poster: Option<Vec<u8>>,
    /// Original unannotated image pixels (RGBA) for lossless re-rendering in the editor.
    pub source_rgba: Option<Rgba>,
    /// Current composited RGBA pixel cache for immediate display in the editor and pins.
    pub composed_rgba: Option<Rgba>,
    /// Editable annotation strokes.
    pub annotations: Vec<Stroke>,
    /// Decoded GIF animation frames (pixel buffer + delay in ms).
    pub gif_frames: Option<Arc<Vec<(Rgba, u32)>>>,
    /// Original physical screen top-left `(x, y)` where this capture was taken, if from screen selection.
    pub capture_origin: Option<(i32, i32)>,
}

/// Selection overlay entry (one per monitor).
pub struct OverlayEntry {
    pub index: usize,
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub monitor_width: u32,
    pub monitor_height: u32,
    pub image: Arc<Rgba>,
}

/// Active screen capture session.
pub struct ActiveCapture {
    pub mode: String,
    pub entries: Vec<OverlayEntry>,
    pub windows: Vec<NativeWindow>,
}

/// Floating desktop pin window state.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct PinState {
    pub pin_id: String,
    pub capture_id: String,
    pub scale: i64,
    pub opacity: i64,
    pub passthrough: bool,
}

/// State of an in-progress local OCR model download.
#[derive(Clone, Debug)]
pub struct OcrDownloadTask {
    pub progress: crate::ocr::OcrDownloadProgress,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// State of an in-progress local translation pack download.
#[derive(Clone, Debug)]
pub struct PackDownloadTask {
    pub progress: crate::translate::PackDownloadProgress,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

pub struct Core {
    pub settings: Value,
    pub account: Option<Account>,
    pub history: Vec<CaptureItem>,
    pub captures: HashMap<String, SessionCapture>,
    pub captures_order: Vec<String>,
    pub pins: HashMap<String, PinState>,
    pub result_id: Option<String>,
    pub quitting: bool,
    pub pins_hidden: bool,
    pub sync_flight: bool,
    pub last_sync: String,
    pub hotkey_warnings: Vec<String>,
    pub active_capture: Option<ActiveCapture>,
    pub recording: Option<crate::recording::SharedRecording>,
    pub ocr_downloads: HashMap<String, OcrDownloadTask>,
    pub pack_downloads: HashMap<String, PackDownloadTask>,
}

pub struct App {
    pub core: Mutex<Core>,
    pub vault_lock: tokio::sync::Mutex<()>,
    pub index_lock: tokio::sync::Mutex<()>,
    pub runtime: tokio::runtime::Runtime,
    pub proxy: Mutex<Option<EventLoopProxy<AppEvent>>>,
    pub data_dir: PathBuf,
    pub capture_dir: PathBuf,
    pub record_dir: PathBuf,
    pub vault_file: PathBuf,
    pub index_file: PathBuf,
    pub packaged: bool,
}

impl App {
    pub fn new(data_dir: PathBuf) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to initialize async runtime");
        let packaged = !cfg!(debug_assertions);
        Self {
            core: Mutex::new(Core {
                settings: crate::settings::normalize(None),
                account: None,
                history: Vec::new(),
                captures: HashMap::new(),
                captures_order: Vec::new(),
                pins: HashMap::new(),
                result_id: None,
                quitting: false,
                pins_hidden: false,
                sync_flight: false,
                last_sync: String::new(),
                hotkey_warnings: Vec::new(),
                active_capture: None,
                recording: None,
                ocr_downloads: HashMap::new(),
                pack_downloads: HashMap::new(),
            }),
            vault_lock: tokio::sync::Mutex::new(()),
            index_lock: tokio::sync::Mutex::new(()),
            runtime,
            proxy: Mutex::new(None),
            capture_dir: data_dir.join("captures"),
            record_dir: data_dir.join("recordings"),
            vault_file: data_dir.join("vault.bin"),
            index_file: data_dir.join("history.json"),
            data_dir,
            packaged,
        }
    }

    pub fn set_proxy(&self, proxy: EventLoopProxy<AppEvent>) {
        *self.proxy.lock().unwrap() = Some(proxy);
    }

    pub fn emit(&self, event: AppEvent) {
        if let Some(proxy) = self.proxy.lock().unwrap().as_ref() {
            let _ = proxy.send_event(event);
        }
    }

    pub fn notify(&self, message: impl Into<String>, error: bool) {
        self.emit(AppEvent::Toast {
            message: message.into(),
            error,
        });
    }

    pub fn picture_dir(&self) -> PathBuf {
        dirs::picture_dir().unwrap_or_else(|| self.data_dir.clone())
    }
}

pub fn account_view(core: &Core) -> Option<AccountView> {
    core.account.as_ref().map(|account| {
        let dirty = account.fingerprint != crate::settings::fingerprint(&core.settings);
        AccountView {
            username: account.username.clone(),
            expires_at: account.expires_at,
            revision: account.revision,
            last_sync: core.last_sync.clone(),
            dirty,
        }
    })
}
