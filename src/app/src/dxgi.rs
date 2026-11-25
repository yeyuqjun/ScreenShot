//! Screen frame capture (fast SDR GDI DIB section + HDR-aware DXGI Desktop Duplication).
//! Outputs physical-resolution RGBA frames per monitor.

use crate::i18n;
use std::sync::OnceLock;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709, DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_MODE_ROTATION_IDENTITY,
    DXGI_MODE_ROTATION_UNSPECIFIED,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutput6,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTPUT_DESC,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors,
    GdiFlush, GetDC, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HDC, HMONITOR, MONITORINFO, SRCCOPY,
};

/// IEEE 754 half-precision float -> f32.
fn half_to_f32(half: u16) -> f32 {
    let sign = ((half >> 15) & 1) as u32;
    let exponent = ((half >> 10) & 0x1f) as u32;
    let mantissa = (half & 0x3ff) as u32;
    let bits = if exponent == 0 {
        if mantissa == 0 {
            sign << 31
        } else {
            let mut e = 1u32;
            let mut m = mantissa;
            while (m & 0x400) == 0 {
                m <<= 1;
                e += 1;
            }
            (sign << 31) | ((127 - 15 + 1 - e) << 23) | ((m & 0x3ff) << 13)
        }
    } else if exponent == 0x1f {
        (sign << 31) | 0x7f80_0000 | (mantissa << 13)
    } else {
        (sign << 31) | ((exponent + 127 - 15) << 23) | (mantissa << 13)
    };
    f32::from_bits(bits)
}

/// Convert linear scRGB component to 8-bit sRGB (for HDR desktop captures).
fn linear_to_srgb(value: f32) -> u8 {
    let c = value.clamp(0.0, 1.0);
    let srgb = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0 + 0.5) as u8
}

/// Precomputed 64 KiB lookup table mapping IEEE-754 FP16 bit patterns directly to sRGB u8.
fn fp16_srgb_lut() -> &'static [u8; 65536] {
    static LUT: OnceLock<Box<[u8; 65536]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut table = vec![0u8; 65536].into_boxed_slice();
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = linear_to_srgb(half_to_f32(i as u16));
        }
        table.try_into().expect("LUT length mismatch")
    })
}

/// Convert a row of FP16 pixels into tightly packed RGBA8 inside `dst` (must be `width * 4` bytes).
fn fp16_row_to_rgba_into(row: &[u8], dst: &mut [u8]) {
    let lut = fp16_srgb_lut();
    for (pixel, out) in row.chunks_exact(8).zip(dst.chunks_exact_mut(4)) {
        let r = lut[u16::from_le_bytes([pixel[0], pixel[1]]) as usize];
        let g = lut[u16::from_le_bytes([pixel[2], pixel[3]]) as usize];
        let b = lut[u16::from_le_bytes([pixel[4], pixel[5]]) as usize];
        out[0] = r;
        out[1] = g;
        out[2] = b;
        out[3] = 255;
    }
}

/// Convert a BGRA8 slice into opaque RGBA8 in a single streaming pass (auto-vectorizable).
#[inline]
fn bgra_to_rgba_slice(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = 255;
    }
}

/// Allocate an RGBA8 buffer from tightly-packed BGRA8 source pixels in a single pass without pre-zeroing.
fn bgra_to_rgba_vec(src: &[u8]) -> Vec<u8> {
    let len = src.len();
    let mut dst = Vec::with_capacity(len);
    // SAFETY: Every byte in `0..len` (a multiple of 4) is written immediately below.
    unsafe {
        let out = std::slice::from_raw_parts_mut(dst.as_mut_ptr(), len);
        bgra_to_rgba_slice(src, out);
        dst.set_len(len);
    }
    dst
}

/// Physical-resolution RGBA frame for a single monitor.
#[derive(Clone)]
pub struct ScreenFrame {
    pub width: u32,
    pub height: u32,
    /// RGBA pixels (tightly packed with stride `width * 4`).
    pub pixels: Vec<u8>,
    /// Output physical desktop coordinates aligned with monitor positions.
    pub rect: RECT,
}

/// Quickly sample whether a frame is all black (avoids uncomposited black first frames from DXGI).
fn is_all_black(rgba: &[u8]) -> bool {
    let pixel_count = rgba.len() / 4;
    if pixel_count == 0 {
        return true;
    }
    let step = (pixel_count / 256).max(1);
    for i in (0..pixel_count).step_by(step) {
        let p = i * 4;
        if rgba[p] != 0 || rgba[p + 1] != 0 || rgba[p + 2] != 0 {
            return false;
        }
    }
    true
}

/// Capture a physical desktop rectangle using a top-down GDI DIB section + single-pass BitBlt.
fn grab_rect_gdi(rect: RECT) -> Result<ScreenFrame, String> {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return Err(i18n::t("Invalid monitor region dimensions").into());
    }
    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() {
            return Err(i18n::t("Failed to acquire screen device context").into());
        }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        if mem_dc.is_invalid() {
            let _ = ReleaseDC(None, screen_dc);
            return Err(i18n::t("Failed to create memory device context").into());
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = match CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) if !bmp.is_invalid() && !bits.is_null() => bmp,
            _ => {
                let _ = DeleteDC(mem_dc);
                let _ = ReleaseDC(None, screen_dc);
                return Err(i18n::t("Failed to create screen bitmap").into());
            }
        };
        let old = SelectObject(mem_dc, dib.into());
        let blit_res = BitBlt(
            mem_dc,
            0,
            0,
            width,
            height,
            Some(screen_dc),
            rect.left,
            rect.top,
            SRCCOPY | CAPTUREBLT,
        );
        let _ = GdiFlush();
        let pixels = if blit_res.is_ok() {
            let byte_len = (width as usize) * (height as usize) * 4;
            let src = std::slice::from_raw_parts(bits as *const u8, byte_len);
            Some(bgra_to_rgba_vec(src))
        } else {
            None
        };
        SelectObject(mem_dc, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);

        let Some(pixels) = pixels else {
            return Err(i18n::t("GDI screen capture failed").into());
        };
        Ok(ScreenFrame {
            width: width as u32,
            height: height as u32,
            pixels,
            rect,
        })
    }
}

/// Enumerate all physical monitor rectangles (for GDI capture & startup warmup).
fn enum_monitor_rects() -> Vec<RECT> {
    let mut rects: Vec<RECT> = Vec::new();
    unsafe extern "system" fn callback(
        hmon: HMONITOR,
        _hdc: HDC,
        _lprect: *mut RECT,
        lparam: LPARAM,
    ) -> windows::core::BOOL {
        let list = unsafe { &mut *(lparam.0 as *mut Vec<RECT>) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(hmon, &mut info).as_bool() } {
            list.push(info.rcMonitor);
        }
        windows::core::BOOL::from(true)
    }
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(callback),
            LPARAM(&mut rects as *mut Vec<RECT> as isize),
        );
    }
    rects
}

/// Return current physical monitor geometries `(x, y, width, height)` sorted by position.
pub fn monitor_rects() -> Vec<(i32, i32, u32, u32)> {
    let mut rects = enum_monitor_rects();
    rects.sort_by_key(|r| (r.left, r.top));
    rects
        .into_iter()
        .filter_map(|r| {
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            if w > 0 && h > 0 {
                Some((r.left, r.top, w as u32, h as u32))
            } else {
                None
            }
        })
        .collect()
}

fn read_duplication_texture(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    resource: IDXGIResource,
) -> Result<(u32, u32, Vec<u8>), String> {
    unsafe {
        let texture: ID3D11Texture2D = resource.cast().map_err(|e| e.to_string())?;
        let mut texture_desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut texture_desc);
        let fp16 = texture_desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT;
        if !fp16 && texture_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(i18n::tf(
                "Unsupported display format: {fmt}",
                &[("fmt", &format!("{:?}", texture_desc.Format))],
            ));
        }
        let mut staging_desc = texture_desc;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.BindFlags = Default::default();
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        staging_desc.MiscFlags = 0;
        staging_desc.MipLevels = 1;
        let mut staging: Option<ID3D11Texture2D> = None;
        device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
            .map_err(|e| i18n::tf("Failed to create staging texture: {err}", &[("err", &e.to_string())]))?;
        let staging = staging.ok_or_else(|| i18n::t("Failed to create staging texture"))?;
        context.CopyResource(&staging, &texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| i18n::tf("Failed to read screen data: {err}", &[("err", &e.to_string())]))?;
        let width = texture_desc.Width as usize;
        let height = texture_desc.Height as usize;
        let row_pitch = mapped.RowPitch as usize;
        let src = std::slice::from_raw_parts(mapped.pData as *const u8, row_pitch * height);
        if std::env::var("SNAPDOCK_DXGI_DEBUG").is_ok() {
            let head: Vec<u16> = src[..32.min(src.len())]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            eprintln!(
                "[dxgi] fmt={:?} pitch={} head={:?}",
                texture_desc.Format, mapped.RowPitch, head
            );
        }
        let total_bytes = width * height * 4;
        let mut rgba = Vec::with_capacity(total_bytes);
        if fp16 {
            let dst_row_bytes = width * 4;
            let src_row_bytes = width * 8;
            let out = std::slice::from_raw_parts_mut(rgba.as_mut_ptr(), total_bytes);
            for (src_row, dst_row) in src
                .chunks_exact(row_pitch)
                .take(height)
                .zip(out.chunks_exact_mut(dst_row_bytes))
            {
                fp16_row_to_rgba_into(&src_row[..src_row_bytes], dst_row);
            }
            rgba.set_len(total_bytes);
        } else if row_pitch == width * 4 {
            let out = std::slice::from_raw_parts_mut(rgba.as_mut_ptr(), total_bytes);
            bgra_to_rgba_slice(&src[..total_bytes], out);
            rgba.set_len(total_bytes);
        } else {
            let dst_row_bytes = width * 4;
            let out = std::slice::from_raw_parts_mut(rgba.as_mut_ptr(), total_bytes);
            for (src_row, dst_row) in src
                .chunks_exact(row_pitch)
                .take(height)
                .zip(out.chunks_exact_mut(dst_row_bytes))
            {
                bgra_to_rgba_slice(&src_row[..dst_row_bytes], dst_row);
            }
            rgba.set_len(total_bytes);
        }
        context.Unmap(&staging, 0);
        Ok((width as u32, height as u32, rgba))
    }
}

struct Duplication {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
}

fn create_duplication(output: &IDXGIOutput) -> Result<Duplication, String> {
    let output1: IDXGIOutput1 = output
        .cast()
        .map_err(|e| i18n::tf("Display output does not support duplication: {err}", &[("err", &e.to_string())]))?;
    let parent_adapter: Option<IDXGIAdapter> = unsafe { output.GetParent().ok() };
    let create = |adapter: Option<&IDXGIAdapter>, driver: D3D_DRIVER_TYPE| unsafe {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        D3D11CreateDevice(
            adapter,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(D3D11_CREATE_DEVICE_BGRA_SUPPORT.0),
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map(|_| {
            (
                device.expect("Missing D3D device pointer"),
                context.expect("Missing D3D context pointer"),
            )
        })
        .map_err(|e| i18n::tf("Failed to create D3D device: {err}", &[("err", &e.to_string())]))
    };
    let (device, context) = if let Some(ref adapter) = parent_adapter {
        create(Some(adapter), D3D_DRIVER_TYPE_UNKNOWN)
            .or_else(|_| create(None, D3D_DRIVER_TYPE_HARDWARE))
            .or_else(|_| create(None, D3D_DRIVER_TYPE_WARP))?
    } else {
        create(None, D3D_DRIVER_TYPE_HARDWARE)
            .or_else(|_| create(None, D3D_DRIVER_TYPE_WARP))?
    };
    let duplication = unsafe { output1.DuplicateOutput(&device) }
        .map_err(|e| i18n::tf("Failed to duplicate display output: {err}", &[("err", &e.to_string())]))?;
    Ok(Duplication {
        device,
        context,
        duplication,
    })
}

/// Detect whether a DXGI output currently uses an HDR color space (HDR10 or scRGB).
fn is_output_hdr(output: &IDXGIOutput) -> bool {
    let Ok(output6) = output.cast::<IDXGIOutput6>() else {
        return false;
    };
    let Ok(desc1) = (unsafe { output6.GetDesc1() }) else {
        return false;
    };
    desc1.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
        || desc1.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709
}

fn grab_output_dxgi(output: &IDXGIOutput, rect: RECT) -> Result<ScreenFrame, String> {
    let Duplication {
        device,
        context,
        duplication,
    } = create_duplication(output)?;
    for _ in 0..4 {
        unsafe {
            let mut info = Default::default();
            let mut resource = None;
            match duplication.AcquireNextFrame(25, &mut info, &mut resource) {
                Ok(()) => {
                    let res = resource
                        .ok_or_else(|| i18n::t("Invalid duplicated frame resource").to_string())
                        .and_then(|r| {
                            let r: IDXGIResource = r.cast().map_err(|e| e.to_string())?;
                            read_duplication_texture(&device, &context, r)
                        });
                    let _ = duplication.ReleaseFrame();
                    let (width, height, pixels) = res?;
                    // Only retry if the first acquired frame is genuinely an uncomposited black frame.
                    if is_all_black(&pixels) {
                        continue;
                    }
                    return Ok(ScreenFrame {
                        width,
                        height,
                        pixels,
                        rect,
                    });
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
                Err(e) => {
                    return Err(i18n::tf(
                        "Screen duplication failed: {err}",
                        &[("err", &e.to_string())],
                    ))
                }
            }
        }
    }
    Err(i18n::t("Timed out while capturing screen image").into())
}

#[derive(Clone, Copy)]
struct MonitorTarget {
    rect: RECT,
    adapter_index: u32,
    output_index: u32,
    use_dxgi_first: bool,
}

fn grab_target_dxgi(adapter_index: u32, output_index: u32, rect: RECT) -> Result<ScreenFrame, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()
            .map_err(|e| i18n::tf("Failed to initialize DXGI: {err}", &[("err", &e.to_string())]))?;
        let adapter = factory
            .EnumAdapters1(adapter_index)
            .map_err(|e| e.to_string())?;
        let output = adapter
            .EnumOutputs(output_index)
            .map_err(|e| e.to_string())?;
        grab_output_dxgi(&output, rect)
    }
}

fn grab_monitor_target(target: MonitorTarget) -> Result<ScreenFrame, String> {
    if target.use_dxgi_first {
        // HDR display: prefer DXGI (FP16 scRGB -> sRGB LUT), fall back to GDI on error/timeout.
        match grab_target_dxgi(target.adapter_index, target.output_index, target.rect) {
            Ok(frame) => Ok(frame),
            Err(dxgi_err) => grab_rect_gdi(target.rect).map_err(|_| dxgi_err),
        }
    } else {
        // SDR display (or rotated display): instant GDI DIB-section capture (avoids D3D11 init & DXGI static-frame stalls).
        // Fall back to DXGI only if GDI fails or captures an all-black surface (e.g. exclusive fullscreen).
        match grab_rect_gdi(target.rect) {
            Ok(frame) if !is_all_black(&frame.pixels) => Ok(frame),
            gdi_res => grab_target_dxgi(target.adapter_index, target.output_index, target.rect)
                .or(gdi_res),
        }
    }
}

/// Capture all monitors in parallel, ordered by desktop coordinates.
pub fn grab_all_screens() -> Result<Vec<ScreenFrame>, String> {
    let mut targets: Vec<MonitorTarget> = Vec::new();
    let dxgi_enum: Result<(), String> = unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()
            .map_err(|e| i18n::tf("Failed to initialize DXGI: {err}", &[("err", &e.to_string())]))?;
        let mut adapter_index = 0u32;
        loop {
            let adapter = match factory.EnumAdapters1(adapter_index) {
                Ok(adapter) => adapter,
                Err(_) => break,
            };
            let mut output_index = 0u32;
            loop {
                let output = match adapter.EnumOutputs(output_index) {
                    Ok(output) => output,
                    Err(_) => break,
                };
                if let Ok(desc) = output.GetDesc() {
                    let r = desc.DesktopCoordinates;
                    if desc.AttachedToDesktop.as_bool() && r.right > r.left && r.bottom > r.top {
                        let rotated = desc.Rotation != DXGI_MODE_ROTATION_IDENTITY
                            && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED;
                        let use_dxgi_first = !rotated && is_output_hdr(&output);
                        targets.push(MonitorTarget {
                            rect: r,
                            adapter_index,
                            output_index,
                            use_dxgi_first,
                        });
                    }
                }
                output_index += 1;
            }
            adapter_index += 1;
        }
        Ok(())
    };

    let mut frames: Vec<ScreenFrame> = if !targets.is_empty() {
        if targets.len() == 1 {
            vec![grab_monitor_target(targets[0])?]
        } else {
            let results: Vec<Result<ScreenFrame, String>> = std::thread::scope(|scope| {
                let handles: Vec<_> = targets
                    .into_iter()
                    .map(|t| scope.spawn(move || grab_monitor_target(t)))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err(i18n::t("Screen capture worker failed").into()))
                    })
                    .collect()
            });
            let mut ok_frames = Vec::with_capacity(results.len());
            let mut first_err: Option<String> = None;
            for res in results {
                match res {
                    Ok(f) => ok_frames.push(f),
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                }
            }
            if ok_frames.is_empty() {
                return Err(first_err.unwrap_or_else(|| {
                    i18n::t(
                        "Unable to capture screen image; please check system screen capture permissions",
                    )
                    .into()
                }));
            }
            ok_frames
        }
    } else {
        let rects = enum_monitor_rects();
        let mut gdi_frames = Vec::new();
        for rect in rects {
            if let Ok(frame) = grab_rect_gdi(rect) {
                gdi_frames.push(frame);
            }
        }
        if gdi_frames.is_empty() {
            if let Err(e) = dxgi_enum {
                return Err(e);
            }
            return Err(i18n::t(
                "Unable to capture screen image; please check system screen capture permissions",
            )
            .into());
        }
        gdi_frames
    };

    frames.sort_by_key(|f| (f.rect.left, f.rect.top));
    Ok(frames)
}

/// Capture a single display output matched by desktop coordinates (for recording sampling).
#[cfg(windows)]
#[allow(dead_code)]
pub fn grab_screen_at(rect: &RECT) -> Result<ScreenFrame, String> {
    let frames = grab_all_screens()?;
    frames
        .into_iter()
        .find(|f| f.rect.left == rect.left && f.rect.top == rect.top)
        .ok_or_else(|| i18n::t("The monitor being recorded was disconnected").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_all_black_detects_black_and_non_black_frames() {
        assert!(is_all_black(&[]));
        assert!(is_all_black(&[0, 0, 0, 255, 0, 0, 0, 255]));
        assert!(!is_all_black(&[0, 0, 0, 255, 1, 0, 0, 255]));
    }

    #[test]
    fn bgra_to_rgba_swizzles_and_sets_opaque_alpha() {
        let src = [10u8, 20, 30, 0, 100, 150, 200, 128];
        let rgba = bgra_to_rgba_vec(&src);
        assert_eq!(rgba, vec![30, 20, 10, 255, 200, 150, 100, 255]);
    }

    #[test]
    fn fp16_lut_matches_exact_srgb_conversion() {
        let lut = fp16_srgb_lut();
        for sample in [0x0000u16, 0x3c00, 0x3800, 0x3400, 0x4000, 0xbc00] {
            assert_eq!(lut[sample as usize], linear_to_srgb(half_to_f32(sample)));
        }
        // One pixel: R=1.0 (0x3c00), G=0.5 (0x3800), B=0.0 (0x0000), A=1.0 (0x3c00)
        let row = [0x00, 0x3c, 0x00, 0x38, 0x00, 0x00, 0x00, 0x3c];
        let mut out = [0u8; 4];
        fp16_row_to_rgba_into(&row, &mut out);
        assert_eq!(out[0], 255);
        assert_eq!(out[1], linear_to_srgb(0.5));
        assert_eq!(out[2], 0);
        assert_eq!(out[3], 255);
    }

    #[test]
    #[ignore = "Requires an interactive Windows desktop session (non-sandboxed / non-headless)"]
    fn grab_all_screens_smoke() {
        match grab_all_screens() {
            Ok(frames) => {
                assert!(!frames.is_empty());
                for frame in &frames {
                    assert!(frame.width > 0 && frame.height > 0);
                    assert_eq!(frame.pixels.len(), (frame.width * frame.height * 4) as usize);
                    let sample_count = 1000usize;
                    let step = (frame.pixels.len() / 4 / sample_count).max(1);
                    let mut sum = 0u64;
                    let mut max = 0u8;
                    for i in (0..(frame.pixels.len() / 4)).step_by(step) {
                        let p = i * 4;
                        let luminance = ((frame.pixels[p] as u32)
                            + frame.pixels[p + 1] as u32
                            + frame.pixels[p + 2] as u32)
                            / 3;
                        sum += luminance as u64;
                        max = max.max(frame.pixels[p]);
                    }
                    eprintln!(
                        "[dxgi] {}x{} mean_luma={} max={}",
                        frame.width,
                        frame.height,
                        sum / sample_count as u64,
                        max
                    );
                }
            }
            Err(error) => panic!("Screen capture failed: {error}"),
        }
    }
}

/// Reusable frame grabber: preserves duplication and the last valid frame across samples so static screens do not time out.
pub struct ScreenGrabber {
    monitor_rect: RECT,
    grabber: Option<Duplication>,
    output: Option<IDXGIOutput>,
    last_frame: Option<ScreenFrame>,
}

impl ScreenGrabber {
    pub fn monitor_origin(&self) -> (i32, i32) {
        (self.monitor_rect.left, self.monitor_rect.top)
    }

    pub fn new(monitor_rect: RECT) -> Result<Self, String> {
        let output = find_output(&monitor_rect).ok().filter(|o| {
            unsafe { o.GetDesc() }.ok().is_some_and(|d| {
                d.Rotation == DXGI_MODE_ROTATION_IDENTITY
                    || d.Rotation == DXGI_MODE_ROTATION_UNSPECIFIED
            })
        });
        Ok(Self {
            monitor_rect,
            grabber: None,
            output,
            last_frame: None,
        })
    }

    /// Capture one full-screen frame; rebuilds duplication on loss, reuses last frame when static, and falls back to GDI if unavailable.
    pub fn grab(&mut self) -> Result<ScreenFrame, String> {
        let Some(output) = self.output.clone() else {
            return grab_rect_gdi(self.monitor_rect);
        };
        if self.grabber.is_none() {
            match create_duplication(&output) {
                Ok(dup) => self.grabber = Some(dup),
                Err(_) => return grab_rect_gdi(self.monitor_rect),
            }
        }
        match grab_output_with(
            self.grabber.as_ref().unwrap(),
            self.monitor_rect,
            self.last_frame.is_some(),
        ) {
            Ok(Some(frame)) => {
                self.last_frame = Some(frame.clone());
                Ok(frame)
            }
            Ok(None) => {
                if let Some(prev) = &self.last_frame {
                    Ok(prev.clone())
                } else {
                    grab_rect_gdi(self.monitor_rect)
                }
            }
            Err(_) => {
                // Rebuild once on errors such as ACCESS_LOST, then fall back to GDI
                self.grabber = create_duplication(&output).ok();
                if let Some(dup) = self.grabber.as_ref() {
                    if let Ok(Some(frame)) = grab_output_with(dup, self.monitor_rect, false) {
                        self.last_frame = Some(frame.clone());
                        return Ok(frame);
                    }
                }
                grab_rect_gdi(self.monitor_rect)
            }
        }
    }
}

fn find_output(monitor_rect: &RECT) -> Result<IDXGIOutput, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()
            .map_err(|e| i18n::tf("Failed to initialize DXGI: {err}", &[("err", &e.to_string())]))?;
        let mut adapter_index = 0;
        loop {
            let adapter = match factory.EnumAdapters1(adapter_index) {
                Ok(adapter) => adapter,
                Err(_) => break,
            };
            let mut output_index = 0;
            loop {
                let output = match adapter.EnumOutputs(output_index) {
                    Ok(output) => output,
                    Err(_) => break,
                };
                let desc: DXGI_OUTPUT_DESC = output.GetDesc().map_err(|e| e.to_string())?;
                if desc.DesktopCoordinates.left == monitor_rect.left
                    && desc.DesktopCoordinates.top == monitor_rect.top
                {
                    return Ok(output);
                }
                output_index += 1;
            }
            adapter_index += 1;
        }
        Err(i18n::t("The monitor being recorded was disconnected").into())
    }
}

/// Capture one frame with an existing duplication; returns `Ok(None)` when `allow_unchanged` is true and the frame did not change.
fn grab_output_with(
    duplication: &Duplication,
    monitor_rect: RECT,
    allow_unchanged: bool,
) -> Result<Option<ScreenFrame>, String> {
    let retries = if allow_unchanged { 1 } else { 10 };
    let timeout_ms = if allow_unchanged { 40 } else { 60 };
    for _ in 0..retries {
        unsafe {
            let mut info = Default::default();
            let mut resource = None;
            match duplication
                .duplication
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
            {
                Ok(()) => {
                    let res = resource
                        .ok_or_else(|| i18n::t("Invalid duplicated frame resource").to_string())
                        .and_then(|r| {
                            let r: IDXGIResource = r.cast().map_err(|e| e.to_string())?;
                            read_duplication_texture(&duplication.device, &duplication.context, r)
                        });
                    let _ = duplication.duplication.ReleaseFrame();
                    let (width, height, pixels) = res?;
                    if !allow_unchanged && info.LastPresentTime == 0 && is_all_black(&pixels) {
                        continue;
                    }
                    return Ok(Some(ScreenFrame {
                        width,
                        height,
                        pixels,
                        rect: monitor_rect,
                    }));
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    if allow_unchanged {
                        return Ok(None);
                    }
                    continue;
                }
                Err(e) => return Err(i18n::tf("Screen duplication failed: {err}", &[("err", &e.to_string())])),
            }
        }
    }
    Ok(None)
}
