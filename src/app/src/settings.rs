//! Preferences: default merging, validation, and normalization.

use crate::i18n;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// Shared default settings loaded from `src/shared/defaults.json`.
pub fn defaults() -> &'static Value {
    static DEFAULTS: OnceLock<Value> = OnceLock::new();
    DEFAULTS.get_or_init(|| {
        serde_json::from_str(include_str!("../../shared/defaults.json"))
            .expect("embedded defaults.json must be valid JSON")
    })
}

/// Merge known fields according to the `defaults` schema; unknown or mismatched fields are dropped.
fn merge_known(base: &Value, input: Option<&Value>) -> Value {
    let mut result = base.clone();
    let Some(input) = input else {
        return result;
    };
    let (Some(base_map), Some(input_map)) = (base.as_object(), input.as_object()) else {
        return result;
    };
    let Value::Object(result_map) = &mut result else {
        return result;
    };
    for (key, base_value) in base_map {
        let Some(input_value) = input_map.get(key) else {
            continue;
        };
        let merged = if base_value.is_object() {
            merge_known(base_value, Some(input_value))
        } else if same_kind(base_value, input_value) {
            input_value.clone()
        } else {
            continue;
        };
        result_map.insert(key.clone(), merged);
    }
    result
}

fn same_kind(base: &Value, input: &Value) -> bool {
    match (base, input) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(_), Value::Bool(_)) => true,
        (Value::Number(_), Value::Number(_)) => true,
        (Value::String(_), Value::String(_)) => true,
        _ => false,
    }
}

fn set_number(settings: &mut Value, path: &str, value: Value) {
    let mut cursor = settings;
    let segments: Vec<&str> = path.split('.').collect();
    for segment in &segments[..segments.len() - 1] {
        let Some(next) = cursor.get_mut(*segment) else {
            return;
        };
        cursor = next;
    }
    if let Value::Object(leaf) = cursor {
        leaf.insert(segments[segments.len() - 1].to_string(), value);
    }
}

fn get_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cursor = value;
    for segment in path.split('.') {
        cursor = cursor.get(segment)?;
    }
    Some(cursor)
}

fn clamp_value(value: Option<&Value>, min: i64, max: i64) -> Value {
    let number = value
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite())
        .map(|n| n.round() as i64)
        .unwrap_or(min);
    json!(number.clamp(min, max))
}

fn valid_hex_color(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    text.len() == 7
        && text.starts_with('#')
        && text[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Merge defaults and clamp every field into its valid range.
pub fn normalize(input: Option<&Value>) -> Value {
    let mut s = merge_known(defaults(), input);
    let clamp = |value: Option<&Value>, min: i64, max: i64| clamp_value(value, min, max);

    let language = s
        .get("general")
        .and_then(|g| g.get("language"))
        .and_then(|v| v.as_str())
        .unwrap_or("system");
    let normalized_lang = i18n::AppLanguage::from_setting(language).as_key();
    set_number(&mut s, "general.language", json!(normalized_lang));

    let theme = s
        .get("general")
        .and_then(|g| g.get("theme"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    if !matches!(theme, "light" | "dark" | "system") {
        set_number(&mut s, "general.theme", json!("light"));
    }

    let accent = s
        .get("general")
        .and_then(|g| g.get("accent"))
        .and_then(|t| t.as_str())
        .unwrap_or("");
    if crate::ui::ThemeAccent::from_key(accent).is_none() {
        set_number(&mut s, "general.accent", json!("mint"));
    }

    let text_color = s.get("general").and_then(|g| g.get("textColor"));
    let text_color_valid = text_color.is_some_and(|v| {
        v.as_str() == Some("auto") || valid_hex_color(v)
    });
    if !text_color_valid {
        set_number(&mut s, "general.textColor", json!("auto"));
    }

    let font_family = s
        .get("general")
        .and_then(|g| g.get("fontFamily"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .chars()
        .take(128)
        .collect::<String>();
    set_number(&mut s, "general.fontFamily", json!(font_family));

    let pairs = [
        ("general.fontSize", 10, 22),
        ("capture.delay", 0, 10),
        ("capture.historyLimit", 5, 500),
        ("pin.opacity", 20, 100),
        ("pin.scale", 25, 200),
        ("record.fps", 2, 15),
        ("record.maxSeconds", 1, 60),
        ("record.maxWidth", 320, 1920),
        ("long.maxFrames", 2, 80),
        ("long.intervalMs", 250, 3000),
        ("long.stickyTop", 0, 500),
        ("long.stickyBottom", 0, 500),
    ];
    for (path, min, max) in pairs {
        let clamped = clamp(get_path(&s, path), min, max);
        set_number(&mut s, path, clamped);
    }

    let default_annotations = defaults()
        .get("annotations")
        .and_then(|a| a.as_object())
        .cloned()
        .unwrap_or_default();
    if let Some(annotations) = s
        .get_mut("annotations")
        .and_then(|a| a.as_object_mut())
    {
        for (tool, default_tool) in &default_annotations {
            let Some(options) = annotations.get_mut(tool).and_then(|o| o.as_object_mut()) else {
                continue;
            };
            let color = options.get("color").cloned();
            if !valid_hex_color(color.as_ref().unwrap_or(&Value::Null)) {
                options.insert("color".into(), default_tool["color"].clone());
            }
            if default_tool.get("fillColor").is_some() {
                let valid_fill = options
                    .get("fillColor")
                    .is_some_and(|v| v.is_null() || valid_hex_color(v));
                if !valid_fill {
                    options.insert("fillColor".into(), Value::Null);
                }
            }
            for (name, min, max) in [("width", 1, 24), ("fontSize", 12, 96), ("opacity", 5, 100)] {
                let clamped = clamp(options.get(name), min, max);
                options.insert(name.to_string(), clamped);
            }
            let fonts = [
                "Microsoft YaHei",
                "SimSun",
                "SimHei",
                "Arial",
                "Consolas",
            ];
            let family = options.get("fontFamily").and_then(|f| f.as_str()).unwrap_or("");
            if !fonts.contains(&family) {
                options.insert("fontFamily".into(), default_tool["fontFamily"].clone());
            }
        }
    }

    let format = s
        .get("storage")
        .and_then(|st| st.get("format"))
        .and_then(|f| f.as_str())
        .unwrap_or("");
    if !matches!(format, "png" | "jpg") {
        set_number(&mut s, "storage.format", json!("png"));
    }
    let clamped = clamp(get_path(&s, "storage.quality"), 10, 100);
    set_number(&mut s, "storage.quality", clamped);

    if let Some(providers) = s.get_mut("providers").and_then(|p| p.as_object_mut()) {
        for (name, provider) in providers.iter_mut() {
            let Some(map) = provider.as_object_mut() else {
                continue;
            };
            let mode = map.get("mode").and_then(|m| m.as_str()).unwrap_or("");
            if name == "ocr" || name == "translation" {
                if !matches!(mode, "local" | "api" | "ai") {
                    map.insert("mode".into(), json!("local"));
                }
            } else if !matches!(mode, "api" | "ai") {
                map.insert("mode".into(), json!("api"));
            }
            if name == "ocr" {
                if let Some(local) = map.get_mut("local").and_then(|l| l.as_object_mut()) {
                    let mirror = local.get("mirror").and_then(|v| v.as_str()).unwrap_or("");
                    if !matches!(
                        mirror,
                        "modelscope-cn" | "modelscope-ai" | "hf-mirror" | "huggingface"
                    ) {
                        local.insert("mirror".into(), json!("modelscope-cn"));
                    }
                    let model = local.get("model").and_then(|v| v.as_str()).unwrap_or("");
                    if !model.is_empty() && crate::ocr::find_model_pack(model).is_none() {
                        local.insert("model".into(), json!(""));
                    }
                }
            } else if name == "translation" {
                if let Some(local) = map.get_mut("local").and_then(|l| l.as_object_mut()) {
                    let mirror = local.get("mirror").and_then(|v| v.as_str()).unwrap_or("");
                    if !matches!(mirror, "hf-mirror" | "huggingface") {
                        local.insert("mirror".into(), json!("hf-mirror"));
                    }
                }
            }
        }
    }
    s
}

/// Portable subset of preferences for export and cloud sync.
#[allow(dead_code)]
pub fn portable(settings: &Value) -> Value {
    let n = normalize(Some(settings));
    let mut out = Map::new();
    out.insert("schema".into(), json!(1));
    let mut general = Map::new();
    general.insert("language".into(), n["general"]["language"].clone());
    general.insert("theme".into(), n["general"]["theme"].clone());
    general.insert("accent".into(), n["general"]["accent"].clone());
    general.insert("textColor".into(), n["general"]["textColor"].clone());
    general.insert("fontFamily".into(), n["general"]["fontFamily"].clone());
    general.insert("fontSize".into(), n["general"]["fontSize"].clone());
    general.insert("fontBold".into(), n["general"]["fontBold"].clone());
    general.insert("fontItalic".into(), n["general"]["fontItalic"].clone());
    general.insert("closeToTray".into(), n["general"]["closeToTray"].clone());
    out.insert("general".into(), Value::Object(general));
    for key in [
        "capture",
        "shortcuts",
        "pin",
        "record",
        "long",
        "annotations",
        "providers",
    ] {
        out.insert(key.into(), n[key].clone());
    }
    Value::Object(out)
}

/// Apply remote synced settings (schema 1 only), preserving local service and storage paths.
#[allow(dead_code)]
pub fn apply_portable(local: &Value, remote: &Value) -> Result<Value, String> {
    if remote.get("schema").and_then(|s| s.as_i64()) != Some(1) {
        return Err(i18n::t("Unsupported remote configuration schema").into());
    }
    let accepted = portable(remote);
    let mut merged = local.clone();
    if let (Value::Object(base), Value::Object(patch)) = (&mut merged, &accepted) {
        for (key, value) in patch {
            if key == "general" {
                let mut combined = base.get("general").cloned().unwrap_or(Value::Null);
                if let (Value::Object(g), Value::Object(n)) = (&mut combined, value) {
                    for (k, v) in n {
                        g.insert(k.clone(), v.clone());
                    }
                }
                base.insert("general".into(), combined);
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    }
    let mut result = normalize(Some(&merged));
    if let Some(service) = local.get("service") {
        set_number(&mut result, "service", service.clone());
    }
    if let Some(storage) = local.get("storage") {
        set_number(&mut result, "storage", storage.clone());
    }
    Ok(result)
}

/// Sync conflict detection fingerprint: SHA-256 of the portable subset.
#[allow(dead_code)]
pub fn fingerprint(settings: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(portable(settings).to_string());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_defaults() {
        let s = normalize(None);
        assert_eq!(s, *defaults());
        assert_eq!(s["general"]["language"], json!("system"));
        assert_eq!(s["general"]["theme"], json!("light"));
        assert_eq!(s["general"]["fontFamily"], json!(""));
        assert_eq!(s["general"]["fontSize"], json!(13));
        assert_eq!(s["general"]["fontBold"], json!(false));
        assert_eq!(s["general"]["fontItalic"], json!(false));
    }

    #[test]
    fn unknown_fields_and_types_are_dropped() {
        let s = normalize(Some(&json!({
            "general": { "theme": "dark", "junk": 1 },
            "capture": { "delay": "5" },
            "extra": true
        })));
        assert_eq!(s["general"]["theme"], json!("dark"));
        assert!(s.get("extra").is_none());
        assert!(s["general"].get("junk").is_none());
        assert_eq!(s["capture"]["delay"], json!(0));
    }

    #[test]
    fn numbers_are_clamped() {
        let s = normalize(Some(&json!({
            "capture": { "delay": 99, "historyLimit": 1 },
            "pin": { "opacity": 3 },
            "record": { "fps": 0.4 },
            "long": { "maxFrames": 1000 }
        })));
        assert_eq!(s["capture"]["delay"], json!(10));
        assert_eq!(s["capture"]["historyLimit"], json!(5));
        assert_eq!(s["pin"]["opacity"], json!(20));
        assert_eq!(s["record"]["fps"], json!(2));
        assert_eq!(s["long"]["maxFrames"], json!(80));
    }

    #[test]
    fn annotation_options_fall_back_to_defaults() {
        let tool = defaults()["annotations"].as_object().unwrap().keys().next().unwrap().clone();
        let mut annotations = Map::new();
        annotations.insert(
            tool.clone(),
            json!({ "color": "red", "width": 99, "fontFamily": "Comic Sans", "fontSize": 1, "opacity": 1 }),
        );
        let s = normalize(Some(&json!({
            "annotations": Value::Object(annotations)
        })));
        let options = &s["annotations"][&tool];
        assert_eq!(options["color"], defaults()["annotations"][&tool]["color"]);
        assert_eq!(options["width"], json!(24));
        assert_eq!(options["fontFamily"], defaults()["annotations"][&tool]["fontFamily"]);
        assert_eq!(options["fontSize"], json!(12));
        assert_eq!(options["opacity"], json!(5));
    }

    #[test]
    fn provider_mode_is_constrained() {
        let s = normalize(Some(&json!({
            "providers": {
                "ocr": { "mode": "nope", "local": { "mirror": "bad", "model": "unknown-model" } },
                "translation": { "mode": "nope", "local": { "mirror": "bad" } }
            }
        })));
        assert_eq!(s["providers"]["ocr"]["mode"], json!("local"));
        assert_eq!(
            s["providers"]["ocr"]["local"]["mirror"],
            json!("modelscope-cn")
        );
        assert_eq!(s["providers"]["ocr"]["local"]["model"], json!(""));
        assert_eq!(s["providers"]["translation"]["mode"], json!("local"));
        assert_eq!(
            s["providers"]["translation"]["local"]["mirror"],
            json!("hf-mirror")
        );

        let s2 = normalize(Some(&json!({
            "providers": {
                "ocr": { "mode": "api", "local": { "mirror": "hf-mirror", "model": "ppocrv6-small" } },
                "translation": { "mode": "ai", "local": { "mirror": "huggingface" } }
            }
        })));
        assert_eq!(s2["providers"]["ocr"]["mode"], json!("api"));
        assert_eq!(
            s2["providers"]["ocr"]["local"]["mirror"],
            json!("hf-mirror")
        );
        assert_eq!(
            s2["providers"]["ocr"]["local"]["model"],
            json!("ppocrv6-small")
        );
        assert_eq!(s2["providers"]["translation"]["mode"], json!("ai"));
        assert_eq!(
            s2["providers"]["translation"]["local"]["mirror"],
            json!("huggingface")
        );
    }

    #[test]
    fn language_setting_is_validated_and_defaults_to_system() {
        assert_eq!(normalize(None)["general"]["language"], json!("system"));
        assert_eq!(
            normalize(Some(&json!({ "general": { "language": "system" } })))["general"]["language"],
            json!("system")
        );
        assert_eq!(
            normalize(Some(&json!({ "general": { "language": "en" } })))["general"]["language"],
            json!("en")
        );
        assert_eq!(
            normalize(Some(&json!({ "general": { "language": "zh-CN" } })))["general"]["language"],
            json!("zh-CN")
        );
        assert_eq!(
            normalize(Some(&json!({ "general": { "language": "zh" } })))["general"]["language"],
            json!("zh-CN")
        );
        assert_eq!(
            normalize(Some(&json!({ "general": { "language": "invalid" } })))["general"]["language"],
            json!("system")
        );
    }

    #[test]
    fn font_settings_are_normalized_and_preserved_in_portable() {
        let s = normalize(Some(&json!({
            "general": {
                "fontFamily": "  Microsoft YaHei  ",
                "fontSize": 99,
                "fontBold": true,
                "fontItalic": true
            }
        })));
        assert_eq!(s["general"]["fontFamily"], json!("Microsoft YaHei"));
        assert_eq!(s["general"]["fontSize"], json!(22));
        assert_eq!(s["general"]["fontBold"], json!(true));
        assert_eq!(s["general"]["fontItalic"], json!(true));

        let low = normalize(Some(&json!({
            "general": { "fontSize": 3 }
        })));
        assert_eq!(low["general"]["fontSize"], json!(10));

        let port = portable(&s);
        assert_eq!(port["general"]["fontFamily"], json!("Microsoft YaHei"));
        assert_eq!(port["general"]["fontSize"], json!(22));
        assert_eq!(port["general"]["fontBold"], json!(true));
        assert_eq!(port["general"]["fontItalic"], json!(true));
    }

    #[test]
    fn portable_roundtrip_keeps_service_and_storage() {
        let local = normalize(Some(&json!({
            "general": { "language": "zh-CN", "theme": "dark", "accent": "violet" },
            "service": { "url": "https://example.com" },
            "storage": { "directory": "C:/shots", "format": "jpg" }
        })));
        let remote = portable(&local);
        let applied = apply_portable(&local, &remote).unwrap();
        assert_eq!(applied["service"]["url"], json!("https://example.com"));
        assert_eq!(applied["storage"]["directory"], json!("C:/shots"));
        assert_eq!(applied["storage"]["format"], json!("jpg"));
        assert_eq!(applied["general"]["language"], json!("zh-CN"));
        assert_eq!(applied["general"]["theme"], json!("dark"));
        assert_eq!(applied["general"]["accent"], json!("violet"));
    }

    #[test]
    fn theme_accent_is_validated_and_falls_back_to_mint() {
        let valid = normalize(Some(&json!({
            "general": { "accent": "ocean" }
        })));
        assert_eq!(valid["general"]["accent"], json!("ocean"));

        let invalid = normalize(Some(&json!({
            "general": { "accent": "neon-rainbow" }
        })));
        assert_eq!(invalid["general"]["accent"], json!("mint"));
    }

    #[test]
    fn text_color_is_validated_and_falls_back_to_auto() {
        let auto = normalize(Some(&json!({
            "general": { "textColor": "auto" }
        })));
        assert_eq!(auto["general"]["textColor"], json!("auto"));

        let custom = normalize(Some(&json!({
            "general": { "textColor": "#1e3a5f" }
        })));
        assert_eq!(custom["general"]["textColor"], json!("#1e3a5f"));

        let invalid = normalize(Some(&json!({
            "general": { "textColor": "not-a-color" }
        })));
        assert_eq!(invalid["general"]["textColor"], json!("auto"));
    }

    #[test]
    fn apply_portable_rejects_unknown_schema() {
        let local = defaults().clone();
        let error = apply_portable(&local, &json!({ "schema": 2 })).unwrap_err();
        assert!(!error.is_empty());
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        let base = defaults().clone();
        let a = fingerprint(&base);
        assert_eq!(a, fingerprint(&base));
        let mut changed = base.clone();
        set_number(&mut changed, "pin.opacity", json!(80));
        assert_ne!(a, fingerprint(&changed));
    }
}
