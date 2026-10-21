//! AktiveStream desktop client (Linux and macOS): the RFB path to macOS Screen Sharing in a winit
//! window via a softbuffer (CPU) surface, with keyboard and mouse driven through the shared
//! `aktivestream_keymap` macOS keyboard profile (same behaviour as the Windows client). The native
//! QUIC + hardware-decode path (for Windows-level performance) is the next milestone; this shell
//! is what it builds on. Reuses the platform-neutral `aktivestream-rfb` and `aktivestream-keymap`.
//!
//! Usage: aktivestream <host> [port]   (env RFB_USER, RFB_PASSWORD)

#[cfg(target_os = "macos")]
mod event_tap;
mod keys;
#[cfg(feature = "native")]
mod native;

use aktivestream_client::{bar, config, picker, tunnel};

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use aktivestream_keymap::native::NativeInput;
use aktivestream_keymap::{Engine, HostEvent, KeyInput, LocalAction, MouseButton, Profile, RfbTranslator};
use aktivestream_proto::Input;
use aktivestream_rfb::{ConnectOptions, CursorShape, Framebuffer, ServerEvent, SessionReader, SessionWriter, connect};
use softbuffer::{Context, Surface};
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton as WinitButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::PhysicalKey;
use winit::window::{Fullscreen, Window, WindowId};

/// Shared state between the RFB reader thread and the UI thread.
/// The keyboard profile for this build: the macOS client drives a remote Mac from a Mac keyboard
/// (identity), every other target drives it from a PC keyboard (the remap profile).
fn default_profile() -> Profile {
    #[cfg(target_os = "macos")]
    {
        Profile::mac_native()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Profile::mac_default()
    }
}

struct Shared {
    framebuffer: Mutex<Framebuffer>,
    error: Mutex<Option<String>>,
    /// The last clipboard text synced in either direction, so neither side echoes it back.
    last_clip: Mutex<Option<String>>,
    /// A newly-received remote cursor shape, picked up and applied by the UI thread.
    cursor: Mutex<Option<CursorShape>>,
}

impl Shared {
    fn new(width: u32, height: u32) -> Self {
        Shared {
            framebuffer: Mutex::new(Framebuffer::new(width, height)),
            error: Mutex::new(None),
            last_clip: Mutex::new(None),
            cursor: Mutex::new(None),
        }
    }
}

/// Pulls updates forever, applying each to the shared framebuffer and waking the UI thread. The
/// reader itself re-requests the next update, so this never touches the writer.
fn run_reader(shared: Arc<Shared>, proxy: EventLoopProxy<()>, mut reader: SessionReader, clipboard_enabled: bool) {
    // Clipboard owner for Mac -> Linux pushes; only one session owns the clipboard, and only if
    // there is a display to serve it.
    let mut clipboard = if clipboard_enabled { arboard::Clipboard::new().ok() } else { None };
    loop {
        match reader.next_event(&shared.framebuffer) {
            Ok(ServerEvent::Update { .. } | ServerEvent::Resized { .. }) => {
                let _ = proxy.send_event(());
            }
            Ok(ServerEvent::CutText(text)) => apply_remote_clipboard(&shared, clipboard.as_mut(), text),
            Ok(ServerEvent::Cursor(shape)) => {
                *shared.cursor.lock().unwrap() = Some(shape);
                let _ = proxy.send_event(());
            }
            Ok(_) => {}
            Err(e) => {
                *shared.error.lock().unwrap() = Some(format!("session: {e}"));
                let _ = proxy.send_event(());
                return;
            }
        }
    }
}

/// Mac -> Linux: store the Mac's clipboard text locally, unless it is what we last synced (our own
/// echo), and remember it so the poller does not send it straight back.
fn apply_remote_clipboard(shared: &Shared, clipboard: Option<&mut arboard::Clipboard>, text: String) {
    let Some(clipboard) = clipboard else { return };
    {
        let mut last = shared.last_clip.lock().unwrap();
        if last.as_deref() == Some(text.as_str()) {
            return;
        }
        *last = Some(text.clone());
    }
    eprintln!("clipboard <- Mac: {} chars", text.chars().count());
    let _ = clipboard.set_text(text);
}

/// Linux -> Mac: the X11 clipboard has no change event, so poll it and forward changes as
/// ClientCutText. Seeded with the current clipboard so launch-time content is not resent.
fn run_clipboard_poller(shared: Arc<Shared>, mut writer: SessionWriter) {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("clipboard: disabled ({e})");
            return;
        }
    };
    if let Ok(text) = clipboard.get_text() {
        *shared.last_clip.lock().unwrap() = Some(text);
    }
    loop {
        thread::sleep(Duration::from_millis(700));
        let Ok(text) = clipboard.get_text() else { continue };
        {
            let mut last = shared.last_clip.lock().unwrap();
            if last.as_deref() == Some(text.as_str()) {
                continue;
            }
            *last = Some(text.clone());
        }
        eprintln!("clipboard -> Mac: {} chars", text.chars().count());
        if writer.cut_text(&text).is_err() {
            return; // Session closed.
        }
    }
}

/// One open connection: its framebuffer, how input reaches it, a title, the last cursor shape it
/// sent, and (RFB) the SSH tunnel kept alive for the session's lifetime.
struct Session {
    shared: Arc<Shared>,
    backend: Backend,
    title: String,
    cursor_shape: Option<CursorShape>,
    _tunnel: Option<tunnel::Tunnel>,
}

impl Session {
    /// A clone of the RFB writer (for the clipboard poller), or `None` for the native backend.
    fn rfb_writer(&self) -> Option<SessionWriter> {
        match &self.backend {
            Backend::Rfb { writer, .. } => Some(writer.clone()),
            Backend::Native { .. } => None,
        }
    }
}

/// System shortcuts (e.g. Cmd+Tab) captured by the macOS event tap, queued for the UI thread to
/// feed through the engine. Stays empty on platforms without the tap.
type SystemKeys = Arc<Mutex<VecDeque<(aktivestream_keymap::Key, bool)>>>;

struct App {
    /// The open sessions; `active` is the one shown in the window and receiving input. Right Ctrl
    /// + Left/Right cycles between them.
    sessions: Vec<Session>,
    active: usize,
    engine: Engine,
    buttons: u8,
    pointer: (u16, u16),
    cursor: (f64, f64),
    caps: bool,
    fullscreen: bool,
    bar_shown: bool,
    /// Pixels per wheel notch for the native path (AKTIVESTREAM_SCROLL), as on Windows.
    scroll_px: f32,
    /// System shortcuts from the macOS event tap, drained on each UI wake-up.
    system_keys: SystemKeys,
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
}

/// Where input goes and how it is encoded: RFB keysyms/pointer, or native `Input` messages over
/// the QUIC channel. `buttons` is kept in the RFB bit order and remapped for native.
enum Backend {
    Rfb {
        writer: SessionWriter,
        translator: RfbTranslator,
    },
    /// Constructed only on Linux (the native path); harmless and unused elsewhere.
    #[allow(dead_code)]
    Native {
        tx: UnboundedSender<Input>,
        translator: NativeInput,
    },
}

/// RFB button bits (left=1, middle=2, right=4) to native bits (left=1, right=2, middle=4).
fn native_buttons(rfb: u8) -> u8 {
    let mut n = 0;
    if rfb & BUTTON_LEFT != 0 {
        n |= 0x1;
    }
    if rfb & BUTTON_RIGHT != 0 {
        n |= 0x2;
    }
    if rfb & BUTTON_MIDDLE != 0 {
        n |= 0x4;
    }
    n
}

const BUTTON_LEFT: u8 = 0x01;
const BUTTON_MIDDLE: u8 = 0x02;
const BUTTON_RIGHT: u8 = 0x04;
const WHEEL_UP: u8 = 0x08;
const WHEEL_DOWN: u8 = 0x10;

fn button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => BUTTON_LEFT,
        MouseButton::Middle => BUTTON_MIDDLE,
        MouseButton::Right => BUTTON_RIGHT,
    }
}

impl App {
    fn new(sessions: Vec<Session>) -> Self {
        App {
            sessions,
            active: 0,
            engine: Engine::new(default_profile()),
            buttons: 0,
            pointer: (0, 0),
            cursor: (0.0, 0.0),
            caps: false,
            fullscreen: false,
            bar_shown: false,
            scroll_px: scroll_px_from_env(),
            system_keys: Arc::new(Mutex::new(VecDeque::new())),
            window: None,
            surface: None,
        }
    }

    /// Cycles to the next (`delta` = 1) or previous (-1) session: releases keys and buttons on the
    /// one being left, then shows and starts driving the new one.
    fn switch(&mut self, delta: isize, event_loop: &ActiveEventLoop) {
        if self.sessions.len() < 2 {
            return;
        }
        self.release_all();
        let n = self.sessions.len() as isize;
        self.active = (((self.active as isize + delta) % n + n) % n) as usize;
        if let Some(window) = &self.window {
            window.set_title(&format!(
                "{} - AktiveStream ({}/{})",
                self.sessions[self.active].title,
                self.active + 1,
                n
            ));
            window.request_redraw();
        }
        self.apply_cursor(event_loop);
    }

    /// Builds and applies the remote cursor at the current framebuffer-to-window scale (the Mac's
    /// cursor shape, so I-beams, resize arrows and the like match what the Mac shows).
    fn apply_cursor(&self, event_loop: &ActiveEventLoop) {
        let (Some(shape), Some(window)) = (&self.sessions[self.active].cursor_shape, &self.window) else {
            return;
        };
        let dw = f64::from(window.inner_size().width);
        let sw = f64::from(self.sessions[self.active].shared.framebuffer.lock().unwrap().width);
        let scale = if sw > 0.0 { dw / sw } else { 1.0 };
        let Some((rgba, w, h, hx, hy)) = scaled_cursor_rgba(shape, scale) else {
            return;
        };
        if let Ok(source) = winit::window::CustomCursor::from_rgba(rgba, w, h, hx, hy) {
            window.set_cursor(event_loop.create_custom_cursor(source));
        }
    }

    /// Nearest-neighbour blit of the remote framebuffer, scaled to the window. Pixels are already
    /// `0x00RRGGBB`, so no colour conversion is needed.
    fn redraw(&mut self) {
        let (Some(window), Some(surface)) = (self.window.clone(), self.surface.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (Some(dw), Some(dh)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if surface.resize(dw, dh).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else { return };
        let (dw, dh) = (dw.get() as usize, dh.get() as usize);

        let fb = self.sessions[self.active].shared.framebuffer.lock().unwrap();
        let (sw, sh) = (fb.width as usize, fb.height as usize);
        if sw == 0 || sh == 0 {
            buffer.fill(0);
        } else {
            for y in 0..dh {
                let src_row = (y * sh / dh) * sw;
                let dst_row = y * dw;
                for x in 0..dw {
                    buffer[dst_row + x] = fb.pixels[src_row + (x * sw / dw)] & 0x00ff_ffff;
                }
            }
        }
        drop(fb);
        if self.fullscreen && self.bar_shown {
            let title = &self.sessions[self.active].title;
            bar::draw_bar(&mut buffer, dw, dh, bar::bar_action_at(self.cursor, dw), title);
        }
        let _ = buffer.present();
    }

    fn send_host_events(&mut self, events: Vec<HostEvent>) {
        let caps = self.caps;
        match &mut self.sessions[self.active].backend {
            Backend::Rfb { writer, translator } => {
                translator.set_caps_lock(caps);
                for event in events {
                    for (keysym, down) in translator.translate(event) {
                        let _ = writer.key_event(keysym, down);
                    }
                }
            }
            Backend::Native { tx, translator } => {
                for event in events {
                    if let Some(nk) = translator.translate(event) {
                        let _ = tx.send(Input::Key { keycode: nk.keycode, down: nk.down, flags: nk.flags });
                    }
                }
            }
        }
    }

    fn on_key(&mut self, event: KeyEvent, event_loop: &ActiveEventLoop) {
        let PhysicalKey::Code(code) = event.physical_key else { return };
        self.feed_key(keys::to_key(code), event.state == ElementState::Pressed, event_loop);
    }

    /// Runs one key transition through the engine and acts on it: local shortcuts here, host key
    /// events to the active session. Shared by the winit keyboard and the macOS event tap.
    fn feed_key(&mut self, key: aktivestream_keymap::Key, down: bool, event_loop: &ActiveEventLoop) {
        if key == aktivestream_keymap::Key::CapsLock && down {
            self.caps = !self.caps;
        }
        let output = self.engine.handle_key(KeyInput { key, down });
        match output.local {
            Some(LocalAction::ToggleFullscreen) => self.toggle_fullscreen(),
            Some(LocalAction::Disconnect) => event_loop.exit(),
            Some(LocalAction::NextSession) => self.switch(1, event_loop),
            Some(LocalAction::PrevSession) => self.switch(-1, event_loop),
            _ => {}
        }
        self.send_host_events(output.host);
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        if !self.fullscreen {
            self.bar_shown = false;
        }
        if let Some(window) = &self.window {
            window.set_fullscreen(self.fullscreen.then_some(Fullscreen::Borderless(None)));
        }
        self.engine.set_fullscreen(self.fullscreen);
        self.request_redraw();
    }

    fn to_framebuffer(&self, x: f64, y: f64) -> (u16, u16) {
        let size = self.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
        let fb = self.sessions[self.active].shared.framebuffer.lock().unwrap();
        let (sw, sh) = (fb.width.max(1), fb.height.max(1));
        let fx = (x.max(0.0) as u32 * sw / size.width.max(1)).min(sw - 1);
        let fy = (y.max(0.0) as u32 * sh / size.height.max(1)).min(sh - 1);
        (fx as u16, fy as u16)
    }

    fn send_pointer(&mut self) {
        let (x, y) = self.pointer;
        let buttons = self.buttons;
        let (fw, fh) = {
            let fb = self.sessions[self.active].shared.framebuffer.lock().unwrap();
            (fb.width.max(1) as f32, fb.height.max(1) as f32)
        };
        match &mut self.sessions[self.active].backend {
            Backend::Rfb { writer, .. } => {
                let _ = writer.pointer_event(buttons, x, y);
            }
            Backend::Native { tx, .. } => {
                let _ =
                    tx.send(Input::Pointer { buttons: native_buttons(buttons), x: x as f32 / fw, y: y as f32 / fh });
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn on_mouse_move(&mut self, x: f64, y: f64) {
        self.cursor = (x, y);
        let over_bar = self.fullscreen && y < bar::BAR_HEIGHT as f64;
        if over_bar != self.bar_shown {
            self.bar_shown = over_bar;
            self.request_redraw();
        } else if over_bar {
            self.request_redraw(); // refresh the hover highlight
        }
        if over_bar {
            return; // the toolbar strip does not drive the Mac cursor
        }
        self.pointer = self.to_framebuffer(x, y);
        self.send_pointer();
    }

    fn on_mouse_button(&mut self, button: WinitButton, pressed: bool, event_loop: &ActiveEventLoop) {
        // Toolbar clicks in full screen take priority over driving the host.
        if self.bar_shown && button == WinitButton::Left {
            if pressed {
                let dw = self.window.as_ref().map_or(0, |w| w.inner_size().width as usize);
                match bar::bar_action_at(self.cursor, dw) {
                    Some(bar::BarAction::Windowed) => self.toggle_fullscreen(),
                    Some(bar::BarAction::Disconnect) => event_loop.exit(),
                    None => {}
                }
            }
            return;
        }
        let button = match button {
            WinitButton::Left => MouseButton::Left,
            WinitButton::Right => MouseButton::Right,
            WinitButton::Middle => MouseButton::Middle,
            _ => return,
        };
        let (events, effective) = self.engine.mouse_button(button, pressed);
        let bit = button_bit(effective);
        if pressed {
            // Send the Ctrl-release host events (if any) before the button-down.
            self.send_host_events(events);
            self.buttons |= bit;
            self.send_pointer();
        } else {
            // Button-up first, then restore any modifiers.
            self.buttons &= !bit;
            self.send_pointer();
            self.send_host_events(events);
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let notches = match delta {
            MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
            MouseScrollDelta::PixelDelta(p) => (p.y / 40.0).round() as i32,
        };
        let (x, y) = self.pointer;
        let buttons = self.buttons;
        match &mut self.sessions[self.active].backend {
            Backend::Rfb { writer, .. } => {
                let (bit, count) = if notches >= 0 { (WHEEL_UP, notches) } else { (WHEEL_DOWN, -notches) };
                for _ in 0..count {
                    let _ = writer.pointer_event(buttons | bit, x, y);
                    let _ = writer.pointer_event(buttons, x, y);
                }
            }
            Backend::Native { tx, .. } => {
                // The native host takes pixel scroll deltas; a notch is `scroll_px` pixels (up
                // positive), configurable via AKTIVESTREAM_SCROLL like the Windows client.
                let _ = tx.send(Input::Scroll { dx: 0.0, dy: notches as f32 * self.scroll_px });
            }
        }
    }

    fn release_all(&mut self) {
        let events = self.engine.release_all();
        self.send_host_events(events);
        if self.buttons != 0 {
            self.buttons = 0;
            self.send_pointer();
        }
    }
}

/// Pixels per wheel notch for the native path, from AKTIVESTREAM_SCROLL (default 160), matching the
/// Windows client so scroll speed is the same across clients.
fn scroll_px_from_env() -> f32 {
    std::env::var("AKTIVESTREAM_SCROLL")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(160.0)
}

/// Resamples the remote cursor (`0xAARRGGBB` pixels) by `scale` (window width / framebuffer width,
/// matching the framebuffer blit) to winit RGBA bytes, with the hotspot scaled to match. Returns
/// `(rgba, width, height, hotspot_x, hotspot_y)`.
fn scaled_cursor_rgba(shape: &CursorShape, scale: f64) -> Option<(Vec<u8>, u16, u16, u16, u16)> {
    let (sw, sh) = (shape.width as usize, shape.height as usize);
    if sw == 0 || sh == 0 || shape.pixels.len() < sw * sh {
        return None;
    }
    let w = ((sw as f64 * scale).round() as usize).max(1);
    let h = ((sh as f64 * scale).round() as usize).max(1);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let sy = (y * sh / h).min(sh - 1);
        for x in 0..w {
            let sx = (x * sw / w).min(sw - 1);
            let px = shape.pixels[sy * sw + sx];
            rgba.push((px >> 16) as u8); // R
            rgba.push((px >> 8) as u8); // G
            rgba.push(px as u8); // B
            rgba.push((px >> 24) as u8); // A
        }
    }
    let hx = ((f64::from(shape.hotspot_x) * scale).round() as u16).min(w as u16 - 1);
    let hy = ((f64::from(shape.hotspot_y) * scale).round() as u16).min(h as u16 - 1);
    Some((rgba, w as u16, h as u16, hx, hy))
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes =
            Window::default_attributes().with_title("AktiveStream").with_inner_size(LogicalSize::new(1280.0, 800.0));
        let window = match event_loop.create_window(attributes) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("window: {e}");
                event_loop.exit();
                return;
            }
        };
        match Context::new(window.clone()).and_then(|ctx| Surface::new(&ctx, window.clone())) {
            Ok(surface) => self.surface = Some(surface),
            Err(e) => {
                eprintln!("surface: {e}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window.clone());
        window.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: ()) {
        // System shortcuts captured by the macOS event tap (e.g. Cmd+Tab), fed through the engine
        // like any other key so the active session's Mac receives them.
        let captured: Vec<_> = { self.system_keys.lock().unwrap().drain(..).collect() };
        for (key, down) in captured {
            self.feed_key(key, down, event_loop);
        }
        // Surface any session's error (for now one failed session ends the app).
        for s in &self.sessions {
            if let Some(error) = s.shared.error.lock().unwrap().take() {
                eprintln!("{error}");
                event_loop.exit();
                return;
            }
        }
        // Keep each session's cursor shape current; apply the active one's.
        let active = self.active;
        let mut active_changed = false;
        for (i, s) in self.sessions.iter_mut().enumerate() {
            if let Some(shape) = s.shared.cursor.lock().unwrap().take() {
                s.cursor_shape = Some(shape);
                active_changed |= i == active;
            }
        }
        if active_changed {
            self.apply_cursor(event_loop);
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_) => {
                self.apply_cursor(event_loop);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event, event_loop),
            WindowEvent::CursorMoved { position, .. } => self.on_mouse_move(position.x, position.y),
            WindowEvent::MouseInput { state, button, .. } => {
                self.on_mouse_button(button, state == ElementState::Pressed, event_loop);
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::Focused(false) => self.release_all(),
            _ => {}
        }
    }
}

/// Connects, decodes the first framebuffer update, writes it to a PNG, and exits. Lets us verify
/// the RFB pipeline on Linux without a display server or a screenshot tool.
/// The connection picker window, shown when the client starts with no target. On a choice it
/// launches `aktivestream <name>` as a fresh process (its own session window) and closes, so the
/// picker and the session never share a winit event loop.
struct PickerApp {
    entries: Vec<config::Connection>,
    selected: usize,
    cursor: (f64, f64),
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    chosen: Option<String>,
}

impl PickerApp {
    fn redraw(&mut self) {
        let (Some(window), Some(surface)) = (self.window.clone(), self.surface.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (Some(dw), Some(dh)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if surface.resize(dw, dh).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        picker::draw_picker(&mut buffer, dw.get() as usize, dh.get() as usize, &self.entries, self.selected);
        let _ = buffer.present();
    }

    fn window_height(&self) -> usize {
        self.window.as_ref().map_or(0, |w| w.inner_size().height as usize)
    }

    fn step(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len() as isize;
        self.selected = (((self.selected as isize + delta) % n + n) % n) as usize;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn choose(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(c) = self.entries.get(self.selected) {
            self.chosen = Some(c.name.clone());
        }
        event_loop.exit();
    }
}

impl ApplicationHandler<()> for PickerApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let rows = self.entries.len().max(1);
        let height = (picker::TOP + rows * picker::ROW_H + 24).min(760) as f64;
        let attrs =
            Window::default_attributes().with_title("AktiveStream").with_inner_size(LogicalSize::new(680.0, height));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("window: {e}");
                event_loop.exit();
                return;
            }
        };
        match Context::new(window.clone()).and_then(|ctx| Surface::new(&ctx, window.clone())) {
            Ok(s) => self.surface = Some(s),
            Err(e) => {
                eprintln!("surface: {e}");
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window.clone());
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_) => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if let Some(idx) = picker::row_at(self.cursor, self.window_height(), self.entries.len())
                    && idx != self.selected
                {
                    self.selected = idx;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: WinitButton::Left, .. } => {
                if let Some(idx) = picker::row_at(self.cursor, self.window_height(), self.entries.len()) {
                    self.selected = idx;
                    self.choose(event_loop);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    use winit::keyboard::KeyCode;
                    match code {
                        KeyCode::ArrowDown => self.step(1),
                        KeyCode::ArrowUp => self.step(-1),
                        KeyCode::Enter | KeyCode::NumpadEnter => self.choose(event_loop),
                        KeyCode::Escape => event_loop.exit(),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Shows the connection picker; on a choice, launches `aktivestream <name>` as a fresh process.
fn run_picker(entries: Vec<config::Connection>) -> Result<(), String> {
    let event_loop = EventLoop::<()>::with_user_event().build().map_err(|e| e.to_string())?;
    let mut app = PickerApp { entries, selected: 0, cursor: (0.0, 0.0), window: None, surface: None, chosen: None };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    if let Some(name) = app.chosen {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Command::new(exe).arg(&name).spawn().map_err(|e| format!("launch {name}: {e}"))?;
    }
    Ok(())
}

fn run_snapshot(options: ConnectOptions, path: &str) -> Result<(), String> {
    let (info, mut reader, _writer) = connect(&options).map_err(|e| e.to_string())?;
    let framebuffer = Mutex::new(Framebuffer::new(info.width as u32, info.height as u32));
    eprintln!("connected to {} ({}x{})", info.name, info.width, info.height);
    for _ in 0..240 {
        if let ServerEvent::Update { .. } = reader.next_event(&framebuffer).map_err(|e| e.to_string())? {
            write_png(&framebuffer.lock().unwrap(), path)?;
            eprintln!("wrote snapshot {path} (bytes read: {})", reader.stats().bytes_read);
            return Ok(());
        }
    }
    Err("no framebuffer update received".into())
}

/// Connects and moves the remote pointer to a few framebuffer coordinates with pauses, so a
/// watcher on the Mac (e.g. /tmp/mousepos) can confirm the RFB input channel works from Linux.
fn run_input_test(options: ConnectOptions) -> Result<(), String> {
    let (info, mut reader, mut writer) = connect(&options).map_err(|e| e.to_string())?;
    eprintln!("connected to {} ({}x{})", info.name, info.width, info.height);
    // Drain updates so the socket stays healthy while we send pointer events.
    let framebuffer = Arc::new(Mutex::new(Framebuffer::new(info.width as u32, info.height as u32)));
    let drain = framebuffer.clone();
    thread::spawn(move || while reader.next_event(&drain).is_ok() {});

    // Sweep a few framebuffer coordinates with pauses. Note: Apple's Screen Sharing only applies
    // injected pointer input when the Mac's console is owned by a logged-in user session; on a
    // headless Mac (loginwindow owns the console) the moves are sent but not applied (the native
    // agent path is the reliable one). The client-side output is unit-tested regardless.
    let (w, h) = (info.width, info.height);
    for (x, y) in [(w / 2, h / 2), (w / 4, h / 4), (w - 100, h / 3), (w / 2, h - 100)] {
        writer.pointer_event(0, x, y).map_err(|e| e.to_string())?;
        eprintln!("pointer -> ({x}, {y})");
        thread::sleep(Duration::from_millis(900));
    }
    Ok(())
}

fn write_png(framebuffer: &Framebuffer, path: &str) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), framebuffer.width, framebuffer.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity(framebuffer.pixels.len() * 3);
    for p in &framebuffer.pixels {
        rgb.push((p >> 16) as u8);
        rgb.push((p >> 8) as u8);
        rgb.push(*p as u8);
    }
    writer.write_image_data(&rgb).map_err(|e| e.to_string())
}

/// Renders the overlay toolbar over a gradient (stand-in for the video) to a PNG, so the look can
/// be checked without a display. Pure-local; no connection.
fn bar_preview(path: &str) -> Result<(), String> {
    let (w, h) = (1280usize, 180usize);
    let mut buf = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            buf[y * w + x] = (((x * 255 / w) as u32) << 16) | (((y * 255 / h) as u32) << 8) | 0x50;
        }
    }
    bar::draw_bar(&mut buf, w, h, Some(bar::BarAction::Disconnect), "studio - Mac Studio");

    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for p in &buf {
        rgb.push((p >> 16) as u8);
        rgb.push((p >> 8) as u8);
        rgb.push(*p as u8);
    }
    writer.write_image_data(&rgb).map_err(|e| e.to_string())
}

/// Headless render of the connection picker to a PNG (AKTIVE_PICKER_PREVIEW), for verifying its
/// layout without a display. Uses the saved phone book, or a sample list when it is empty.
fn picker_preview(path: &str) -> Result<(), String> {
    use config::{Connection, Mode};
    let mut entries = config::load();
    if entries.is_empty() {
        let sample = |name: &str, host: &str, port: u16, mode: Mode| Connection {
            name: name.into(),
            host: host.into(),
            port,
            mode,
            username: None,
            tunnel: None,
        };
        entries = vec![
            sample("studio", "mac-studio.local", 5900, Mode::Native),
            sample("air", "192.168.1.50", 5900, Mode::Rfb),
            sample("work-mac", "10.0.0.9", 5900, Mode::Rfb),
        ];
    }
    let w = 680usize;
    let h = (picker::TOP + entries.len() * picker::ROW_H + 24).min(760);
    let mut buf = vec![0u32; w * h];
    picker::draw_picker(&mut buf, w, h, &entries, 0);

    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for p in &buf {
        rgb.push((p >> 16) as u8);
        rgb.push((p >> 8) as u8);
        rgb.push(*p as u8);
    }
    writer.write_image_data(&rgb).map_err(|e| e.to_string())
}

fn main() -> Result<(), String> {
    let _ = env_logger::try_init();
    if let Ok(path) = std::env::var("AKTIVE_BAR_PREVIEW") {
        return bar_preview(&path);
    }
    if let Ok(path) = std::env::var("AKTIVE_PICKER_PREVIEW") {
        return picker_preview(&path);
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Phone book: `--list` prints the saved connections.
    if args.first().is_some_and(|a| a == "--list") {
        for c in config::load() {
            let mode = match c.mode {
                config::Mode::Native => "native",
                config::Mode::Rfb => "rfb",
            };
            let user = c.username.map(|u| format!(" {u}")).unwrap_or_default();
            println!("{:16} {}:{} {mode}{user}", c.name, c.host, c.port);
        }
        return Ok(());
    }

    // `--tunnel <spec>` routes the RFB path through an SSH forward (TCP).
    let mut tunnel_flag: Option<String> = None;
    if let Some(i) = args.iter().position(|a| a == "--tunnel") {
        args.remove(i);
        if i < args.len() {
            tunnel_flag = Some(args.remove(i));
        }
    }

    let native_flag = args.first().is_some_and(|a| a == "--native");
    if native_flag {
        args.remove(0);
    }
    if args.is_empty() {
        // No target: show the connection picker, which launches the chosen connection.
        return run_picker(config::load());
    }
    // Multiple targets open together as switchable sessions (RFB only; Right Ctrl + Left/Right
    // cycles between them).
    if args.len() > 1 {
        if native_flag {
            return Err("multi-session is RFB only; drop --native and pass RFB hosts".into());
        }
        let mut built = Vec::new();
        for target in &args {
            built.push(connect_rfb(target, &tunnel_flag)?);
        }
        return run_sessions(built);
    }

    // Single target: the native path, a headless test mode, or one RFB session. A `host:port`
    // argument sets the port; a saved name carries its own.
    let target = args[0].clone();
    let title = target.clone();
    let saved = config::find(&target);
    let (host, host_port) = match &saved {
        Some(c) => (c.host.clone(), Some(c.port)),
        None => match target.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().ok()),
            _ => (target.clone(), None),
        },
    };
    let use_native = native_flag || saved.as_ref().is_some_and(|c| c.mode == config::Mode::Native);
    let port = host_port.unwrap_or(if use_native { 35390 } else { 5900 });
    let saved_user = saved.as_ref().and_then(|c| c.username.clone());
    let tunnel = tunnel_flag.or_else(|| saved.and_then(|c| c.tunnel));

    // Native path (QUIC + hardware video decode): windowed (saved `native` mode or `--native`), or
    // a headless test/bench mode. Uses AS_PASSWORD (the host accepts any for now). Built only with
    // `--features native` (needs ffmpeg); otherwise this binary is the RFB client only.
    #[cfg(feature = "native")]
    {
        let as_password = || std::env::var("AS_PASSWORD").unwrap_or_else(|_| "native".into());
        let native_host = format!("{host}:{port}");
        if let Ok(path) = std::env::var("AKTIVE_NATIVE_SNAPSHOT") {
            return native::run_native_snapshot(native_host, as_password(), path);
        }
        if std::env::var("AKTIVE_NATIVE_INPUT_TEST").is_ok() {
            return native::run_native_input_test(native_host, as_password());
        }
        if std::env::var("AKTIVE_NATIVE_BENCH").is_ok() {
            return native::run_native_bench(native_host, as_password());
        }
        if use_native {
            if tunnel.is_some() {
                return Err(
                    "the native path is QUIC/UDP and cannot use an SSH tunnel; connect to a Tailscale address directly"
                        .into(),
                );
            }
            return native::run_native(native_host, as_password(), title);
        }
    }
    #[cfg(not(feature = "native"))]
    if use_native {
        return Err(
            "the native path is not built in this binary; rebuild with `--features native` on a Linux box with ffmpeg"
                .into(),
        );
    }

    // RFB path, optionally through an SSH tunnel. Keep the tunnel alive for the session.
    let tunnel_guard: Option<tunnel::Tunnel>;
    let (rfb_host, rfb_port) = if let Some(spec) = &tunnel {
        let active = tunnel::open(spec, &host, port)?;
        let local_port = active.local_port;
        tunnel_guard = Some(active);
        ("127.0.0.1".to_string(), local_port)
    } else {
        tunnel_guard = None;
        (host, port)
    };
    let username = saved_user.or_else(|| std::env::var("RFB_USER").ok());
    let password = std::env::var("RFB_PASSWORD").unwrap_or_default();
    let options = ConnectOptions::new(rfb_host, rfb_port, username, password);

    // Headless verification modes (single session only).
    if let Ok(path) = std::env::var("AKTIVE_SNAPSHOT") {
        return run_snapshot(options, &path);
    }
    if std::env::var("AKTIVE_INPUT_TEST").is_ok() {
        return run_input_test(options);
    }

    let (info, reader, writer) = connect(&options).map_err(|e| format!("connect: {e}"))?;
    eprintln!("connected to {} ({}x{})", info.name, info.width, info.height);
    let session = Session {
        shared: Arc::new(Shared::new(info.width as u32, info.height as u32)),
        backend: Backend::Rfb { writer, translator: RfbTranslator::new() },
        title,
        cursor_shape: None,
        _tunnel: tunnel_guard,
    };
    run_sessions(vec![(session, reader)])
}

/// Resolves an RFB target (saved name or `host[:port]`, optionally through an SSH tunnel) and
/// connects. For the multi-session path; the native path stays single-session.
fn connect_rfb(target: &str, tunnel_flag: &Option<String>) -> Result<(Session, SessionReader), String> {
    let saved = config::find(target);
    if saved.as_ref().is_some_and(|c| c.mode == config::Mode::Native) {
        return Err(format!("{target} is a native connection; multi-session is RFB only"));
    }
    let (host, port) = match &saved {
        Some(c) => (c.host.clone(), c.port),
        None => match target.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().unwrap()),
            _ => (target.to_string(), 5900),
        },
    };
    let username = saved.as_ref().and_then(|c| c.username.clone()).or_else(|| std::env::var("RFB_USER").ok());
    let tunnel = tunnel_flag.clone().or_else(|| saved.and_then(|c| c.tunnel));
    let tunnel_guard: Option<tunnel::Tunnel>;
    let (rfb_host, rfb_port) = if let Some(spec) = &tunnel {
        let active = tunnel::open(spec, &host, port)?;
        let local_port = active.local_port;
        tunnel_guard = Some(active);
        ("127.0.0.1".to_string(), local_port)
    } else {
        tunnel_guard = None;
        (host, port)
    };
    let password = std::env::var("RFB_PASSWORD").unwrap_or_default();
    let options = ConnectOptions::new(rfb_host, rfb_port, username, password);
    let (info, reader, writer) = connect(&options).map_err(|e| format!("connect {target}: {e}"))?;
    eprintln!("connected to {} ({}x{})", info.name, info.width, info.height);
    let session = Session {
        shared: Arc::new(Shared::new(info.width as u32, info.height as u32)),
        backend: Backend::Rfb { writer, translator: RfbTranslator::new() },
        title: target.to_string(),
        cursor_shape: None,
        _tunnel: tunnel_guard,
    };
    Ok((session, reader))
}

/// Runs the window over one or more sessions: spawns a reader thread per session (only the first
/// owns the clipboard), and a clipboard poller for the first, then drives the event loop.
fn run_sessions(built: Vec<(Session, SessionReader)>) -> Result<(), String> {
    let event_loop = EventLoop::<()>::with_user_event().build().map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let mut sessions = Vec::with_capacity(built.len());
    for (i, (session, reader)) in built.into_iter().enumerate() {
        let clipboard = i == 0;
        if clipboard && let Some(writer) = session.rfb_writer() {
            let shared = session.shared.clone();
            thread::Builder::new()
                .name("clipboard".into())
                .spawn(move || run_clipboard_poller(shared, writer))
                .map_err(|e| e.to_string())?;
        }
        let shared = session.shared.clone();
        let proxy = proxy.clone();
        thread::Builder::new()
            .name("rfb-reader".into())
            .spawn(move || run_reader(shared, proxy, reader, clipboard))
            .map_err(|e| e.to_string())?;
        sessions.push(session);
    }
    let mut app = App::new(sessions);
    // macOS: capture system shortcuts (Cmd+Tab) that winit never sees and forward them to the Mac.
    #[cfg(target_os = "macos")]
    event_tap::spawn(proxy.clone(), app.system_keys.clone());
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    #[test]
    fn scales_cursor_and_hotspot() {
        // 2x2 ARGB cursor: opaque red, green, blue, fully transparent.
        let shape = CursorShape {
            hotspot_x: 1,
            hotspot_y: 0,
            width: 2,
            height: 2,
            pixels: vec![0xff_ff_00_00, 0xff_00_ff_00, 0xff_00_00_ff, 0x00_00_00_00],
        };
        let (rgba, w, h, hx, hy) = scaled_cursor_rgba(&shape, 2.0).unwrap();
        assert_eq!((w, h), (4, 4));
        assert_eq!((hx, hy), (2, 0), "hotspot scales with the image");
        assert_eq!(rgba.len(), 4 * 4 * 4);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "top-left stays opaque red (R,G,B,A)");
        assert_eq!(rgba[rgba.len() - 1], 0, "bottom-right stays transparent");
    }

    #[test]
    fn rejects_empty_cursor() {
        let empty = CursorShape { hotspot_x: 0, hotspot_y: 0, width: 0, height: 0, pixels: vec![] };
        assert!(scaled_cursor_rgba(&empty, 1.0).is_none());
    }
}
