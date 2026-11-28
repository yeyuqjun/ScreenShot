//! Win32 window utilities: enumeration, hit-testing, child controls, scroll injection, opacity, click-through, capture exclusion, single-instance lock, and hollow scrolling border window.

use crate::i18n;
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, COLORREF, ERROR_ALREADY_EXISTS, ERROR_NO_MORE_ITEMS, HANDLE, HWND,
    LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_CLOAKED, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject,
    EndPaint, FillRect, GetMonitorInfoW, MonitorFromPoint, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumValueW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ, REG_EXPAND_SZ, REG_SZ, RRF_RT_REG_DWORD,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    mouse_event, SetActiveWindow, MOUSEEVENTF_WHEEL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EnumChildWindows, EnumWindows, FindWindowW, GetAncestor, GetCursorPos, GetForegroundWindow,
    GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, PostMessageW, PostQuitMessage, RegisterClassW, SetCursorPos,
    SetForegroundWindow, SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, UpdateLayeredWindow, WindowFromPoint, GA_ROOT,
    GWL_EXSTYLE, HWND_TOPMOST, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, LWA_COLORKEY, MSG,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    SW_HIDE, SW_SHOWNOACTIVATE,
    ULW_ALPHA, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_APP, WM_DESTROY, WM_PAINT, WNDCLASSW,
    WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};

/// Physical-pixel window rectangle and title.
#[derive(Clone, Debug)]
pub struct NativeWindow {
    pub hwnd: isize,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl NativeWindow {
    pub fn hwnd_handle(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    #[allow(dead_code)]
    pub fn rect_value(&self) -> serde_json::Value {
        json!({
            "hwnd": self.hwnd,
            "title": self.title,
            "x": self.x,
            "y": self.y,
            "width": self.width,
            "height": self.height,
        })
    }
}

fn window_rect(hwnd: HWND) -> Option<RECT> {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some(rect)
    }
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let mut buffer = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buffer);
        String::from_utf16_lossy(&buffer[..len.max(0) as usize])
    }
}

fn is_cloaked(hwnd: HWND) -> bool {
    unsafe {
        let mut cloaked: u32 = 0;
        let result = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        result.is_ok() && cloaked != 0
    }
}

fn own_process_window(hwnd: HWND) -> bool {
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == std::process::id()
    }
}

fn collect_top_windows() -> Vec<HWND> {
    let mut handles: Vec<HWND> = Vec::new();
    unsafe extern "system" fn bridged(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
        list.push(hwnd);
        windows::core::BOOL::from(true)
    }
    unsafe {
        let _ = EnumWindows(
            Some(bridged),
            LPARAM(&mut handles as *mut Vec<HWND> as isize),
        );
    }
    handles
}

fn collect_child_windows(parent: HWND) -> Vec<HWND> {
    let mut handles: Vec<HWND> = Vec::new();
    unsafe extern "system" fn bridged(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
        list.push(hwnd);
        windows::core::BOOL::from(true)
    }
    unsafe {
        let _ = EnumChildWindows(
            Some(parent),
            Some(bridged),
            LPARAM(&mut handles as *mut Vec<HWND> as isize),
        );
    }
    handles
}

fn visible_candidate(hwnd: HWND) -> Option<(RECT, String)> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
    }
    if own_process_window(hwnd) || is_cloaked(hwnd) {
        return None;
    }
    let rect = window_rect(hwnd)?;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width < 25 || height < 25 {
        return None;
    }
    let title = window_title(hwnd);
    if title.is_empty() {
        return None;
    }
    Some((rect, title))
}

/// Visible, non-minimized, non-cloaked, non-self top-level windows with titles and sufficient size.
pub fn list_windows() -> Vec<NativeWindow> {
    collect_top_windows()
        .into_iter()
        .filter_map(|hwnd| {
            let (rect, title) = visible_candidate(hwnd)?;
            Some(NativeWindow {
                hwnd: hwnd.0 as isize,
                title,
                x: rect.left,
                y: rect.top,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
            })
        })
        .collect()
}

/// Top-level window hit test in physical coordinates.
pub fn window_at(x: i32, y: i32) -> Option<NativeWindow> {
    unsafe {
        let point = POINT { x, y };
        let hwnd = WindowFromPoint(point);
        if hwnd.is_invalid() {
            return None;
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        let rect = window_rect(root)?;
        Some(NativeWindow {
            hwnd: root.0 as isize,
            title: window_title(root),
            x: rect.left,
            y: rect.top,
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
        })
    }
}

/// Smallest visible child control within the target window.
pub fn child_at(parent: &NativeWindow, x: i32, y: i32) -> Option<NativeWindow> {
    let mut found: Vec<(i64, NativeWindow)> = Vec::new();
    for hwnd in collect_child_windows(parent.hwnd_handle()) {
        if !unsafe { IsWindowVisible(hwnd).as_bool() } {
            continue;
        }
        let Some(rect) = window_rect(hwnd) else { continue };
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 3 || height <= 3 {
            continue;
        }
        if x >= rect.left && y >= rect.top && x < rect.right && y < rect.bottom {
            found.push((
                (width as i64) * (height as i64),
                NativeWindow {
                    hwnd: hwnd.0 as isize,
                    title: i18n::t("Window Control").into(),
                    x: rect.left,
                    y: rect.top,
                    width,
                    height,
                },
            ));
        }
    }
    found.sort_by_key(|(area, _)| *area);
    found.into_iter().next().map(|(_, window)| window)
}

/// Inject a mouse wheel scroll event at the specified window position.
pub fn scroll_at(x: i32, y: i32, delta: i32, hwnd: isize) -> Result<(), String> {
    if hwnd == 0 {
        return Err(i18n::t("No scrollable target window found").into());
    }
    let target = HWND(hwnd as *mut _);
    unsafe {
        let _ = SetForegroundWindow(target);
        let hit = window_at(x, y).ok_or_else(|| {
            i18n::t("Capture area is covered by another window; please return to the target window to continue").to_string()
        })?;
        if hit.hwnd != hwnd {
            return Err(i18n::t(
                "Capture area is covered by another window; please return to the target window to continue",
            ).into());
        }
        if delta == 0 {
            return Ok(());
        }
        let mut cursor = POINT::default();
        let has_cursor = GetCursorPos(&mut cursor).is_ok();
        if SetCursorPos(x, y).is_err() {
            return Err(i18n::t("Unable to position cursor over the scroll area").into());
        }
        mouse_event(MOUSEEVENTF_WHEEL, 0, 0, delta, 0);
        if has_cursor {
            let _ = SetCursorPos(cursor.x, cursor.y);
        }
    }
    Ok(())
}

/// Path to the `snapdock-inspect` helper process.
pub fn inspect_helper() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    if cfg!(debug_assertions) {
        let candidate =
            exe_dir.join("../../../src/server/target/debug/snapdock-inspect.exe");
        if candidate.exists() {
            return candidate;
        }
        let runtime_candidate = exe_dir.join("../../.runtime/native/snapdock-inspect.exe");
        if runtime_candidate.exists() {
            return runtime_candidate;
        }
    }
    exe_dir.join("resources/native/snapdock-inspect.exe")
}

/// Invoke the UIA helper process to identify the smallest control under the cursor (1400ms timeout).
pub fn inspect_control(
    parent: &NativeWindow,
    x: i32,
    y: i32,
) -> Option<serde_json::Value> {
    let helper = inspect_helper();
    if !helper.exists() {
        return None;
    }
    let mut child = std::process::Command::new(&helper)
        .args([parent.hwnd.to_string(), x.to_string(), y.to_string()])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1400);
    let stdout = loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut buffer = Vec::new();
                let _ = child.stdout.take()?.read_to_end(&mut buffer);
                break buffer;
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    };
    let text = String::from_utf8_lossy(&stdout).to_string();
    serde_json::from_str(&text).ok()
}

/// Current cursor physical screen coordinates.
pub fn cursor_pos() -> (i32, i32) {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    (point.x, point.y)
}

/// Work area `(x, y, width, height)` of the monitor containing the specified physical point.
pub fn work_area_at_point(x: i32, y: i32) -> (i32, i32, i32, i32) {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let rc = info.rcWork;
            return (rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top);
        }
    }
    (0, 0, 1920, 1080)
}

/// Physical screen rectangle `(x, y, width, height)` for a window handle.
pub fn window_rect_physical(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    let rc = window_rect(HWND(hwnd as *mut _))?;
    Some((rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top))
}

/// Detect whether Windows is currently configured for dark mode.
pub fn system_prefers_dark() -> bool {
    unsafe {
        let mut value: u32 = 1;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        );
        status.is_ok() && value == 0
    }
}

/// Set overall window opacity (`WS_EX_LAYERED` + `LWA_ALPHA`).
#[allow(dead_code)]
pub fn set_window_opacity(hwnd: isize, alpha: u8) -> Result<(), String> {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let style = GetWindowLongPtrW(handle, GWL_EXSTYLE) as u32;
        SetWindowLongPtrW(handle, GWL_EXSTYLE, (style | WS_EX_LAYERED.0) as isize);
        SetLayeredWindowAttributes(
            handle,
            COLORREF(0),
            alpha,
            LAYERED_WINDOW_ATTRIBUTES_FLAGS(LWA_ALPHA.0),
        )
        .map_err(|e| e.to_string())
    }
}

/// Enable `WS_EX_LAYERED` for per-pixel alpha presentation via `UpdateLayeredWindow`
/// (must not be combined with `SetLayeredWindowAttributes`).
pub fn enable_window_layered(hwnd: isize) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let style = GetWindowLongPtrW(handle, GWL_EXSTYLE) as u32;
        if (style & WS_EX_LAYERED.0) == 0 {
            SetWindowLongPtrW(handle, GWL_EXSTYLE, (style | WS_EX_LAYERED.0) as isize);
        }
    }
}

/// Reusable top-down 32-bit premultiplied BGRA DIB section + memory DC for `UpdateLayeredWindow`.
pub struct LayeredDib {
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_obj: HGDIOBJ,
    bits: *mut u8,
    width: u32,
    height: u32,
}

impl LayeredDib {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let width = width.max(1);
        let height = height.max(1);
        unsafe {
            let mem_dc = CreateCompatibleDC(None);
            if mem_dc.is_invalid() {
                return Err("CreateCompatibleDC failed".to_string());
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // top-down DIB matching wgpu row order
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(
                Some(mem_dc),
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            ) {
                Ok(bmp) if !bmp.is_invalid() && !bits.is_null() => bmp,
                Ok(bmp) => {
                    if !bmp.is_invalid() {
                        let _ = DeleteObject(bmp.into());
                    }
                    let _ = DeleteDC(mem_dc);
                    return Err("CreateDIBSection returned null pixel buffer".to_string());
                }
                Err(e) => {
                    let _ = DeleteDC(mem_dc);
                    return Err(format!("CreateDIBSection failed: {}", e));
                }
            };
            let old_obj = SelectObject(mem_dc, bitmap.into());
            Ok(Self {
                mem_dc,
                bitmap,
                old_obj,
                bits: bits as *mut u8,
                width,
                height,
            })
        }
    }

    /// Copy tightly- or 256B-aligned premultiplied BGRA8 rows into the DIB buffer and clamp
    /// RGB <= A on translucent pixels to satisfy Win32 `AC_SRC_ALPHA` invariants.
    pub fn copy_from_padded_bgra(&mut self, src: &[u8], padded_bytes_per_row: usize) {
        let unpadded = (self.width as usize) * 4;
        if padded_bytes_per_row < unpadded {
            return;
        }
        let rows = (self.height as usize).min(src.len() / padded_bytes_per_row);
        for y in 0..rows {
            let src_start = y * padded_bytes_per_row;
            let src_row = &src[src_start..src_start + unpadded];
            let dst_row = unsafe {
                std::slice::from_raw_parts_mut(self.bits.add(y * unpadded), unpadded)
            };
            dst_row.copy_from_slice(src_row);
            for px in dst_row.chunks_exact_mut(4) {
                let a = px[3];
                if a < 255 {
                    if px[0] > a {
                        px[0] = a;
                    }
                    if px[1] > a {
                        px[1] = a;
                    }
                    if px[2] > a {
                        px[2] = a;
                    }
                }
            }
        }
    }

    /// Present the current premultiplied DIB to a `WS_EX_LAYERED` window with global `opacity`.
    pub fn present(&self, hwnd: isize, opacity: u8) -> Result<(), String> {
        let size = SIZE {
            cx: self.width as i32,
            cy: self.height as i32,
        };
        let src_pt = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: opacity,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                HWND(hwnd as *mut _),
                None,
                None,
                Some(&size),
                Some(self.mem_dc),
                Some(&src_pt),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .map_err(|e| e.to_string())
        }
    }
}

impl Drop for LayeredDib {
    fn drop(&mut self) {
        unsafe {
            if !self.mem_dc.is_invalid() {
                if !self.old_obj.is_invalid() {
                    SelectObject(self.mem_dc, self.old_obj);
                }
                if !self.bitmap.is_invalid() {
                    let _ = DeleteObject(self.bitmap.into());
                }
                let _ = DeleteDC(self.mem_dc);
            }
        }
    }
}

/// Toggle window mouse click-through (`WS_EX_TRANSPARENT`).
pub fn set_window_click_through(hwnd: isize, enabled: bool) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let mut style = GetWindowLongPtrW(handle, GWL_EXSTYLE) as u32;
        if enabled {
            style |= WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0;
        } else {
            style &= !WS_EX_TRANSPARENT.0;
        }
        SetWindowLongPtrW(handle, GWL_EXSTYLE, style as isize);
        // Notify Win32/DWM to refresh cached extended window styles immediately.
        let _ = SetWindowPos(
            handle,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Hide a window via Win32 `ShowWindow(SW_HIDE)` without relying on winit's cached visibility flag
/// or clobbering custom `GWL_EXSTYLE` bits (`WS_EX_TOOLWINDOW`, `WS_EX_LAYERED`).
pub fn hide_window(hwnd: isize) {
    unsafe {
        let _ = ShowWindow(HWND(hwnd as *mut _), SW_HIDE);
    }
}

/// Show a window via Win32 `ShowWindow(SW_SHOWNOACTIVATE)` without clobbering custom `GWL_EXSTYLE` bits.
pub fn show_window_no_activate(hwnd: isize) {
    unsafe {
        let _ = ShowWindow(HWND(hwnd as *mut _), SW_SHOWNOACTIVATE);
    }
}

/// Enumerate installed TrueType / OpenType font entries `(registry_display_name, file_path)` from
/// Windows system and per-user font registry keys.
pub fn list_system_font_entries() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let win_fonts_dir = std::env::var_os("WINDIR")
        .map(|w| PathBuf::from(w).join("Fonts"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\Fonts"));
    let user_fonts_dir = std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("Microsoft").join("Windows").join("Fonts"));

    let roots: &[(HKEY, Option<&PathBuf>)] = &[
        (HKEY_LOCAL_MACHINE, Some(&win_fonts_dir)),
        (HKEY_CURRENT_USER, user_fonts_dir.as_ref()),
    ];

    for &(root, base_dir) in roots {
        unsafe {
            let mut key = HKEY::default();
            let err = RegOpenKeyExW(
                root,
                w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts"),
                None,
                KEY_READ,
                &mut key,
            );
            if err.is_err() {
                continue;
            }
            let mut index = 0u32;
            let mut name_buf = vec![0u16; 512];
            let mut data_buf = vec![0u8; 2048];
            loop {
                let mut name_len = name_buf.len() as u32;
                let mut data_len = data_buf.len() as u32;
                let mut reg_type = 0u32;
                let res = RegEnumValueW(
                    key,
                    index,
                    Some(PWSTR(name_buf.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut reg_type),
                    Some(data_buf.as_mut_ptr()),
                    Some(&mut data_len),
                );
                if res == ERROR_NO_MORE_ITEMS {
                    break;
                }
                index += 1;
                if res.is_err() || (reg_type != REG_SZ.0 && reg_type != REG_EXPAND_SZ.0) {
                    continue;
                }
                let name = String::from_utf16_lossy(&name_buf[..name_len as usize])
                    .trim()
                    .to_string();
                let u16_count = (data_len as usize) / 2;
                let mut wide = Vec::with_capacity(u16_count);
                for chunk in data_buf[..u16_count * 2].chunks_exact(2) {
                    let code = u16::from_le_bytes([chunk[0], chunk[1]]);
                    if code == 0 {
                        break;
                    }
                    wide.push(code);
                }
                let raw_file = String::from_utf16_lossy(&wide).trim().to_string();
                if name.is_empty() || raw_file.is_empty() {
                    continue;
                }
                let raw_path = PathBuf::from(&raw_file);
                let resolved = if raw_path.is_absolute() {
                    raw_path
                } else if let Some(dir) = base_dir {
                    let candidate = dir.join(&raw_path);
                    if candidate.is_file() {
                        candidate
                    } else {
                        win_fonts_dir.join(&raw_path)
                    }
                } else {
                    win_fonts_dir.join(&raw_path)
                };
                let ext_ok = resolved.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    matches!(
                        e.to_ascii_lowercase().as_str(),
                        "ttf" | "ttc" | "otf" | "otc"
                    )
                });
                if ext_ok && resolved.is_file() {
                    out.push((name, resolved));
                }
            }
            let _ = RegCloseKey(key);
        }
    }
    out
}

/// Local timestamp for default filenames, formatted like `20261122_174212`.
pub fn local_timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// Enable Windows 11 rounded corners for borderless windows (ignored on older OS versions).
pub fn set_window_round_corners(hwnd: isize) {
    unsafe {
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            HWND(hwnd as *mut _),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const _,
            std::mem::size_of_val(&pref) as u32,
        );
    }
}

/// Mark a window as a tool window (hidden from the taskbar and Alt+Tab).
pub fn set_window_tool_window(hwnd: isize) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let mut style = GetWindowLongPtrW(handle, GWL_EXSTYLE) as u32;
        style |= WS_EX_TOOLWINDOW.0;
        style &= !WS_EX_APPWINDOW.0;
        SetWindowLongPtrW(handle, GWL_EXSTYLE, style as isize);
    }
}

/// Exclude or include a window in screen capture (`WDA_EXCLUDEFROMCAPTURE`).
pub fn set_window_exclude_from_capture(hwnd: isize, exclude: bool) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let affinity = if exclude {
            WDA_EXCLUDEFROMCAPTURE
        } else {
            WDA_NONE
        };
        let _ = SetWindowDisplayAffinity(handle, affinity);
    }
}

/// Position and size a window in physical coordinates without showing it yet (eliminates black flash before first render).
pub fn set_window_pos_physical_hidden(
    hwnd: isize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    topmost: bool,
) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let mut flags = SWP_NOACTIVATE;
        if !topmost {
            flags |= SWP_NOZORDER;
        }
        let insert_after = if topmost {
            Some(HWND_TOPMOST)
        } else {
            None
        };
        let _ = SetWindowPos(handle, insert_after, x, y, w.max(1), h.max(1), flags);
    }
}

/// Reliably bring a window to foreground input focus (works around foreground lock on global hotkeys so Esc/Enter work immediately).
pub fn activate_window(hwnd: isize) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let fg = GetForegroundWindow();
        let cur_tid = GetCurrentThreadId();
        let fg_tid = if !fg.is_invalid() {
            GetWindowThreadProcessId(fg, None)
        } else {
            0
        };
        let attached = fg_tid != 0 && fg_tid != cur_tid
            && AttachThreadInput(fg_tid, cur_tid, true).as_bool();
        let _ = BringWindowToTop(handle);
        let _ = SetForegroundWindow(handle);
        let _ = SetActiveWindow(handle);
        if attached {
            let _ = AttachThreadInput(fg_tid, cur_tid, false);
        }
    }
}

/// Position, resize, and show a window in physical coordinates.
pub fn set_window_pos_physical(
    hwnd: isize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    topmost: bool,
    no_activate: bool,
) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let mut flags = SWP_SHOWWINDOW;
        if !topmost {
            flags |= SWP_NOZORDER;
        }
        if no_activate {
            flags |= SWP_NOACTIVATE;
        }
        let insert_after = if topmost {
            Some(HWND_TOPMOST)
        } else {
            None
        };
        let _ = SetWindowPos(handle, insert_after, x, y, w.max(1), h.max(1), flags);
    }
}

/// Move a window to physical coordinates without resizing.
pub fn move_window_physical(hwnd: isize, x: i32, y: i32) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let _ = SetWindowPos(
            handle,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Resize a window in physical pixels without moving it.
pub fn resize_window_physical(hwnd: isize, w: i32, h: i32) {
    unsafe {
        let handle = HWND(hwnd as *mut _);
        let _ = SetWindowPos(
            handle,
            None,
            0,
            0,
            w.max(1),
            h.max(1),
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

// ============================================================================
// Single-instance mutex and cross-process wake message
// ============================================================================

const WM_WAKE_INSTANCE: u32 = WM_APP + 42;
static WAKE_CALLBACK: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

pub struct SingleInstanceGuard {
    mutex: HANDLE,
    wake_hwnd: AtomicIsize,
}

unsafe impl Send for SingleInstanceGuard {}
unsafe impl Sync for SingleInstanceGuard {}

impl SingleInstanceGuard {
    /// Attempt to acquire the single-instance lock; if already running, notify the existing instance and return `None`.
    pub fn acquire(on_wake: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        unsafe {
            let mutex = CreateMutexW(None, false, w!("Local\\SnapDockPureRustSingleInstance"))
                .ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mutex);
                if let Ok(existing) = FindWindowW(w!("SnapDockWakeClass"), PCWSTR::null()) {
                    if !existing.is_invalid() {
                        let _ = PostMessageW(Some(existing), WM_WAKE_INSTANCE, WPARAM(0), LPARAM(0));
                    }
                }
                return None;
            }
            let _ = WAKE_CALLBACK.set(Box::new(on_wake));
            let wake_hwnd = AtomicIsize::new(0);
            let (tx, rx) = std::sync::mpsc::channel::<isize>();
            std::thread::spawn(move || run_wake_window(tx));
            if let Ok(hwnd_val) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
                wake_hwnd.store(hwnd_val, Ordering::SeqCst);
            }
            Some(Self { mutex, wake_hwnd })
        }
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        let hwnd = self.wake_hwnd.swap(0, Ordering::SeqCst);
        if hwnd != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_DESTROY, WPARAM(0), LPARAM(0));
            }
        }
        unsafe {
            let _ = CloseHandle(self.mutex);
        }
    }
}

unsafe extern "system" fn wake_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_WAKE_INSTANCE => {
            if let Some(cb) = WAKE_CALLBACK.get() {
                cb();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn run_wake_window(tx: std::sync::mpsc::Sender<isize>) {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wake_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: w!("SnapDockWakeClass"),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            Default::default(),
            w!("SnapDockWakeClass"),
            w!("SnapDockWake"),
            Default::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .unwrap_or_default();
        let _ = tx.send(hwnd.0 as isize);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ============================================================================
// Hollow high-contrast click-through border window for scrolling capture (pure Win32, zero GPU cost, capture-excluded)
// ============================================================================

const BORDER_COLORKEY: u32 = 0x00FF00FF; // Magenta (transparent interior)
const BORDER_OUTER_DEFAULT_BGR: u32 = 0x0066AA00; // BGR for #00AA66 (SnapDock mint)
const BORDER_INNER_RGB: u32 = 0x00FFFFFF; // White inner 1px contrast line

static BORDER_OUTER_BGR: AtomicU32 = AtomicU32::new(BORDER_OUTER_DEFAULT_BGR);

pub struct LongBorderWindow {
    hwnd: AtomicIsize,
}

impl LongBorderWindow {
    pub fn spawn(x: i32, y: i32, width: i32, height: i32, outer_bgr: u32) -> Self {
        BORDER_OUTER_BGR.store(outer_bgr, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel::<isize>();
        std::thread::spawn(move || run_border_window(x, y, width, height, tx));
        let hwnd_val = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap_or(0);
        Self {
            hwnd: AtomicIsize::new(hwnd_val),
        }
    }

    pub fn close(&self) {
        let hwnd = self.hwnd.swap(0, Ordering::SeqCst);
        if hwnd != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP + 99, WPARAM(0), LPARAM(0));
            }
        }
    }
}

impl Drop for LongBorderWindow {
    fn drop(&mut self) {
        self.close();
    }
}

unsafe extern "system" fn border_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;

            let full = RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            let outer_brush = CreateSolidBrush(COLORREF(BORDER_OUTER_BGR.load(Ordering::SeqCst)));
            FillRect(hdc, &full, outer_brush);
            let _ = DeleteObject(outer_brush.into());

            if w > 6 && h > 6 {
                let mid = RECT {
                    left: 2,
                    top: 2,
                    right: w - 2,
                    bottom: h - 2,
                };
                let white_brush = CreateSolidBrush(COLORREF(BORDER_INNER_RGB));
                FillRect(hdc, &mid, white_brush);
                let _ = DeleteObject(white_brush.into());

                let inner = RECT {
                    left: 3,
                    top: 3,
                    right: w - 3,
                    bottom: h - 3,
                };
                let key_brush = CreateSolidBrush(COLORREF(BORDER_COLORKEY));
                FillRect(hdc, &inner, key_brush);
                let _ = DeleteObject(key_brush.into());
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        },
        m if m == WM_APP + 99 => unsafe {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        },
        WM_DESTROY => unsafe {
            PostQuitMessage(0);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn run_border_window(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    tx: std::sync::mpsc::Sender<isize>,
) {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(border_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: w!("SnapDockLongBorderClass"),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
        let ex_style = WS_EX_TOPMOST
            | WS_EX_TOOLWINDOW
            | WS_EX_TRANSPARENT
            | WS_EX_LAYERED
            | WS_EX_NOACTIVATE;
        let hwnd = CreateWindowExW(
            ex_style,
            w!("SnapDockLongBorderClass"),
            w!("SnapDock - Scrolling Capture Area"),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            width.max(8),
            height.max(8),
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .unwrap_or_default();
        if !hwnd.is_invalid() {
            let _ = SetLayeredWindowAttributes(
                hwnd,
                COLORREF(BORDER_COLORKEY),
                255,
                LAYERED_WINDOW_ATTRIBUTES_FLAGS(LWA_COLORKEY.0),
            );
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        let _ = tx.send(hwnd.0 as isize);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_helper_falls_back_to_resource_path() {
        let path = inspect_helper();
        assert!(path.ends_with("snapdock-inspect.exe"));
    }

    #[test]
    fn list_windows_smoke() {
        let list = list_windows();
        assert!(list.len() < 10_000);
    }

    #[test]
    fn window_at_offscreen_returns_none() {
        assert!(window_at(-50_000, -50_000).is_none());
    }
}
