//! Local offline OCR engine (RapidOCR / `rapidocr-core` + dynamically loaded ONNX Runtime).
//!
//! Highlights:
//! - On-demand OCR models downloaded into `<exe_dir>/models/ocr/<model_id>/` from user-selected mirrors.
//! - Shares `onnxruntime.dll` with local offline translation, running completely offline once downloaded.

use crate::annotations::{Point, Stroke, Tool};
use crate::i18n;
use crate::images::Rgba;
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

/// Default preferred model set (PP-OCRv5 mobile multilingual/CJK).
pub const DEFAULT_MODEL_SET: &ModelSetSpec = &PPOCRV5_CH_MOBILE;

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
    /// True when the orientation classifier (`cls`) model file is also present.
    pub has_cls: bool,
    /// True when installed in its managed `<storage_dir>/<id>/` subdirectory (safe to delete).
    pub managed: bool,
    /// Directory containing this model pack (or canonical target directory if not installed).
    pub dir: PathBuf,
    /// Total disk usage in bytes for this pack's present files.
    pub disk_bytes: u64,
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

static ENGINE_CACHE: Mutex<Option<CachedEngine>> = Mutex::new(None);

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

/// Locate an available `onnxruntime.dll` shared by local OCR and local translation.
pub fn resolve_runtime_dll(
    custom_dir: &str,
    preferred_dir: Option<&Path>,
    data_dir: &Path,
) -> Option<PathBuf> {
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
    if let Some(mdir) = resolved_model_dir {
        paths.push(mdir.join("onnxruntime.dll"));
        if let Some(parent) = mdir.parent() {
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

fn has_non_empty_file(dir: &Path, filename: &str) -> bool {
    dir.join(filename)
        .metadata()
        .is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn is_model_complete_in(dir: &Path, spec: &ModelSetSpec, use_cls: bool) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let pipeline = if use_cls {
        PipelineConfig::full()
    } else {
        PipelineConfig::without_cls()
    };
    spec.assets_for_pipeline(pipeline)
        .into_iter()
        .all(|asset| has_non_empty_file(dir, asset.filename))
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
    if is_model_complete_in(&managed_dir, pack.spec, use_cls) {
        let has_cls = has_non_empty_file(&managed_dir, pack.spec.cls.asset.filename);
        let disk_bytes = model_disk_bytes_in(&managed_dir, pack.spec);
        return OcrModelInstallState {
            pack,
            installed: true,
            has_cls,
            managed: true,
            dir: managed_dir,
            disk_bytes,
        };
    }

    for dir in fallback_dirs {
        if is_model_complete_in(dir, pack.spec, use_cls) {
            let has_cls = has_non_empty_file(dir, pack.spec.cls.asset.filename);
            let disk_bytes = model_disk_bytes_in(dir, pack.spec);
            return OcrModelInstallState {
                pack,
                installed: true,
                has_cls,
                managed: false,
                dir: dir.clone(),
                disk_bytes,
            };
        }
    }

    let managed_has_base = is_model_complete_in(&managed_dir, pack.spec, false);
    let disk_bytes = if managed_has_base {
        model_disk_bytes_in(&managed_dir, pack.spec)
    } else {
        0
    };
    OcrModelInstallState {
        pack,
        installed: false,
        has_cls: has_non_empty_file(&managed_dir, pack.spec.cls.asset.filename),
        managed: managed_has_base,
        dir: managed_dir,
        disk_bytes,
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

    let preferred_pack = find_model_pack(selected_model);
    let mut resolved_model_dir: Option<PathBuf> = None;
    let mut resolved_spec: Option<&'static ModelSetSpec> = None;
    let mut missing = Vec::new();

    // 1. Honor explicitly selected model when installed.
    if let Some(pref) = preferred_pack {
        if let Some(state) = models.iter().find(|m| m.pack.id == pref.id && m.installed) {
            resolved_model_dir = Some(state.dir.clone());
            resolved_spec = Some(state.pack.spec);
        } else if use_cls {
            // If user selected this model and turned on orientation classifier, check if only cls is missing.
            let managed_dir = model_pack_dir_in(&storage_dir, pref.id);
            let base_dir = if is_model_complete_in(&managed_dir, pref.spec, false) {
                Some(managed_dir)
            } else {
                fallback_dirs
                    .iter()
                    .find(|d| is_model_complete_in(d, pref.spec, false))
                    .cloned()
            };
            if let Some(dir) = base_dir {
                resolved_model_dir = Some(dir);
                missing.push(pref.spec.cls.asset.filename.to_string());
            }
        }
    }

    // 2. Otherwise fall back to the highest-priority installed model.
    if resolved_spec.is_none() && missing.is_empty() {
        if let Some(state) = models.iter().find(|m| m.installed) {
            resolved_model_dir = Some(state.dir.clone());
            resolved_spec = Some(state.pack.spec);
        }
    }

    // 3. If orientation classification is enabled and only the cls model is missing on any installed base model.
    if resolved_spec.is_none() && missing.is_empty() && use_cls {
        for pack in OCR_MODEL_CATALOG {
            let managed_dir = model_pack_dir_in(&storage_dir, pack.id);
            if is_model_complete_in(&managed_dir, pack.spec, false) {
                resolved_model_dir = Some(managed_dir);
                missing.push(pack.spec.cls.asset.filename.to_string());
                break;
            }
            if let Some(dir) = fallback_dirs
                .iter()
                .find(|d| is_model_complete_in(d, pack.spec, false))
            {
                resolved_model_dir = Some(dir.clone());
                missing.push(pack.spec.cls.asset.filename.to_string());
                break;
            }
        }
    }

    // 4. Report missing files for the preferred or default model set.
    if resolved_spec.is_none() && missing.is_empty() {
        let target_spec = preferred_pack.map(|p| p.spec).unwrap_or(DEFAULT_MODEL_SET);
        let pipeline = if use_cls {
            PipelineConfig::full()
        } else {
            PipelineConfig::without_cls()
        };
        let target_dir = model_pack_dir_in(&storage_dir, target_spec.name);
        for asset in target_spec.assets_for_pipeline(pipeline) {
            if !has_non_empty_file(&target_dir, asset.filename)
                && !has_non_empty_file(&storage_dir, asset.filename)
            {
                missing.push(asset.filename.to_string());
            }
        }
    }

    let dll_candidates = candidate_runtime_dlls(
        custom_dir,
        resolved_model_dir.as_deref().or(Some(&storage_dir)),
        data_dir,
    );
    let runtime_dll = dll_candidates.into_iter().find(|p| p.is_file());
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

fn existing_asset_verified(path: &Path, expected_sha256: Option<&str>) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    match expected_sha256 {
        Some(expected) => file_sha256_hex(path)
            .is_some_and(|actual| actual.eq_ignore_ascii_case(expected)),
        None => true,
    }
}

fn cleanup_part_files(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("part") {
                let _ = std::fs::remove_file(path);
            }
        }
    }
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
            cleanup_part_files(&target_dir);
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

        let remote_len = resp.content_length().unwrap_or(approx_sizes[idx]);
        let remaining_est: u64 = approx_sizes[idx + 1..].iter().sum();
        let effective_total = completed_bytes
            .saturating_add(remote_len)
            .saturating_add(remaining_est);

        let part_path = target_dir.join(format!("{}.part", asset.filename));
        let mut out = std::fs::File::create(&part_path).map_err(|e| {
            i18n::tf(
                "Unable to write temporary file {path}: {err}",
                &[
                    ("path", &part_path.display().to_string()),
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
                drop(out);
                let _ = std::fs::remove_file(&part_path);
                cleanup_part_files(&target_dir);
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
            let _ = std::fs::remove_file(&part_path);
            return Err(i18n::tf(
                "Downloaded file {file} is empty; please try again",
                &[("file", asset.filename)],
            ));
        }

        if let Some(expected_sha) = asset.sha256 {
            let actual_sha = format!("{:x}", hasher.finalize());
            if !actual_sha.eq_ignore_ascii_case(expected_sha) {
                let _ = std::fs::remove_file(&part_path);
                return Err(i18n::tf(
                    "Checksum verification failed for {file}; try switching the download mirror in Settings",
                    &[("file", asset.filename)],
                ));
            }
        }

        if dest.exists() {
            let _ = std::fs::remove_file(&dest);
        }
        std::fs::rename(&part_path, &dest).map_err(|e| {
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

/// Evict the cached OCR engine session if it uses `model_id`.
pub fn evict_cached_model(model_id: &str) {
    let mut guard = ENGINE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard
        .as_ref()
        .is_some_and(|cached| cached.key.model_set_name == model_id)
    {
        *guard = None;
    }
}

/// Delete a downloaded OCR model pack directory and evict any cached ONNX session.
pub fn remove_model_pack(model_id: &str, base_dir: &Path) -> Result<(), String> {
    let pack = find_model_pack(model_id)
        .ok_or_else(|| i18n::tf("OCR model pack not found: {id}", &[("id", model_id)]))?;
    evict_cached_model(pack.id);
    let dir = model_pack_dir_in(base_dir, pack.id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| {
            i18n::tf(
                "Failed to remove OCR model directory ({path}): {err}",
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

    let status = inspect_status_with_options(custom_dir, selected_model, use_cls, data_dir);
    if !status.ready {
        if status.runtime_dll.is_none() {
            return Err(i18n::t(
                "onnxruntime.dll not found; ensure onnxruntime.dll exists in the application directory",
            )
            .into());
        }
        return Err(i18n::tf(
            "Local RapidOCR model is not ready, missing: {missing}. Download an OCR model in Settings -> Text & Translation -> Text Recognition (OCR) ({dir})",
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

    let mut guard = ENGINE_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let needs_rebuild = guard.as_ref().is_none_or(|cached| cached.key != key);
    if needs_rebuild {
        let runtime = OcrRuntime::load_from(&key.dll_path).map_err(|e| {
            i18n::tf(
                "Unable to load ONNX Runtime ({path}): {err}",
                &[
                    ("path", &key.dll_path.display().to_string()),
                    ("err", &e.to_string()),
                ],
            )
        })?;
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

        // Write stub files to verify flat asset discovery.
        for name in [
            "ch_PP-OCRv5_det_mobile.onnx",
            "ch_PP-OCRv5_rec_mobile.onnx",
            "ppocrv5_dict.txt",
            "onnxruntime.dll",
        ] {
            std::fs::write(tmp.join(name), b"stub").unwrap();
        }
        let status_ready = inspect_status_with_options(tmp.to_str().unwrap(), "", false, &tmp);
        assert!(status_ready.ready);
        assert_eq!(status_ready.model_set_name, Some("ppocrv5-ch-mobile"));
        assert!(status_ready.missing.is_empty());

        // Write a managed subdirectory pack (ppocrv6-small) and select it explicitly.
        let v6_dir = model_pack_dir_in(&tmp, "ppocrv6-small");
        std::fs::create_dir_all(&v6_dir).unwrap();
        for asset in PPOCRV6_SMALL.assets() {
            std::fs::write(v6_dir.join(asset.filename), b"stub-v6").unwrap();
        }
        let status_v6 =
            inspect_status_with_options(tmp.to_str().unwrap(), "ppocrv6-small", true, &tmp);
        assert!(status_v6.ready);
        assert_eq!(status_v6.model_set_name, Some("ppocrv6-small"));
        assert_eq!(status_v6.model_dir.as_deref(), Some(v6_dir.as_path()));
        let v6_state = status_v6
            .models
            .iter()
            .find(|m| m.pack.id == "ppocrv6-small")
            .unwrap();
        assert!(v6_state.installed);
        assert!(v6_state.managed);
        assert!(v6_state.has_cls);

        // Removing the managed pack deletes its directory cleanly.
        remove_model_pack("ppocrv6-small", &tmp).unwrap();
        assert!(!v6_dir.exists());

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
