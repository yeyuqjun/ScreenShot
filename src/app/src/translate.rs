//! Local offline neural machine translation engine (OPUS-MT INT8 ONNX + shared ONNX Runtime).
//!
//! Highlights:
//! - Zero extra native runtime: shares `onnxruntime.dll` and `ort::Environment` with local RapidOCR.
//! - On-demand language packs: downloaded into `<exe_dir>/models/translate/<pack_id>/`.
//! - Automatic direct and English-pivot routing (e.g. `ja-en` + `en-zh` enables offline Japanese -> Chinese).

use crate::i18n;
use crate::model_files::{self, FileStamp, PendingFile};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionInputValue, SessionInputs};
use ort::value::{DynValue, Tensor};
use rapidocr_core::OcrRuntime;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// File specification for a single downloadable file inside a language pack.
#[derive(Clone, Copy, Debug)]
pub struct PackFileSpec {
    /// Relative path inside the remote HuggingFace repository.
    pub remote_path: &'static str,
    /// Local filename saved under `<translate_dir>/<pack_id>/`.
    pub local_name: &'static str,
    /// Approximate byte size used for UI size estimates and fallback progress.
    pub approx_bytes: u64,
}

/// Language pack category: universal multilingual, language-group, or dedicated single-pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackKind {
    Multilingual,
    Group,
    Dedicated,
}

/// Metadata for a downloadable open-source local translation pack (Helsinki-NLP OPUS-MT INT8 ONNX).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TranslationPackSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub source_lang: &'static str,
    pub target_lang: &'static str,
    pub repo: &'static str,
    pub approx_mb: u32,
    pub kind: PackKind,
    pub files: &'static [PackFileSpec],
}

impl TranslationPackSpec {
    /// Localized display label for this pack.
    pub fn display_label(&self) -> String {
        i18n::pack_label(self)
    }
}

impl PartialEq for PackFileSpec {
    fn eq(&self, other: &Self) -> bool {
        self.remote_path == other.remote_path && self.local_name == other.local_name
    }
}
impl Eq for PackFileSpec {}

const OPUS_FILES_EN_ZH: &[PackFileSpec] = &[
    PackFileSpec {
        remote_path: "config.json",
        local_name: "config.json",
        approx_bytes: 1_503,
    },
    PackFileSpec {
        remote_path: "tokenizer.json",
        local_name: "tokenizer.json",
        approx_bytes: 6_380_952,
    },
    PackFileSpec {
        remote_path: "onnx/encoder_model_quantized.onnx",
        local_name: "encoder_model_quantized.onnx",
        approx_bytes: 52_899_742,
    },
    PackFileSpec {
        remote_path: "onnx/decoder_model_merged_quantized.onnx",
        local_name: "decoder_model_merged_quantized.onnx",
        approx_bytes: 60_212_804,
    },
];

const OPUS_FILES_STD: &[PackFileSpec] = &[
    PackFileSpec {
        remote_path: "config.json",
        local_name: "config.json",
        approx_bytes: 1_400,
    },
    PackFileSpec {
        remote_path: "tokenizer.json",
        local_name: "tokenizer.json",
        approx_bytes: 6_000_000,
    },
    PackFileSpec {
        remote_path: "onnx/encoder_model_quantized.onnx",
        local_name: "encoder_model_quantized.onnx",
        approx_bytes: 51_000_000,
    },
    PackFileSpec {
        remote_path: "onnx/decoder_model_merged_quantized.onnx",
        local_name: "decoder_model_merged_quantized.onnx",
        approx_bytes: 58_000_000,
    },
];

const OPUS_FILES_EN_JA: &[PackFileSpec] = &[
    PackFileSpec {
        remote_path: "config.json",
        local_name: "config.json",
        approx_bytes: 1_377,
    },
    PackFileSpec {
        remote_path: "tokenizer.json",
        local_name: "tokenizer.json",
        approx_bytes: 5_068_572,
    },
    PackFileSpec {
        remote_path: "onnx/encoder_model_quantized.onnx",
        local_name: "encoder_model_quantized.onnx",
        approx_bytes: 43_312_542,
    },
    PackFileSpec {
        remote_path: "onnx/decoder_model_merged_quantized.onnx",
        local_name: "decoder_model_merged_quantized.onnx",
        approx_bytes: 50_550_704,
    },
];

/// Metadata for a supported translation language (42 languages, full bidirectional support).
#[derive(Clone, Copy, Debug)]
pub struct LanguageSpec {
    pub code: &'static str,
    pub en_name: &'static str,
    pub aliases: &'static [&'static str],
    /// Target prefix token candidates for `Xenova/opus-mt-en-mul` (in priority order).
    pub mul_prefixes: &'static [&'static str],
    /// Target prefix token candidates for `Xenova/opus-mt-en-ROMANCE` (Romance languages only).
    pub romance_prefixes: &'static [&'static str],
}

impl LanguageSpec {
    /// Display label used in dropdown pickers (localized name + native name when distinct).
    pub fn option_label(&self) -> String {
        i18n::lang_option_label(self.code)
    }
}

/// Full list of supported languages (42 common languages, any pair supported bidirectionally).
pub const LANGUAGES: &[LanguageSpec] = &[
    LanguageSpec {
        code: "zh",
        en_name: "Chinese Simplified",
        aliases: &["zh-cn", "zh-hans", "zho", "cmn", "chinese", "simplified chinese", "cn"],
        mul_prefixes: &[">>cmn_Hans<<", ">>cmn<<", ">>zho_Hans<<", ">>zho<<", ">>zh<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "zh-tw",
        en_name: "Chinese Traditional",
        aliases: &["zh-hant", "zh-hk", "zh_tw", "traditional chinese", "tw", "hk"],
        mul_prefixes: &[">>cmn_Hant<<", ">>zho_Hant<<", ">>cmn<<", ">>zho<<", ">>zh<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "en",
        en_name: "English",
        aliases: &["en-us", "en-gb", "eng"],
        mul_prefixes: &[">>eng<<", ">>en<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "ja",
        en_name: "Japanese",
        aliases: &["jp", "jpn", "jap"],
        mul_prefixes: &[">>jpn<<", ">>jap<<", ">>ja<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "ko",
        en_name: "Korean",
        aliases: &["kr", "kor"],
        mul_prefixes: &[">>kor<<", ">>ko<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "fr",
        en_name: "French",
        aliases: &["fra", "fre", "francais"],
        mul_prefixes: &[">>fra<<", ">>fr<<"],
        romance_prefixes: &[">>fr<<", ">>fra<<"],
    },
    LanguageSpec {
        code: "de",
        en_name: "German",
        aliases: &["deu", "ger", "deutsch"],
        mul_prefixes: &[">>deu<<", ">>de<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "es",
        en_name: "Spanish",
        aliases: &["spa", "espanol"],
        mul_prefixes: &[">>spa<<", ">>es<<"],
        romance_prefixes: &[">>es<<", ">>spa<<"],
    },
    LanguageSpec {
        code: "pt",
        en_name: "Portuguese",
        aliases: &["por", "pt-br", "pt-pt", "portugues"],
        mul_prefixes: &[">>por<<", ">>pt<<", ">>pt_br<<"],
        romance_prefixes: &[">>pt<<", ">>pt_br<<", ">>por<<"],
    },
    LanguageSpec {
        code: "ru",
        en_name: "Russian",
        aliases: &["rus"],
        mul_prefixes: &[">>rus<<", ">>ru<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "it",
        en_name: "Italian",
        aliases: &["ita", "italiano"],
        mul_prefixes: &[">>ita<<", ">>it<<"],
        romance_prefixes: &[">>it<<", ">>ita<<"],
    },
    LanguageSpec {
        code: "ar",
        en_name: "Arabic",
        aliases: &["ara"],
        mul_prefixes: &[">>ara<<", ">>ar<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "th",
        en_name: "Thai",
        aliases: &["tha"],
        mul_prefixes: &[">>tha<<", ">>th<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "vi",
        en_name: "Vietnamese",
        aliases: &["vie"],
        mul_prefixes: &[">>vie<<", ">>vi<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "id",
        en_name: "Indonesian",
        aliases: &["ind", "bahasa indonesia"],
        mul_prefixes: &[">>ind<<", ">>id<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "ms",
        en_name: "Malay",
        aliases: &["msa", "zsm", "may", "bahasa melayu"],
        mul_prefixes: &[">>zsm<<", ">>msa<<", ">>ind<<", ">>ms<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "nl",
        en_name: "Dutch",
        aliases: &["nld", "dut", "nederlands"],
        mul_prefixes: &[">>nld<<", ">>nl<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "pl",
        en_name: "Polish",
        aliases: &["pol", "polski"],
        mul_prefixes: &[">>pol<<", ">>pl<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "tr",
        en_name: "Turkish",
        aliases: &["tur", "turkce"],
        mul_prefixes: &[">>tur<<", ">>tr<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "uk",
        en_name: "Ukrainian",
        aliases: &["ukr"],
        mul_prefixes: &[">>ukr<<", ">>uk<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "hi",
        en_name: "Hindi",
        aliases: &["hin"],
        mul_prefixes: &[">>hin<<", ">>hi<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "sv",
        en_name: "Swedish",
        aliases: &["swe", "svenska"],
        mul_prefixes: &[">>swe<<", ">>sv<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "fi",
        en_name: "Finnish",
        aliases: &["fin", "suomi"],
        mul_prefixes: &[">>fin<<", ">>fi<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "da",
        en_name: "Danish",
        aliases: &["dan", "dansk"],
        mul_prefixes: &[">>dan<<", ">>da<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "no",
        en_name: "Norwegian",
        aliases: &["nb", "nob", "nno", "nor", "norsk"],
        mul_prefixes: &[">>nob<<", ">>nor<<", ">>no<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "cs",
        en_name: "Czech",
        aliases: &["ces", "cze", "cestina"],
        mul_prefixes: &[">>ces<<", ">>cs<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "sk",
        en_name: "Slovak",
        aliases: &["slk", "slo", "slovencina"],
        mul_prefixes: &[">>slk<<", ">>sk<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "ro",
        en_name: "Romanian",
        aliases: &["ron", "rum", "romana"],
        mul_prefixes: &[">>ron<<", ">>ro<<"],
        romance_prefixes: &[">>ro<<", ">>ron<<"],
    },
    LanguageSpec {
        code: "hu",
        en_name: "Hungarian",
        aliases: &["hun", "magyar"],
        mul_prefixes: &[">>hun<<", ">>hu<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "el",
        en_name: "Greek",
        aliases: &["ell", "gre"],
        mul_prefixes: &[">>ell<<", ">>el<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "he",
        en_name: "Hebrew",
        aliases: &["heb", "iw"],
        mul_prefixes: &[">>heb<<", ">>he<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "bg",
        en_name: "Bulgarian",
        aliases: &["bul"],
        mul_prefixes: &[">>bul<<", ">>bg<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "hr",
        en_name: "Croatian",
        aliases: &["hrv", "hrvatski"],
        mul_prefixes: &[">>hrv<<", ">>hr<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "sr",
        en_name: "Serbian",
        aliases: &["srp"],
        mul_prefixes: &[">>srp_Cyrl<<", ">>srp_Latn<<", ">>srp<<", ">>sr<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "ca",
        en_name: "Catalan",
        aliases: &["cat", "catala"],
        mul_prefixes: &[">>cat<<", ">>ca<<"],
        romance_prefixes: &[">>ca<<", ">>cat<<"],
    },
    LanguageSpec {
        code: "tl",
        en_name: "Tagalog",
        aliases: &["tgl", "fil", "filipino"],
        mul_prefixes: &[">>tgl<<", ">>tl<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "bn",
        en_name: "Bengali",
        aliases: &["ben"],
        mul_prefixes: &[">>ben<<", ">>bn<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "fa",
        en_name: "Persian",
        aliases: &["fas", "pes", "per", "farsi"],
        mul_prefixes: &[">>pes<<", ">>fas<<", ">>fa<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "ur",
        en_name: "Urdu",
        aliases: &["urd"],
        mul_prefixes: &[">>urd<<", ">>ur<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "lt",
        en_name: "Lithuanian",
        aliases: &["lit"],
        mul_prefixes: &[">>lit<<", ">>lt<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "lv",
        en_name: "Latvian",
        aliases: &["lav", "lvs"],
        mul_prefixes: &[">>lav<<", ">>lvs<<", ">>lv<<"],
        romance_prefixes: &[],
    },
    LanguageSpec {
        code: "et",
        en_name: "Estonian",
        aliases: &["est", "ekk", "eesti"],
        mul_prefixes: &[">>est<<", ">>ekk<<", ">>et<<"],
        romance_prefixes: &[],
    },
];

/// Catalog of downloadable open-source local translation packs (downloaded on demand).
/// The top two universal packs (`mul-en` and `en-mul`) unlock all 42 languages bidirectionally;
/// dedicated language packs below offer higher accuracy and are preferred automatically when installed.
pub const PACK_CATALOG: &[TranslationPackSpec] = &[
    // --- Universal multilingual base packs ---
    TranslationPackSpec {
        id: "mul-en",
        label: "Multilingual -> English (Universal, 40+ Languages)",
        source_lang: "mul",
        target_lang: "en",
        repo: "Xenova/opus-mt-mul-en",
        approx_mb: 114,
        kind: PackKind::Multilingual,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-mul",
        label: "English -> Multilingual (Universal, 40+ Languages)",
        source_lang: "en",
        target_lang: "mul",
        repo: "Xenova/opus-mt-en-mul",
        approx_mb: 114,
        kind: PackKind::Multilingual,
        files: OPUS_FILES_STD,
    },
    // --- Dedicated popular bidirectional packs ---
    TranslationPackSpec {
        id: "en-zh",
        label: "English -> Chinese Simplified",
        source_lang: "en",
        target_lang: "zh",
        repo: "Xenova/opus-mt-en-zh",
        approx_mb: 114,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_EN_ZH,
    },
    TranslationPackSpec {
        id: "zh-en",
        label: "Chinese Simplified -> English",
        source_lang: "zh",
        target_lang: "en",
        repo: "Xenova/opus-mt-zh-en",
        approx_mb: 114,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_EN_ZH,
    },
    TranslationPackSpec {
        id: "ja-en",
        label: "Japanese -> English",
        source_lang: "ja",
        target_lang: "en",
        repo: "Xenova/opus-mt-ja-en",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-ja",
        label: "English -> Japanese",
        source_lang: "en",
        target_lang: "ja",
        repo: "Xenova/opus-mt-en-jap",
        approx_mb: 95,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_EN_JA,
    },
    TranslationPackSpec {
        id: "ko-en",
        label: "Korean -> English",
        source_lang: "ko",
        target_lang: "en",
        repo: "Xenova/opus-mt-ko-en",
        approx_mb: 114,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_EN_ZH,
    },
    TranslationPackSpec {
        id: "fr-en",
        label: "French -> English",
        source_lang: "fr",
        target_lang: "en",
        repo: "Xenova/opus-mt-fr-en",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-fr",
        label: "English -> French",
        source_lang: "en",
        target_lang: "fr",
        repo: "Xenova/opus-mt-en-fr",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "de-en",
        label: "German -> English",
        source_lang: "de",
        target_lang: "en",
        repo: "Xenova/opus-mt-de-en",
        approx_mb: 107,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-de",
        label: "English -> German",
        source_lang: "en",
        target_lang: "de",
        repo: "Xenova/opus-mt-en-de",
        approx_mb: 107,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "es-en",
        label: "Spanish -> English",
        source_lang: "es",
        target_lang: "en",
        repo: "Xenova/opus-mt-es-en",
        approx_mb: 114,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_EN_ZH,
    },
    TranslationPackSpec {
        id: "en-es",
        label: "English -> Spanish",
        source_lang: "en",
        target_lang: "es",
        repo: "Xenova/opus-mt-en-es",
        approx_mb: 114,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_EN_ZH,
    },
    TranslationPackSpec {
        id: "ru-en",
        label: "Russian -> English",
        source_lang: "ru",
        target_lang: "en",
        repo: "Xenova/opus-mt-ru-en",
        approx_mb: 113,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-ru",
        label: "English -> Russian",
        source_lang: "en",
        target_lang: "ru",
        repo: "Xenova/opus-mt-en-ru",
        approx_mb: 113,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "it-en",
        label: "Italian -> English",
        source_lang: "it",
        target_lang: "en",
        repo: "Xenova/opus-mt-it-en",
        approx_mb: 112,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-it",
        label: "English -> Italian",
        source_lang: "en",
        target_lang: "it",
        repo: "Xenova/opus-mt-en-it",
        approx_mb: 112,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    // --- Romance family packs (covers PT / CA / ES / FR / IT / RO) ---
    TranslationPackSpec {
        id: "romance-en",
        label: "Romance -> English (PT/ES/FR/IT/RO/CA)",
        source_lang: "romance",
        target_lang: "en",
        repo: "Xenova/opus-mt-ROMANCE-en",
        approx_mb: 112,
        kind: PackKind::Group,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-romance",
        label: "English -> Romance (PT/ES/FR/IT/RO/CA)",
        source_lang: "en",
        target_lang: "romance",
        repo: "Xenova/opus-mt-en-ROMANCE",
        approx_mb: 112,
        kind: PackKind::Group,
        files: OPUS_FILES_STD,
    },
    // --- Additional dedicated language packs ---
    TranslationPackSpec {
        id: "ar-en",
        label: "Arabic -> English",
        source_lang: "ar",
        target_lang: "en",
        repo: "Xenova/opus-mt-ar-en",
        approx_mb: 110,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-ar",
        label: "English -> Arabic",
        source_lang: "en",
        target_lang: "ar",
        repo: "Xenova/opus-mt-en-ar",
        approx_mb: 110,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "vi-en",
        label: "Vietnamese -> English",
        source_lang: "vi",
        target_lang: "en",
        repo: "Xenova/opus-mt-vi-en",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-vi",
        label: "English -> Vietnamese",
        source_lang: "en",
        target_lang: "vi",
        repo: "Xenova/opus-mt-en-vi",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "id-en",
        label: "Indonesian -> English",
        source_lang: "id",
        target_lang: "en",
        repo: "Xenova/opus-mt-id-en",
        approx_mb: 107,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-id",
        label: "English -> Indonesian",
        source_lang: "en",
        target_lang: "id",
        repo: "Xenova/opus-mt-en-id",
        approx_mb: 107,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "nl-en",
        label: "Dutch -> English",
        source_lang: "nl",
        target_lang: "en",
        repo: "Xenova/opus-mt-nl-en",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-nl",
        label: "English -> Dutch",
        source_lang: "en",
        target_lang: "nl",
        repo: "Xenova/opus-mt-en-nl",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "hi-en",
        label: "Hindi -> English",
        source_lang: "hi",
        target_lang: "en",
        repo: "Xenova/opus-mt-hi-en",
        approx_mb: 110,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-hi",
        label: "English -> Hindi",
        source_lang: "en",
        target_lang: "hi",
        repo: "Xenova/opus-mt-en-hi",
        approx_mb: 110,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "uk-en",
        label: "Ukrainian -> English",
        source_lang: "uk",
        target_lang: "en",
        repo: "Xenova/opus-mt-uk-en",
        approx_mb: 110,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-uk",
        label: "English -> Ukrainian",
        source_lang: "en",
        target_lang: "uk",
        repo: "Xenova/opus-mt-en-uk",
        approx_mb: 110,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "sv-en",
        label: "Swedish -> English",
        source_lang: "sv",
        target_lang: "en",
        repo: "Xenova/opus-mt-sv-en",
        approx_mb: 107,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-sv",
        label: "English -> Swedish",
        source_lang: "en",
        target_lang: "sv",
        repo: "Xenova/opus-mt-en-sv",
        approx_mb: 107,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "fi-en",
        label: "Finnish -> English",
        source_lang: "fi",
        target_lang: "en",
        repo: "Xenova/opus-mt-fi-en",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-fi",
        label: "English -> Finnish",
        source_lang: "en",
        target_lang: "fi",
        repo: "Xenova/opus-mt-en-fi",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "da-en",
        label: "Danish -> English",
        source_lang: "da",
        target_lang: "en",
        repo: "Xenova/opus-mt-da-en",
        approx_mb: 107,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-da",
        label: "English -> Danish",
        source_lang: "en",
        target_lang: "da",
        repo: "Xenova/opus-mt-en-da",
        approx_mb: 107,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "cs-en",
        label: "Czech -> English",
        source_lang: "cs",
        target_lang: "en",
        repo: "Xenova/opus-mt-cs-en",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-cs",
        label: "English -> Czech",
        source_lang: "en",
        target_lang: "cs",
        repo: "Xenova/opus-mt-en-cs",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "ro-en",
        label: "Romanian -> English",
        source_lang: "ro",
        target_lang: "en",
        repo: "Xenova/opus-mt-ro-en",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-ro",
        label: "English -> Romanian",
        source_lang: "en",
        target_lang: "ro",
        repo: "Xenova/opus-mt-en-ro",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "hu-en",
        label: "Hungarian -> English",
        source_lang: "hu",
        target_lang: "en",
        repo: "Xenova/opus-mt-hu-en",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "en-hu",
        label: "English -> Hungarian",
        source_lang: "en",
        target_lang: "hu",
        repo: "Xenova/opus-mt-en-hu",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "pl-en",
        label: "Polish -> English",
        source_lang: "pl",
        target_lang: "en",
        repo: "Xenova/opus-mt-pl-en",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "tr-en",
        label: "Turkish -> English",
        source_lang: "tr",
        target_lang: "en",
        repo: "Xenova/opus-mt-tr-en",
        approx_mb: 109,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
    TranslationPackSpec {
        id: "th-en",
        label: "Thai -> English",
        source_lang: "th",
        target_lang: "en",
        repo: "Xenova/opus-mt-th-en",
        approx_mb: 108,
        kind: PackKind::Dedicated,
        files: OPUS_FILES_STD,
    },
];

/// Look up language metadata by canonical language code.
pub fn find_language(code: &str) -> Option<&'static LanguageSpec> {
    LANGUAGES.iter().find(|l| l.code == code)
}

fn lang_matches_term(lang: &LanguageSpec, term: &str, allow_short: bool) -> bool {
    let q = term.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    if lang.code.to_lowercase().contains(&q)
        || lang.en_name.to_lowercase().contains(&q)
        || lang.aliases.iter().any(|a| a.to_lowercase().contains(&q))
    {
        return true;
    }
    if let Some(native) = i18n::native_lang_name(lang.code) {
        if native.to_lowercase().contains(&q) {
            return true;
        }
    }
    if let Some(zh) = i18n::zh_lang_entry(lang.code) {
        if zh.name.to_lowercase().contains(&q)
            || zh.aliases.iter().any(|a| a.to_lowercase().contains(&q))
            || (allow_short && zh.short.iter().any(|s| s.to_lowercase() == q))
        {
            return true;
        }
    }
    false
}

fn lang_exact_match(lang: &LanguageSpec, term: &str, allow_short: bool) -> bool {
    let q = term.trim().to_lowercase();
    if q.is_empty() {
        return false;
    }
    if lang.code.to_lowercase() == q
        || lang.en_name.to_lowercase() == q
        || lang.aliases.iter().any(|a| a.to_lowercase() == q)
    {
        return true;
    }
    if let Some(native) = i18n::native_lang_name(lang.code) {
        if native.to_lowercase() == q {
            return true;
        }
    }
    if let Some(zh) = i18n::zh_lang_entry(lang.code) {
        if zh.name.to_lowercase() == q
            || zh.aliases.iter().any(|a| a.to_lowercase() == q)
            || (allow_short && zh.short.iter().any(|s| s.to_lowercase() == q))
        {
            return true;
        }
    }
    false
}

/// Match a language against a search query (supports English, localized, native names, codes, and aliases).
pub fn matches_language_query(lang: &LanguageSpec, query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    trimmed
        .split_whitespace()
        .all(|term| lang_matches_term(lang, term, false))
}

/// Match the "Auto Detect" entry against a search query.
pub fn matches_auto_query(query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    let zh_auto = &i18n::zh_search_config().auto_aliases;
    trimmed.split_whitespace().all(|term| {
        let q = term.to_lowercase();
        "auto".contains(&q)
            || "auto detect".contains(&q)
            || "detect".contains(&q)
            || zh_auto.iter().any(|a| a.to_lowercase().contains(&q))
    })
}

/// Filter the underlying OPUS-MT pack catalog by keyword, including multilingual fallback coverage.
pub fn matches_pack_query(spec: &TranslationPackSpec, query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    let search_cfg = i18n::zh_search_config();
    let localized_label = spec.display_label().to_lowercase();
    let direct_hit = trimmed.split_whitespace().all(|term| {
        let q = term.to_lowercase();
        spec.id.to_lowercase().contains(&q)
            || spec.label.to_lowercase().contains(&q)
            || localized_label.contains(&q)
            || spec.repo.to_lowercase().contains(&q)
            || spec.source_lang.to_lowercase().contains(&q)
            || spec.target_lang.to_lowercase().contains(&q)
            || find_language(spec.source_lang)
                .is_some_and(|l| matches_language_query(l, term))
            || find_language(spec.target_lang)
                .is_some_and(|l| matches_language_query(l, term))
            || (spec.kind == PackKind::Multilingual
                && ("multilingual universal all any".contains(&q)
                    || search_cfg
                        .multilingual_aliases
                        .iter()
                        .any(|a| a.to_lowercase().contains(&q))))
            || (spec.kind == PackKind::Group
                && ("romance".contains(&q)
                    || search_cfg
                        .romance_aliases
                        .iter()
                        .any(|a| a.to_lowercase().contains(&q))))
    });
    if direct_hit {
        return true;
    }
    // If searching for a specific language covered via Romance or universal packs, include those packs.
    let matched_langs: Vec<&LanguageSpec> = LANGUAGES
        .iter()
        .filter(|l| matches_language_query(l, trimmed))
        .collect();
    if matched_langs.is_empty() {
        return false;
    }
    match spec.id {
        "romance-en" | "en-romance" => matched_langs
            .iter()
            .any(|l| !l.romance_prefixes.is_empty()),
        "mul-en" => matched_langs.iter().any(|l| {
            l.code != "en"
                && !PACK_CATALOG
                    .iter()
                    .any(|p| p.kind == PackKind::Dedicated && p.source_lang == l.code && p.target_lang == "en")
        }),
        "en-mul" => matched_langs.iter().any(|l| {
            l.code != "en"
                && !PACK_CATALOG
                    .iter()
                    .any(|p| p.kind == PackKind::Dedicated && p.source_lang == "en" && p.target_lang == l.code)
        }),
        _ => false,
    }
}

/// A directed translation pair surfaced by language-pair search (may be direct or English-bridged).
#[derive(Clone, Debug)]
pub struct TranslationPairEntry {
    pub source_code: &'static str,
    pub target_code: &'static str,
    pub plan: RoutePlan,
}

impl TranslationPairEntry {
    pub fn ready(&self) -> bool {
        self.plan.ready()
    }

    pub fn is_bridged(&self) -> bool {
        self.plan.steps.len() > 1
    }

    pub fn missing_mb(&self) -> u32 {
        self.plan.missing.iter().map(|p| p.approx_mb).sum()
    }

    pub fn total_mb(&self) -> u32 {
        self.plan.steps.iter().map(|s| s.pack.approx_mb).sum()
    }

    pub fn installed_bytes(&self, storage_dir: &Path) -> u64 {
        self.plan
            .steps
            .iter()
            .filter(|s| is_pack_complete(&pack_dir_in(storage_dir, s.pack.id), s.pack))
            .map(|s| inspect_pack(pack_dir_in(storage_dir, s.pack.id), s.pack).disk_bytes)
            .sum()
    }
}

/// Result of searching for translation pairs by language name, code, or directional expression.
#[derive(Clone, Debug)]
pub struct PairSearchResult {
    /// Set when the query resolves to a single focus language (e.g. "Japanese"),
    /// enabling `From <Lang>` / `To <Lang>` direction filters in the UI.
    pub focus_lang: Option<&'static str>,
    pub pairs: Vec<TranslationPairEntry>,
}

fn resolve_query_languages(term: &str, allow_short: bool) -> Vec<&'static LanguageSpec> {
    let trimmed = term.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let exact: Vec<&'static LanguageSpec> = LANGUAGES
        .iter()
        .filter(|l| lang_exact_match(l, trimmed, allow_short))
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    LANGUAGES
        .iter()
        .filter(|l| {
            trimmed
                .split_whitespace()
                .all(|part| lang_matches_term(l, part, allow_short))
        })
        .collect()
}

fn split_directional_query(query: &str) -> Option<(String, String)> {
    let trimmed = query.trim();
    // ASCII and Unicode directional arrows / English " to " / " into ".
    let lower = trimmed.to_lowercase();
    for sep in ["->", "=>", "\u{2192}", " into ", " to "] {
        if let Some(pos) = lower.find(sep) {
            let left = trimmed[..pos].trim();
            let right = trimmed[pos + sep.len()..].trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left.to_string(), right.to_string()));
            }
        }
    }
    // Localized directional connectors from `zh-CN.json` (sorted longest-first in catalog).
    for sep in &i18n::zh_search_config().direction_separators {
        if sep.is_empty() {
            continue;
        }
        if let Some(pos) = trimmed.find(sep.as_str()) {
            let left = trimmed[..pos].trim();
            let right = trimmed[pos + sep.len()..].trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left.to_string(), right.to_string()));
            }
        }
    }
    None
}

fn push_pair(
    out: &mut Vec<TranslationPairEntry>,
    seen: &mut HashSet<(&'static str, &'static str)>,
    src: &'static str,
    tgt: &'static str,
    storage_dir: &Path,
) {
    if src == tgt || !seen.insert((src, tgt)) {
        return;
    }
    if let Ok(plan) = plan_translation_route(src, tgt, storage_dir) {
        out.push(TranslationPairEntry {
            source_code: src,
            target_code: tgt,
            plan,
        });
    }
}

/// Search directed translation pairs from a user query.
///
/// Supports:
/// - Directional queries (e.g. `ja->zh`, `Japanese to Chinese`, or localized `X->Y` connectors)
/// - Two-language queries (e.g. `Japanese Chinese`, `ja zh`) -> both directions
/// - Single-language queries (e.g. `Japanese`, `ja`) -> all `L -> Other` and `Other -> L` pairs
///
/// Returns `None` when the query is empty or matches pack-level keywords rather than languages.
pub fn search_translation_pairs(query: &str, storage_dir: &Path) -> Option<PairSearchResult> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Do not hijack explicit pack ID / repo / universal-pack searches (e.g. "mul-en", "Xenova", "romance").
    let lower = trimmed.to_lowercase();
    if PACK_CATALOG
        .iter()
        .any(|p| p.id == lower || p.repo.to_lowercase().contains(&lower))
        || lower == "mul"
        || lower == "romance"
        || lower.contains("opus")
        || lower.contains("xenova")
    {
        return None;
    }
    let search_cfg = i18n::zh_search_config();
    if search_cfg
        .multilingual_aliases
        .iter()
        .any(|a| a.to_lowercase() == lower)
        || "multilingual".contains(&lower) && lower.len() >= 3
    {
        return None;
    }

    let mut pairs = Vec::new();
    let mut seen: HashSet<(&'static str, &'static str)> = HashSet::new();

    // 1. Explicit directional query (e.g. "ja->zh", "Japanese to Chinese", or localized separator).
    if let Some((left, right)) = split_directional_query(trimmed) {
        let srcs = resolve_query_languages(&left, true);
        let tgts = resolve_query_languages(&right, true);
        if !srcs.is_empty() && !tgts.is_empty() {
            for s in &srcs {
                for t in &tgts {
                    push_pair(&mut pairs, &mut seen, s.code, t.code, storage_dir);
                }
            }
            if !pairs.is_empty() {
                return Some(PairSearchResult {
                    focus_lang: None,
                    pairs,
                });
            }
        }
    }

    // 2. Two-language whitespace-separated query (e.g. "ja zh", "Japanese Chinese") when not a single language name.
    let single_matches = resolve_query_languages(trimmed, false);
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if single_matches.is_empty() && tokens.len() >= 2 {
        for split_idx in 1..tokens.len() {
            let left = tokens[..split_idx].join(" ");
            let right = tokens[split_idx..].join(" ");
            let a = resolve_query_languages(&left, true);
            let b = resolve_query_languages(&right, true);
            if !a.is_empty() && !b.is_empty() {
                for la in &a {
                    for lb in &b {
                        push_pair(&mut pairs, &mut seen, la.code, lb.code, storage_dir);
                        push_pair(&mut pairs, &mut seen, lb.code, la.code, storage_dir);
                    }
                }
                if !pairs.is_empty() {
                    return Some(PairSearchResult {
                        focus_lang: None,
                        pairs,
                    });
                }
            }
        }
    }

    // 3. Single-language query (e.g. "Japanese", "ja"): expand all `L -> Other` and `Other -> L` pairs.
    let matched = if !single_matches.is_empty() {
        single_matches
    } else {
        resolve_query_languages(trimmed, true)
    };
    if matched.is_empty() {
        return None;
    }

    let focus_lang = if matched.len() == 1 {
        Some(matched[0].code)
    } else {
        None
    };

    // Cap anchor languages to avoid an explosion on single-letter ASCII queries like "a".
    for anchor in matched.iter().take(3) {
        for other in LANGUAGES {
            if other.code == anchor.code {
                continue;
            }
            push_pair(&mut pairs, &mut seen, anchor.code, other.code, storage_dir);
            push_pair(&mut pairs, &mut seen, other.code, anchor.code, storage_dir);
        }
    }

    if pairs.is_empty() {
        None
    } else {
        Some(PairSearchResult { focus_lang, pairs })
    }
}

/// Local installation status for a single translation pack.
#[derive(Clone, Debug)]
pub struct PackInstallState {
    pub spec: &'static TranslationPackSpec,
    pub installed: bool,
    pub dir: PathBuf,
    pub disk_bytes: u64,
    pub missing: Vec<&'static str>,
    pub invalid: Vec<&'static str>,
    pub has_local_files: bool,
}

/// Overall local translation engine status for the Settings UI.
#[derive(Clone, Debug)]
pub struct LocalTranslateStatus {
    pub storage_dir: PathBuf,
    pub runtime_dll: Option<PathBuf>,
    pub packs: Vec<PackInstallState>,
    pub installed_count: usize,
}

/// Download progress snapshot for a single language pack.
#[derive(Clone, Debug)]
pub struct PackDownloadProgress {
    #[allow(dead_code)]
    pub pack_id: String,
    pub file_index: usize,
    pub file_count: usize,
    pub current_file: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

impl PackDownloadProgress {
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.downloaded_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// Look up a translation pack specification by ID.
pub fn find_pack_spec(pack_id: &str) -> Option<&'static TranslationPackSpec> {
    PACK_CATALOG.iter().find(|p| p.id == pack_id)
}

/// Default language pack directory next to the executable: `<exe_dir>/models/translate`.
pub fn default_translate_dir() -> PathBuf {
    crate::ocr::default_exe_model_dir().join("translate")
}

/// Resolve the configured language pack root directory (defaults to `<exe_dir>/models/translate`).
pub fn resolve_translate_dir(settings: &Value) -> PathBuf {
    let custom = settings["providers"]["translation"]["local"]["modelDir"]
        .as_str()
        .unwrap_or("")
        .trim();
    if custom.is_empty() {
        default_translate_dir()
    } else {
        PathBuf::from(custom)
    }
}

/// Return the directory path for a specific language pack.
pub fn pack_dir_in(base_dir: &Path, pack_id: &str) -> PathBuf {
    base_dir.join(pack_id)
}

fn validate_pack_file(path: &Path, name: &str) -> bool {
    match name {
        "config.json" => std::fs::read(path).ok().is_some_and(|bytes| {
            let Ok(config) = serde_json::from_slice::<MarianConfigJson>(&bytes) else {
                return false;
            };
            config.model_type == "marian"
                && config.decoder_layers > 0
                && config.decoder_attention_heads > 0
                && config.d_model > 0
                && config.d_model % config.decoder_attention_heads == 0
                && config.max_position_embeddings > 0
        }),
        "tokenizer.json" => std::fs::read(path).ok().is_some_and(|bytes| {
            serde_json::from_slice::<HfTokenizerJson>(&bytes)
                .is_ok_and(|tokenizer| !tokenizer.model.vocab.is_empty())
        }),
        _ => model_files::has_onnx_envelope(path),
    }
}

fn check_pack_file(path: &Path, name: &'static str) -> Option<FileStamp> {
    model_files::checked_file(path, name, || validate_pack_file(path, name))
}

/// Check the required translation config, tokenizer, and ONNX files.
pub fn is_pack_complete(dir: &Path, spec: &TranslationPackSpec) -> bool {
    spec.files
        .iter()
        .all(|f| check_pack_file(&dir.join(f.local_name), f.local_name).is_some())
}

fn inspect_pack(dir: PathBuf, spec: &'static TranslationPackSpec) -> PackInstallState {
    let mut missing = Vec::new();
    let mut invalid = Vec::new();
    let mut has_local_files = false;
    let mut disk_bytes = 0;
    for f in spec.files {
        let path = dir.join(f.local_name);
        has_local_files |= model_files::has_local_file(&path);
        if let Ok(metadata) = path.metadata() {
            if metadata.is_file() {
                disk_bytes += metadata.len();
            }
            if check_pack_file(&path, f.local_name).is_none() {
                invalid.push(f.local_name);
            }
        } else {
            missing.push(f.local_name);
        }
    }
    PackInstallState {
        spec,
        installed: missing.is_empty() && invalid.is_empty(),
        dir,
        disk_bytes,
        missing,
        invalid,
        has_local_files,
    }
}

/// Inspect local translation runtime and installation status for all packs.
pub fn inspect_status(data_dir: &Path, settings: &Value) -> LocalTranslateStatus {
    let storage_dir = resolve_translate_dir(settings);
    let custom_ocr_dir = settings["providers"]["ocr"]["local"]["modelDir"]
        .as_str()
        .unwrap_or("");
    let runtime_dll = crate::ocr::resolve_runtime_dll(custom_ocr_dir, Some(&storage_dir), data_dir);

    let mut packs = Vec::with_capacity(PACK_CATALOG.len());
    let mut installed_count = 0usize;
    for spec in PACK_CATALOG {
        let pack = inspect_pack(pack_dir_in(&storage_dir, spec.id), spec);
        installed_count += usize::from(pack.installed);
        packs.push(pack);
    }

    LocalTranslateStatus {
        storage_dir,
        runtime_dll,
        packs,
        installed_count,
    }
}

/// Base URL for HuggingFace or its mirror according to configuration.
pub fn mirror_base_url(mirror: &str) -> &'static str {
    match mirror.trim() {
        "huggingface" => "https://huggingface.co",
        _ => "https://hf-mirror.com",
    }
}

/// Build the download URL for a single model file.
pub fn build_download_url(mirror: &str, repo: &str, remote_path: &str) -> String {
    format!(
        "{}/{}/resolve/main/{}",
        mirror_base_url(mirror),
        repo,
        remote_path
    )
}

/// Stream-download a language pack into `base_dir` with atomic rename, progress callbacks, and cancellation.
pub async fn download_pack<F>(
    pack_id: &str,
    base_dir: &Path,
    mirror: &str,
    cancel: Arc<AtomicBool>,
    mut on_progress: F,
) -> Result<PathBuf, String>
where
    F: FnMut(PackDownloadProgress),
{
    let spec = find_pack_spec(pack_id)
        .ok_or_else(|| i18n::tf("Language pack not found: {id}", &[("id", pack_id)]))?;

    let target_dir = pack_dir_in(base_dir, spec.id);
    std::fs::create_dir_all(&target_dir).map_err(|e| {
        i18n::tf(
            "Unable to create language pack directory ({path}): {err}",
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

    let total_estimated: u64 = spec.files.iter().map(|f| f.approx_bytes).sum();
    let mut completed_bytes: u64 = 0;
    let file_count = spec.files.len();

    for (idx, file) in spec.files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(i18n::t("Download cancelled").into());
        }

        let dest = target_dir.join(file.local_name);
        if let Some(stamp) = check_pack_file(&dest, file.local_name) {
            completed_bytes = completed_bytes.saturating_add(stamp.len);
            on_progress(PackDownloadProgress {
                pack_id: spec.id.to_string(),
                file_index: idx + 1,
                file_count,
                current_file: file.local_name.to_string(),
                downloaded_bytes: completed_bytes,
                total_bytes: total_estimated.max(completed_bytes),
            });
            continue;
        }

        let url = build_download_url(mirror, spec.repo, file.remote_path);
        let mut resp = client.get(&url).send().await.map_err(|e| {
            i18n::tf(
                "Failed to connect to download source ({file}): {err}",
                &[("file", file.local_name), ("err", &e.to_string())],
            )
        })?;

        if !resp.status().is_success() {
            return Err(i18n::tf(
                "Failed to download {file} (HTTP {status}); try switching the download mirror in Settings",
                &[
                    ("file", file.local_name),
                    ("status", &resp.status().as_u16().to_string()),
                ],
            ));
        }

        let content_len = resp.content_length();
        let remote_len = content_len.unwrap_or(file.approx_bytes);
        let remaining_est: u64 = spec.files[idx + 1..].iter().map(|f| f.approx_bytes).sum();
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

        let mut file_downloaded: u64 = 0;
        let mut last_emit = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(200))
            .unwrap_or_else(std::time::Instant::now);

        while let Some(chunk) = resp.chunk().await.map_err(|e| {
            i18n::tf(
                "Download interrupted for {file}: {err}",
                &[("file", file.local_name), ("err", &e.to_string())],
            )
        })? {
            if cancel.load(Ordering::Relaxed) {
                return Err(i18n::t("Download cancelled").into());
            }
            out.write_all(&chunk).map_err(|e| {
                i18n::tf(
                    "Failed to write file {file}: {err}",
                    &[("file", file.local_name), ("err", &e.to_string())],
                )
            })?;
            file_downloaded = file_downloaded.saturating_add(chunk.len() as u64);

            if last_emit.elapsed() >= std::time::Duration::from_millis(120) {
                last_emit = std::time::Instant::now();
                let current_total = completed_bytes.saturating_add(file_downloaded);
                on_progress(PackDownloadProgress {
                    pack_id: spec.id.to_string(),
                    file_index: idx + 1,
                    file_count,
                    current_file: file.local_name.to_string(),
                    downloaded_bytes: current_total,
                    total_bytes: effective_total.max(current_total),
                });
            }
        }

        out.flush().map_err(|e| {
            i18n::tf(
                "Failed to flush {file}: {err}",
                &[("file", file.local_name), ("err", &e.to_string())],
            )
        })?;
        drop(out);

        if cancel.load(Ordering::Relaxed) {
            return Err(i18n::t("Download cancelled").into());
        }
        if file_downloaded == 0
            || content_len.is_some_and(|expected| expected != file_downloaded)
            || !validate_pack_file(pending.path(), file.local_name)
        {
            return Err(i18n::tf(
                "Model file failed validation: {file}",
                &[("file", file.local_name)],
            ));
        }
        pending.install(&dest).map_err(|e| {
            i18n::tf(
                "Failed to save file {file}: {err}",
                &[("file", file.local_name), ("err", &e.to_string())],
            )
        })?;

        completed_bytes = completed_bytes.saturating_add(file_downloaded);
        on_progress(PackDownloadProgress {
            pack_id: spec.id.to_string(),
            file_index: idx + 1,
            file_count,
            current_file: file.local_name.to_string(),
            downloaded_bytes: completed_bytes,
            total_bytes: effective_total.max(completed_bytes),
        });
    }

    Ok(target_dir)
}

/// Delete the selected language pack files and evict its ONNX session.
pub fn remove_pack(pack_id: &str, base_dir: &Path) -> Result<(), String> {
    let spec = find_pack_spec(pack_id)
        .ok_or_else(|| i18n::tf("Language pack not found: {id}", &[("id", pack_id)]))?;
    let mut guard = ENGINE_CACHE.lock().unwrap_or_else(|err| err.into_inner());
    if guard
        .as_ref()
        .is_some_and(|cached| cached.key.pack_id == spec.id)
    {
        *guard = None;
    }
    let dir = pack_dir_in(base_dir, spec.id);
    let remove = || -> std::io::Result<()> {
        for file in spec.files {
            model_files::remove_model_file(&dir.join(file.local_name))?;
        }
        model_files::remove_empty_dir(&dir)
    };
    remove().map_err(|e| {
        i18n::tf(
            "Failed to delete model files ({path}): {err}",
            &[
                ("path", &dir.display().to_string()),
                ("err", &e.to_string()),
            ],
        )
    })
}

// ============================================================================
// Language detection and route planning
// ============================================================================

/// Normalize a user-supplied language name or code into a canonical code.
pub fn normalize_lang_code(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "auto".into();
    }
    let lower = trimmed.to_lowercase();
    if lower == "auto"
        || lower == "auto detect"
        || i18n::zh_search_config()
            .auto_aliases
            .iter()
            .any(|a| a.to_lowercase() == lower)
    {
        return "auto".into();
    }
    for lang in LANGUAGES {
        if lang_exact_match(lang, trimmed, false) {
            return lang.code.to_string();
        }
    }
    lower
}

pub fn lang_display_name(code: &str) -> &'static str {
    i18n::lang_name(code)
}

/// Automatically detect the dominant source language from script and diacritic counts.
pub fn detect_source_language(text: &str, target_code: &str) -> &'static str {
    let mut cjk = 0usize;
    let mut kana = 0usize;
    let mut hangul = 0usize;
    let mut cyrillic = 0usize;
    let mut uk_marks = 0usize;
    let mut arabic = 0usize;
    let mut persian_marks = 0usize;
    let mut urdu_marks = 0usize;
    let mut thai = 0usize;
    let mut greek = 0usize;
    let mut hebrew = 0usize;
    let mut devanagari = 0usize;
    let mut bengali = 0usize;
    let mut latin = 0usize;
    let mut vi_marks = 0usize;
    let mut tr_marks = 0usize;
    let mut pl_marks = 0usize;
    let mut cs_marks = 0usize;
    let mut ro_marks = 0usize;
    let mut hu_marks = 0usize;
    let mut pt_marks = 0usize;
    let mut german_marks = 0usize;
    let mut french_marks = 0usize;
    let mut spanish_marks = 0usize;

    for ch in text.chars() {
        let cp = ch as u32;
        if (0x3040..=0x30FF).contains(&cp) || (0x31F0..=0x31FF).contains(&cp) {
            kana += 1;
        } else if (0xAC00..=0xD7AF).contains(&cp) || (0x1100..=0x11FF).contains(&cp) {
            hangul += 1;
        } else if (0x4E00..=0x9FFF).contains(&cp) || (0x3400..=0x4DBF).contains(&cp) {
            cjk += 1;
        } else if (0x0400..=0x04FF).contains(&cp) {
            cyrillic += 1;
            if matches!(ch, 'є' | 'Є' | 'і' | 'І' | 'ї' | 'Ї' | 'ґ' | 'Ґ') {
                uk_marks += 1;
            }
        } else if (0x0600..=0x06FF).contains(&cp) {
            arabic += 1;
            if matches!(ch, 'پ' | 'چ' | 'ژ' | 'گ') {
                persian_marks += 1;
            }
            if matches!(ch, 'ٹ' | 'ڈ' | 'ڑ' | 'ے' | 'ں') {
                urdu_marks += 1;
            }
        } else if (0x0E00..=0x0E7F).contains(&cp) {
            thai += 1;
        } else if (0x0370..=0x03FF).contains(&cp) {
            greek += 1;
        } else if (0x0590..=0x05FF).contains(&cp) {
            hebrew += 1;
        } else if (0x0900..=0x097F).contains(&cp) {
            devanagari += 1;
        } else if (0x0980..=0x09FF).contains(&cp) {
            bengali += 1;
        } else if (0x1EA0..=0x1EFF).contains(&cp) || matches!(ch, 'ă' | 'Ă' | 'đ' | 'Đ' | 'ơ' | 'Ơ' | 'ư' | 'Ư') {
            latin += 1;
            vi_marks += 1;
        } else if ch.is_ascii_alphabetic() {
            latin += 1;
        } else {
            match ch {
                'ğ' | 'Ğ' | 'ı' | 'İ' | 'ş' | 'Ş' => {
                    latin += 1;
                    tr_marks += 1;
                }
                'ą' | 'Ą' | 'ć' | 'Ć' | 'ę' | 'Ę' | 'ł' | 'Ł' | 'ń' | 'Ń' | 'ś' | 'Ś' | 'ź' | 'Ź' | 'ż' | 'Ż' => {
                    latin += 1;
                    pl_marks += 1;
                }
                'ě' | 'Ě' | 'ř' | 'Ř' | 'ů' | 'Ů' | 'č' | 'Č' | 'š' | 'Š' | 'ž' | 'Ž' => {
                    latin += 1;
                    cs_marks += 1;
                }
                'ș' | 'Ș' | 'ț' | 'Ț' => {
                    latin += 1;
                    ro_marks += 1;
                }
                'ő' | 'Ő' | 'ű' | 'Ű' => {
                    latin += 1;
                    hu_marks += 1;
                }
                'ã' | 'Ã' | 'õ' | 'Õ' => {
                    latin += 1;
                    pt_marks += 1;
                }
                'ä' | 'ö' | 'ü' | 'Ä' | 'Ö' | 'Ü' | 'ß' => {
                    latin += 1;
                    german_marks += 1;
                }
                'é' | 'è' | 'ê' | 'ë' | 'à' | 'â' | 'ç' | 'ù' | 'û' | 'ô' | 'î' | 'ï' | 'œ' => {
                    latin += 1;
                    french_marks += 1;
                }
                'ñ' | 'Ñ' | '¡' | '¿' | 'á' | 'í' | 'ó' | 'ú' => {
                    latin += 1;
                    spanish_marks += 1;
                }
                _ => {}
            }
        }
    }

    if kana > 0 {
        return "ja";
    }
    if hangul > 0 {
        return "ko";
    }
    if thai > 0 {
        return "th";
    }
    if greek > 0 && greek >= latin {
        return "el";
    }
    if hebrew > 0 {
        return "he";
    }
    if devanagari > 0 {
        return "hi";
    }
    if bengali > 0 {
        return "bn";
    }
    if arabic > 0 {
        if urdu_marks > 0 && urdu_marks >= persian_marks {
            return "ur";
        }
        if persian_marks > 0 {
            return "fa";
        }
        return "ar";
    }
    if cyrillic > latin && cyrillic > 0 {
        return if uk_marks > 0 { "uk" } else { "ru" };
    }
    if cjk > 0 && cjk * 3 >= latin {
        return "zh";
    }
    if vi_marks > 0 {
        return "vi";
    }
    if tr_marks > 0 {
        return "tr";
    }
    if pl_marks > 0 {
        return "pl";
    }
    if cs_marks > 0 {
        return "cs";
    }
    if ro_marks > 0 {
        return "ro";
    }
    if hu_marks > 0 {
        return "hu";
    }
    if pt_marks > 0 {
        return "pt";
    }
    if german_marks > 0 && german_marks >= french_marks && german_marks >= spanish_marks {
        return "de";
    }
    if spanish_marks > 0 && spanish_marks >= french_marks {
        return "es";
    }
    if french_marks > 0 {
        return "fr";
    }
    if latin > 0 {
        return "en";
    }
    if target_code == "en" {
        "zh"
    } else {
        "en"
    }
}

/// Single execution step in a translation route (including target code and multilingual prefix candidates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteStep {
    pub pack: &'static TranslationPackSpec,
    pub target_lang: &'static str,
    pub prefix_candidates: &'static [&'static str],
}

/// Translation route plan containing recommended steps and any missing packs.
#[derive(Clone, Debug)]
pub struct RoutePlan {
    pub steps: Vec<RouteStep>,
    pub missing: Vec<&'static TranslationPackSpec>,
}

impl RoutePlan {
    pub fn ready(&self) -> bool {
        self.missing.is_empty()
    }
}

fn pick_hop_to_en(src: &str, storage_dir: &Path) -> Option<(RouteStep, bool)> {
    let lang = find_language(src);
    let mut candidates: Vec<&'static TranslationPackSpec> = Vec::new();
    // 1. Dedicated source -> English pack (Traditional Chinese also compatible with zh-en).
    let dedicated_src = if src == "zh-tw" { "zh" } else { src };
    if let Some(p) = PACK_CATALOG
        .iter()
        .find(|p| p.kind == PackKind::Dedicated && p.source_lang == dedicated_src && p.target_lang == "en")
    {
        candidates.push(p);
    }
    // 2. Romance family -> English.
    if lang.is_some_and(|l| !l.romance_prefixes.is_empty()) {
        if let Some(p) = find_pack_spec("romance-en") {
            candidates.push(p);
        }
    }
    // 3. Universal multilingual -> English.
    if let Some(p) = find_pack_spec("mul-en") {
        candidates.push(p);
    }
    if candidates.is_empty() {
        return None;
    }
    // Prefer the highest-priority installed pack; otherwise recommend the best pack to download.
    if let Some(&installed_pack) = candidates
        .iter()
        .find(|p| is_pack_complete(&pack_dir_in(storage_dir, p.id), p))
    {
        return Some((
            RouteStep {
                pack: installed_pack,
                target_lang: "en",
                prefix_candidates: &[],
            },
            true,
        ));
    }
    Some((
        RouteStep {
            pack: candidates[0],
            target_lang: "en",
            prefix_candidates: &[],
        },
        false,
    ))
}

fn pick_hop_from_en(tgt: &str, storage_dir: &Path) -> Option<(RouteStep, bool)> {
    let lang = find_language(tgt)?;
    let mut candidates: Vec<RouteStep> = Vec::new();
    // 1. Dedicated English -> target pack.
    if let Some(p) = PACK_CATALOG
        .iter()
        .find(|p| p.kind == PackKind::Dedicated && p.source_lang == "en" && p.target_lang == tgt)
    {
        candidates.push(RouteStep {
            pack: p,
            target_lang: lang.code,
            prefix_candidates: &[],
        });
    }
    // 2. Romance multilingual pack (Portuguese, Catalan, etc.).
    if !lang.romance_prefixes.is_empty() {
        if let Some(p) = find_pack_spec("en-romance") {
            candidates.push(RouteStep {
                pack: p,
                target_lang: lang.code,
                prefix_candidates: lang.romance_prefixes,
            });
        }
    }
    // 3. Universal English -> multilingual pack (covers all 40+ languages).
    if !lang.mul_prefixes.is_empty() {
        if let Some(p) = find_pack_spec("en-mul") {
            candidates.push(RouteStep {
                pack: p,
                target_lang: lang.code,
                prefix_candidates: lang.mul_prefixes,
            });
        }
    }
    // Allow Traditional Chinese to fall back to installed en-zh when en-mul is absent.
    if tgt == "zh-tw" {
        if let Some(p) = find_pack_spec("en-zh") {
            if is_pack_complete(&pack_dir_in(storage_dir, p.id), p) {
                candidates.push(RouteStep {
                    pack: p,
                    target_lang: "zh-tw",
                    prefix_candidates: &[],
                });
            }
        }
    }
    if candidates.is_empty() {
        return None;
    }
    if let Some(&installed_step) = candidates
        .iter()
        .find(|s| is_pack_complete(&pack_dir_in(storage_dir, s.pack.id), s.pack))
    {
        return Some((installed_step, true));
    }
    Some((candidates[0], false))
}

/// Plan a local translation route between any two languages (prefers installed/dedicated packs, bridges via English).
pub fn plan_translation_route(
    source_code: &str,
    target_code: &str,
    storage_dir: &Path,
) -> Result<RoutePlan, String> {
    if source_code == target_code {
        return Ok(RoutePlan {
            steps: Vec::new(),
            missing: Vec::new(),
        });
    }

    // 1. Single-hop direct pack (en <-> X or future direct non-English packs).
    if source_code == "en" {
        let (step, installed) = pick_hop_from_en(target_code, storage_dir).ok_or_else(|| {
            i18n::tf(
                "Local language packs do not currently support {src} -> {tgt}; switch to AI or API mode",
                &[
                    ("src", lang_display_name(source_code)),
                    ("tgt", lang_display_name(target_code)),
                ],
            )
        })?;
        return Ok(RoutePlan {
            steps: vec![step],
            missing: if installed { vec![] } else { vec![step.pack] },
        });
    }
    if target_code == "en" {
        let (step, installed) = pick_hop_to_en(source_code, storage_dir).ok_or_else(|| {
            i18n::tf(
                "Local language packs do not currently support {src} -> {tgt}; switch to AI or API mode",
                &[
                    ("src", lang_display_name(source_code)),
                    ("tgt", lang_display_name(target_code)),
                ],
            )
        })?;
        return Ok(RoutePlan {
            steps: vec![step],
            missing: if installed { vec![] } else { vec![step.pack] },
        });
    }

    // Use a dedicated non-English direct pack when present in the catalog.
    if let Some(direct) = PACK_CATALOG.iter().find(|p| {
        p.kind == PackKind::Dedicated && p.source_lang == source_code && p.target_lang == target_code
    }) {
        let tgt_static = find_language(target_code).map(|l| l.code).unwrap_or("zh");
        let step = RouteStep {
            pack: direct,
            target_lang: tgt_static,
            prefix_candidates: &[],
        };
        let installed = is_pack_complete(&pack_dir_in(storage_dir, direct.id), direct);
        return Ok(RoutePlan {
            steps: vec![step],
            missing: if installed { vec![] } else { vec![direct] },
        });
    }

    // 2. Any two non-English languages: two-hop bridge via English (source -> en -> target).
    let (step1, ok1) = pick_hop_to_en(source_code, storage_dir).ok_or_else(|| {
        i18n::tf(
            "Local language packs do not currently support source language {src}; switch to AI or API mode",
            &[("src", lang_display_name(source_code))],
        )
    })?;
    let (step2, ok2) = pick_hop_from_en(target_code, storage_dir).ok_or_else(|| {
        i18n::tf(
            "Local language packs do not currently support target language {tgt}; switch to AI or API mode",
            &[("tgt", lang_display_name(target_code))],
        )
    })?;
    let mut missing = Vec::new();
    if !ok1 {
        missing.push(step1.pack);
    }
    if !ok2 && !missing.iter().any(|p| p.id == step2.pack.id) {
        missing.push(step2.pack);
    }
    Ok(RoutePlan {
        steps: vec![step1, step2],
        missing,
    })
}

/// Resolve an executable local translation route, or return actionable download guidance if packs are missing.
pub fn resolve_translation_route(
    source_code: &str,
    target_code: &str,
    storage_dir: &Path,
) -> Result<Vec<RouteStep>, String> {
    let plan = plan_translation_route(source_code, target_code, storage_dir)?;
    if plan.missing.is_empty() {
        return Ok(plan.steps);
    }
    let names: Vec<String> = plan
        .missing
        .iter()
        .map(|p| format!("[{}]", p.display_label()))
        .collect();
    let joined = names.join(", ");
    if plan.steps.len() > 1 {
        Err(i18n::tf(
            "Offline translation ({src} -> {tgt}) requires these packs to bridge via English: {packs}. Download or repair them in Settings -> Text & Translation -> Translation",
            &[
                ("src", lang_display_name(source_code)),
                ("tgt", lang_display_name(target_code)),
                ("packs", &joined),
            ],
        ))
    } else {
        Err(i18n::tf(
            "Language pack unavailable: {packs}. Download or repair it in Settings -> Text & Translation -> Translation",
            &[("packs", &joined)],
        ))
    }
}

// ============================================================================
// Pure-Rust Marian Unigram tokenizer (parses HuggingFace tokenizer.json)
// ============================================================================

#[derive(Debug, Deserialize)]
struct HfTokenizerJson {
    model: HfUnigramModel,
    #[serde(default)]
    added_tokens: Vec<HfAddedToken>,
}

#[derive(Debug, Deserialize)]
struct HfUnigramModel {
    #[serde(default)]
    unk_id: Option<usize>,
    vocab: Vec<(String, f64)>,
}

#[derive(Debug, Deserialize)]
struct HfAddedToken {
    id: usize,
    content: String,
    #[serde(default)]
    special: bool,
}

#[derive(Debug, Deserialize)]
struct MarianConfigJson {
    model_type: String,
    #[serde(default = "default_decoder_layers")]
    decoder_layers: usize,
    #[serde(default = "default_decoder_heads")]
    decoder_attention_heads: usize,
    #[serde(default = "default_d_model")]
    d_model: usize,
    #[serde(default)]
    eos_token_id: usize,
    #[serde(default = "default_pad_token_id")]
    pad_token_id: usize,
    #[serde(default)]
    decoder_start_token_id: Option<usize>,
    #[serde(default = "default_max_pos")]
    max_position_embeddings: usize,
    #[serde(default)]
    bad_words_ids: Vec<Vec<usize>>,
}

fn default_decoder_layers() -> usize {
    6
}
fn default_decoder_heads() -> usize {
    8
}
fn default_d_model() -> usize {
    512
}
fn default_pad_token_id() -> usize {
    65000
}
fn default_max_pos() -> usize {
    512
}

struct MarianTokenizer {
    /// Token ID -> piece string.
    id_to_piece: Vec<String>,
    /// Piece string -> (id, score), used for Unigram Viterbi encoding.
    piece_to_entry: HashMap<String, (usize, f64)>,
    /// Multilingual target prefix tag (e.g. `>>kor<<`, `>>pt<<`) -> token ID.
    prefix_token_to_id: HashMap<String, usize>,
    /// Maximum piece character length in vocab (bounds Viterbi inner loop).
    max_piece_chars: usize,
    unk_id: usize,
    eos_id: usize,
    pad_id: usize,
    special_ids: Vec<usize>,
    /// Penalty applied to unknown single characters.
    unk_penalty: f64,
}

impl MarianTokenizer {
    fn from_file(path: &Path, eos_id: usize, pad_id: usize) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| {
            i18n::tf(
                "Failed to read tokenizer ({path}): {err}",
                &[
                    ("path", &path.display().to_string()),
                    ("err", &e.to_string()),
                ],
            )
        })?;
        Self::from_json_bytes(&bytes, eos_id, pad_id)
    }

    fn from_json_bytes(bytes: &[u8], eos_id: usize, pad_id: usize) -> Result<Self, String> {
        let parsed: HfTokenizerJson = serde_json::from_slice(bytes).map_err(|e| {
            i18n::tf(
                "Failed to parse tokenizer.json: {err}",
                &[("err", &e.to_string())],
            )
        })?;
        if parsed.model.vocab.is_empty() {
            return Err(i18n::t("tokenizer.json vocabulary is empty").into());
        }

        let mut special_ids = vec![eos_id, pad_id];
        let mut unk_id = parsed.model.unk_id.unwrap_or(1);
        let mut prefix_token_to_id = HashMap::new();

        for tok in &parsed.added_tokens {
            if tok.special && !special_ids.contains(&tok.id) {
                special_ids.push(tok.id);
            }
            if tok.content == "<unk>" {
                unk_id = tok.id;
            }
            if is_lang_prefix_piece(&tok.content) {
                prefix_token_to_id.insert(tok.content.clone(), tok.id);
            }
        }
        // Some Xenova-exported tokenizer.json files point `model.unk_id` at a comma instead of `<unk>`.
        if parsed
            .model
            .vocab
            .get(unk_id)
            .is_none_or(|(p, _)| p != "<unk>")
        {
            if let Some(pos) = parsed.model.vocab.iter().position(|(p, _)| p == "<unk>") {
                unk_id = pos;
            }
        }

        let mut id_to_piece = Vec::with_capacity(parsed.model.vocab.len());
        let mut piece_to_entry = HashMap::with_capacity(parsed.model.vocab.len());
        let mut max_piece_chars = 1usize;
        let mut min_valid_score = 0.0_f64;

        for (id, (piece, score)) in parsed.model.vocab.into_iter().enumerate() {
            if is_lang_prefix_piece(&piece) {
                prefix_token_to_id.entry(piece.clone()).or_insert(id);
            }
            let is_special = special_ids.contains(&id)
                || piece == "</s>"
                || piece == "<pad>"
                || piece == "<unk>";
            if !is_special && !piece.is_empty() && score > -99.0 {
                let char_len = piece.chars().count();
                if char_len > max_piece_chars {
                    max_piece_chars = char_len;
                }
                if score < min_valid_score {
                    min_valid_score = score;
                }
                piece_to_entry.entry(piece.clone()).or_insert((id, score));
            }
            id_to_piece.push(piece);
        }

        Ok(Self {
            id_to_piece,
            piece_to_entry,
            prefix_token_to_id,
            max_piece_chars: max_piece_chars.min(48),
            unk_id,
            eos_id,
            pad_id,
            special_ids,
            unk_penalty: min_valid_score - 10.0,
        })
    }

    /// Look up the token ID for a multilingual OPUS-MT target prefix tag (e.g. `>>kor<<`, `>>pt<<`).
    fn find_target_prefix_id(&self, candidates: &[&str]) -> Option<i64> {
        for &cand in candidates {
            if let Some(&id) = self.prefix_token_to_id.get(cand) {
                return Some(id as i64);
            }
            let with_meta = format!("\u{2581}{}", cand);
            if let Some(&id) = self.prefix_token_to_id.get(&with_meta) {
                return Some(id as i64);
            }
            if let Some(&(id, _)) = self.piece_to_entry.get(cand) {
                return Some(id as i64);
            }
            if let Some(&(id, _)) = self.piece_to_entry.get(&with_meta) {
                return Some(id as i64);
            }
        }
        None
    }

    /// Encode text with WhitespaceSplit + Metaspace (U+2581) + Unigram Viterbi, appending `</s>`.
    #[cfg(test)]
    fn encode(&self, text: &str, max_tokens: usize) -> Vec<i64> {
        self.encode_with_prefix(text, &[], max_tokens)
    }

    /// Encode text with an optional multilingual target prefix token (e.g. `>>kor<<`).
    fn encode_with_prefix(
        &self,
        text: &str,
        prefix_candidates: &[&str],
        max_tokens: usize,
    ) -> Vec<i64> {
        let mut ids = Vec::new();
        if !prefix_candidates.is_empty() {
            if let Some(pid) = self.find_target_prefix_id(prefix_candidates) {
                ids.push(pid);
            }
        }
        for word in text.split_whitespace() {
            if word.is_empty() {
                continue;
            }
            let metaspace_word = format!("\u{2581}{}", word);
            self.encode_unigram_word(&metaspace_word, &mut ids);
            if ids.len() + 1 >= max_tokens {
                ids.truncate(max_tokens.saturating_sub(1));
                break;
            }
        }
        ids.push(self.eos_id as i64);
        ids
    }

    fn encode_unigram_word(&self, word: &str, out: &mut Vec<i64>) {
        let char_boundaries: Vec<usize> = word
            .char_indices()
            .map(|(idx, _)| idx)
            .chain(std::iter::once(word.len()))
            .collect();
        let n = char_boundaries.len().saturating_sub(1);
        if n == 0 {
            return;
        }

        let mut best_score = vec![f64::NEG_INFINITY; n + 1];
        let mut best_prev = vec![0usize; n + 1];
        let mut best_tok = vec![self.unk_id; n + 1];
        best_score[0] = 0.0;

        for i in 0..n {
            if !best_score[i].is_finite() {
                continue;
            }
            let max_end = (i + self.max_piece_chars).min(n);
            let mut matched_single = false;
            for j in (i + 1)..=max_end {
                let slice = &word[char_boundaries[i]..char_boundaries[j]];
                if let Some(&(tok_id, score)) = self.piece_to_entry.get(slice) {
                    if j == i + 1 {
                        matched_single = true;
                    }
                    let cand = best_score[i] + score;
                    if cand > best_score[j] {
                        best_score[j] = cand;
                        best_prev[j] = i;
                        best_tok[j] = tok_id;
                    }
                }
            }
            if !matched_single {
                let cand = best_score[i] + self.unk_penalty;
                if cand > best_score[i + 1] {
                    best_score[i + 1] = cand;
                    best_prev[i + 1] = i;
                    best_tok[i + 1] = self.unk_id;
                }
            }
        }

        let mut reversed = Vec::new();
        let mut pos = n;
        while pos > 0 {
            let tok = best_tok[pos];
            reversed.push(tok as i64);
            let prev = best_prev[pos];
            if prev >= pos {
                break;
            }
            pos = prev;
        }
        reversed.reverse();

        // Merge consecutive `<unk>` tokens to match SentencePiece default behavior.
        for tok in reversed {
            if tok as usize == self.unk_id && out.last().copied() == Some(self.unk_id as i64) {
                continue;
            }
            out.push(tok);
        }
    }

    /// Decode generated token IDs back into natural-language text.
    fn decode(&self, ids: &[i64], target_lang: &str) -> String {
        let mut raw = String::new();
        for &id in ids {
            if id < 0 {
                continue;
            }
            let uid = id as usize;
            if uid == self.eos_id || uid == self.pad_id || self.special_ids.contains(&uid) {
                continue;
            }
            if let Some(piece) = self.id_to_piece.get(uid) {
                if piece == "</s>"
                    || piece == "<pad>"
                    || piece == "<unk>"
                    || is_lang_prefix_piece(piece)
                {
                    continue;
                }
                raw.push_str(piece);
            }
        }
        let spaced = raw.replace('\u{2581}', " ");
        let trimmed = spaced.trim();
        if matches!(target_lang, "zh" | "zh-tw" | "ja") {
            compact_cjk_spaces(trimmed)
        } else {
            collapse_ascii_spaces(trimmed)
        }
    }
}

fn is_lang_prefix_piece(piece: &str) -> bool {
    let s = piece.trim_start_matches('\u{2581}');
    s.len() >= 6 && s.starts_with(">>") && s.ends_with("<<")
}

/// Remove redundant spaces adjacent to CJK/fullwidth characters while preserving Latin word spacing.
fn compact_cjk_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &ch) in chars.iter().enumerate() {
        if ch == ' ' {
            let prev = i.checked_sub(1).and_then(|idx| chars.get(idx)).copied();
            let next = chars.get(i + 1).copied();
            let skip = match (prev, next) {
                (Some(p), Some(n)) => is_cjk_or_fullwidth(p) || is_cjk_or_fullwidth(n),
                _ => true,
            };
            if skip || out.ends_with(' ') {
                continue;
            }
        }
        out.push(ch);
    }
    out.trim().to_string()
}

fn collapse_ascii_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn is_cjk_or_fullwidth(ch: char) -> bool {
    let cp = ch as u32;
    (0x3000..=0x303F).contains(&cp)
        || (0x3040..=0x30FF).contains(&cp)
        || (0x3400..=0x9FFF).contains(&cp)
        || (0xFF00..=0xFFEF).contains(&cp)
}

// ============================================================================
// ONNX Runtime Marian Encoder-Decoder inference engine (KV-Cache + session cache)
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
struct TranslatorKey {
    dll_path: PathBuf,
    pack_dir: PathBuf,
    pack_id: &'static str,
    files: Vec<FileStamp>,
    intra_threads: usize,
}

struct CachedTranslator {
    key: TranslatorKey,
    _runtime: OcrRuntime,
    config: MarianConfigJson,
    tokenizer: MarianTokenizer,
    encoder: Session,
    decoder: Session,
    decoder_uses_cache_branch: bool,
}

static ENGINE_CACHE: Mutex<Option<CachedTranslator>> = Mutex::new(None);

fn default_intra_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 4))
        .unwrap_or(2)
}

fn build_session(
    env: &ort::environment::Environment,
    model_path: &Path,
    intra_threads: usize,
) -> ort::Result<Session> {
    Session::builder(env)?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(intra_threads)?
        .with_inter_threads(1)?
        .with_parallel_execution(false)?
        .commit_from_file(model_path)
}

fn load_or_get_translator<'a>(
    guard: &'a mut Option<CachedTranslator>,
    dll_path: &Path,
    pack_dir: &Path,
    spec: &'static TranslationPackSpec,
) -> Result<&'a mut CachedTranslator, String> {
    let files = spec
        .files
        .iter()
        .map(|file| {
            check_pack_file(&pack_dir.join(file.local_name), file.local_name).ok_or_else(|| {
                i18n::tf(
                    "Model file failed validation: {file}",
                    &[("file", file.local_name)],
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let key = TranslatorKey {
        dll_path: dll_path.to_path_buf(),
        pack_dir: pack_dir.to_path_buf(),
        pack_id: spec.id,
        files,
        intra_threads: default_intra_threads(),
    };

    let needs_rebuild = guard.as_ref().is_none_or(|c| c.key != key);
    if needs_rebuild {
        let runtime = crate::ocr::load_shared_runtime(&key.dll_path)?;
        let env = runtime.environment();

        let cfg_bytes = std::fs::read(pack_dir.join("config.json")).map_err(|e| {
            i18n::tf(
                "Failed to read language pack config.json: {err}",
                &[("err", &e.to_string())],
            )
        })?;
        let config: MarianConfigJson = serde_json::from_slice(&cfg_bytes).map_err(|e| {
            i18n::tf(
                "Failed to parse language pack config.json: {err}",
                &[("err", &e.to_string())],
            )
        })?;

        let tokenizer = MarianTokenizer::from_file(
            &pack_dir.join("tokenizer.json"),
            config.eos_token_id,
            config.pad_token_id,
        )?;

        let pack_name = spec.display_label();
        let encoder = build_session(
            env,
            &pack_dir.join("encoder_model_quantized.onnx"),
            key.intra_threads,
        )
        .map_err(|e| {
            i18n::tf(
                "Failed to load encoder model ({pack}): {err}",
                &[("pack", &pack_name), ("err", &e.to_string())],
            )
        })?;

        let decoder = build_session(
            env,
            &pack_dir.join("decoder_model_merged_quantized.onnx"),
            key.intra_threads,
        )
        .map_err(|e| {
            i18n::tf(
                "Failed to load decoder model ({pack}): {err}",
                &[("pack", &pack_name), ("err", &e.to_string())],
            )
        })?;

        let decoder_uses_cache_branch = decoder
            .inputs()
            .iter()
            .any(|inp| inp.name() == "use_cache_branch");

        *guard = Some(CachedTranslator {
            key,
            _runtime: runtime,
            config,
            tokenizer,
            encoder,
            decoder,
            decoder_uses_cache_branch,
        });
    }

    Ok(guard.as_mut().unwrap())
}

impl CachedTranslator {
    fn translate_chunk(
        &mut self,
        text: &str,
        target_lang: &str,
        prefix_candidates: &[&str],
    ) -> Result<String, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(String::new());
        }

        let max_src = self.config.max_position_embeddings.clamp(64, 512);
        let input_ids = self
            .tokenizer
            .encode_with_prefix(trimmed, prefix_candidates, max_src);
        let seq_len = input_ids.len();
        let attention_mask = vec![1_i64; seq_len];

        // 1. Encoder forward pass.
        let enc_ids_tensor = Tensor::from_array(([1usize, seq_len], input_ids))
            .map_err(|e| format!("Failed to build encoder input tensor: {}", e))?;
        let enc_mask_tensor = Tensor::from_array(([1usize, seq_len], attention_mask.clone()))
            .map_err(|e| format!("Failed to build encoder mask tensor: {}", e))?;

        let enc_inputs: Vec<(String, SessionInputValue<'_>)> = vec![
            ("input_ids".into(), enc_ids_tensor.into()),
            ("attention_mask".into(), enc_mask_tensor.into()),
        ];
        let mut enc_outputs = self
            .encoder
            .run(SessionInputs::from(enc_inputs))
            .map_err(|e| format!("Encoder inference failed: {}", e))?;

        let encoder_hidden_states: DynValue = enc_outputs
            .remove("last_hidden_state")
            .ok_or_else(|| "Encoder output missing last_hidden_state".to_string())?;
        drop(enc_outputs);

        // 2. Autoregressive greedy decoding with KV-Cache.
        let decoder_start = self
            .config
            .decoder_start_token_id
            .unwrap_or(self.config.pad_token_id) as i64;
        let eos_id = self.config.eos_token_id as i64;
        let pad_id = self.config.pad_token_id as i64;
        let num_layers = self.config.decoder_layers;
        let num_heads = self.config.decoder_attention_heads;
        let head_dim = (self.config.d_model / num_heads.max(1)).max(1);
        let max_new_tokens = (seq_len * 3 + 32).clamp(48, 256);

        let mut bad_tokens: Vec<usize> = self
            .config
            .bad_words_ids
            .iter()
            .filter(|seq| seq.len() == 1)
            .map(|seq| seq[0])
            .collect();
        if !bad_tokens.contains(&(pad_id as usize)) {
            bad_tokens.push(pad_id as usize);
        }

        let enc_mask_for_dec = Tensor::from_array(([1usize, seq_len], attention_mask))
            .map_err(|e| format!("Failed to build decoder mask tensor: {}", e))?;

        let mut generated: Vec<i64> = Vec::with_capacity(64);
        let mut cur_token = decoder_start;
        let mut decoder_kv: Vec<(DynValue, DynValue)> = Vec::new();
        let mut encoder_kv: Vec<(DynValue, DynValue)> = Vec::new();

        for step in 0..max_new_tokens {
            let use_cache = step > 0 && self.decoder_uses_cache_branch;

            let step_ids = if self.decoder_uses_cache_branch {
                Tensor::from_array(([1usize, 1], vec![cur_token]))
            } else {
                let mut all = Vec::with_capacity(generated.len() + 1);
                all.push(decoder_start);
                all.extend_from_slice(&generated);
                Tensor::from_array(([1usize, all.len()], all))
            }
            .map_err(|e| format!("Failed to build decoder input tensor: {}", e))?;

            let mut dec_inputs: Vec<(String, SessionInputValue<'_>)> =
                Vec::with_capacity(4 + num_layers * 4);
            dec_inputs.push(("input_ids".into(), step_ids.into()));
            dec_inputs.push((
                "encoder_attention_mask".into(),
                SessionInputValue::from(&enc_mask_for_dec),
            ));
            dec_inputs.push((
                "encoder_hidden_states".into(),
                SessionInputValue::from(&encoder_hidden_states),
            ));

            if self.decoder_uses_cache_branch {
                let cache_flag = Tensor::from_array(([1usize], vec![use_cache]))
                    .map_err(|e| format!("Failed to build use_cache_branch tensor: {}", e))?;
                dec_inputs.push(("use_cache_branch".into(), cache_flag.into()));

                if use_cache {
                    for layer in 0..num_layers {
                        let (dk, dv) = &decoder_kv[layer];
                        let (ek, ev) = &encoder_kv[layer];
                        dec_inputs.push((
                            format!("past_key_values.{}.decoder.key", layer),
                            SessionInputValue::from(dk),
                        ));
                        dec_inputs.push((
                            format!("past_key_values.{}.decoder.value", layer),
                            SessionInputValue::from(dv),
                        ));
                        dec_inputs.push((
                            format!("past_key_values.{}.encoder.key", layer),
                            SessionInputValue::from(ek),
                        ));
                        dec_inputs.push((
                            format!("past_key_values.{}.encoder.value", layer),
                            SessionInputValue::from(ev),
                        ));
                    }
                } else {
                    for layer in 0..num_layers {
                        for kind in ["decoder.key", "decoder.value", "encoder.key", "encoder.value"]
                        {
                            let empty = Tensor::from_array((
                                [1usize, num_heads, 0, head_dim],
                                Vec::<f32>::new(),
                            ))
                            .map_err(|e| format!("Failed to build empty KV-Cache tensor: {}", e))?;
                            dec_inputs.push((
                                format!("past_key_values.{}.{}", layer, kind),
                                empty.into(),
                            ));
                        }
                    }
                }
            }

            let mut dec_outputs = self
                .decoder
                .run(SessionInputs::from(dec_inputs))
                .map_err(|e| format!("Decoder inference failed (step={}): {}", step, e))?;

            let next_token = {
                let logits_val = dec_outputs
                    .get("logits")
                    .ok_or_else(|| "Decoder output missing logits".to_string())?;
                let (shape, data) = logits_val
                    .try_extract_tensor::<f32>()
                    .map_err(|e| format!("Failed to extract logits: {}", e))?;
                let vocab_size = *shape.last().unwrap_or(&0) as usize;
                if vocab_size == 0 || data.len() < vocab_size {
                    return Err("Decoder logits shape is invalid".into());
                }
                let last_slice = &data[data.len() - vocab_size..];
                select_next_token(last_slice, &bad_tokens, &generated)
            };

            if next_token == eos_id {
                break;
            }
            generated.push(next_token);
            cur_token = next_token;

            // Degenerate repetition guard: stop immediately if the tail repeats a 1..=4 token cycle 4 times.
            if has_degenerate_repeat(&generated) {
                break;
            }

            if self.decoder_uses_cache_branch {
                let mut next_dec_kv = Vec::with_capacity(num_layers);
                for layer in 0..num_layers {
                    let dk = dec_outputs
                        .remove(format!("present.{}.decoder.key", layer))
                        .ok_or_else(|| format!("Missing present.{}.decoder.key", layer))?;
                    let dv = dec_outputs
                        .remove(format!("present.{}.decoder.value", layer))
                        .ok_or_else(|| format!("Missing present.{}.decoder.value", layer))?;
                    next_dec_kv.push((dk, dv));
                }
                decoder_kv = next_dec_kv;

                if !use_cache {
                    let mut init_enc_kv = Vec::with_capacity(num_layers);
                    for layer in 0..num_layers {
                        let ek = dec_outputs
                            .remove(format!("present.{}.encoder.key", layer))
                            .ok_or_else(|| format!("Missing present.{}.encoder.key", layer))?;
                        let ev = dec_outputs
                            .remove(format!("present.{}.encoder.value", layer))
                            .ok_or_else(|| format!("Missing present.{}.encoder.value", layer))?;
                        init_enc_kv.push((ek, ev));
                    }
                    encoder_kv = init_enc_kv;
                }
            }
        }

        Ok(self.tokenizer.decode(&generated, target_lang))
    }
}

fn select_next_token(logits: &[f32], bad_tokens: &[usize], generated: &[i64]) -> i64 {
    let mut best_id = 0usize;
    let mut best_val = f32::NEG_INFINITY;
    let last_tok = generated.last().copied();
    let prev2_same = generated.len() >= 2
        && generated[generated.len() - 1] == generated[generated.len() - 2];

    for (idx, &raw) in logits.iter().enumerate() {
        if bad_tokens.contains(&idx) {
            continue;
        }
        // Apply a mild penalty when the same token repeats more than twice to prevent stalling.
        let mut score = raw;
        if prev2_same && last_tok == Some(idx as i64) {
            score -= 6.0;
        }
        if score > best_val {
            best_val = score;
            best_id = idx;
        }
    }
    best_id as i64
}

fn has_degenerate_repeat(tokens: &[i64]) -> bool {
    for period in 1..=4 {
        let needed = period * 4;
        if tokens.len() < needed {
            continue;
        }
        let tail = &tokens[tokens.len() - needed..];
        let unit = &tail[..period];
        if tail.chunks_exact(period).all(|c| c == unit) {
            return true;
        }
    }
    false
}

/// Split overly long lines on sentence punctuation to stay well below the 512-token limit.
fn split_long_line(line: &str, max_chars: usize) -> Vec<String> {
    if line.chars().count() <= max_chars {
        return vec![line.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;

    for ch in line.chars() {
        current.push(ch);
        current_chars += 1;
        let is_sentence_end = matches!(
            ch,
            '.' | '!' | '?' | '\u{3002}' | '\u{FF01}' | '\u{FF1F}' | ';' | '\u{FF1B}'
        );
        if (is_sentence_end && current_chars >= max_chars / 2) || current_chars >= max_chars {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                chunks.push(trimmed.to_string());
            }
            current.clear();
            current_chars = 0;
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        chunks.push(trimmed.to_string());
    }
    chunks
}

/// Translate a slice of lines using local ONNX language packs (guaranteed 1-to-1 length preservation).
pub fn translate_lines(
    lines: &[String],
    data_dir: &Path,
    settings: &Value,
) -> Result<Vec<String>, String> {
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Err(i18n::t("No text available to translate").into());
    }

    let storage_dir = resolve_translate_dir(settings);
    let custom_ocr_dir = settings["providers"]["ocr"]["local"]["modelDir"]
        .as_str()
        .unwrap_or("");
    let dll_path = crate::ocr::resolve_runtime_dll(custom_ocr_dir, Some(&storage_dir), data_dir)
        .ok_or_else(|| {
            i18n::t(
                "onnxruntime.dll not found; ensure onnxruntime.dll exists in the application directory",
            )
            .to_string()
        })?;

    let raw_target = settings["providers"]["translation"]["target"]
        .as_str()
        .unwrap_or("en");
    let raw_source = settings["providers"]["translation"]["source"]
        .as_str()
        .unwrap_or("auto");

    let target_code = normalize_lang_code(raw_target);
    let target_code = if target_code == "auto" {
        "en".to_string()
    } else {
        target_code
    };
    let combined = lines.join("\n");
    let source_code = match normalize_lang_code(raw_source).as_str() {
        "auto" => detect_source_language(&combined, &target_code).to_string(),
        other => other.to_string(),
    };

    if source_code == target_code {
        return Ok(lines.to_vec());
    }

    let route = resolve_translation_route(&source_code, &target_code, &storage_dir)?;
    if route.is_empty() {
        return Ok(lines.to_vec());
    }

    let mut current_lines: Vec<String> = lines.to_vec();
    for step in route {
        let pack_dir = pack_dir_in(&storage_dir, step.pack.id);
        let mut guard = ENGINE_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let engine = load_or_get_translator(&mut guard, &dll_path, &pack_dir, step.pack)?;

        let mut translated_lines = Vec::with_capacity(current_lines.len());
        for line in &current_lines {
            if line.trim().is_empty() {
                translated_lines.push(String::new());
                continue;
            }
            let segments = split_long_line(line, 320);
            let mut joined = Vec::with_capacity(segments.len());
            for seg in segments {
                let piece =
                    engine.translate_chunk(&seg, step.target_lang, step.prefix_candidates)?;
                if !piece.is_empty() {
                    joined.push(piece);
                }
            }
            let sep = if matches!(step.target_lang, "zh" | "zh-tw" | "ja") {
                ""
            } else {
                " "
            };
            translated_lines.push(joined.join(sep));
        }
        current_lines = translated_lines;
    }

    Ok(current_lines)
}

/// Translate text using local ONNX language packs (preserves line breaks; auto-routes direct or English pivot).
pub fn translate_text(text: &str, data_dir: &Path, settings: &Value) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err(i18n::t("No text available to translate").into());
    }
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let translated = translate_lines(&lines, data_dir, settings)?;
    Ok(translated.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_language_names_and_codes() {
        assert_eq!(normalize_lang_code("zh-CN"), "zh");
        assert_eq!(normalize_lang_code("Chinese Simplified"), "zh");
        assert_eq!(normalize_lang_code("Chinese Traditional"), "zh-tw");
        assert_eq!(normalize_lang_code("English"), "en");
        assert_eq!(normalize_lang_code("Japanese"), "ja");
        assert_eq!(normalize_lang_code("Korean"), "ko");
        assert_eq!(normalize_lang_code("Portuguese"), "pt");
        assert_eq!(normalize_lang_code("Thai"), "th");
        assert_eq!(normalize_lang_code("Greek"), "el");
        assert_eq!(normalize_lang_code("auto"), "auto");

        // Localized Chinese names from zh-CN.json must also normalize cleanly.
        let zh_ja = i18n::zh_lang_entry("ja").unwrap().name.as_str();
        let zh_cn = i18n::zh_lang_entry("zh").unwrap().name.as_str();
        assert_eq!(normalize_lang_code(zh_ja), "ja");
        assert_eq!(normalize_lang_code(zh_cn), "zh");
    }

    #[test]
    fn detects_source_language_from_script() {
        assert_eq!(detect_source_language("Hello, welcome to SnapDock.", "zh"), "en");
        assert_eq!(
            detect_source_language("\u{4F60}\u{597D}\u{FF0C}\u{6B22}\u{8FCE}\u{4F7F}\u{7528}\u{62FE}\u{50CF}\u{3002}", "en"),
            "zh"
        );
        assert_eq!(
            detect_source_language("\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}\u{4E16}\u{754C}", "zh"),
            "ja"
        );
        assert_eq!(
            detect_source_language("\u{C548}\u{B155}\u{D558}\u{C138}\u{C694}", "zh"),
            "ko"
        );
        assert_eq!(
            detect_source_language("\u{041F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442} \u{043C}\u{0438}\u{0440}", "zh"),
            "ru"
        );
        assert_eq!(
            detect_source_language("\u{0E2A}\u{0E27}\u{0E31}\u{0E2A}\u{0E14}\u{0E35}", "zh"),
            "th"
        );
        assert_eq!(
            detect_source_language("\u{0393}\u{03B5}\u{03B9}\u{03B1} \u{03C3}\u{03BF}\u{03C5}", "zh"),
            "el"
        );
        assert_eq!(
            detect_source_language("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}", "zh"),
            "ar"
        );
        assert_eq!(detect_source_language("Xin ch\u{00E0}o th\u{1EBF} gi\u{1EDB}i", "zh"), "vi");
    }

    #[test]
    fn resolves_direct_and_bridged_routes() {
        let tmp = std::env::temp_dir().join(format!(
            "snapdock-translate-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        // Missing pack should return a clear error message.
        let err = resolve_translation_route("en", "zh", &tmp).unwrap_err();
        assert!(!err.is_empty());

        // Installing ja-en and en-zh enables automatic 2-hop ja -> zh bridging.
        for pack_id in ["ja-en", "en-zh"] {
            let spec = find_pack_spec(pack_id).unwrap();
            let dir = pack_dir_in(&tmp, pack_id);
            std::fs::create_dir_all(&dir).unwrap();
            for f in spec.files {
                let contents: &[u8] = match f.local_name {
                    "config.json" => br#"{"model_type":"marian"}"#,
                    "tokenizer.json" => br#"{"model":{"vocab":[["<unk>",0]]}}"#,
                    // ModelProto with ir_version and a nonempty graph field.
                    _ => &[8, 9, 58, 3, 18, 1, b'g'],
                };
                std::fs::write(dir.join(f.local_name), contents).unwrap();
            }
        }
        let route = resolve_translation_route("ja", "zh", &tmp).unwrap();
        assert_eq!(route.len(), 2);
        assert_eq!(route[0].pack.id, "ja-en");
        assert_eq!(route[1].pack.id, "en-zh");

        // Installing mul-en + en-mul unlocks every pair across all 42 languages.
        for pack_id in ["mul-en", "en-mul"] {
            let spec = find_pack_spec(pack_id).unwrap();
            let dir = pack_dir_in(&tmp, pack_id);
            std::fs::create_dir_all(&dir).unwrap();
            for f in spec.files {
                let contents: &[u8] = match f.local_name {
                    "config.json" => br#"{"model_type":"marian"}"#,
                    "tokenizer.json" => br#"{"model":{"vocab":[["<unk>",0]]}}"#,
                    // ModelProto with ir_version and a nonempty graph field.
                    _ => &[8, 9, 58, 3, 18, 1, b'g'],
                };
                std::fs::write(dir.join(f.local_name), contents).unwrap();
            }
        }
        for src in LANGUAGES {
            for tgt in LANGUAGES {
                let steps = resolve_translation_route(src.code, tgt.code, &tmp)
                    .unwrap_or_else(|e| panic!("{} -> {} route failed: {}", src.code, tgt.code, e));
                if src.code == tgt.code {
                    assert!(steps.is_empty());
                } else {
                    assert!(!steps.is_empty() && steps.len() <= 2);
                }
            }
        }
        // Dedicated packs take precedence over universal packs; zh -> ko uses en-mul with >>kor<<.
        let zh_ko = resolve_translation_route("zh", "ko", &tmp).unwrap();
        assert_eq!(zh_ko.len(), 2);
        assert_eq!(zh_ko[1].pack.id, "en-mul");
        assert!(zh_ko[1].prefix_candidates.contains(&">>kor<<"));

        let dir = pack_dir_in(&tmp, "en-zh");
        let spec = find_pack_spec("en-zh").unwrap();
        let encoder = dir.join("encoder_model_quantized.onnx");
        std::fs::write(&encoder, [8, 9, 58, 3, 18]).unwrap();
        std::fs::remove_file(dir.join("tokenizer.json")).unwrap();
        std::fs::write(dir.join("keep.txt"), b"unrelated").unwrap();
        let state = inspect_pack(dir.clone(), spec);
        assert!(!state.installed && state.has_local_files);
        assert_eq!(state.invalid, ["encoder_model_quantized.onnx"]);
        assert_eq!(state.missing, ["tokenizer.json"]);
        assert_eq!(
            resolve_translation_route("en", "zh", &tmp).unwrap()[0]
                .pack
                .id,
            "en-mul"
        );
        remove_pack(spec.id, &tmp).unwrap();
        assert!(!encoder.exists());
        assert!(dir.join("keep.txt").is_file());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn language_and_pack_search_matches_names_codes_and_multilingual_fallbacks() {
        assert!(matches_language_query(find_language("ko").unwrap(), "korean"));
        assert!(matches_language_query(
            find_language("ko").unwrap(),
            i18n::zh_lang_entry("ko").unwrap().name.as_str()
        ));
        assert!(matches_language_query(
            find_language("pt").unwrap(),
            i18n::zh_lang_entry("pt").unwrap().name.as_str()
        ));
        assert!(matches_pack_query(find_pack_spec("en-mul").unwrap(), "korean"));
        assert!(matches_pack_query(find_pack_spec("ko-en").unwrap(), "korean"));
        assert!(matches_pack_query(find_pack_spec("en-romance").unwrap(), "portuguese"));
        assert!(matches_pack_query(find_pack_spec("mul-en").unwrap(), "greek"));
    }

    #[test]
    fn pair_search_expands_single_language_and_directional_queries() {
        let tmp = std::env::temp_dir().join(format!(
            "snapdock-pair-search-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        // Single-language query in English or Chinese lists all (N-1)*2 directed pairs.
        let expected_pairs = (LANGUAGES.len() - 1) * 2;
        let res_en = search_translation_pairs("Japanese", &tmp).unwrap();
        assert_eq!(res_en.focus_lang, Some("ja"));
        assert_eq!(res_en.pairs.len(), expected_pairs);
        assert!(res_en.pairs.iter().any(|p| p.source_code == "ja" && p.target_code == "zh"));
        assert!(res_en.pairs.iter().any(|p| p.source_code == "zh" && p.target_code == "ja"));

        let zh_ja = i18n::zh_lang_entry("ja").unwrap().name.as_str();
        let res_zh = search_translation_pairs(zh_ja, &tmp).unwrap();
        assert_eq!(res_zh.focus_lang, Some("ja"));
        assert_eq!(res_zh.pairs.len(), expected_pairs);

        // Directional query narrows to the exact direction.
        let dir_en = search_translation_pairs("ja->zh", &tmp).unwrap();
        assert_eq!(dir_en.pairs.len(), 1);
        assert_eq!(dir_en.pairs[0].source_code, "ja");
        assert_eq!(dir_en.pairs[0].target_code, "zh");
        assert!(dir_en.pairs[0].is_bridged());

        // Localized short directional query (e.g. short(ja) + separator + short(zh)).
        let sep = &i18n::zh_search_config().direction_separators[0];
        let ja_short = &i18n::zh_lang_entry("ja").unwrap().short[0];
        let zh_short = &i18n::zh_lang_entry("zh").unwrap().short[0];
        let dir_zh = search_translation_pairs(&format!("{}{}{}", ja_short, sep, zh_short), &tmp)
            .unwrap();
        assert!(dir_zh.pairs.iter().any(|p| p.source_code == "ja" && p.target_code == "zh"));

        // Two-language unordered query returns both directions.
        let both = search_translation_pairs("ja zh", &tmp).unwrap();
        assert_eq!(both.pairs.len(), 2);

        // Explicit pack query falls back to catalog mode.
        assert!(search_translation_pairs("mul-en", &tmp).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unigram_tokenizer_encodes_and_decodes_cleanly() {
        let sample = serde_json::json!({
            "added_tokens": [
                {"id": 0, "content": "</s>", "special": true},
                {"id": 1, "content": "<unk>", "special": true},
                {"id": 9, "content": "<pad>", "special": true}
            ],
            "model": {
                "unk_id": 1,
                "vocab": [
                    ["</s>", 0.0],
                    ["<unk>", 0.0],
                    ["\u{2581}Hello", -1.5],
                    ["\u{2581}world", -1.8],
                    [",", -1.0],
                    ["\u{2581}\u{4F60}", -2.0],
                    ["\u{597D}", -1.2],
                    ["\u{2581}\u{4E16}\u{754C}", -1.3],
                    ["!", -1.0],
                    ["<pad>", 0.0],
                    [">>kor<<", 0.0]
                ]
            }
        });
        let tok = MarianTokenizer::from_json_bytes(sample.to_string().as_bytes(), 0, 9).unwrap();
        let ids = tok.encode("Hello, world!", 64);
        assert_eq!(ids, vec![2, 4, 3, 8, 0]);
        let prefixed = tok.encode_with_prefix("Hello", &[">>kor<<", ">>ko<<"], 64);
        assert_eq!(prefixed, vec![10, 2, 0]);
        // Echoed >>kor<< prefix token should be stripped on decode.
        assert_eq!(tok.decode(&[10, 2, 3, 0], "en"), "Hello world");
        // Out-of-vocabulary characters should fall back to <unk> without dropping the rest of the word.
        let unk_ids = tok.encode("HelloZ", 64);
        assert_eq!(unk_ids, vec![2, 1, 0]);
        assert_eq!(
            tok.decode(&[5, 6, 7, 0], "zh"),
            "\u{4F60}\u{597D}\u{4E16}\u{754C}"
        );
        assert_eq!(tok.decode(&[2, 3, 0], "en"), "Hello world");
    }

    #[test]
    fn split_long_line_breaks_at_sentence_boundaries() {
        let line = "First sentence here. Second sentence here! Third sentence here?";
        let parts = split_long_line(line, 30);
        assert!(parts.len() >= 2);
        assert_eq!(parts.join(" "), line);
    }

    #[test]
    fn build_download_url_respects_mirror() {
        assert_eq!(
            build_download_url("hf-mirror", "Xenova/opus-mt-en-zh", "config.json"),
            "https://hf-mirror.com/Xenova/opus-mt-en-zh/resolve/main/config.json"
        );
        assert_eq!(
            build_download_url("huggingface", "Xenova/opus-mt-en-zh", "onnx/encoder_model_quantized.onnx"),
            "https://huggingface.co/Xenova/opus-mt-en-zh/resolve/main/onnx/encoder_model_quantized.onnx"
        );
    }

    #[test]
    #[ignore = "Requires internet access to download ~114 MB en-zh pack and run real ONNX inference"]
    fn end_to_end_download_and_translate_en_zh() {
        let tmp = std::env::temp_dir().join(format!(
            "snapdock-e2e-translate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut last_pct = 0u32;
        let dl_res = rt.block_on(async {
            match download_pack("en-zh", &tmp, "hf-mirror", cancel.clone(), |p| {
                let pct = (p.fraction() * 100.0) as u32;
                if pct >= last_pct + 20 || pct == 100 {
                    last_pct = pct;
                    eprintln!("[e2e] {} {}% ({})", p.current_file, pct, p.file_index);
                }
            })
            .await
            {
                Ok(dir) => Ok(dir),
                Err(_) => {
                    download_pack("en-zh", &tmp, "huggingface", cancel, |_| {}).await
                }
            }
        });
        let pack_dir = dl_res.expect("en-zh pack should download successfully");
        assert!(is_pack_complete(&pack_dir, find_pack_spec("en-zh").unwrap()));

        let mut settings = crate::settings::defaults().clone();
        settings["providers"]["translation"]["mode"] = serde_json::json!("local");
        settings["providers"]["translation"]["source"] = serde_json::json!("en");
        settings["providers"]["translation"]["target"] = serde_json::json!("zh");
        settings["providers"]["translation"]["local"]["modelDir"] =
            serde_json::json!(tmp.to_string_lossy().to_string());

        let out = translate_text("Hello, welcome to SnapDock.", &tmp, &settings)
            .expect("local ONNX translation should succeed");
        eprintln!("[e2e] en->zh output: {}", out);
        assert!(
            out.chars().any(|c| (0x4E00..=0x9FFF).contains(&(c as u32))),
            "translation should contain CJK characters, got: {}",
            out
        );

        let multi = translate_text(
            "Good morning.\nThis is an offline screenshot translator.",
            &tmp,
            &settings,
        )
        .expect("multi-line local translation should succeed");
        eprintln!("[e2e] multi-line output:\n{}", multi);
        assert_eq!(multi.lines().count(), 2);

        remove_pack("en-zh", &tmp).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
