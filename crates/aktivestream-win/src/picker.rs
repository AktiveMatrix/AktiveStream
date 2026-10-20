//! The connection picker window (Win32), shown when the client starts with no arguments. It draws
//! the shared picker list with GDI and, on a choice, launches `aktivestream <name>` as a fresh
//! process (its own session window) before closing — so the picker and session stay independent.

use std::cell::RefCell;
use std::process::Command;

use aktivestream_client::config::Connection;
use aktivestream_client::picker;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, COLORONCOLOR, DIB_RGB_COLORS, EndPaint, InvalidateRect,
    PAINTSTRUCT, SRCCOPY, SetStretchBltMode, StretchDIBits,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    IDC_ARROW, LoadCursorW, MSG, PostQuitMessage, RegisterClassExW, SW_SHOW, SWP_NOMOVE, SWP_NOZORDER,
    SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_DESTROY, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

struct Picker {
    entries: Vec<Connection>,
    selected: usize,
    chosen: Option<String>,
}

thread_local! {
    static PICKER: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

fn with_picker<R>(f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    PICKER.with(|c| c.borrow_mut().as_mut().map(f))
}

/// Shows the connection picker window; on a choice, launches `aktivestream <name>` as a fresh
/// process. Returns when the window closes (a choice was made, or the user cancelled).
pub fn run(entries: Vec<Connection>) -> Result<(), String> {
    let rows = entries.len().max(1);
    let client_h = (picker::TOP + rows * picker::ROW_H + 24).min(760) as i32;
    let client_w = 680i32;
    PICKER.with(|c| *c.borrow_mut() = Some(Picker { entries, selected: 0, chosen: None }));

    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("AktiveStreamPicker"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("AktiveStreamPicker"),
            w!("AktiveStream"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            client_w,
            client_h,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| e.to_string())?;
        // Grow the window so the *client* area is client_w x client_h (the frame is extra).
        let mut frame = RECT { left: 0, top: 0, right: client_w, bottom: client_h };
        let _ =
            AdjustWindowRectExForDpi(&mut frame, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0), GetDpiForWindow(hwnd));
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            frame.right - frame.left,
            frame.bottom - frame.top,
            SWP_NOMOVE | SWP_NOZORDER,
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    let chosen = PICKER.with(|c| c.borrow_mut().take()).and_then(|p| p.chosen);
    if let Some(name) = chosen {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Command::new(exe).arg(&name).spawn().map_err(|e| format!("launch {name}: {e}"))?;
    }
    Ok(())
}

fn client_size(hwnd: HWND) -> (usize, usize) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    ((rc.right - rc.left).max(1) as usize, (rc.bottom - rc.top).max(1) as usize)
}

fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn paint(hwnd: HWND) {
    let (w, h) = client_size(hwnd);
    let mut buf = vec![0u32; w * h];
    with_picker(|p| picker::draw_picker(&mut buf, w, h, &p.entries, p.selected));
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        SetStretchBltMode(hdc, COLORONCOLOR);
        StretchDIBits(
            hdc,
            0,
            0,
            w as i32,
            h as i32,
            0,
            0,
            w as i32,
            h as i32,
            Some(buf.as_ptr().cast()),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
        let _ = EndPaint(hwnd, &ps);
    }
}

fn step(hwnd: HWND, delta: isize) {
    with_picker(|p| {
        if p.entries.is_empty() {
            return;
        }
        let n = p.entries.len() as isize;
        p.selected = (((p.selected as isize + delta) % n + n) % n) as usize;
    });
    invalidate(hwnd);
}

fn hover(hwnd: HWND, x: i32, y: i32) {
    let h = client_size(hwnd).1;
    let changed = with_picker(|p| {
        if let Some(idx) = picker::row_at((x as f64, y as f64), h, p.entries.len())
            && idx != p.selected
        {
            p.selected = idx;
            return true;
        }
        false
    })
    .unwrap_or(false);
    if changed {
        invalidate(hwnd);
    }
}

/// Records the chosen connection and closes the window. `at` is a click position (hit-tests a
/// row); `None` confirms the current selection (Enter).
fn choose(hwnd: HWND, at: Option<(i32, i32)>) {
    let h = client_size(hwnd).1;
    let chose = with_picker(|p| {
        if let Some((x, y)) = at {
            match picker::row_at((x as f64, y as f64), h, p.entries.len()) {
                Some(idx) => p.selected = idx,
                None => return false,
            }
        }
        p.chosen = p.entries.get(p.selected).map(|c| c.name.clone());
        p.chosen.is_some()
    })
    .unwrap_or(false);
    if chose {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let px = || (lparam.0 as i16) as i32;
    let py = || ((lparam.0 >> 16) as i16) as i32;
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            match VIRTUAL_KEY(wparam.0 as u16) {
                VK_DOWN => step(hwnd, 1),
                VK_UP => step(hwnd, -1),
                VK_RETURN => choose(hwnd, None),
                VK_ESCAPE => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                _ => {}
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            hover(hwnd, px(), py());
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            choose(hwnd, Some((px(), py())));
            return LRESULT(0);
        }
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}
