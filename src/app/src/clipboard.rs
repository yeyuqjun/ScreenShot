//! Clipboard image and text read/write (arboard), replacing legacy clipboard.cjs.

use crate::i18n;
use crate::images::Rgba;

pub fn write_image(image: &Rgba) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard
        .set_image(arboard::ImageData {
            width: image.width as usize,
            height: image.height as usize,
            bytes: std::borrow::Cow::Borrowed(&image.pixels),
        })
        .map_err(|e| e.to_string())
}

pub fn write_text(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text).map_err(|e| e.to_string())
}

/// Read an image from the clipboard and convert it to RGBA.
pub fn read_image() -> Result<Rgba, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let data = clipboard.get_image().map_err(|e| e.to_string())?;
    let width = data.width as u32;
    let height = data.height as u32;
    if width == 0 || height == 0 {
        return Err(i18n::t("No image in clipboard").into());
    }
    Ok(Rgba::new(width, height, data.bytes.into_owned()))
}
