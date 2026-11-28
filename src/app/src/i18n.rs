//! Internationalization (i18n) runtime and locale catalog loader.
//!
//! All source text in code is written in English. Simplified Chinese translations
//! are loaded from `src/shared/locales/zh-CN.json` (embedded at compile time and
//! optionally overridden by `<exe_dir>/locales/zh-CN.json` at runtime).

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// User-configured UI language preference stored in `general.language`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppLanguage {
    System,
    En,
    ZhCn,
}

impl AppLanguage {
    pub fn from_setting(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "en" | "en-us" | "en_us" => Self::En,
            "zh-cn" | "zh" | "zh_cn" | "zh-hans" => Self::ZhCn,
            _ => Self::System,
        }
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }
}

/// Resolved runtime UI language after evaluating `AppLanguage::System`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveLanguage {
    En = 0,
    ZhCn = 1,
}

#[derive(Debug, Default, Deserialize)]
struct LocaleMeta {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct LocaleLanguageEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub short: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct LocaleSearchConfig {
    #[serde(default)]
    pub auto_aliases: Vec<String>,
    #[serde(default)]
    pub multilingual_aliases: Vec<String>,
    #[serde(default)]
    pub romance_aliases: Vec<String>,
    #[serde(default)]
    pub direction_separators: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct LocaleCatalog {
    #[serde(default)]
    meta: LocaleMeta,
    #[serde(default)]
    strings: HashMap<String, String>,
    #[serde(default)]
    languages: HashMap<String, LocaleLanguageEntry>,
    #[serde(default)]
    search: LocaleSearchConfig,
}

static ACTIVE_LANG: AtomicU8 = AtomicU8::new(EffectiveLanguage::En as u8);

fn zh_catalog() -> &'static LocaleCatalog {
    static CATALOG: OnceLock<LocaleCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut base: LocaleCatalog =
            serde_json::from_str(include_str!("../../shared/locales/zh-CN.json"))
                .expect("embedded zh-CN.json locale file must be valid JSON");

        // Allow optional runtime override from `<exe_dir>/locales/zh-CN.json`.
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let external = dir.join("locales").join("zh-CN.json");
                if let Ok(raw) = std::fs::read_to_string(&external) {
                    if let Ok(override_cat) = serde_json::from_str::<LocaleCatalog>(&raw) {
                        if !override_cat.meta.name.is_empty() {
                            base.meta.name = override_cat.meta.name;
                        }
                        for (k, v) in override_cat.strings {
                            base.strings.insert(k, v);
                        }
                        for (k, v) in override_cat.languages {
                            base.languages.insert(k, v);
                        }
                        if !override_cat.search.auto_aliases.is_empty() {
                            base.search.auto_aliases = override_cat.search.auto_aliases;
                        }
                        if !override_cat.search.multilingual_aliases.is_empty() {
                            base.search.multilingual_aliases =
                                override_cat.search.multilingual_aliases;
                        }
                        if !override_cat.search.romance_aliases.is_empty() {
                            base.search.romance_aliases = override_cat.search.romance_aliases;
                        }
                        if !override_cat.search.direction_separators.is_empty() {
                            base.search.direction_separators =
                                override_cat.search.direction_separators;
                        }
                    }
                }
            }
        }

        base
    })
}

fn native_names() -> &'static HashMap<String, String> {
    static NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        serde_json::from_str(include_str!("../../shared/locales/native_names.json"))
            .expect("embedded native_names.json must be valid JSON")
    })
}

fn sample_texts() -> &'static HashMap<String, String> {
    static SAMPLES: OnceLock<HashMap<String, String>> = OnceLock::new();
    SAMPLES.get_or_init(|| {
        serde_json::from_str(include_str!("../../shared/locales/sample_texts.json"))
            .expect("embedded sample_texts.json must be valid JSON")
    })
}

/// Return a short sample sentence in the source language for testing translation providers.
pub fn sample_text_for_lang(src_code: &str, tgt_code: &str) -> &'static str {
    let effective = if src_code == "auto" {
        if tgt_code == "en" {
            "zh"
        } else {
            "en"
        }
    } else {
        src_code
    };
    sample_texts()
        .get(effective)
        .map(String::as_str)
        .unwrap_or("Hello, welcome to SnapDock.")
}

/// Detect the operating system UI language.
pub fn detect_system_language() -> EffectiveLanguage {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetUserDefaultUILanguage() -> u16;
        }
        // Primary language ID is in the lower 10 bits; LANG_CHINESE = 0x04.
        let lang_id = unsafe { GetUserDefaultUILanguage() };
        if (lang_id & 0x03ff) == 0x04 {
            return EffectiveLanguage::ZhCn;
        }
        return EffectiveLanguage::En;
    }

    #[cfg(not(windows))]
    {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(val) = std::env::var(key) {
                let lower = val.to_ascii_lowercase();
                if lower.starts_with("zh") {
                    return EffectiveLanguage::ZhCn;
                }
                if !lower.is_empty() {
                    return EffectiveLanguage::En;
                }
            }
        }
        EffectiveLanguage::En
    }
}

/// Resolve a configured `AppLanguage` to a concrete `EffectiveLanguage`.
pub fn resolve_language(pref: AppLanguage) -> EffectiveLanguage {
    match pref {
        AppLanguage::System => detect_system_language(),
        AppLanguage::En => EffectiveLanguage::En,
        AppLanguage::ZhCn => EffectiveLanguage::ZhCn,
    }
}

/// Set the active runtime language explicitly.
pub fn set_effective_language(lang: EffectiveLanguage) {
    ACTIVE_LANG.store(lang as u8, Ordering::Relaxed);
}

/// Apply language preference from a raw setting string (`"system"`, `"en"`, `"zh-CN"`).
pub fn set_language_from_str(raw: &str) -> EffectiveLanguage {
    let eff = resolve_language(AppLanguage::from_setting(raw));
    set_effective_language(eff);
    eff
}

/// Apply language preference from the root settings JSON object.
pub fn apply_settings(settings: &Value) -> EffectiveLanguage {
    let raw = settings
        .get("general")
        .and_then(|g| g.get("language"))
        .and_then(|v| v.as_str())
        .unwrap_or("system");
    set_language_from_str(raw)
}

/// Return the currently active UI language.
pub fn effective_language() -> EffectiveLanguage {
    if ACTIVE_LANG.load(Ordering::Relaxed) == EffectiveLanguage::ZhCn as u8 {
        EffectiveLanguage::ZhCn
    } else {
        EffectiveLanguage::En
    }
}

/// Return `true` when Simplified Chinese UI is currently active.
pub fn is_zh_cn() -> bool {
    effective_language() == EffectiveLanguage::ZhCn
}

/// Translate an English UI string into the currently active language.
pub fn t(key: &'static str) -> &'static str {
    if is_zh_cn() {
        zh_catalog()
            .strings
            .get(key)
            .map(String::as_str)
            .unwrap_or(key)
    } else {
        key
    }
}

/// Translate a template string and substitute `{name}` placeholders.
pub fn tf(template: &'static str, args: &[(&str, &str)]) -> String {
    let mut out = t(template).to_string();
    for (name, value) in args {
        let placeholder = format!("{{{}}}", name);
        out = out.replace(&placeholder, value);
    }
    out
}

/// Return the native display name of Simplified Chinese from `zh-CN.json`.
pub fn zh_cn_native_name() -> &'static str {
    let name = zh_catalog().meta.name.as_str();
    if name.is_empty() {
        "zh-CN"
    } else {
        name
    }
}

/// Return the native script display name of a language code (e.g. `ja` -> native Japanese name).
pub fn native_lang_name(code: &str) -> Option<&'static str> {
    native_names().get(code).map(String::as_str)
}

/// Return the localized display name of a language code in the active UI language.
pub fn lang_name(code: &str) -> &'static str {
    if code == "auto" {
        return t("Auto Detect");
    }
    if is_zh_cn() {
        if let Some(entry) = zh_catalog().languages.get(code) {
            if !entry.name.is_empty() {
                return entry.name.as_str();
            }
        }
    }
    if let Some(lang) = crate::translate::find_language(code) {
        return lang.en_name;
    }
    t("Target Language")
}

/// Return the option label for dropdowns, e.g. `Japanese (native)` or `English`.
pub fn lang_option_label(code: &str) -> String {
    let localized = lang_name(code);
    if let Some(native) = native_lang_name(code) {
        if native != localized {
            return format!("{} ({})", localized, native);
        }
    }
    localized.to_string()
}

/// Return extra localized metadata for a language code from `zh-CN.json` (used for multilingual search).
pub fn zh_lang_entry(code: &str) -> Option<&'static LocaleLanguageEntry> {
    zh_catalog().languages.get(code)
}

/// Return localized search keywords/separators from `zh-CN.json`.
pub fn zh_search_config() -> &'static LocaleSearchConfig {
    &zh_catalog().search
}

/// Return the localized label for a translation pack ID.
pub fn pack_label(spec: &crate::translate::TranslationPackSpec) -> String {
    match spec.id {
        "mul-en" => t("Multilingual -> English (Universal, 40+ Languages)").to_string(),
        "en-mul" => t("English -> Multilingual (Universal, 40+ Languages)").to_string(),
        "romance-en" => {
            t("Romance -> English (PT/ES/FR/IT/RO/CA)").to_string()
        }
        "en-romance" => {
            t("English -> Romance (PT/ES/FR/IT/RO/CA)").to_string()
        }
        _ => format!("{} -> {}", lang_name(spec.source_lang), lang_name(spec.target_lang)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_setting_normalizes_and_defaults_to_system() {
        assert_eq!(AppLanguage::from_setting(""), AppLanguage::System);
        assert_eq!(AppLanguage::from_setting("unknown"), AppLanguage::System);
        assert_eq!(AppLanguage::from_setting("system"), AppLanguage::System);
        assert_eq!(AppLanguage::from_setting("en"), AppLanguage::En);
        assert_eq!(AppLanguage::from_setting("zh-CN"), AppLanguage::ZhCn);
        assert_eq!(AppLanguage::from_setting("zh"), AppLanguage::ZhCn);
    }

    #[test]
    fn translation_lookup_and_placeholder_formatting_work() {
        let prev = effective_language();
        set_effective_language(EffectiveLanguage::En);
        assert_eq!(t("General"), "General");
        assert_eq!(tf("Shown {shown} / {total}", &[("shown", "3"), ("total", "10")]), "Shown 3 / 10");

        set_effective_language(EffectiveLanguage::ZhCn);
        assert_ne!(t("General"), "General");
        assert!(zh_cn_native_name().chars().any(|c| !c.is_ascii()));
        assert!(zh_lang_entry("ja").is_some());

        set_effective_language(prev);
    }
}
