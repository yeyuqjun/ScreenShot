//! GIF encoding (image/gif crate), replacing legacy gif-worker.cjs.

use crate::i18n;
use crate::images::{decode_png, Rgba};

/// Encode a sequence of PNG frames into a looping GIF; delays are in milliseconds.
pub fn encode(frames: &[Vec<u8>], delays: &[u32]) -> Result<Vec<u8>, String> {
    if frames.is_empty() {
        return Err(i18n::t("No animated frames captured yet").into());
    }
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(&mut cursor, 10);
    encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .map_err(|e| i18n::tf("GIF encoding failed: {err}", &[("err", &e.to_string())]))?;
    for (index, input) in frames.iter().enumerate() {
        let frame: Rgba = decode_png(input)?;
        let delay_ms = delays.get(index).copied().unwrap_or(125);
        let buffer = image::RgbaImage::from_raw(frame.width, frame.height, frame.pixels.clone())
            .expect("Frame buffer size mismatch");
        let frame_out = image::Frame::from_parts(
            buffer,
            0,
            0,
            image::Delay::from_numer_denom_ms(delay_ms.max(10), 1),
        );
        encoder
            .encode_frame(frame_out)
            .map_err(|e| i18n::tf("GIF encoding failed: {err}", &[("err", &e.to_string())]))?;
    }
    drop(encoder);
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_png(color: [u8; 3]) -> Vec<u8> {
        let image = Rgba::new(8, 8, {
            let mut pixels = Vec::new();
            for _ in 0..64 {
                pixels.extend_from_slice(&[color[0], color[1], color[2], 255]);
            }
            pixels
        });
        crate::images::encode_png(&image).unwrap()
    }

    #[test]
    fn encodes_two_frame_gif() {
        let frames = vec![frame_png([255, 0, 0]), frame_png([0, 255, 0])];
        let bytes = encode(&frames, &[100, 100]).unwrap();
        assert!(bytes.len() > 24);
        assert_eq!(&bytes[..3], b"GIF");
    }

    #[test]
    fn empty_frames_rejected() {
        assert!(encode(&[], &[]).is_err());
    }
}
