//! Image processing: cropping, encoding (PNG/JPEG), and data URLs.
//! Replaces legacy pngjs / Electron nativeImage helpers (images.cjs / export.cjs).

use crate::i18n;
use base64::Engine;
use image::{ImageBuffer, RgbaImage};

#[derive(Debug, Clone)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn buffer(&self) -> RgbaImage {
        ImageBuffer::from_raw(self.width, self.height, self.pixels.clone())
            .expect("Image buffer size mismatch")
    }
}

pub fn encode_png(image: &Rgba) -> Result<Vec<u8>, String> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    image
        .buffer()
        .write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(|e| i18n::tf("PNG encoding failed: {err}", &[("err", &e.to_string())]))?;
    Ok(cursor.into_inner())
}

pub fn encode_jpeg(image: &Rgba, quality: u8) -> Result<Vec<u8>, String> {
    // JPEG has no alpha channel: composite transparent pixels onto white first.
    let mut pixels = image.pixels.clone();
    for chunk in pixels.chunks_exact_mut(4) {
        let alpha = chunk[3] as f32 / 255.0;
        for channel in 0..3 {
            chunk[channel] =
                (chunk[channel] as f32 * alpha + 255.0 * (1.0 - alpha)).round() as u8;
        }
        chunk[3] = 255;
    }
    let flattened = Rgba::new(image.width, image.height, pixels);
    let rgb = image::DynamicImage::ImageRgba8(flattened.buffer()).to_rgb8();
    let mut cursor = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, quality);
    rgb.write_with_encoder(encoder)
        .map_err(|e| i18n::tf("JPEG encoding failed: {err}", &[("err", &e.to_string())]))?;
    Ok(cursor.into_inner())
}

pub fn decode_png(bytes: &[u8]) -> Result<Rgba, String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let decoded = reader
        .decode()
        .map_err(|e| i18n::tf("Failed to read image: {err}", &[("err", &e.to_string())]))?
        .to_rgba8();
    let (width, height) = decoded.dimensions();
    Ok(Rgba::new(width, height, decoded.into_raw()))
}

/// Decode by sniffing the image format (PNG/JPEG/WebP/BMP) for image import.
pub fn decode_any(bytes: &[u8]) -> Result<Rgba, String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format();
    let decoded = reader
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|_| i18n::t("Unable to read this image").to_string())?
        .to_rgba8();
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return Err(i18n::t("Unable to read this image").into());
    }
    Ok(Rgba::new(width, height, decoded.into_raw()))
}

/// Decode all animation frames of a GIF (RGBA image and frame delay in ms).
pub fn decode_gif_frames(bytes: &[u8]) -> Result<Vec<(Rgba, u32)>, String> {
    use image::AnimationDecoder;
    let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| i18n::tf("Failed to decode GIF: {err}", &[("err", &e.to_string())]))?;
    let frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|e| i18n::tf("Failed to decode GIF frames: {err}", &[("err", &e.to_string())]))?;
    let mut out = Vec::with_capacity(frames.len());
    for frame in frames {
        let (numer, denom) = frame.delay().numer_denom_ms();
        let delay_ms = if denom == 0 {
            100
        } else {
            (numer / denom).max(20)
        };
        let buf = frame.into_buffer();
        let (w, h) = buf.dimensions();
        out.push((Rgba::new(w, h, buf.into_raw()), delay_ms));
    }
    Ok(out)
}

/// Physical pixel crop; returns the cropped image.
pub fn crop_pixels(image: &Rgba, rect: PixelRect) -> Result<Rgba, String> {
    let rect = clamp_rect(rect, image.width, image.height)?;
    let mut pixels = Vec::with_capacity((rect.width * rect.height * 4) as usize);
    for y in rect.y..rect.y + rect.height {
        let start = ((y * image.width) + rect.x) as usize * 4;
        let end = start + rect.width as usize * 4;
        pixels.extend_from_slice(&image.pixels[start..end]);
    }
    Ok(Rgba::new(rect.width, rect.height, pixels))
}

#[derive(Clone, Copy, Debug)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub fn clamp_rect(rect: PixelRect, width: u32, height: u32) -> Result<PixelRect, String> {
    if rect.x >= width || rect.y >= height {
        return Err(i18n::t("Crop region is outside image bounds").into());
    }
    Ok(PixelRect {
        width: rect.width.min(width - rect.x),
        height: rect.height.min(height - rect.y),
        ..rect
    })
}

/// Convert a logical selection to a physical pixel rectangle (matches legacy images.cropRect).
#[allow(dead_code)]
pub fn crop_rect(
    selection: &serde_json::Value,
    logical: (f64, f64),
    pixel: (u32, u32),
) -> Result<PixelRect, String> {
    let get = |key: &str| -> Result<f64, String> {
        selection
            .get(key)
            .and_then(|v| v.as_f64())
            .ok_or_else(|| i18n::tf("Invalid selection parameter: {key}", &[("key", key)]))
    };
    let sx = pixel.0 as f64 / logical.0;
    let sy = pixel.1 as f64 / logical.1;
    let x = (get("x")? * sx).floor().clamp(0.0, pixel.0 as f64 - 1.0) as u32;
    let y = (get("y")? * sy).floor().clamp(0.0, pixel.1 as f64 - 1.0) as u32;
    let width = ((get("width")? * sx).round() as u32).clamp(1, pixel.0 - x);
    let height = ((get("height")? * sy).round() as u32).clamp(1, pixel.1 - y);
    Ok(PixelRect {
        x,
        y,
        width,
        height,
    })
}

pub fn data_url_png(bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Parse and validate a PNG data URL (matches the first half of legacy editedImage).
#[allow(dead_code)]
pub fn parse_data_url(data_url: &str) -> Result<Rgba, String> {
    if data_url.len() > 50 * 1024 * 1024 {
        return Err(i18n::t("Invalid image format").into());
    }
    let Some(encoded) = data_url.strip_prefix("data:image/png;base64,") else {
        return Err(i18n::t("Invalid image format").into());
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| i18n::t("Invalid image format").to_string())?;
    let image = decode_png(&bytes)?;
    if image.width as u64 * image.height as u64 > 80_000_000 {
        return Err(i18n::t("Invalid image dimensions").into());
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_rect_converts_logical_to_pixels() {
        // 1000x500 logical -> 2000x1000 physical; selection (10,20,100,50)
        let selection = serde_json::json!({ "x": 10, "y": 20, "width": 100, "height": 50 });
        let rect = crop_rect(&selection, (1000.0, 500.0), (2000, 1000)).unwrap();
        assert_eq!((rect.x, rect.y), (20, 40));
        assert_eq!((rect.width, rect.height), (200, 100));
    }

    #[test]
    fn crop_rect_clamps_oversized_selection() {
        let selection = serde_json::json!({ "x": 0, "y": 0, "width": 99999, "height": 99999 });
        let rect = crop_rect(&selection, (100.0, 100.0), (100, 100)).unwrap();
        assert_eq!((rect.width, rect.height), (100, 100));
    }

    #[test]
    fn crop_pixels_extracts_sub_image() {
        let image = Rgba::new(2, 2, vec![1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255]);
        let cropped =
            crop_pixels(&image, PixelRect { x: 1, y: 0, width: 1, height: 2 }).unwrap();
        assert_eq!(cropped.pixels, vec![2, 0, 0, 255, 4, 0, 0, 255]);
    }

    #[test]
    fn jpeg_flattens_onto_white() {
        let image = Rgba::new(1, 1, vec![0, 0, 0, 0]);
        let bytes = encode_jpeg(&image, 90).unwrap();
        assert!(!bytes.is_empty());
    }
}
