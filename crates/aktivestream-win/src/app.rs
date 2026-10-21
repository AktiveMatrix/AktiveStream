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
use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use aktivestream_client::bar;
use aktivestream_keymap::native::NativeInput;
use aktivestream_keymap::{
    Disposition, Engine, HostEvent, Key, KeyInput, LocalAction, MouseButton, Profile, RfbTranslator,
};
use aktivestream_proto::Input;
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
const LOCAL_NEXT_SESSION: usize = 4;
const LOCAL_PREV_SESSION: usize = 5;

const BUTTON_LEFT: u8 = 1;
const BUTTON_MIDDLE: u8 = 2;
const BUTTON_RIGHT: u8 = 4;
const WHEEL_UP: u8 = 8;
const WHEEL_DOWN: u8 = 16;
const WHEEL_LEFT: u8 = 32;
const WHEEL_RIGHT: u8 = 64;
const WHEEL_DELTA: i32 = 120;

/// Where input is sent: the RFB writer (key symbols) or the native host (keycodes).
#[derive(Clone)]
enum Sink {
    Rfb(Sender<Out>),
    Native(tokio::sync::mpsc::UnboundedSender<Input>),
}

/// How keyboard events become wire events for the chosen backend.
enum KeyTranslator {
    Rfb(RfbTranslator),
    Native(NativeInput),
}

/// Which backend a session uses, chosen at startup (RFB or the Phase 2 native host).
pub enum Backend {
    Rfb,
    Native(tokio::sync::mpsc::UnboundedSender<Input>),
}

/// Keyboard state shared by the hook thread and the UI thread.
struct KeyCtx {
    hwnd: AtomicIsize,
    view_only: bool,
    state: Mutex<KeyState>,
}

struct KeyState {
    engine: Engine,
    translator: KeyTranslator,
    sink: Sink,
    /// Caps Lock toggle, tracked from the hook: the hook thread receives no keyboard
    /// messages, so its own GetKeyState would be stale.
    caps: bool,
    caps_held: bool,
    keylog: Option<std::io::BufWriter<std::fs::File>>,
}

impl KeyState {
    /// Sends host events to the backend (unless view-only) and returns `(code, down)` pairs
    /// for the key log (a key symbol for RFB, a keycode for native).
    fn send_host(&mut self, events: Vec<HostEvent>, view_only: bool) -> Vec<(u32, bool)> {
        let mut sent = Vec::new();
        let caps = self.caps;
        match (&mut self.translator, &self.sink) {
            (KeyTranslator::Rfb(translator), Sink::Rfb(tx)) => {
                translator.set_caps_lock(caps);
                for event in events {
                    for (keysym, down) in translator.translate(event) {
                        if !view_only {
                            let _ = tx.send(Out::Key(keysym, down));
                        }
                        sent.push((keysym, down));
                    }
                }
            }
            (KeyTranslator::Native(input), Sink::Native(tx)) => {
                for event in events {
                    if let Some(k) = input.translate(event) {
                        if !view_only {
                            let _ = tx.send(Input::Key { keycode: k.keycode, down: k.down, flags: k.flags });
                        }
                        sent.push((u32::from(k.keycode), k.down));
                    }
                }
            }
            _ => {}
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

/// Pixels the native host scrolls per standard wheel notch (WHEEL_DELTA). Configurable via the
/// AKTIVESTREAM_SCROLL env var (a float; higher is faster). Default 160; Apple's own notch is
/// roughly 120. Touchpad "precision" wheel messages carry smaller deltas and scale the same way.
fn scroll_px_from_env() -> f32 {
    std::env::var("AKTIVESTREAM_SCROLL")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(160.0)
}

/// An inactive session's handle, kept so Right Ctrl + Left/Right can swap it into the active
/// slot. The active session lives in the `App`'s own `shared`/`sink`/`name` fields, so all the
/// single-session rendering, mouse and clipboard code keeps working against whichever is active.
pub(crate) struct WinSession {
    shared: Arc<Shared>,
    sink: Sink,
    name: String,
}

struct App {
    hwnd: HWND,
    hook_thread: u32,
    shared: Arc<Shared>,
    sink: Sink,
    name: String,
    /// The other open sessions, in ring order (empty for a single session).
    others: VecDeque<WinSession>,
    view: View,
    renderer: Option<Renderer>,
    buttons: u8,
    left_sent_as: MouseButton,
    last_pointer: (u16, u16),
    cursor_px: (i32, i32),
    bar_shown: bool,
    bar_force: bool,
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
    scroll_px: f32,
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
        Some(LocalAction::NextSession) => Some(LOCAL_NEXT_SESSION),
        Some(LocalAction::PrevSession) => Some(LOCAL_PREV_SESSION),
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
        match &self.sink {
            Sink::Rfb(tx) => {
                let _ = tx.send(message);
            }
            Sink::Native(tx) => match message {
                Out::Pointer(buttons, x, y) => {
                    let (fw, fh) = (self.view.fb_w.max(1) as f32, self.view.fb_h.max(1) as f32);
                    // Wire button order: bit0 left, bit1 right, bit2 middle.
                    let native = (buttons & 1)
                        | (if buttons & BUTTON_RIGHT != 0 { 2 } else { 0 })
                        | (if buttons & BUTTON_MIDDLE != 0 { 4 } else { 0 });
                    let _ = tx.send(Input::Pointer { buttons: native, x: f32::from(x) / fw, y: f32::from(y) / fh });
                }
                // Key events go through KeyState for native; clipboard sync is not wired yet.
                Out::Key(..) | Out::CutText(_) => {}
            },
        }
    }

    fn is_native(&self) -> bool {
        matches!(self.sink, Sink::Native(_))
    }

    fn send_scroll(&self, dx: f32, dy: f32) {
        if let Sink::Native(tx) = &self.sink {
            let _ = tx.send(Input::Scroll { dx, dy });
        }
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

    /// Swaps the active session with the next (`delta >= 0`) or previous one in the ring; returns
    /// the new active session's name, or `None` when there is only one session.
    fn switch(&mut self, delta: isize) -> Option<String> {
        if self.others.is_empty() {
            return None;
        }
        let current = WinSession { shared: self.shared.clone(), sink: self.sink.clone(), name: self.name.clone() };
        let next = if delta >= 0 {
            self.others.push_back(current);
            self.others.pop_front()
        } else {
            self.others.push_front(current);
            self.others.pop_back()
        }
        .expect("others is non-empty");
        self.shared = next.shared;
        self.sink = next.sink;
        self.name = next.name.clone();
        Some(next.name)
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

    /// Whether the overlay toolbar should be shown: full screen, with the pointer at the top edge.
    fn bar_visible(&self) -> bool {
        (self.fullscreen || self.bar_force)
            && self.cursor_px.0 >= 0
            && self.cursor_px.1 >= 0
            && (self.cursor_px.1 as usize) < bar::BAR_HEIGHT
    }

    /// Builds the overlay toolbar for the current window width (full width, `BAR_HEIGHT` tall,
    /// `0x00RRGGBB`), highlighting whichever button the pointer is over. `None` when hidden.
    fn bar_overlay(&self) -> Option<(Vec<u32>, u32)> {
        if !self.bar_visible() {
            return None;
        }
        let cw = self.client_size().0.max(1) as usize;
        let mut buf = vec![0u32; cw * bar::BAR_HEIGHT];
        let hover = bar::bar_action_at((self.cursor_px.0 as f64, self.cursor_px.1 as f64), cw);
        bar::draw_bar(&mut buf, cw, bar::BAR_HEIGHT, hover, &self.name);
        Some((buf, cw as u32))
    }

    /// Uploads `area` (None = everything) and draws one frame with Direct3D. Returns false
    /// when there is no usable renderer, so the caller falls back to GDI.
    fn render(&mut self, area: Option<Rect>, capture: bool) -> bool {
        let started = Instant::now();
        let overlay = self.bar_overlay();
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
        if let Some((buf, w)) = &overlay {
            renderer.draw_bar(buf, *w, bar::BAR_HEIGHT as u32);
        }
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
        self.cursor_px = (point.x, point.y);
        if self.fullscreen {
            let over = point.x >= 0 && point.y >= 0 && (point.y as usize) < bar::BAR_HEIGHT;
            if over || self.bar_shown {
                // Repaint the top strip so the toolbar reveals, updates its hover, or clears.
                let strip =
                    RECT { left: 0, top: 0, right: self.client_size().0.max(1), bottom: bar::BAR_HEIGHT as i32 };
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd), Some(&strip), false);
                }
            }
            self.bar_shown = over;
            if over {
                if message == WM_LBUTTONDOWN {
                    let cw = self.client_size().0.max(1) as usize;
                    let code = match bar::bar_action_at((point.x as f64, point.y as f64), cw) {
                        Some(bar::BarAction::Windowed) => Some(LOCAL_FULLSCREEN),
                        Some(bar::BarAction::Disconnect) => Some(LOCAL_DISCONNECT),
                        None => None,
                    };
                    if let Some(code) = code {
                        unsafe {
                            let _ = PostMessageW(Some(self.hwnd), WM_APP_LOCAL, WPARAM(code), LPARAM(0));
                        }
                    }
                }
                // Swallow all pointer input over the toolbar so it never reaches the Mac.
                return;
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
                if self.is_native() {
                    // Native host takes pixel scroll deltas; scale a notch to scroll_px pixels.
                    self.send_scroll(0.0, wheel_delta as f32 / WHEEL_DELTA as f32 * self.scroll_px);
                    return;
                }
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
                if self.is_native() {
                    self.send_scroll(wheel_delta as f32 / WHEEL_DELTA as f32 * self.scroll_px, 0.0);
                    return;
                }
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

/// Cycles the active session (Right Ctrl + Left/Right): releases held input on the one being left,
/// swaps the next into the active slot, repoints the keyboard sink, retitles, and repaints.
fn switch_session(hwnd: HWND, delta: isize) {
    with_app(App::release_all);
    let new_sink = with_app(|app| app.switch(delta).map(|_| app.sink.clone()));
    if let Some(Some(sink)) = new_sink {
        keys().sink = sink;
        with_app(App::update_view);
        update_title(hwnd);
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
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
    // Overlay toolbar (full screen only), composited 1:1 over the top of the framebuffer blit.
    if let Some((buf, bw)) = with_app(|app| app.bar_overlay()).flatten() {
        let bh = bar::BAR_HEIGHT as i32;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bw as i32,
                biHeight: -bh,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            SetStretchBltMode(hdc, COLORONCOLOR);
            StretchDIBits(
                hdc,
                0,
                0,
                bw as i32,
                bh,
                0,
                0,
                bw as i32,
                bh,
                Some(buf.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
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
            let captured = with_app(|app| {
                if app.screenshot.is_none() {
                    return false;
                }
                if app.bar_force {
                    // Diagnostic (AKTIVE_BAR_SHOT): force the toolbar on and hover the leave-full-screen
                    // button so the capture exercises the Direct3D overlay path end to end.
                    let cw = app.client_size().0.max(1);
                    app.cursor_px = (cw - bar::BAR_HEIGHT as i32 * 2 + 4, 6);
                }
                app.render(None, true)
            })
            .unwrap_or(false);
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
                LOCAL_NEXT_SESSION => switch_session(hwnd, 1),
                LOCAL_PREV_SESSION => switch_session(hwnd, -1),
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
    run_window(args, shared, info.name, Backend::Rfb, Vec::new(), move |shared, rx| {
        net::spawn_reader(shared.clone(), reader);
        net::spawn_writer(shared, writer, rx);
    })
}

/// Opens multiple RFB sessions in one window; the first is active and the rest cycle in with
/// Right Ctrl + Left/Right. Each gets its own reader and writer threads.
pub fn run_multi(
    args: &Args,
    sessions: Vec<(String, (ServerInfo, SessionReader, SessionWriter))>,
) -> Result<(), String> {
    let mut sessions = sessions.into_iter();
    let (active_name, (active_info, active_reader, active_writer)) = sessions.next().ok_or("no sessions to open")?;
    let active_shared = Arc::new(Shared::new(active_info.width, active_info.height));

    let mut others = Vec::new();
    for (name, (info, reader, writer)) in sessions {
        let shared = Arc::new(Shared::new(info.width, info.height));
        let (tx, rx) = mpsc::channel();
        net::spawn_reader(shared.clone(), reader);
        net::spawn_writer(shared.clone(), writer, rx);
        others.push(WinSession { shared, sink: Sink::Rfb(tx), name });
    }

    run_window(args, active_shared, active_name, Backend::Rfb, others, move |shared, rx| {
        net::spawn_reader(shared.clone(), active_reader);
        net::spawn_writer(shared, active_writer, rx);
    })
}

/// The window, renderer, keyboard and event loop, shared by the RFB and native paths. The
/// `shared` framebuffer is already sized to the remote screen; `start_source` spawns whatever
/// fills it (the RFB reader/writer, or the native QUIC/decode thread).
pub fn run_window(
    args: &Args,
    shared: Arc<Shared>,
    host_name: String,
    backend: Backend,
    others: Vec<WinSession>,
    start_source: impl FnOnce(Arc<Shared>, mpsc::Receiver<Out>),
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let (sink, translator) = match backend {
        Backend::Rfb => (Sink::Rfb(tx.clone()), KeyTranslator::Rfb(RfbTranslator::new())),
        Backend::Native(input_tx) => (Sink::Native(input_tx), KeyTranslator::Native(NativeInput::new())),
    };
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
    for other in &others {
        other.shared.set_hwnd(hwnd);
    }

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
            translator,
            sink: sink.clone(),
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
        sink,
        name: host_name,
        others: others.into(),
        view: View::default(),
        renderer,
        buttons: 0,
        left_sent_as: MouseButton::Left,
        last_pointer: (0, 0),
        cursor_px: (0, 0),
        bar_shown: false,
        bar_force: std::env::var_os("AKTIVE_BAR_SHOT").is_some(),
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
        scroll_px: scroll_px_from_env(),
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
