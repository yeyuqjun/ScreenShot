//! Export: save parameter validation and file writing (pure Rust + native `rfd` dialogs).

use crate::i18n;
use serde_json::Value;
use std::io::Write;

#[derive(Clone, Debug)]
pub struct SaveOptions {
    pub directory: String,
    #[allow(dead_code)]
    pub file_name: String,
    pub format: String,
    pub quality: i64,
    pub file_path: String,
}

fn strip_image_extension(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    for ext in [".png", ".jpeg", ".jpg", ".gif"] {
        if lower.ends_with(ext) {
            return &name[..name.len() - ext.len()];
        }
    }
    name
}

fn has_invalid_chars(name: &str) -> bool {
    name.chars().any(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            || (c as u32) < 0x20
    })
}

fn is_reserved_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.split('.').next().unwrap_or("");
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5",
        "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
        "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    RESERVED.contains(&stem)
}

/// Validate and normalize save options.
pub fn save_options(input: &Value, animated: bool) -> Result<SaveOptions, String> {
    let directory_raw = input
        .get("directory")
        .and_then(|v| v.as_str())
        .ok_or_else(|| i18n::t("Please select a valid absolute save path"))?
        .trim()
        .to_string();
    let directory_path = std::path::Path::new(&directory_raw);
    if !directory_path.is_absolute() {
        return Err(i18n::t("Please select a valid absolute save path").into());
    }
    let file_name_raw = input
        .get("fileName")
        .and_then(|v| v.as_str())
        .ok_or_else(|| i18n::t("Please enter a filename"))?
        .trim();
    let file_name = strip_image_extension(file_name_raw);
    if file_name.is_empty()
        || file_name.len() > 180
        || has_invalid_chars(file_name)
        || file_name.ends_with('.')
        || file_name.ends_with(' ')
        || is_reserved_name(file_name)
    {
        return Err(i18n::t("Filename contains invalid characters or a reserved system name").into());
    }
    let format = input
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let allowed: &[&str] = if animated { &["gif"] } else { &["png", "jpg"] };
    if !allowed.contains(&format) {
        return Err(i18n::t("Unsupported image format").into());
    }
    let quality = input.get("quality").and_then(|v| v.as_i64());
    if quality.is_none_or(|q| !(10..=100).contains(&q)) {
        return Err(i18n::t("Image quality must be between 10 and 100").into());
    }
    let file_path = directory_path
        .join(format!("{file_name}.{format}"))
        .to_string_lossy()
        .to_string();
    Ok(SaveOptions {
        directory: directory_raw,
        file_name: file_name.to_string(),
        format: format.to_string(),
        quality: quality.unwrap(),
        file_path,
    })
}

/// Create a new file exclusively; prompt via native dialog if it already exists. Returns `false` if cancelled.
pub fn write_export(
    buffer: &[u8],
    options: &SaveOptions,
    parent: Option<&winit::window::Window>,
) -> Result<bool, String> {
    let target = std::path::Path::new(&options.file_path);
    if let Some(parent_dir) = target.parent() {
        std::fs::create_dir_all(parent_dir).map_err(|e| e.to_string())?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
    {
        Ok(mut file) => file.write_all(buffer).map_err(|e| e.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut dlg = rfd::MessageDialog::new()
                .set_title(i18n::t("Replace File"))
                .set_description(i18n::t("File already exists. Replace it?"))
                .set_buttons(rfd::MessageButtons::YesNo);
            if let Some(win) = parent {
                dlg = dlg.set_parent(win);
            }
            let answer = dlg.show();
            if answer != rfd::MessageDialogResult::Yes {
                return Ok(false);
            }
            std::fs::write(target, buffer).map_err(|e| e.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_directory() {
        let error = save_options(
            &serde_json::json!({ "directory": "shots", "fileName": "a", "format": "png", "quality": 90 }),
            false,
        )
        .unwrap_err();
        assert!(!error.is_empty());
    }

    #[test]
    fn rejects_reserved_and_invalid_names() {
        for name in ["con", "aux.png", "bad?name", "name<", "end."] {
            let result = save_options(
                &serde_json::json!({
                    "directory": "C:/shots",
                    "fileName": name,
                    "format": "png",
                    "quality": 90
                }),
                false,
            );
            assert!(result.is_err(), "{name} should be rejected");
        }
    }

    #[test]
    fn strips_extension_and_builds_path() {
        let options = save_options(
            &serde_json::json!({
                "directory": "C:/shots",
                "fileName": "photo.png",
                "format": "jpg",
                "quality": 90
            }),
            false,
        )
        .unwrap();
        assert_eq!(options.file_name, "photo");
        assert!(options.file_path.ends_with("photo.jpg"));
    }

    #[test]
    fn gif_only_when_animated() {
        assert!(save_options(
            &serde_json::json!({ "directory": "C:/shots", "fileName": "a", "format": "png", "quality": 90 }),
            true
        )
        .is_err());
        assert!(save_options(
            &serde_json::json!({ "directory": "C:/shots", "fileName": "a", "format": "gif", "quality": 90 }),
            true
        )
        .is_ok());
    }

    #[test]
    fn quality_bounds_enforced() {
        for quality in [
            serde_json::json!(9),
            serde_json::json!(101),
            serde_json::json!(3.5),
        ] {
            assert!(save_options(
                &serde_json::json!({ "directory": "C:/shots", "fileName": "a", "format": "png", "quality": quality }),
                false
            )
            .is_err());
        }
    }
}
