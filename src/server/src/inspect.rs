//! Isolated accessibility probe: stalled providers cannot block the capture UI.
#[cfg(windows)]
fn inspect() -> windows::core::Result<()> {
    use std::time::{Duration, Instant};
    use windows::Win32::{
        Foundation::HWND,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
        },
    };
    unsafe fn walk(
        element: &IUIAutomationElement,
        walker: &IUIAutomationTreeWalker,
        point: (i32, i32),
        best: &mut Option<serde_json::Value>,
        best_area: &mut i64,
        started: Instant,
        depth: u32,
        count: &mut u32,
    ) {
        if depth > 40 || *count > 1800 || started.elapsed() > Duration::from_millis(900) {
            return;
        }
        *count += 1;
        let rect = unsafe { element.CurrentBoundingRectangle() }.ok();
        if let Some(rect) = rect {
            if rect.right > rect.left && rect.bottom > rect.top {
                if point.0 < rect.left
                    || point.0 >= rect.right
                    || point.1 < rect.top
                    || point.1 >= rect.bottom
                {
                    return;
                }
                let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                if area > 16 && area < *best_area {
                    *best_area = area;
                    // Read geometry only, never the text in third-party controls.
                    *best = Some(
                        serde_json::json!({"x":rect.left,"y":rect.top,"width":rect.right-rect.left,"height":rect.bottom-rect.top,"title":"Window Control"}),
                    );
                }
            }
        }
        let mut child = unsafe { walker.GetFirstChildElement(element) };
        while let Ok(current) = child {
            unsafe {
                walk(
                    &current,
                    walker,
                    point,
                    best,
                    best_area,
                    started,
                    depth + 1,
                    count,
                )
            };
            if *count > 1800 || started.elapsed() > Duration::from_millis(900) {
                break;
            }
            child = unsafe { walker.GetNextSiblingElement(&current) };
        }
    }
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Ok(());
    }
    let (Ok(hwnd), Ok(x), Ok(y)) = (
        args[1].parse::<isize>(),
        args[2].parse::<i32>(),
        args[3].parse::<i32>(),
    ) else {
        return Ok(());
    };
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let root = automation.ElementFromHandle(HWND(hwnd as *mut _))?;
            let walker = automation.ControlViewWalker()?;
            let mut best = None;
            let mut area = i64::MAX;
            let mut count = 0;
            walk(
                &root,
                &walker,
                (x, y),
                &mut best,
                &mut area,
                Instant::now(),
                0,
                &mut count,
            );
            println!("{}", best.unwrap_or(serde_json::Value::Null));
        }
        CoUninitialize();
    }
    Ok(())
}
fn main() {
    #[cfg(windows)]
    {
        let _ = inspect();
    }
    #[cfg(not(windows))]
    println!("null");
}
