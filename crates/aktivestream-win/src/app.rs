//! The session window: keyboard hook, mouse, rendering, clipboard and full screen.
//!
//! Threads: the UI thread owns the window and rendering; a dedicated high-priority thread
//! owns the low-level keyboard hook, so no keystroke on the PC ever waits for a frame to be
//! drawn. Keyboard state shared by both lives in `KeyCtx`.
//!
//! UI state lives in a thread-local `App`. Win32 calls that can re-enter the window
//! procedure (SetWindowPos, clipboard writes, DestroyWindow) are made only after the borrow
//! of `App` has been dropped.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use aktivestream_keymap::{
    Disposition, Engine, HostEvent, Key, KeyInput, LocalAction, MouseButton, Profile, RfbTranslator,
};
use aktivestream_rfb::{CursorShape, Rect, ServerInfo, SessionReader, SessionWriter};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACK_BRUSH, BeginPaint, COLORONCOLOR, DIB_RGB_COLORS, EndPaint, FillRect,
    GetMonitorInfoW, GetStockObject, HALFTONE, HBRUSH, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, PAINTSTRUCT, SRCCOPY, ScreenToClient, SetBrushOrgEx, SetStretchBltMode, StretchDIBits,
};
use windows::Win32::System::DataExchange::AddClipboardFormatListener;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    ReleaseCapture, SendInput, SetCapture, VIRTUAL_KEY, VK_CAPITAL, VK_LWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_STYLE,
    GetClientRect, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowPlacement, HC_ACTION,
    HCURSOR, HTCLIENT, HWND_TOP, IDC_ARROW, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LoadCursorW, MB_ICONERROR, MB_OK, MSG,
    MessageBoxW, PostMessageW, PostQuitMessage, PostThreadMessageW, RegisterClassExW, SC_KEYMENU, SW_SHOW,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPlacement, SetWindowPos, SetWindowTextW,
    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, WA_INACTIVE, WH_KEYBOARD_LL, WINDOW_EX_STYLE,
    WINDOWPLACEMENT, WM_ACTIVATE, WM_APP, WM_CLIPBOARDUPDATE, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_PAINT, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SIZE, WM_SYSCOMMAND,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WNDCLASSEXW, WS_OVERLAPPEDWINDOW, WS_POPUP, WindowFromPoint,
};
use windows::core::{HSTRING, w};

use crate::net::{self, Out, Shared};
use crate::render::Renderer;
use crate::view::View;
use crate::{Args, clipboard, cursor, diag};

const WM_APP_LOCAL: u32 = WM_APP + 10;
const TIMER_EXIT: usize = 1;
const LOCAL_FULLSCREEN: usize = 1;
const LOCAL_TITLE: usize = 2;
const LOCAL_DISCONNECT: usize = 3;

const BUTTON_LEFT: u8 = 1;
const BUTTON_MIDDLE: u8 = 2;
const BUTTON_RIGHT: u8 = 4;
const WHEEL_UP: u8 = 8;
const WHEEL_DOWN: u8 = 16;
const WHEEL_LEFT: u8 = 32;
const WHEEL_RIGHT: u8 = 64;
const WHEEL_DELTA: i32 = 120;

/// Keyboard state shared by the hook thread and the UI thread.
struct KeyCtx {
    hwnd: AtomicIsize,
    view_only: bool,
    state: Mutex<KeyState>,
}

struct KeyState {
    engine: Engine,
    translator: RfbTranslator,
    tx: Sender<Out>,
    /// Caps Lock toggle, tracked from the hook: the hook thread receives no keyboard
    /// messages, so its own GetKeyState would be stale.
    caps: bool,
    caps_held: bool,
    keylog: Option<std::io::BufWriter<std::fs::File>>,
}

impl KeyState {
    /// Sends host events as keysyms (unless view-only) and returns what was sent.
    fn send_host(&mut self, events: Vec<HostEvent>, view_only: bool) -> Vec<(u32, bool)> {
        let mut sent = Vec::new();
        self.translator.set_caps_lock(self.caps);
        for event in events {
            for (keysym, down) in self.translator.translate(event) {
                if !view_only {
                    let _ = self.tx.send(Out::Key(keysym, down));
                }
                sent.push((keysym, down));
            }
        }
        sent
    }

    fn log(&mut self, line: impl FnOnce() -> String) {
        if let Some(log) = self.keylog.as_mut() {
            let _ = writeln!(log, "{}", line());
            let _ = log.flush();
        }
    }
}

static KEYS: OnceLock<Arc<KeyCtx>> = OnceLock::new();

fn keys() -> MutexGuard<'static, KeyState> {
    let ctx = KEYS.get().expect("keyboard context");
    ctx.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct App {
    hwnd: HWND,
    hook_thread: u32,
    shared: Arc<Shared>,
    tx: Sender<Out>,
    name: String,
    view: View,
    renderer: Option<Renderer>,
    buttons: u8,
    left_sent_as: MouseButton,
    last_pointer: (u16, u16),
    wheel: i32,
    hwheel: i32,
    fullscreen: bool,
    windowed: WINDOWPLACEMENT,
    cursor_shape: Option<CursorShape>,
    cursor: Option<cursor::Cursor>,
    last_server_clip: Option<String>,
    paints: u64,
    paint_time: Duration,
    screenshot: Option<std::path::PathBuf>,
    view_only: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static CLOSED_BY_USER: Cell<bool> = const { Cell::new(false) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.as_mut().map(f)))
}

fn button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => BUTTON_LEFT,
        MouseButton::Middle => BUTTON_MIDDLE,
        MouseButton::Right => BUTTON_RIGHT,
    }
}

pub fn message_box(title: &str, text: &str) {
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_OK | MB_ICONERROR);
    }
}

/// Low-level keyboard hook, running on its own thread. Everything here is short: one mutex
/// held for the engine step, and channel sends to the writer thread.
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && let Some(ctx) = KEYS.get()
    {
        let hwnd = HWND(ctx.hwnd.load(Ordering::Acquire) as *mut _);
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if kb.dwExtraInfo != REPLAY_TAG
            && !hwnd.is_invalid()
            && unsafe { GetForegroundWindow() } == hwnd
            && on_key(ctx, hwnd, kb, wparam.0 as u32)
        {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn on_key(ctx: &KeyCtx, hwnd: HWND, kb: &KBDLLHOOKSTRUCT, message: u32) -> bool {
    let down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
    let extended = kb.flags.0 & LLKHF_EXTENDED.0 != 0;
    let key = Key::from_windows(kb.vkCode as u16, kb.scanCode as u16, extended);
    let mut state = ctx.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if key == Key::CapsLock {
        if down && !state.caps_held {
            state.caps = !state.caps;
        }
        state.caps_held = down;
    }
    let output = state.engine.handle_key(KeyInput { key, down });
    let host = output.host.clone();
    let sent = state.send_host(output.host, ctx.view_only);
    state.log(|| {
        let keysyms: Vec<String> = sent.iter().map(|(k, d)| format!("{}{k:#x}", if *d { "+" } else { "-" })).collect();
        format!(
            "{} vk={:#04x} scan={:#04x} flags={:#04x} {key:?} -> {:?} host={host:?} keysyms=[{}] local={:?}",
            if down { "down" } else { "up  " },
            kb.vkCode,
            kb.scanCode,
            kb.flags.0,
            output.disposition,
            keysyms.join(" "),
            output.local
        )
    });
    drop(state);
    if ctx.view_only {
        return false;
    }
    let code = match output.local {
        None => None,
        Some(LocalAction::ToggleFullscreen) => Some(LOCAL_FULLSCREEN),
        Some(LocalAction::ToggleSystemShortcuts) => Some(LOCAL_TITLE),
        Some(LocalAction::Disconnect) => Some(LOCAL_DISCONNECT),
        Some(LocalAction::PassChordToWindows) => {
            // Replayed from the hook thread's message loop, after this callback returns.
            let flags = usize::from(extended);
            unsafe {
                let _ = PostThreadMessageW(
                    GetCurrentThreadId(),
                    WM_APP_REPLAY,
                    WPARAM(kb.vkCode as usize),
                    LPARAM(((flags << 16) | kb.scanCode as usize) as isize),
                );
            }
            None
        }
    };
    if let Some(code) = code {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_LOCAL, WPARAM(code), LPARAM(0));
        }
    }
    output.disposition == Disposition::Consume
}

/// Marks keyboard input AktiveStream injects itself, so its own hook hands it straight to Windows.
const REPLAY_TAG: usize = 0x5254_4947;
const WM_APP_REPLAY: u32 = WM_APP + 20;

/// Types Win + the given key into Windows (for the Magnifier shortcuts in full screen).
fn replay_with_win(vk: u16, scan: u16, extended: bool) {
    let key = |vk: u16, scan: u16, extended: bool, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: KEYBD_EVENT_FLAGS(
                    if extended { KEYEVENTF_EXTENDEDKEY.0 } else { 0 } | if up { KEYEVENTF_KEYUP.0 } else { 0 },
                ),
                time: 0,
                dwExtraInfo: REPLAY_TAG,
            },
        },
    };
    let win = VK_LWIN.0;
    let inputs = [
        key(win, 0x5b, true, false),
        key(vk, scan, extended, false),
        key(vk, scan, extended, true),
        key(win, 0x5b, true, true),
    ];
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Starts the hook thread; returns its thread id (for WM_QUIT on shutdown).
fn spawn_hook_thread() -> Result<u32, String> {
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("keyboard-hook".into())
        .spawn(move || unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            let hook = GetModuleHandleW(None)
                .and_then(|module| SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(module.into()), 0));
            let _ = ready_tx.send(hook.as_ref().map(|_| GetCurrentThreadId()).map_err(|e| e.to_string()));
            let Ok(hook) = hook else { return };
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_APP_REPLAY {
                    let extended = (msg.lParam.0 >> 16) & 1 != 0;
                    replay_with_win(msg.wParam.0 as u16, (msg.lParam.0 & 0xffff) as u16, extended);
                    continue;
                }
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(hook);
        })
        .map_err(|e| format!("keyboard thread: {e}"))?;
    ready_rx.recv().map_err(|e| e.to_string())?.map_err(|e| format!("SetWindowsHookEx failed: {e}"))
}

impl App {
    fn send(&self, message: Out) {
        // A closed channel means the writer saw the connection drop; the reader reports it.
        let _ = self.tx.send(message);
    }

    fn send_host(&self, events: Vec<HostEvent>) {
        keys().send_host(events, self.view_only);
    }

    fn release_all(&mut self) {
        {
            let mut state = keys();
            let events = state.engine.release_all();
            let sent = state.send_host(events, self.view_only);
            if !sent.is_empty() {
                state.log(|| format!("release_all keysyms={sent:x?}"));
            }
            // Caps Lock may have been toggled in another window meanwhile.
            state.caps = unsafe { GetKeyState(i32::from(VK_CAPITAL.0)) } & 1 != 0;
            state.caps_held = false;
        }
        if self.buttons != 0 {
            self.buttons = 0;
            let (x, y) = self.last_pointer;
            self.send(Out::Pointer(0, x, y));
        }
        self.wheel = 0;
        self.hwheel = 0;
    }

    fn client_size(&self) -> (i32, i32) {
        let mut client = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut client);
        }
        (client.right - client.left, client.bottom - client.top)
    }

    fn update_view(&mut self) {
        let (cw, ch) = self.client_size();
        if let Some(renderer) = self.renderer.as_mut()
            && let Err(e) = renderer.resize(cw.max(1) as u32, ch.max(1) as u32)
        {
            eprintln!("Direct3D resize failed ({e}); falling back to GDI");
            self.renderer = None;
        }
        let (w, h) = {
            let fb = self.shared.framebuffer.lock().unwrap();
            (fb.width, fb.height)
        };
        let view = View::fit(w, h, cw, ch);
        if view != self.view {
            let rescaled = view.w != self.view.w;
            self.view = view;
            if rescaled {
                self.rebuild_cursor();
            }
        }
    }

    fn rebuild_cursor(&mut self) {
        let scale = if self.view.fb_w > 0 { f64::from(self.view.w) / f64::from(self.view.fb_w) } else { 1.0 };
        self.cursor = self.cursor_shape.as_ref().and_then(|shape| cursor::create(shape, scale));
    }

    fn cursor_handle(&self) -> Option<HCURSOR> {
        self.cursor.as_ref().map(|c| c.0)
    }

    /// Uploads `area` (None = everything) and draws one frame with Direct3D. Returns false
    /// when there is no usable renderer, so the caller falls back to GDI.
    fn render(&mut self, area: Option<Rect>, capture: bool) -> bool {
        let started = Instant::now();
        let Some(renderer) = self.renderer.as_mut() else { return false };
        let uploaded = {
            let fb = self.shared.framebuffer.lock().unwrap();
            renderer.upload(&fb, area)
        };
        if let Err(e) = uploaded {
            eprintln!("Direct3D upload failed ({e}); falling back to GDI");
            self.renderer = None;
            return false;
        }
        renderer.draw(self.view);
        if capture && let Some(path) = &self.screenshot {
            match renderer
                .capture()
                .map_err(|e| e.to_string())
                .and_then(|(w, h, rgba)| diag::write_png(path, w, h, &rgba))
            {
                Ok(()) => println!("screenshot: {}", path.display()),
                Err(e) => eprintln!("screenshot failed: {e}"),
            }
        }
        if let Err(e) = renderer.present() {
            eprintln!("Direct3D present failed ({e}); falling back to GDI");
            self.renderer = None;
            return false;
        }
        self.paints += 1;
        self.paint_time += started.elapsed();
        true
    }

    fn take_dirty(&mut self) -> Option<Rect> {
        self.shared.frame_seen();
        self.shared.dirty.lock().unwrap().take()
    }

    fn take_server_clip(&mut self) -> Option<String> {
        let text = self.shared.cut_text.lock().unwrap().take()?;
        keys().log(|| format!("clipboard <- Mac: {} chars", text.chars().count()));
        self.last_server_clip = Some(text.clone());
        Some(text)
    }

    fn local_clipboard_changed(&mut self, text: Option<String>) {
        match text {
            Some(text) if self.last_server_clip.as_deref() != Some(text.as_str()) => {
                keys().log(|| format!("clipboard -> Mac: {} chars", text.chars().count()));
                self.send(Out::CutText(text));
            }
            Some(_) => keys().log(|| "clipboard update is our own echo; not sent".into()),
            None => keys().log(|| "clipboard update without text".into()),
        }
    }

    fn title(&self) -> String {
        let routing = if keys().engine.system_shortcuts_to_host() { "  [Windows shortcuts go to the Mac]" } else { "" };
        format!("{} - AktiveStream{routing}", self.name)
    }

    fn mouse(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) {
        if self.view_only {
            return;
        }
        let mut point = POINT { x: i32::from(lparam.0 as i16), y: i32::from((lparam.0 >> 16) as i16) };
        if message == WM_MOUSEWHEEL || message == WM_MOUSEHWHEEL {
            // Wheel messages carry screen coordinates.
            unsafe {
                let _ = ScreenToClient(self.hwnd, &mut point);
            }
        }
        let (x, y) = self.view.fb_point(point.x, point.y);
        self.last_pointer = (x, y);
        let wheel_delta = i32::from((wparam.0 >> 16) as i16);
        match message {
            WM_LBUTTONDOWN => {
                let (events, button) = keys().engine.mouse_button(MouseButton::Left, true);
                self.send_host(events);
                self.left_sent_as = button;
                self.buttons |= button_bit(button);
                unsafe { SetCapture(self.hwnd) };
            }
            WM_LBUTTONUP => {
                let (events, _) = keys().engine.mouse_button(MouseButton::Left, false);
                self.buttons &= !button_bit(self.left_sent_as);
                self.send(Out::Pointer(self.buttons, x, y));
                self.send_host(events);
                self.release_capture_if_idle();
                return;
            }
            WM_RBUTTONDOWN => self.press(BUTTON_RIGHT),
            WM_RBUTTONUP => self.buttons &= !BUTTON_RIGHT,
            WM_MBUTTONDOWN => self.press(BUTTON_MIDDLE),
            WM_MBUTTONUP => self.buttons &= !BUTTON_MIDDLE,
            WM_MOUSEWHEEL => {
                self.wheel += wheel_delta;
                while self.wheel >= WHEEL_DELTA {
                    self.click(WHEEL_UP, x, y);
                    self.wheel -= WHEEL_DELTA;
                }
                while self.wheel <= -WHEEL_DELTA {
                    self.click(WHEEL_DOWN, x, y);
                    self.wheel += WHEEL_DELTA;
                }
                return;
            }
            WM_MOUSEHWHEEL => {
                self.hwheel += wheel_delta;
                while self.hwheel >= WHEEL_DELTA {
                    self.click(WHEEL_RIGHT, x, y);
                    self.hwheel -= WHEEL_DELTA;
                }
                while self.hwheel <= -WHEEL_DELTA {
                    self.click(WHEEL_LEFT, x, y);
                    self.hwheel += WHEEL_DELTA;
                }
                return;
            }
            _ => {}
        }
        self.send(Out::Pointer(self.buttons, x, y));
        if matches!(message, WM_RBUTTONUP | WM_MBUTTONUP) {
            self.release_capture_if_idle();
        }
    }

    fn press(&mut self, bit: u8) {
        self.buttons |= bit;
        unsafe { SetCapture(self.hwnd) };
    }

    fn click(&mut self, bit: u8, x: u16, y: u16) {
        self.send(Out::Pointer(self.buttons | bit, x, y));
        self.send(Out::Pointer(self.buttons, x, y));
    }

    fn release_capture_if_idle(&self) {
        if self.buttons & (BUTTON_LEFT | BUTTON_MIDDLE | BUTTON_RIGHT) == 0 {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
    }
}

fn monitor_info(hwnd: HWND) -> MONITORINFO {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let _ = GetMonitorInfoW(monitor, &mut info);
    }
    info
}

/// Sizes the window so the framebuffer fits the monitor's work area, centred.
fn size_window(hwnd: HWND, fb_w: u32, fb_h: u32) {
    let work = monitor_info(hwnd).rcWork;
    let mut frame = RECT::default();
    unsafe {
        let _ =
            AdjustWindowRectExForDpi(&mut frame, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0), GetDpiForWindow(hwnd));
    }
    let (frame_w, frame_h) = (frame.right - frame.left, frame.bottom - frame.top);
    let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
    let view = View::fit(fb_w, fb_h, work_w - frame_w, work_h - frame_h);
    let (w, h) = (view.w + frame_w, view.h + frame_h);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            work.left + (work_w - w) / 2,
            work.top + (work_h - h) / 2,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn update_title(hwnd: HWND) {
    if let Some(title) = with_app(|app| app.title()) {
        unsafe {
            let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
        }
    }
}

fn toggle_fullscreen(hwnd: HWND) {
    let Some(was_fullscreen) = with_app(|app| app.fullscreen) else { return };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        if was_fullscreen {
            let placement = with_app(|app| {
                app.fullscreen = false;
                app.windowed
            });
            keys().engine.set_fullscreen(false);
            SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !(WS_POPUP.0 as isize)) | WS_OVERLAPPEDWINDOW.0 as isize);
            if let Some(placement) = placement {
                let _ = SetWindowPlacement(hwnd, &placement);
            }
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
            );
        } else {
            let mut placement = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            let _ = GetWindowPlacement(hwnd, &mut placement);
            with_app(|app| {
                app.windowed = placement;
                app.fullscreen = true;
            });
            keys().engine.set_fullscreen(true);
            let monitor = monitor_info(hwnd).rcMonitor;
            SetWindowLongPtrW(hwnd, GWL_STYLE, (style & !(WS_OVERLAPPEDWINDOW.0 as isize)) | WS_POPUP.0 as isize);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                monitor.left,
                monitor.top,
                monitor.right - monitor.left,
                monitor.bottom - monitor.top,
                SWP_NOOWNERZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
            );
        }
    }
    update_title(hwnd);
}

/// GDI fallback for when Direct3D is unavailable.
fn paint_gdi(hwnd: HWND, ps: &PAINTSTRUCT, hdc: windows::Win32::Graphics::Gdi::HDC) {
    let started = Instant::now();
    let Some((view, shared)) = with_app(|app| (app.view, app.shared.clone())) else { return };
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
        let black = HBRUSH(GetStockObject(BLACK_BRUSH).0);
        let bars = [
            RECT { left: 0, top: 0, right: client.right, bottom: view.y },
            RECT { left: 0, top: view.y + view.h, right: client.right, bottom: client.bottom },
            RECT { left: 0, top: view.y, right: view.x, bottom: view.y + view.h },
            RECT { left: view.x + view.w, top: view.y, right: client.right, bottom: view.y + view.h },
        ];
        for bar in bars.iter().filter(|r| r.right > r.left && r.bottom > r.top) {
            FillRect(hdc, bar, black);
        }
    }
    if let Some((source, dest)) = view.paint_mapping(ps.rcPaint) {
        let fb = shared.framebuffer.lock().unwrap();
        if fb.width as i32 == view.fb_w && fb.height as i32 == view.fb_h && !fb.pixels.is_empty() {
            // Hand GDI just the source rows as a top-down DIB, so no bottom-up origin rules apply.
            let rows = &fb.pixels[usize::from(source.y) * fb.width as usize..];
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: fb.width as i32,
                    biHeight: -i32::from(source.h),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            unsafe {
                SetStretchBltMode(hdc, if view.is_one_to_one() { COLORONCOLOR } else { HALFTONE });
                let _ = SetBrushOrgEx(hdc, 0, 0, None);
                StretchDIBits(
                    hdc,
                    dest.left,
                    dest.top,
                    dest.right - dest.left,
                    dest.bottom - dest.top,
                    i32::from(source.x),
                    0,
                    i32::from(source.w),
                    i32::from(source.h),
                    Some(rows.as_ptr().cast()),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            }
        }
    }
    with_app(|app| {
        app.paints += 1;
        app.paint_time += started.elapsed();
    });
}

fn on_paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let drawn = with_app(|app| app.render(None, false)).unwrap_or(false);
    if !drawn {
        paint_gdi(hwnd, &ps, hdc);
    }
    unsafe {
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            on_paint(hwnd);
            return LRESULT(0);
        }
        WM_SIZE | net::WM_APP_RESIZED => {
            with_app(App::update_view);
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            return LRESULT(0);
        }
        net::WM_APP_FRAME => {
            let gdi_area = with_app(|app| {
                let dirty = app.take_dirty()?;
                if app.render(Some(dirty), false) { None } else { Some(app.view.fb_rect_to_client(dirty)) }
            });
            if let Some(Some(area)) = gdi_area {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), Some(&area), false);
                }
            }
            return LRESULT(0);
        }
        net::WM_APP_CURSOR => {
            let handle = with_app(|app| {
                app.cursor_shape = app.shared.cursor.lock().unwrap().take();
                app.rebuild_cursor();
                app.cursor_handle()
            });
            let mut pos = POINT::default();
            unsafe {
                if let Some(Some(handle)) = handle
                    && GetCursorPos(&mut pos).is_ok()
                    && WindowFromPoint(pos) == hwnd
                {
                    SetCursor(Some(handle));
                }
            }
            return LRESULT(0);
        }
        WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == HTCLIENT => {
            if let Some(Some(handle)) = with_app(|app| app.cursor_handle()) {
                unsafe { SetCursor(Some(handle)) };
                return LRESULT(1);
            }
        }
        net::WM_APP_CUT_TEXT => {
            if let Some(Some(text)) = with_app(App::take_server_clip) {
                clipboard::write_text(hwnd, &text);
            }
            return LRESULT(0);
        }
        WM_CLIPBOARDUPDATE => {
            let text = clipboard::read_text(hwnd);
            with_app(|app| app.local_clipboard_changed(text));
            return LRESULT(0);
        }
        net::WM_APP_DISCONNECTED => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == TIMER_EXIT => {
            let captured = with_app(|app| app.screenshot.is_some() && app.render(None, true)).unwrap_or(false);
            if !captured && let Some(Some(path)) = with_app(|app| app.screenshot.clone()) {
                match diag::capture_client(hwnd, &path) {
                    Ok(()) => println!("screenshot: {}", path.display()),
                    Err(e) => eprintln!("screenshot failed: {e}"),
                }
            }
            CLOSED_BY_USER.set(true);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_APP_LOCAL => {
            match wparam.0 {
                LOCAL_FULLSCREEN => toggle_fullscreen(hwnd),
                LOCAL_TITLE => update_title(hwnd),
                LOCAL_DISCONNECT => {
                    CLOSED_BY_USER.set(true);
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP | WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            with_app(|app| app.mouse(message, wparam, lparam));
            return LRESULT(0);
        }
        WM_ACTIVATE if (wparam.0 & 0xffff) as u32 == WA_INACTIVE => {
            with_app(App::release_all);
        }
        WM_KILLFOCUS => {
            with_app(App::release_all);
        }
        // Modifiers reach the window when system shortcuts stay on the PC (so Windows can
        // still run Alt+Tab); Alt must never open the window menu or fire F10/Alt accelerators.
        WM_SYSKEYDOWN | WM_SYSKEYUP => return LRESULT(0),
        WM_SYSCOMMAND if (wparam.0 & 0xfff0) as u32 == SC_KEYMENU => return LRESULT(0),
        WM_CLOSE => {
            CLOSED_BY_USER.set(true);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

pub fn run(args: &Args, session: (ServerInfo, SessionReader, SessionWriter)) -> Result<(), String> {
    let (info, reader, writer) = session;
    let shared = Arc::new(Shared::new(info.width, info.height));
    run_window(args, shared, info.name, move |shared, rx| {
        net::spawn_reader(shared.clone(), reader);
        net::spawn_writer(shared, writer, rx);
    })
}

/// The window, renderer, keyboard and event loop, shared by the RFB and native paths. The
/// `shared` framebuffer is already sized to the remote screen; `start_source` spawns whatever
/// fills it (the RFB reader/writer, or the native QUIC/decode thread).
pub fn run_window(
    args: &Args,
    shared: Arc<Shared>,
    host_name: String,
    start_source: impl FnOnce(Arc<Shared>, mpsc::Receiver<Out>),
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let fail = |what: &str, e: windows::core::Error| format!("{what} failed: {e}");
    let (fb_w, fb_h) = {
        let fb = shared.framebuffer.lock().unwrap();
        (fb.width, fb.height)
    };

    let hwnd = unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| fail("GetModuleHandle", e))?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("AktiveStreamSession"),
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(fail("RegisterClassEx", windows::core::Error::from_win32()));
        }
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("AktiveStreamSession"),
            &HSTRING::from(format!("{host_name} - AktiveStream")),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1280,
            800,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| fail("CreateWindowEx", e))?
    };
    size_window(hwnd, fb_w, fb_h);
    shared.set_hwnd(hwnd);

    let keylog = match &args.keylog {
        Some(path) => Some(std::io::BufWriter::new(
            std::fs::File::create(path).map_err(|e| format!("--keylog {}: {e}", path.display()))?,
        )),
        None => None,
    };
    let ctx = Arc::new(KeyCtx {
        hwnd: AtomicIsize::new(hwnd.0 as isize),
        view_only: args.view_only,
        state: Mutex::new(KeyState {
            engine: Engine::new(Profile::mac_default()),
            translator: RfbTranslator::new(),
            tx: tx.clone(),
            caps: unsafe { GetKeyState(i32::from(VK_CAPITAL.0)) } & 1 != 0,
            caps_held: false,
            keylog,
        }),
    });
    KEYS.set(ctx).map_err(|_| "keyboard context already set".to_string())?;
    let hook_thread = spawn_hook_thread()?;

    let renderer = match Renderer::new(hwnd) {
        Ok(renderer) => Some(renderer),
        Err(e) => {
            eprintln!("Direct3D unavailable ({e}); using GDI");
            None
        }
    };
    let app = App {
        hwnd,
        hook_thread,
        shared: shared.clone(),
        tx,
        name: host_name,
        view: View::default(),
        renderer,
        buttons: 0,
        left_sent_as: MouseButton::Left,
        last_pointer: (0, 0),
        wheel: 0,
        hwheel: 0,
        fullscreen: false,
        windowed: WINDOWPLACEMENT::default(),
        cursor_shape: None,
        cursor: None,
        last_server_clip: None,
        paints: 0,
        paint_time: Duration::ZERO,
        screenshot: args.screenshot.clone(),
        view_only: args.view_only,
    };
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with_app(App::update_view);
    update_title(hwnd);

    start_source(shared.clone(), rx);

    unsafe {
        let _ = AddClipboardFormatListener(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    if args.fullscreen {
        toggle_fullscreen(hwnd);
    }
    if let Some(secs) = args.exit_after {
        unsafe { SetTimer(Some(hwnd), TIMER_EXIT, secs.saturating_mul(1000), None) };
    }
    let started = Instant::now();

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    if let Some(app) = APP.with(|cell| cell.borrow_mut().take()) {
        unsafe {
            let _ = PostThreadMessageW(app.hook_thread, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if args.exit_after.is_some() {
            let stats = diag::Stats {
                updates: shared.updates.load(Ordering::Relaxed),
                update_pixels: shared.update_pixels.load(Ordering::Relaxed),
                paints: app.paints,
                paint_time: app.paint_time,
                renderer: if app.renderer.is_some() { "Direct3D 11" } else { "GDI" },
            };
            println!("{}", stats.report(started.elapsed()));
        }
    }
    let error = shared.error.lock().unwrap().take();
    match error {
        Some(error) if !CLOSED_BY_USER.get() => Err(error),
        _ => Ok(()),
    }
}
