//! The connection manager window (Win32). Shown when the client starts with no arguments: it
//! drives the shared [`Manager`] (list with search and favorites, add/edit form, delete), draws it
//! with GDI, stores passwords in Windows Credential Manager, and on a choice launches
//! `aktivestream <name>` as a fresh process (its own session window) before closing.

use std::cell::RefCell;
use std::process::Command;

use aktivestream_client::config::Connection;
use aktivestream_client::manager::{KeyCode, KeyEvent, Manager, Outcome};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, COLORONCOLOR, DIB_RGB_COLORS, EndPaint, InvalidateRect,
    PAINTSTRUCT, SRCCOPY, SetStretchBltMode, StretchDIBits,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    IDC_ARROW, LoadCursorW, MSG, PostQuitMessage, RegisterClassExW, SW_SHOW, SWP_NOMOVE, SWP_NOZORDER,
    SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_CHAR, WM_DESTROY,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_PAINT, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

struct Picker {
    manager: Manager,
    /// Set when the user chooses a connection; launched after the window closes.
    chosen: Option<String>,
}

thread_local! {
    static PICKER: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

fn with_picker<R>(f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    PICKER.with(|c| c.borrow_mut().as_mut().map(f))
}

/// Shows the connection manager; on a choice, launches `aktivestream <name>` as a fresh process.
/// Returns when the window closes.
pub fn run(entries: Vec<Connection>) -> Result<(), String> {
    let client_w = 760i32;
    let client_h = 680i32;
    PICKER.with(|c| *c.borrow_mut() = Some(Picker { manager: Manager::new(entries), chosen: None }));

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
    with_picker(|p| p.manager.draw(&mut buf, w, h));
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

fn ctrl_down() -> bool {
    unsafe { GetKeyState(i32::from(VK_CONTROL.0)) < 0 }
}

fn shift_down() -> bool {
    unsafe { GetKeyState(i32::from(VK_SHIFT.0)) < 0 }
}

/// Maps a `WM_KEYDOWN` virtual key to a manager event. Returns `None` for keys the manager does not
/// consume here (printable letters arrive via `WM_CHAR` instead).
fn keydown_event(vk: VIRTUAL_KEY) -> Option<KeyEvent> {
    let code = match vk {
        VK_UP => KeyCode::Up,
        VK_DOWN => KeyCode::Down,
        VK_LEFT => KeyCode::Left,
        VK_RIGHT => KeyCode::Right,
        VK_RETURN => KeyCode::Enter,
        VK_ESCAPE => KeyCode::Escape,
        VK_BACK => KeyCode::Backspace,
        VK_DELETE => KeyCode::Delete,
        VK_TAB => {
            return Some(KeyEvent::new(if shift_down() { KeyCode::BackTab } else { KeyCode::Tab }));
        }
        _ => {
            // Ctrl + a letter is an action chord (new, edit, duplicate, favorite, save, show password).
            if ctrl_down() && (0x41..=0x5A).contains(&vk.0) {
                let c = (b'a' + (vk.0 as u8 - 0x41)) as char;
                return Some(KeyEvent::ctrl(KeyCode::Char(c)));
            }
            return None;
        }
    };
    Some(KeyEvent::new(code))
}

fn dispatch(hwnd: HWND, ev: KeyEvent) {
    let outcome = with_picker(|p| p.manager.key(ev)).unwrap_or(Outcome::Idle);
    handle(hwnd, outcome);
}

fn handle(hwnd: HWND, outcome: Outcome) {
    match outcome {
        Outcome::Redraw => invalidate(hwnd),
        Outcome::Idle => {}
        Outcome::Quit => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        Outcome::Connect(name) => {
            with_picker(|p| p.chosen = Some(name));
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        Outcome::StorePassword { host, username, password } => {
            crate::creds::write(&format!("AktiveStream/{host}"), &username, &password);
            invalidate(hwnd);
        }
        Outcome::ForgetPassword { host } => {
            crate::creds::delete(&format!("AktiveStream/{host}"));
            invalidate(hwnd);
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
            if let Some(ev) = keydown_event(VIRTUAL_KEY(wparam.0 as u16)) {
                dispatch(hwnd, ev);
            }
            return LRESULT(0);
        }
        WM_CHAR => {
            // Printable typing for the search box and text fields; control characters (Enter, Tab,
            // Backspace, Ctrl+letter) are handled in WM_KEYDOWN, so ignore them here.
            let c = wparam.0 as u16;
            if c >= 0x20
                && c != 0x7F
                && !ctrl_down()
                && let Some(ch) = char::from_u32(c as u32)
            {
                dispatch(hwnd, KeyEvent::new(KeyCode::Char(ch)));
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let h = client_size(hwnd).1;
            let outcome = with_picker(|p| p.manager.click(px(), py(), h)).unwrap_or(Outcome::Idle);
            handle(hwnd, outcome);
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
