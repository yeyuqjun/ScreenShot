//! Local offline OCR engine (RapidOCR / `rapidocr-core` + dynamically loaded ONNX Runtime).
//!
//! Highlights:
//! - On-demand OCR models downloaded into `<exe_dir>/models/ocr/<model_id>/` from user-selected mirrors.
//! - Shares `onnxruntime.dll` with local offline translation, running completely offline once downloaded.

use crate::annotations::{Point, Stroke, Tool};
use crate::i18n;
use crate::images::Rgba;
use crate::model_files::{self, PendingFile};
use image::RgbImage;
use rapidocr_core::config::{ExecutionProvider, InferenceOptions, PipelineConfig};
use rapidocr_core::model::{
    ModelAssetKind, ModelAssetSpec, ModelSetSpec, PPOCRV4_EN_MOBILE, PPOCRV5_CH_MOBILE,
    PPOCRV5_CH_SERVER, PPOCRV5_EN_MOBILE, PPOCRV6_MEDIUM, PPOCRV6_SMALL, PPOCRV6_TINY,
};
use rapidocr_core::types::OcrLine;
use rapidocr_core::{OcrRuntime, RapidOcr};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Model tier tag displayed in the Settings UI catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OcrModelTier {
    Recommended,
    HighAccuracy,
    Lightweight,
}

/// Downloadable RapidOCR model set metadata.
#[derive(Clone, Copy, Debug)]
pub struct OcrModelPackSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub languages: &'static str,
    pub approx_mb: u32,
    pub tier: OcrModelTier,
    pub spec: &'static ModelSetSpec,
}

impl OcrModelPackSpec {
    /// Localized display title for this OCR model pack.
    pub fn display_label(&self) -> &'static str {
        i18n::t(self.label)
    }

    /// Localized short description for this OCR model pack.
    pub fn display_description(&self) -> &'static str {
        i18n::t(self.description)
    }

    /// Localized language coverage label.
    pub fn display_languages(&self) -> &'static str {
        i18n::t(self.languages)
    }

    /// Assets to download in logical order (det, rec, dict, cls).
    pub fn download_assets(&self) -> [ModelAssetSpec; 4] {
        [
            self.spec.det.asset,
            self.spec.rec.asset,
            self.spec.rec.dict,
            self.spec.cls.asset,
        ]
    }
}

/// Catalog of downloadable RapidOCR ONNX model sets in priority order.
pub const OCR_MODEL_CATALOG: &[OcrModelPackSpec] = &[
    OcrModelPackSpec {
        id: "ppocrv5-ch-mobile",
        label: "PP-OCRv5 Mobile (Chinese / Multilingual)",
        description: "Fast & balanced for Chinese, English, Japanese, and Traditional Chinese",
        languages: "ZH / EN / JA / CJK",
        approx_mb: 23,
        tier: OcrModelTier::Recommended,
        spec: &PPOCRV5_CH_MOBILE,
    },
    OcrModelPackSpec {
        id: "ppocrv6-small",
        label: "PP-OCRv6 Small (50+ Languages)",
        description: "Universal multilingual OCR with balanced speed and accuracy",
        languages: "50+ Languages",
        approx_mb: 30,
        tier: OcrModelTier::Recommended,
        spec: &PPOCRV6_SMALL,
    },
    OcrModelPackSpec {
        id: "ppocrv6-tiny",
        label: "PP-OCRv6 Tiny (Ultra-Light)",
        description: "Fastest startup and lowest memory footprint",
        languages: "ZH / EN / Multi",
        approx_mb: 13,
        tier: OcrModelTier::Lightweight,
        spec: &PPOCRV6_TINY,
    },
    OcrModelPackSpec {
        id: "ppocrv6-medium",
        label: "PP-OCRv6 Medium (50+ Languages · High Accuracy)",
        description: "Higher precision on dense or small multilingual text",
        languages: "50+ Languages",
        approx_mb: 116,
        tier: OcrModelTier::HighAccuracy,
        spec: &PPOCRV6_MEDIUM,
    },
    OcrModelPackSpec {
        id: "ppocrv5-ch-server",
        label: "PP-OCRv5 Server (Chinese / Multilingual · High Accuracy)",
        description: "Server-grade accuracy for complex Chinese and English layouts",
        languages: "ZH / EN / JA / CJK",
        approx_mb: 170,
        tier: OcrModelTier::HighAccuracy,
        spec: &PPOCRV5_CH_SERVER,
    },
    OcrModelPackSpec {
        id: "ppocrv5-en-mobile",
        label: "PP-OCRv5 English Mobile",
        description: "Compact model focused on English and Latin scripts",
        languages: "EN / Latin",
        approx_mb: 13,
        tier: OcrModelTier::Lightweight,
        spec: &PPOCRV5_EN_MOBILE,
    },
    OcrModelPackSpec {
        id: "ppocrv4-en-mobile",
        label: "PP-OCRv4 English Mobile (Legacy)",
        description: "Classic lightweight English OCR model",
        languages: "EN",
        approx_mb: 11,
        tier: OcrModelTier::Lightweight,
        spec: &PPOCRV4_EN_MOBILE,
    },
];

/// Look up an OCR model pack specification by ID.
pub fn find_model_pack(model_id: &str) -> Option<&'static OcrModelPackSpec> {
    OCR_MODEL_CATALOG
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(model_id.trim()))
}

/// Local installation status for a single OCR model pack.
#[derive(Clone, Debug)]
pub struct OcrModelInstallState {
    pub pack: &'static OcrModelPackSpec,
    /// True when all files required for the active pipeline (`use_cls`) are present.
    pub installed: bool,
    /// Directory containing this model pack (or canonical target directory if not installed).
    pub dir: PathBuf,
    /// Total disk usage in bytes for this pack's present files.
    pub disk_bytes: u64,
    pub missing: Vec<&'static str>,
    pub invalid: Vec<&'static str>,
    pub local_dirs: Vec<PathBuf>,
}

/// Download progress snapshot for a single OCR model pack.
#[derive(Clone, Debug)]
pub struct OcrDownloadProgress {
    #[allow(dead_code)]
    pub model_id: String,
    pub file_index: usize,
    pub file_count: usize,
    pub current_file: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

impl OcrDownloadProgress {
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.downloaded_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// Local OCR asset status surfaced in the Settings UI.
#[derive(Clone, Debug)]
pub struct LocalOcrStatus {
    pub ready: bool,
    pub model_set_name: Option<&'static str>,
    pub model_set_family: Option<&'static str>,
    pub model_dir: Option<PathBuf>,
    pub runtime_dll: Option<PathBuf>,
    pub storage_dir: PathBuf,
    pub models: Vec<OcrModelInstallState>,
    pub installed_count: usize,
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EngineKey {
    dll_path: PathBuf,
    model_dir: PathBuf,
    model_set_name: &'static str,
    use_cls: bool,
    intra_threads: usize,
}

struct CachedEngine {
    key: EngineKey,
    engine: RapidOcr,
}

struct SharedRuntimeState {
    runtime: OcrRuntime,
    dll_path: PathBuf,
}

static ENGINE_CACHE: Mutex<Option<CachedEngine>> = Mutex::new(None);
static SHARED_RUNTIME: Mutex<Option<SharedRuntimeState>> = Mutex::new(None);

/// Default root models directory next to the executable: `<exe_dir>/models`.
pub fn default_exe_model_dir() -> PathBuf {
    exe_dir().join("models")
}

/// Default OCR model directory next to the executable: `<exe_dir>/models/ocr`.
pub fn default_ocr_dir() -> PathBuf {
    default_exe_model_dir().join("ocr")
}

/// Return the managed directory path for a specific OCR model pack.
pub fn model_pack_dir_in(base_dir: &Path, model_id: &str) -> PathBuf {
    base_dir.join(model_id)
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default()
}

/// Walk up from `target/debug` or `target/release` to locate the workspace root during development.
fn workspace_root_from_exe(exe_dir: &Path) -> Option<PathBuf> {
    let mut cursor = Some(exe_dir);
    for _ in 0..5 {
        let dir = cursor?;
        if dir.join(".runtime").join("ocr").is_dir() || dir.join("Cargo.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        cursor = dir.parent();
    }
    None
}

/// Collect fallback/flat directories searched in addition to `<storage_dir>/<model_id>/`.
fn fallback_model_dirs(custom_dir: &str, storage_dir: &Path, data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let trimmed = custom_dir.trim();
    if !trimmed.is_empty() {
        let custom = PathBuf::from(trimmed);
        dirs.push(custom.clone());
        dirs.push(custom.join("ocr"));
        dirs.push(custom.join("models"));
        return dedup_paths(dirs);
    }

    dirs.push(storage_dir.to_path_buf());
    let exe = exe_dir();
    if !exe.as_os_str().is_empty() {
        dirs.push(exe.join("models"));
        dirs.push(exe.clone());
        dirs.push(exe.join("ocr"));
        dirs.push(exe.join("resources").join("ocr").join("models"));
        dirs.push(exe.join("resources").join("ocr"));
        if let Some(root) = workspace_root_from_exe(&exe) {
            dirs.push(root.join(".runtime").join("ocr").join("models"));
            dirs.push(root.join(".runtime").join("ocr"));
        }
    }
    dirs.push(data_dir.join("ocr").join("models"));
    dirs.push(data_dir.join("ocr"));
    dedup_paths(dirs)
}

fn format_error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !text.is_empty() && !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

/// Return the `onnxruntime.dll` path already loaded into this process, if any.
fn loaded_runtime_dll() -> Option<PathBuf> {
    SHARED_RUNTIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|state| state.dll_path.clone())
}

/// Load or reuse the process-wide shared ONNX Runtime environment.
///
/// ONNX Runtime and `rapidocr-core` only allow a single loaded library and
/// environment per process. Once initialized, both local OCR and local
/// translation reuse the same runtime even if model directories change later.
pub fn load_shared_runtime(dll_path: &Path) -> Result<OcrRuntime, String> {
    let mut guard = SHARED_RUNTIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = guard.as_ref() {
        return Ok(state.runtime.clone());
    }
    let runtime = OcrRuntime::load_from(dll_path).map_err(|e| {
        i18n::tf(
            "Unable to load ONNX Runtime ({path}): {err}",
            &[
                ("path", &dll_path.display().to_string()),
                ("err", &format_error_chain(&e)),
            ],
        )
    })?;
    *guard = Some(SharedRuntimeState {
        runtime: runtime.clone(),
        dll_path: dll_path.to_path_buf(),
    });
    Ok(runtime)
}

/// Locate an available `onnxruntime.dll` shared by local OCR and local translation.
pub fn resolve_runtime_dll(
    custom_dir: &str,
    preferred_dir: Option<&Path>,
    data_dir: &Path,
) -> Option<PathBuf> {
    if let Some(loaded) = loaded_runtime_dll().filter(|p| p.is_file()) {
        return Some(loaded);
    }
    candidate_runtime_dlls(custom_dir, preferred_dir, data_dir)
        .into_iter()
        .find(|p| p.is_file())
}

/// Collect candidate paths for `onnxruntime.dll`.
fn candidate_runtime_dlls(
    custom_dir: &str,
    resolved_model_dir: Option<&Path>,
    data_dir: &Path,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let trimmed = custom_dir.trim();
    if !trimmed.is_empty() {
        let custom = PathBuf::from(trimmed);
        paths.push(custom.join("onnxruntime.dll"));
        if let Some(parent) = custom.parent() {
            paths.push(parent.join("onnxruntime.dll"));
            if let Some(grandparent) = parent.parent() {
                paths.push(grandparent.join("onnxruntime.dll"));
            }
        }
    }
    let exe = exe_dir();
    if !exe.as_os_str().is_empty() {
        paths.push(exe.join("onnxruntime.dll"));
        paths.push(exe.join("models").join("onnxruntime.dll"));
        paths.push(exe.join("models").join("ocr").join("onnxruntime.dll"));
        paths.push(exe.join("resources").join("ocr").join("onnxruntime.dll"));
    }
    if let Some(mdir) = resolved_model_dir {
        paths.push(mdir.join("onnxruntime.dll"));
        if let Some(parent) = mdir.parent() {
            paths.push(parent.join("onnxruntime.dll"));
            if let Some(grandparent) = parent.parent() {
                paths.push(grandparent.join("onnxruntime.dll"));
            }
        }
    }
    if !exe.as_os_str().is_empty() {
        if let Some(root) = workspace_root_from_exe(&exe) {
            paths.push(root.join(".runtime").join("ocr").join("onnxruntime.dll"));
        }
    }
    paths.push(data_dir.join("ocr").join("onnxruntime.dll"));
    dedup_paths(paths)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        if !out.iter().any(|existing| existing == &p) {
            out.push(p);
        }
    }
    out
}

fn has_flat_pack(dir: &Path, pack: &OcrModelPackSpec) -> bool {
    // A shared detector, dictionary, or classifier alone doesn't identify a pack.
    pack.spec.assets().iter().any(|asset| {
        model_files::has_local_file(&dir.join(asset.filename))
            && !OCR_MODEL_CATALOG.iter().any(|other| {
                other.id != pack.id
                    && other
                        .spec
                        .assets()
                        .iter()
                        .any(|a| a.filename == asset.filename)
            })
    })
}

fn model_disk_bytes_in(dir: &Path, spec: &ModelSetSpec) -> u64 {
    spec.assets()
        .iter()
        .filter_map(|asset| dir.join(asset.filename).metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

fn inspect_model_pack(
    pack: &'static OcrModelPackSpec,
    storage_dir: &Path,
    fallback_dirs: &[PathBuf],
    use_cls: bool,
) -> OcrModelInstallState {
    let managed_dir = model_pack_dir_in(storage_dir, pack.id);
    let mut local_dirs = Vec::new();
    if pack
        .spec
        .assets()
        .iter()
        .any(|asset| model_files::has_local_file(&managed_dir.join(asset.filename)))
    {
        local_dirs.push(managed_dir.clone());
    }
    local_dirs.extend(
        fallback_dirs
            .iter()
            .filter(|dir| has_flat_pack(dir, pack))
            .cloned(),
    );
    let check = |dir: &Path| {
        let mut missing = Vec::new();
        let mut invalid = Vec::new();
        let mut installed = true;
        for asset in pack.spec.assets() {
            let path = dir.join(asset.filename);
            let required = use_cls || asset.kind != ModelAssetKind::Classification;
            if !existing_asset_verified(&path, asset.sha256) {
                if path.exists() {
                    invalid.push(asset.filename);
                } else if required {
                    missing.push(asset.filename);
                }
                installed &= !required;
            }
        }
        (installed, missing, invalid)
    };
    let mut best_dir = local_dirs.first().unwrap_or(&managed_dir);
    let mut best = check(best_dir);
    for dir in local_dirs.iter().skip(1) {
        if best.0 {
            break;
        }
        let candidate = check(dir);
        if candidate.0
            || (candidate.1.len() + candidate.2.len(), candidate.1.len())
                < (best.1.len() + best.2.len(), best.1.len())
        {
            best_dir = dir;
            best = candidate;
        }
    }
    OcrModelInstallState {
        pack,
        installed: best.0,
        disk_bytes: model_disk_bytes_in(best_dir, pack.spec),
        dir: best_dir.clone(),
        missing: best.1,
        invalid: best.2,
        local_dirs,
    }
}

/// Inspect whether local RapidOCR models and runtime DLL are ready.
pub fn inspect_status(data_dir: &Path, settings: &Value) -> LocalOcrStatus {
    let local = &settings["providers"]["ocr"]["local"];
    let custom_dir = local["modelDir"].as_str().unwrap_or("");
    let selected_model = local["model"].as_str().unwrap_or("");
    let use_cls = local["useCls"].as_bool().unwrap_or(false);
    inspect_status_with_options(custom_dir, selected_model, use_cls, data_dir)
}

fn inspect_status_with_options(
    custom_dir: &str,
    selected_model: &str,
    use_cls: bool,
    data_dir: &Path,
) -> LocalOcrStatus {
    let storage_dir = if !custom_dir.trim().is_empty() {
        PathBuf::from(custom_dir.trim())
    } else {
        default_ocr_dir()
    };

    let fallback_dirs = fallback_model_dirs(custom_dir, &storage_dir, data_dir);
    let models: Vec<OcrModelInstallState> = OCR_MODEL_CATALOG
        .iter()
        .map(|pack| inspect_model_pack(pack, &storage_dir, &fallback_dirs, use_cls))
        .collect();
    let installed_count = models.iter().filter(|m| m.installed).count();

    let preferred = find_model_pack(selected_model);
    let active = match preferred {
        Some(pack) => models.iter().find(|state| state.pack.id == pack.id),
        None => models
            .iter()
            .find(|state| state.installed)
            .or_else(|| models.iter().find(|state| !state.local_dirs.is_empty()))
            .or_else(|| models.first()),
    };
    let resolved_model_dir = active
        .filter(|state| !state.local_dirs.is_empty())
        .map(|state| state.dir.clone());
    let resolved_spec = active
        .filter(|state| state.installed)
        .map(|state| state.pack.spec);
    let mut missing: Vec<String> = active
        .into_iter()
        .flat_map(|state| state.missing.iter().chain(state.invalid.iter()))
        .map(|name| (*name).to_string())
        .collect();

    let runtime_dll = resolve_runtime_dll(
        custom_dir,
        resolved_model_dir.as_deref().or(Some(&storage_dir)),
        data_dir,
    );
    if runtime_dll.is_none() {
        missing.push("onnxruntime.dll".to_string());
    }

    let ready = resolved_spec.is_some() && runtime_dll.is_some();
    LocalOcrStatus {
        ready,
        model_set_name: resolved_spec.map(|s| s.name),
        model_set_family: resolved_spec.map(|s| s.family),
        model_dir: resolved_model_dir,
        runtime_dll,
        storage_dir,
        models,
        installed_count,
        missing,
    }
}

// ============================================================================
// OCR model download & lifecycle management
// ============================================================================

const MODELSCOPE_CN_PREFIX: &str =
    "https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/";

/// Build the download URL for a single RapidOCR asset according to the selected mirror.
pub fn build_download_url(mirror: &str, upstream_url: &str) -> String {
    let Some(rest) = upstream_url.strip_prefix(MODELSCOPE_CN_PREFIX) else {
        return upstream_url.to_string();
    };
    let rel_path = rest.split_once('/').map(|(_, tail)| tail).unwrap_or(rest);
    match mirror.trim() {
        "modelscope-ai" => {
            format!(
                "https://www.modelscope.ai/models/RapidAI/RapidOCR/resolve/{}",
                rest
            )
        }
        "hf-mirror" => {
            format!(
                "https://hf-mirror.com/DjB314/RapidOCR/resolve/main/v3.9.2/{}",
                rel_path
            )
        }
        "huggingface" => {
            format!(
                "https://huggingface.co/DjB314/RapidOCR/resolve/main/v3.9.2/{}",
                rel_path
            )
        }
        _ => upstream_url.to_string(),
    }
}

/// Approximate byte size for an entire OCR model pack (used for initial download progress).
pub fn approx_pack_bytes(pack: &OcrModelPackSpec) -> u64 {
    (pack.approx_mb as u64) * 1024 * 1024
}

fn approx_asset_bytes(pack: &OcrModelPackSpec, asset: &ModelAssetSpec) -> u64 {
    let total = approx_pack_bytes(pack);
    let dict_bytes = 80 * 1024u64;
    let cls_bytes = if pack.id.contains("server") {
        6 * 1024 * 1024
    } else {
        1_500_000
    };
    let core = total.saturating_sub(dict_bytes + cls_bytes).max(1024 * 1024);
    match asset.kind {
        ModelAssetKind::Dictionary => dict_bytes,
        ModelAssetKind::Classification => cls_bytes,
        ModelAssetKind::Detection => (core * 35) / 100,
        ModelAssetKind::Recognition => (core * 65) / 100,
        _ => core / 2,
    }
}

fn file_sha256_hex(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

fn existing_asset_verified(path: &Path, expected_sha256: Option<&'static str>) -> bool {
    model_files::checked_file(
        path,
        expected_sha256.unwrap_or("onnx"),
        || match expected_sha256 {
            Some(expected) => {
                file_sha256_hex(path).is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
            }
            None => model_files::has_onnx_envelope(path),
        },
    )
    .is_some()
}

/// Stream-download an OCR model pack into `<base_dir>/<model_id>/` with SHA-256 verification,
/// atomic rename, live progress callbacks, and cancellation support.
pub async fn download_model_pack<F>(
    model_id: &str,
    base_dir: &Path,
    mirror: &str,
    cancel: Arc<AtomicBool>,
    mut on_progress: F,
) -> Result<PathBuf, String>
where
    F: FnMut(OcrDownloadProgress),
{
    let pack = find_model_pack(model_id)
        .ok_or_else(|| i18n::tf("OCR model pack not found: {id}", &[("id", model_id)]))?;

    let target_dir = model_pack_dir_in(base_dir, pack.id);
    std::fs::create_dir_all(&target_dir).map_err(|e| {
        i18n::tf(
            "Unable to create OCR model directory ({path}): {err}",
            &[
                ("path", &target_dir.display().to_string()),
                ("err", &e.to_string()),
            ],
        )
    })?;

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .user_agent(format!("SnapDock/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| {
            i18n::tf(
                "Failed to create download client: {err}",
                &[("err", &e.to_string())],
            )
        })?;

    let assets = pack.download_assets();
    let approx_sizes: Vec<u64> = assets
        .iter()
        .map(|a| approx_asset_bytes(pack, a))
        .collect();
    let total_estimated: u64 = approx_sizes.iter().sum();
    let mut completed_bytes: u64 = 0;
    let file_count = assets.len();

    for (idx, asset) in assets.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(i18n::t("Download cancelled").into());
        }

        let dest = target_dir.join(asset.filename);
        if existing_asset_verified(&dest, asset.sha256) {
            let size = dest
                .metadata()
                .map(|m| m.len())
                .unwrap_or(approx_sizes[idx]);
            completed_bytes = completed_bytes.saturating_add(size);
            on_progress(OcrDownloadProgress {
                model_id: pack.id.to_string(),
                file_index: idx + 1,
                file_count,
                current_file: asset.filename.to_string(),
                downloaded_bytes: completed_bytes,
                total_bytes: total_estimated.max(completed_bytes),
            });
            continue;
        }

        let url = build_download_url(mirror, asset.url);
        let mut resp = client.get(&url).send().await.map_err(|e| {
            i18n::tf(
                "Failed to connect to download source ({file}): {err}",
                &[("file", asset.filename), ("err", &e.to_string())],
            )
        })?;

        if !resp.status().is_success() {
            return Err(i18n::tf(
                "Failed to download {file} (HTTP {status}); try switching the download mirror in Settings",
                &[
                    ("file", asset.filename),
                    ("status", &resp.status().as_u16().to_string()),
                ],
            ));
        }

        let content_len = resp.content_length();
        let remote_len = content_len.unwrap_or(approx_sizes[idx]);
        let remaining_est: u64 = approx_sizes[idx + 1..].iter().sum();
        let effective_total = completed_bytes
            .saturating_add(remote_len)
            .saturating_add(remaining_est);

        let (pending, mut out) = PendingFile::create(&dest).map_err(|e| {
            i18n::tf(
                "Unable to write temporary file {path}: {err}",
                &[
                    ("path", &model_files::part_path(&dest).display().to_string()),
                    ("err", &e.to_string()),
                ],
            )
        })?;

        let mut hasher = Sha256::new();
        let mut file_downloaded: u64 = 0;
        let mut last_emit = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(200))
            .unwrap_or_else(std::time::Instant::now);

        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            i18n::tf(
                "Download interrupted for {file}: {err}",
                &[("file", asset.filename), ("err", &e.to_string())],
            )
        })? {
            if cancel.load(Ordering::Relaxed) {
                return Err(i18n::t("Download cancelled").into());
            }
            out.write_all(&chunk).map_err(|e| {
                i18n::tf(
                    "Failed to write file {file}: {err}",
                    &[("file", asset.filename), ("err", &e.to_string())],
                )
            })?;
            hasher.update(&chunk);
            file_downloaded = file_downloaded.saturating_add(chunk.len() as u64);

            if last_emit.elapsed() >= std::time::Duration::from_millis(120) {
                last_emit = std::time::Instant::now();
                let current_total = completed_bytes.saturating_add(file_downloaded);
                on_progress(OcrDownloadProgress {
                    model_id: pack.id.to_string(),
                    file_index: idx + 1,
                    file_count,
                    current_file: asset.filename.to_string(),
                    downloaded_bytes: current_total,
                    total_bytes: effective_total.max(current_total),
                });
            }
        }

        out.flush().map_err(|e| {
            i18n::tf(
                "Failed to flush {file}: {err}",
                &[("file", asset.filename), ("err", &e.to_string())],
            )
        })?;
        drop(out);

        if file_downloaded == 0 {
            return Err(i18n::tf(
                "Downloaded file {file} is empty; please try again",
                &[("file", asset.filename)],
            ));
        }

        if cancel.load(Ordering::Relaxed) {
            return Err(i18n::t("Download cancelled").into());
        }
        if content_len.is_some_and(|expected| expected != file_downloaded) {
            return Err(i18n::tf(
                "Model file failed validation: {file}",
                &[("file", asset.filename)],
            ));
        }

        if let Some(expected_sha) = asset.sha256 {
            let actual_sha = format!("{:x}", hasher.finalize());
            if !actual_sha.eq_ignore_ascii_case(expected_sha) {
                return Err(i18n::tf(
                    "Checksum verification failed for {file}; try switching the download mirror in Settings",
                    &[("file", asset.filename)],
                ));
            }
        }

        if asset.sha256.is_none() && !existing_asset_verified(pending.path(), asset.sha256) {
            return Err(i18n::tf(
                "Model file failed validation: {file}",
                &[("file", asset.filename)],
            ));
        }
        pending.install(&dest).map_err(|e| {
            i18n::tf(
                "Failed to save file {file}: {err}",
                &[("file", asset.filename), ("err", &e.to_string())],
            )
        })?;

        completed_bytes = completed_bytes.saturating_add(file_downloaded);
        on_progress(OcrDownloadProgress {
            model_id: pack.id.to_string(),
            file_index: idx + 1,
            file_count,
            current_file: asset.filename.to_string(),
            downloaded_bytes: completed_bytes,
            total_bytes: effective_total.max(completed_bytes),
        });
    }

    Ok(target_dir)
}

/// Delete the selected pack while retaining files shared with other flat packs.
pub fn remove_model_pack(
    model_id: &str,
    base_dir: &Path,
    local_dirs: &[PathBuf],
) -> Result<(), String> {
    let pack = find_model_pack(model_id)
        .ok_or_else(|| i18n::tf("OCR model pack not found: {id}", &[("id", model_id)]))?;
    let managed_dir = model_pack_dir_in(base_dir, pack.id);
    let mut guard = ENGINE_CACHE.lock().unwrap_or_else(|err| err.into_inner());
    if guard
        .as_ref()
        .is_some_and(|cached| cached.key.model_set_name == pack.id)
    {
        *guard = None;
    }
    for dir in local_dirs {
        let remove = || -> std::io::Result<()> {
            for asset in pack.spec.assets() {
                let used_by_other_pack = dir != &managed_dir
                    && OCR_MODEL_CATALOG.iter().any(|other| {
                        other.id != pack.id
                            && other
                                .spec
                                .assets()
                                .iter()
                                .any(|a| a.filename == asset.filename)
                            && has_flat_pack(dir, other)
                    });
                if !used_by_other_pack {
                    model_files::remove_model_file(&dir.join(asset.filename))?;
                }
            }
            if dir == &managed_dir {
                model_files::remove_empty_dir(dir)?;
            }
            Ok(())
        };
        remove().map_err(|e| {
            i18n::tf(
                "Failed to delete model files ({path}): {err}",
                &[
                    ("path", &dir.display().to_string()),
                    ("err", &e.to_string()),
                ],
            )
        })?;
    }
    Ok(())
}

fn default_intra_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 4))
        .unwrap_or(2)
}

/// Flatten RGBA onto a white background for crisp OCR contrast.
pub fn rgba_to_rgb(image: &Rgba) -> RgbImage {
    let mut rgb = Vec::with_capacity((image.width as usize) * (image.height as usize) * 3);
    for px in image.pixels.chunks_exact(4) {
        let a = px[3] as f32 / 255.0;
        let inv = 255.0 * (1.0 - a);
        rgb.push((px[0] as f32 * a + inv).round().clamp(0.0, 255.0) as u8);
        rgb.push((px[1] as f32 * a + inv).round().clamp(0.0, 255.0) as u8);
        rgb.push((px[2] as f32 * a + inv).round().clamp(0.0, 255.0) as u8);
    }
    RgbImage::from_raw(image.width, image.height, rgb).expect("RGB buffer size mismatch")
}

/// Single recognized text block in original image pixel coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrTextBlock {
    /// Bounding box in original image pixels: `[min_x, min_y, max_x, max_y]`.
    pub rect: [f32; 4],
    /// Visual row index (used to join same-row blocks with space vs newline).
    pub row: usize,
    /// Recognized text in this block.
    pub text: String,
    /// Optional translated text corresponding to this block.
    pub translated: Option<String>,
    /// Sampled background RGB color around the block for in-place translation overlay.
    pub bg_rgb: [u8; 3],
    /// High-contrast foreground RGB color for overlay text rendering.
    pub fg_rgb: [u8; 3],
}

/// Complete OCR / translation payload for pin text-overlay rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrOverlayData {
    pub blocks: Vec<OcrTextBlock>,
    /// Full joined OCR text in natural reading order.
    pub full_text: String,
    /// Full joined translated text when translated.
    pub full_translated: Option<String>,
}

fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3000..=0x303F
            | 0x3040..=0x309F
            | 0x30A0..=0x30FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFF00..=0xFFEF
    )
}

fn join_segments_text(left: &str, right: &str) -> String {
    let mut out = String::with_capacity(left.len() + right.len() + 1);
    out.push_str(left);
    let last_cjk = left.chars().last().is_some_and(is_cjk_char);
    let first_cjk = right.chars().next().is_some_and(is_cjk_char);
    if !(last_cjk && first_cjk) {
        out.push(' ');
    }
    out.push_str(right);
    out
}

fn sample_pixel_rgb(image: &Rgba, x: i32, y: i32) -> [u8; 3] {
    if image.width == 0 || image.height == 0 {
        return [255, 255, 255];
    }
    let cx = x.clamp(0, image.width as i32 - 1) as u32;
    let cy = y.clamp(0, image.height as i32 - 1) as u32;
    let idx = ((cy * image.width + cx) * 4) as usize;
    if idx + 3 >= image.pixels.len() {
        return [255, 255, 255];
    }
    let a = image.pixels[idx + 3] as f32 / 255.0;
    let inv = 255.0 * (1.0 - a);
    [
        (image.pixels[idx] as f32 * a + inv).round().clamp(0.0, 255.0) as u8,
        (image.pixels[idx + 1] as f32 * a + inv).round().clamp(0.0, 255.0) as u8,
        (image.pixels[idx + 2] as f32 * a + inv).round().clamp(0.0, 255.0) as u8,
    ]
}

fn median_u8(values: &mut [u8]) -> u8 {
    if values.is_empty() {
        return 255;
    }
    values.sort_unstable();
    values[values.len() / 2]
}

fn relative_luminance_rgb(rgb: [u8; 3]) -> f32 {
    fn channel(v: u8) -> f32 {
        let s = v as f32 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

fn contrast_ratio_rgb(a: [u8; 3], b: [u8; 3]) -> f32 {
    let la = relative_luminance_rgb(a);
    let lb = relative_luminance_rgb(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Sample median background color around `rect` and high-contrast foreground color inside `rect`.
pub fn sample_block_colors(image: &Rgba, rect: [f32; 4]) -> ([u8; 3], [u8; 3]) {
    let w = image.width as i32;
    let h = image.height as i32;
    if w <= 0 || h <= 0 {
        return ([255, 255, 255], [20, 24, 28]);
    }
    let x0 = (rect[0].floor() as i32 - 2).clamp(0, w - 1);
    let y0 = (rect[1].floor() as i32 - 2).clamp(0, h - 1);
    let x1 = (rect[2].ceil() as i32 + 2).clamp(0, w - 1);
    let y1 = (rect[3].ceil() as i32 + 2).clamp(0, h - 1);

    let mut bg_r = Vec::with_capacity(64);
    let mut bg_g = Vec::with_capacity(64);
    let mut bg_b = Vec::with_capacity(64);

    let step_x = ((x1 - x0).max(1) / 16).max(1);
    let mut x = x0;
    while x <= x1 {
        for y in [y0, y1] {
            let px = sample_pixel_rgb(image, x, y);
            bg_r.push(px[0]);
            bg_g.push(px[1]);
            bg_b.push(px[2]);
        }
        x += step_x;
    }
    let step_y = ((y1 - y0).max(1) / 8).max(1);
    let mut y = y0;
    while y <= y1 {
        for sx in [x0, x1] {
            let px = sample_pixel_rgb(image, sx, y);
            bg_r.push(px[0]);
            bg_g.push(px[1]);
            bg_b.push(px[2]);
        }
        y += step_y;
    }

    let bg = [median_u8(&mut bg_r), median_u8(&mut bg_g), median_u8(&mut bg_b)];

    // Sample interior pixels and collect the highest-contrast ink candidates.
    let ix0 = (rect[0].floor() as i32).clamp(0, w - 1);
    let iy0 = (rect[1].floor() as i32).clamp(0, h - 1);
    let ix1 = (rect[2].ceil() as i32).clamp(ix0, w - 1);
    let iy1 = (rect[3].ceil() as i32).clamp(iy0, h - 1);
    let dx = ((ix1 - ix0).max(1) / 24).max(1);
    let dy = ((iy1 - iy0).max(1) / 10).max(1);

    let mut interior: Vec<(u32, [u8; 3])> = Vec::with_capacity(256);
    let mut sy = iy0;
    while sy <= iy1 {
        let mut sx = ix0;
        while sx <= ix1 {
            let px = sample_pixel_rgb(image, sx, sy);
            let dr = (px[0] as i32 - bg[0] as i32).unsigned_abs();
            let dg = (px[1] as i32 - bg[1] as i32).unsigned_abs();
            let db = (px[2] as i32 - bg[2] as i32).unsigned_abs();
            interior.push((dr + dg + db, px));
            sx += dx;
        }
        sy += dy;
    }

    let fallback_fg = if relative_luminance_rgb(bg) >= 0.42 {
        [20, 24, 28]
    } else {
        [244, 246, 250]
    };

    if interior.is_empty() {
        return (bg, fallback_fg);
    }

    interior.sort_by(|a, b| b.0.cmp(&a.0));
    let top_n = (interior.len() / 6).clamp(1, 24);
    let mut fg_r = Vec::with_capacity(top_n);
    let mut fg_g = Vec::with_capacity(top_n);
    let mut fg_b = Vec::with_capacity(top_n);
    for &(_, px) in interior.iter().take(top_n) {
        fg_r.push(px[0]);
        fg_g.push(px[1]);
        fg_b.push(px[2]);
    }
    let candidate_fg = [median_u8(&mut fg_r), median_u8(&mut fg_g), median_u8(&mut fg_b)];
    let fg = if contrast_ratio_rgb(bg, candidate_fg) >= 4.5 {
        candidate_fg
    } else {
        fallback_fg
    };
    (bg, fg)
}

/// Arrange detected OCR boxes into structured reading-order blocks with bounding boxes and sampled colors.
pub fn build_ocr_overlay(lines: &[OcrLine], image: &Rgba) -> OcrOverlayData {
    struct BoxedLine<'a> {
        min_x: f32,
        min_y: f32,
        max_x: f32,
        max_y: f32,
        center_y: f32,
        height: f32,
        text: &'a str,
    }

    let img_w = image.width.max(1) as f32;
    let img_h = image.height.max(1) as f32;

    let mut items: Vec<BoxedLine<'_>> = lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                return None;
            }
            let pts = &line.bbox.points;
            let min_x = pts
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min)
                .clamp(0.0, img_w);
            let max_x = pts
                .iter()
                .map(|p| p[0])
                .fold(f32::NEG_INFINITY, f32::max)
                .clamp(min_x, img_w);
            let min_y = pts
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min)
                .clamp(0.0, img_h);
            let max_y = pts
                .iter()
                .map(|p| p[1])
                .fold(f32::NEG_INFINITY, f32::max)
                .clamp(min_y, img_h);
            Some(BoxedLine {
                min_x,
                min_y,
                max_x: max_x.max(min_x + 1.0),
                max_y: max_y.max(min_y + 1.0),
                center_y: (min_y + max_y) * 0.5,
                height: (max_y - min_y).max(1.0),
                text: trimmed,
            })
        })
        .collect();

    if items.is_empty() {
        return OcrOverlayData {
            blocks: Vec::new(),
            full_text: String::new(),
            full_translated: None,
        };
    }

    // Deduplicate heavily overlapping OCR boxes (e.g. duplicate detections of the same line)
    // before grouping into rows so duplicate text does not get concatenated or painted twice.
    items.sort_by(|a, b| {
        let area_a = (a.max_x - a.min_x) * (a.max_y - a.min_y);
        let area_b = (b.max_x - b.min_x) * (b.max_y - b.min_y);
        b.text
            .chars()
            .count()
            .cmp(&a.text.chars().count())
            .then_with(|| area_b.total_cmp(&area_a))
    });
    let mut deduped: Vec<BoxedLine<'_>> = Vec::with_capacity(items.len());
    for item in items {
        let item_area = ((item.max_x - item.min_x) * (item.max_y - item.min_y)).max(1.0);
        let is_duplicate = deduped.iter().any(|kept| {
            let ix0 = item.min_x.max(kept.min_x);
            let iy0 = item.min_y.max(kept.min_y);
            let ix1 = item.max_x.min(kept.max_x);
            let iy1 = item.max_y.min(kept.max_y);
            if ix1 <= ix0 || iy1 <= iy0 {
                return false;
            }
            let inter = (ix1 - ix0) * (iy1 - iy0);
            let kept_area = ((kept.max_x - kept.min_x) * (kept.max_y - kept.min_y)).max(1.0);
            let overlap_ratio = inter / item_area.min(kept_area);
            overlap_ratio >= 0.72
        });
        if !is_duplicate {
            deduped.push(item);
        }
    }

    deduped.sort_by(|a, b| {
        a.center_y
            .total_cmp(&b.center_y)
            .then_with(|| a.min_x.total_cmp(&b.min_x))
    });

    let mut rows: Vec<Vec<BoxedLine<'_>>> = Vec::new();
    for item in deduped {
        let same_row = rows.last().is_some_and(|row| {
            let avg_y = row.iter().map(|r| r.center_y).sum::<f32>() / row.len() as f32;
            let row_min_y = row.iter().map(|r| r.min_y).fold(f32::INFINITY, f32::min);
            let row_max_y = row.iter().map(|r| r.max_y).fold(f32::NEG_INFINITY, f32::max);
            let min_h = row
                .iter()
                .map(|r| r.height)
                .fold(item.height, f32::min);
            let v_overlap = (item.max_y.min(row_max_y) - item.min_y.max(row_min_y)).max(0.0);
            (item.center_y - avg_y).abs() <= min_h * 0.50 && v_overlap >= min_h * 0.35
        });
        if same_row {
            rows.last_mut().unwrap().push(item);
        } else {
            rows.push(vec![item]);
        }
    }

    let mut blocks = Vec::new();
    let mut output_lines = Vec::with_capacity(rows.len());

    for (row_idx, mut row) in rows.into_iter().enumerate() {
        row.sort_by(|a, b| a.min_x.total_cmp(&b.min_x));

        // Merge horizontally adjacent segments on the same row into natural sentence blocks,
        // while keeping distant columns and incompatible font heights separate.
        let mut merged: Vec<([f32; 4], String)> = Vec::new();
        for item in row {
            let can_merge = merged.last().is_some_and(|(prev_rect, _)| {
                let prev_h = (prev_rect[3] - prev_rect[1]).max(1.0);
                let min_h = prev_h.min(item.height);
                let max_h = prev_h.max(item.height);
                let gap = item.min_x - prev_rect[2];
                let v_overlap = (prev_rect[3].min(item.max_y) - prev_rect[1].max(item.min_y)).max(0.0);
                gap <= max_h * 1.45 && max_h <= min_h * 1.55 && v_overlap >= min_h * 0.40
            });
            if can_merge {
                let (prev_rect, prev_text) = merged.last_mut().unwrap();
                prev_rect[0] = prev_rect[0].min(item.min_x);
                prev_rect[1] = prev_rect[1].min(item.min_y);
                prev_rect[2] = prev_rect[2].max(item.max_x);
                prev_rect[3] = prev_rect[3].max(item.max_y);
                *prev_text = join_segments_text(prev_text, item.text);
            } else {
                merged.push(([item.min_x, item.min_y, item.max_x, item.max_y], item.text.to_string()));
            }
        }

        let joined_row = merged
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        output_lines.push(joined_row);

        for (rect, text) in merged {
            let (bg_rgb, fg_rgb) = sample_block_colors(image, rect);
            blocks.push(OcrTextBlock {
                rect,
                row: row_idx,
                text,
                translated: None,
                bg_rgb,
                fg_rgb,
            });
        }
    }

    OcrOverlayData {
        blocks,
        full_text: output_lines.join("\n"),
        full_translated: None,
    }
}

/// Arrange detected OCR boxes in natural reading order (same row joined horizontally, new line per row).
pub fn format_ocr_lines(lines: &[OcrLine]) -> String {
    struct BoxedLine<'a> {
        min_x: f32,
        center_y: f32,
        height: f32,
        text: &'a str,
    }

    let mut items: Vec<BoxedLine<'_>> = lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                return None;
            }
            let pts = &line.bbox.points;
            let min_x = pts.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
            let min_y = pts.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
            let max_y = pts.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
            Some(BoxedLine {
                min_x,
                center_y: (min_y + max_y) * 0.5,
                height: (max_y - min_y).max(1.0),
                text: trimmed,
            })
        })
        .collect();

    if items.is_empty() {
        return String::new();
    }

    items.sort_by(|a, b| {
        a.center_y
            .total_cmp(&b.center_y)
            .then_with(|| a.min_x.total_cmp(&b.min_x))
    });

    let mut rows: Vec<Vec<BoxedLine<'_>>> = Vec::new();
    for item in items {
        let same_row = rows.last().is_some_and(|row| {
            let avg_y = row.iter().map(|r| r.center_y).sum::<f32>() / row.len() as f32;
            let min_h = row
                .iter()
                .map(|r| r.height)
                .fold(item.height, f32::min);
            (item.center_y - avg_y).abs() <= min_h * 0.55
        });
        if same_row {
            rows.last_mut().unwrap().push(item);
        } else {
            rows.push(vec![item]);
        }
    }

    let mut output_lines = Vec::with_capacity(rows.len());
    for mut row in rows {
        row.sort_by(|a, b| a.min_x.total_cmp(&b.min_x));
        let joined = row
            .into_iter()
            .map(|item| item.text)
            .collect::<Vec<_>>()
            .join(" ");
        output_lines.push(joined);
    }
    output_lines.join("\n")
}

fn run_engine_lines(
    image: &Rgba,
    data_dir: &Path,
    settings: &Value,
) -> Result<Vec<OcrLine>, String> {
    if image.width == 0 || image.height == 0 {
        return Err(i18n::t("Invalid image dimensions").into());
    }
    let local = &settings["providers"]["ocr"]["local"];
    let custom_dir = local["modelDir"].as_str().unwrap_or("");
    let selected_model = local["model"].as_str().unwrap_or("");
    let use_cls = local["useCls"].as_bool().unwrap_or(false);

    let mut guard = ENGINE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let status = inspect_status_with_options(custom_dir, selected_model, use_cls, data_dir);
    if !status.ready {
        if status.runtime_dll.is_none() {
            return Err(i18n::t(
                "onnxruntime.dll not found; ensure onnxruntime.dll exists in the application directory",
            )
            .into());
        }
        return Err(i18n::tf(
            "Local OCR files are unavailable: {missing}. Download or repair the model in Settings -> Text & Translation -> Text Recognition (OCR) ({dir})",
            &[
                ("missing", &status.missing.join(", ")),
                ("dir", &status.storage_dir.display().to_string()),
            ],
        ));
    }

    let dll_path = status.runtime_dll.unwrap();
    let model_dir = status.model_dir.unwrap();
    let spec_name = status.model_set_name.unwrap();
    let spec = find_model_pack(spec_name)
        .map(|p| p.spec)
        .ok_or_else(|| {
            i18n::tf("Model set configuration not found: {name}", &[("name", spec_name)])
        })?;

    let key = EngineKey {
        dll_path: dll_path.clone(),
        model_dir: model_dir.clone(),
        model_set_name: spec.name,
        use_cls,
        intra_threads: default_intra_threads(),
    };

    let rgb = rgba_to_rgb(image);

    let needs_rebuild = guard.as_ref().is_none_or(|cached| cached.key != key);
    if needs_rebuild {
        let runtime = load_shared_runtime(&key.dll_path)?;
        let mut cfg = spec.config(&key.model_dir);
        cfg.pipeline = if use_cls {
            PipelineConfig::full()
        } else {
            PipelineConfig::without_cls()
        };
        if !use_cls {
            cfg.cls = None;
        }
        cfg.inference = InferenceOptions::new()
            .with_intra_threads(key.intra_threads)
            .with_inter_threads(1)
            .with_parallel_execution(false)
            .with_cpu_mem_arena(true)
            .with_execution_provider(ExecutionProvider::Cpu);

        let engine = RapidOcr::new_with_runtime(cfg, &runtime).map_err(|e| {
            i18n::tf(
                "Failed to initialize RapidOCR model: {err}",
                &[("err", &e.to_string())],
            )
        })?;
        *guard = Some(CachedEngine { key, engine });
    }

    let cached = guard.as_mut().unwrap();
    let output = cached.engine.run_image(&rgb).map_err(|e| {
        i18n::tf(
            "Local OCR recognition failed: {err}",
            &[("err", &e.to_string())],
        )
    })?;
    Ok(output.lines)
}

/// Run local RapidOCR on an RGBA image (caches the initialized ONNX session).
pub fn recognize_rgba(
    image: &Rgba,
    data_dir: &Path,
    settings: &Value,
) -> Result<String, String> {
    let lines = run_engine_lines(image, data_dir, settings)?;
    let text = format_ocr_lines(&lines);
    if text.trim().is_empty() {
        return Err(i18n::t("No text detected in the image").into());
    }
    Ok(text)
}

/// Run local RapidOCR on an RGBA image and return per-block bounding boxes, colors, and full text.
pub fn recognize_rgba_with_blocks(
    image: &Rgba,
    data_dir: &Path,
    settings: &Value,
) -> Result<OcrOverlayData, String> {
    let lines = run_engine_lines(image, data_dir, settings)?;
    let overlay = build_ocr_overlay(&lines, image);
    if overlay.full_text.trim().is_empty() {
        return Err(i18n::t("No text detected in the image").into());
    }
    Ok(overlay)
}

/// Build an in-memory test image for testing local OCR from Settings when no capture exists.
pub fn build_test_image() -> Rgba {
    let width = 560u32;
    let height = 160u32;
    let base = Rgba::new(width, height, vec![255u8; (width * height * 4) as usize]);
    let strokes = vec![
        Stroke {
            id: "test-line-1".into(),
            tool: Tool::Text,
            points: vec![Point { x: 32.0, y: 28.0 }],
            color: "#111111".into(),
            fill_color: None,
            width: 3.0,
            text: Some("SnapDock Local OCR Test".into()),
            number: None,
            font_size: Some(32.0),
            font_family: Some("Microsoft YaHei".into()),
            opacity: Some(100.0),
            scale: Some(1.0),
            rotation: Some(0.0),
        },
        Stroke {
            id: "test-line-2".into(),
            tool: Tool::Text,
            points: vec![Point { x: 32.0, y: 88.0 }],
            color: "#111111".into(),
            fill_color: None,
            width: 3.0,
            text: Some("Hello RapidOCR 2026".into()),
            number: None,
            font_size: Some(30.0),
            font_family: Some("Microsoft YaHei".into()),
            opacity: Some(100.0),
            scale: Some(1.0),
            rotation: Some(0.0),
        },
    ];
    crate::annotations::rasterize_annotations(&base, &strokes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rapidocr_core::types::Quad;

    #[test]
    fn rgba_to_rgb_flattens_transparency_onto_white() {
        let img = Rgba::new(2, 1, vec![0, 0, 0, 0, 20, 40, 60, 255]);
        let rgb = rgba_to_rgb(&img);
        assert_eq!(rgb.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(rgb.get_pixel(1, 0).0, [20, 40, 60]);
    }

    #[test]
    fn format_ocr_lines_groups_same_row_and_orders_paragraphs() {
        let lines = vec![
            OcrLine::new(Quad::from_xyxy(120.0, 12.0, 220.0, 32.0), "World", 0.99),
            OcrLine::new(Quad::from_xyxy(10.0, 60.0, 150.0, 80.0), "Second line", 0.98),
            OcrLine::new(Quad::from_xyxy(10.0, 10.0, 90.0, 30.0), "Hello", 0.99),
        ];
        assert_eq!(format_ocr_lines(&lines), "Hello World\nSecond line");
    }

    #[test]
    fn build_ocr_overlay_merges_close_row_segments_and_separates_columns() {
        let img = Rgba::new(400, 120, vec![255u8; 400 * 120 * 4]);
        let lines = vec![
            OcrLine::new(Quad::from_xyxy(10.0, 10.0, 90.0, 30.0), "Hello", 0.99),
            OcrLine::new(Quad::from_xyxy(100.0, 11.0, 180.0, 31.0), "World", 0.99),
            // Far-right column on the same visual row should remain a separate block.
            OcrLine::new(Quad::from_xyxy(300.0, 10.0, 380.0, 30.0), "Right", 0.98),
            OcrLine::new(Quad::from_xyxy(10.0, 60.0, 160.0, 80.0), "Second row", 0.97),
        ];
        let overlay = build_ocr_overlay(&lines, &img);
        assert_eq!(overlay.blocks.len(), 3);
        assert_eq!(overlay.blocks[0].text, "Hello World");
        assert_eq!(overlay.blocks[0].row, 0);
        assert_eq!(overlay.blocks[0].rect, [10.0, 10.0, 180.0, 31.0]);
        assert_eq!(overlay.blocks[1].text, "Right");
        assert_eq!(overlay.blocks[1].row, 0);
        assert_eq!(overlay.blocks[2].text, "Second row");
        assert_eq!(overlay.blocks[2].row, 1);
        assert_eq!(overlay.full_text, "Hello World Right\nSecond row");
    }

    #[test]
    fn build_ocr_overlay_deduplicates_overlapping_boxes() {
        let img = Rgba::new(300, 80, vec![255u8; 300 * 80 * 4]);
        let lines = vec![
            OcrLine::new(Quad::from_xyxy(10.0, 10.0, 160.0, 32.0), "Hello World", 0.99),
            // Duplicate overlapping detection of almost the same box with shorter text.
            OcrLine::new(Quad::from_xyxy(12.0, 11.0, 155.0, 31.0), "Hello", 0.95),
            OcrLine::new(Quad::from_xyxy(10.0, 38.0, 140.0, 58.0), "Next line", 0.98),
        ];
        let overlay = build_ocr_overlay(&lines, &img);
        assert_eq!(overlay.blocks.len(), 2);
        assert_eq!(overlay.blocks[0].text, "Hello World");
        assert_eq!(overlay.blocks[1].text, "Next line");
        assert_eq!(overlay.full_text, "Hello World\nNext line");
    }

    #[test]
    fn sample_block_colors_adapts_to_light_and_dark_backgrounds() {
        // White background with dark ink pixels inside.
        let mut light_pixels = vec![255u8; 60 * 40 * 4];
        for y in 12..28 {
            for x in 12..48 {
                let idx = (y * 60 + x) * 4;
                light_pixels[idx] = 18;
                light_pixels[idx + 1] = 22;
                light_pixels[idx + 2] = 26;
                light_pixels[idx + 3] = 255;
            }
        }
        let light_img = Rgba::new(60, 40, light_pixels);
        let (bg, fg) = sample_block_colors(&light_img, [10.0, 10.0, 50.0, 30.0]);
        assert_eq!(bg, [255, 255, 255]);
        assert!(fg[0] < 60 && fg[1] < 60 && fg[2] < 60);

        // Dark background with light ink pixels inside.
        let mut dark_pixels = vec![0u8; 60 * 40 * 4];
        for px in dark_pixels.chunks_exact_mut(4) {
            px[0] = 24;
            px[1] = 26;
            px[2] = 30;
            px[3] = 255;
        }
        for y in 12..28 {
            for x in 12..48 {
                let idx = (y * 60 + x) * 4;
                dark_pixels[idx] = 240;
                dark_pixels[idx + 1] = 242;
                dark_pixels[idx + 2] = 245;
                dark_pixels[idx + 3] = 255;
            }
        }
        let dark_img = Rgba::new(60, 40, dark_pixels);
        let (bg2, fg2) = sample_block_colors(&dark_img, [10.0, 10.0, 50.0, 30.0]);
        assert_eq!(bg2, [24, 26, 30]);
        assert!(fg2[0] > 200 && fg2[1] > 200 && fg2[2] > 200);
    }

    #[test]
    fn default_ocr_dir_is_under_models_ocr() {
        let dir = default_ocr_dir();
        assert_eq!(dir.file_name().and_then(|n| n.to_str()), Some("ocr"));
        assert_eq!(
            dir.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()),
            Some("models")
        );
    }

    #[test]
    fn build_download_url_supports_all_mirrors() {
        let v5_url = PPOCRV5_CH_MOBILE.det.asset.url;
        assert_eq!(build_download_url("modelscope-cn", v5_url), v5_url);
        assert_eq!(
            build_download_url("modelscope-ai", v5_url),
            "https://www.modelscope.ai/models/RapidAI/RapidOCR/resolve/v3.9.0/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx"
        );
        assert_eq!(
            build_download_url("hf-mirror", v5_url),
            "https://hf-mirror.com/DjB314/RapidOCR/resolve/main/v3.9.2/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx"
        );
        assert_eq!(
            build_download_url("huggingface", v5_url),
            "https://huggingface.co/DjB314/RapidOCR/resolve/main/v3.9.2/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx"
        );

        let v6_dict_url = PPOCRV6_SMALL.rec.dict.url;
        assert_eq!(
            build_download_url("modelscope-ai", v6_dict_url),
            "https://www.modelscope.ai/models/RapidAI/RapidOCR/resolve/master/paddle/PP-OCRv6/rec/PP-OCRv6_rec_small/ppocrv6_dict.txt"
        );
        assert_eq!(
            build_download_url("hf-mirror", v6_dict_url),
            "https://hf-mirror.com/DjB314/RapidOCR/resolve/main/v3.9.2/paddle/PP-OCRv6/rec/PP-OCRv6_rec_small/ppocrv6_dict.txt"
        );
    }

    #[test]
    fn inspect_status_detects_missing_and_complete_assets_in_custom_dir() {
        let tmp = std::env::temp_dir().join(format!(
            "snapdock-ocr-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let status_missing = inspect_status_with_options(tmp.to_str().unwrap(), "", false, &tmp);
        assert!(!status_missing.ready);
        assert_eq!(status_missing.installed_count, 0);
        assert!(status_missing
            .missing
            .iter()
            .any(|f| f == "ch_PP-OCRv5_det_mobile.onnx"));
        assert!(status_missing
            .missing
            .iter()
            .any(|f| f == "ch_PP-OCRv5_rec_mobile.onnx"));
        assert!(status_missing
            .missing
            .iter()
            .any(|f| f == "ppocrv5_dict.txt"));

        // Nonempty junk must stay unavailable while remaining removable.
        let pack = find_model_pack("ppocrv5-ch-mobile").unwrap();
        let rec = tmp.join(pack.spec.rec.asset.filename);
        let shared_dict = tmp.join(pack.spec.rec.dict.filename);
        let other_rec = tmp.join(PPOCRV5_CH_SERVER.rec.asset.filename);
        std::fs::write(&rec, b"invalid model").unwrap();
        std::fs::write(&shared_dict, b"shared dictionary").unwrap();
        std::fs::write(&other_rec, b"other model").unwrap();

        let dir = model_pack_dir_in(&tmp, pack.id);
        std::fs::create_dir_all(&dir).unwrap();
        let part = model_files::part_path(&dir.join(pack.spec.det.asset.filename));
        std::fs::write(&part, b"partial").unwrap();
        let status = inspect_status_with_options(tmp.to_str().unwrap(), pack.id, false, &tmp);
        let state = status
            .models
            .iter()
            .find(|state| state.pack.id == pack.id)
            .unwrap();
        assert!(!status.ready);
        assert!(!state.installed);
        assert!(state.invalid.contains(&pack.spec.rec.asset.filename));
        assert!(state.missing.contains(&pack.spec.det.asset.filename));
        assert_eq!(state.local_dirs, vec![dir.clone(), tmp.clone()]);

        remove_model_pack(pack.id, &tmp, &state.local_dirs).unwrap();
        assert!(!rec.exists());
        assert!(!dir.exists());
        assert!(shared_dict.exists());
        assert!(other_rec.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn candidate_runtime_dlls_prefers_exe_dir_before_fallback_model_dir() {
        let model_dir = PathBuf::from(r"C:\fallback\ocr\models");
        let data_dir = PathBuf::from(r"C:\data");
        let candidates = candidate_runtime_dlls("", Some(&model_dir), &data_dir);
        let exe_dll = exe_dir().join("onnxruntime.dll");
        let model_dll = model_dir.join("onnxruntime.dll");
        let exe_idx = candidates.iter().position(|p| p == &exe_dll).unwrap();
        let model_idx = candidates.iter().position(|p| p == &model_dll).unwrap();
        assert!(exe_idx < model_idx);
    }

    #[test]
    fn load_shared_runtime_reuses_initialized_library_across_different_paths() {
        let Some(dll_a) = resolve_runtime_dll("", None, &std::env::temp_dir()) else {
            return;
        };
        let _rt1 = load_shared_runtime(&dll_a).expect("first runtime load should succeed");
        let tmp = std::env::temp_dir().join(format!(
            "snapdock-ort-reuse-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let dll_b = tmp.join("onnxruntime.dll");
        std::fs::write(&dll_b, b"stub").unwrap();
        let _rt2 = load_shared_runtime(&dll_b)
            .expect("second runtime load with another path should reuse initialized runtime");
        assert_eq!(
            resolve_runtime_dll(tmp.to_str().unwrap(), None, &tmp).as_deref(),
            Some(dll_a.as_path())
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn end_to_end_local_rapidocr_when_assets_present() {
        let settings = crate::settings::defaults();
        let data_dir = std::env::temp_dir();
        let status = inspect_status(&data_dir, settings);
        if !status.ready {
            // Skip live inference when models/runtime are not present.
            return;
        }
        let test_img = build_test_image();
        let text = recognize_rgba(&test_img, &data_dir, settings)
            .expect("local RapidOCR should recognize the built-in test image");
        assert!(
            text.contains("SnapDock") && text.contains("RapidOCR"),
            "unexpected OCR output: {}",
            text
        );
    }
}
