//! AktiveStream Linux client. Milestone 1-2: the RFB path to macOS Screen Sharing in a winit
//! window via a softbuffer (CPU) surface, with keyboard and mouse driven through the shared
//! `aktivestream_keymap` macOS keyboard profile (same behaviour as the Windows client). The native
//! QUIC + hardware-decode path (for Windows-level performance) is the next milestone; this shell
//! is what it builds on. Reuses the platform-neutral `aktivestream-rfb` and `aktivestream-keymap`.
//!
//! Usage: aktivestream <host> [port]   (env RFB_USER, RFB_PASSWORD)

mod keys;
#[cfg(feature = "native")]
mod native;

use aktivestream_client::{config, tunnel};

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use aktivestream_keymap::native::NativeInput;
use aktivestream_keymap::{Engine, HostEvent, KeyInput, LocalAction, MouseButton, Profile, RfbTranslator};
use aktivestream_proto::Input;
use aktivestream_rfb::{ConnectOptions, Framebuffer, ServerEvent, SessionReader, SessionWriter, connect};
use softbuffer::{Context, Surface};
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton as WinitButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::PhysicalKey;
use winit::window::{Fullscreen, Window, WindowId};

/// Shared state between the RFB reader thread and the UI thread.
struct Shared {
    framebuffer: Mutex<Framebuffer>,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn new(width: u32, height: u32) -> Self {
        Shared { framebuffer: Mutex::new(Framebuffer::new(width, height)), error: Mutex::new(None) }
    }
}

/// Pulls updates forever, applying each to the shared framebuffer and waking the UI thread. The
/// reader itself re-requests the next update, so this never touches the writer.
fn run_reader(shared: Arc<Shared>, proxy: EventLoopProxy<()>, mut reader: SessionReader) {
    loop {
        match reader.next_event(&shared.framebuffer) {
            Ok(ServerEvent::Update { .. } | ServerEvent::Resized { .. }) => {
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

struct App {
    shared: Arc<Shared>,
    backend: Backend,
    title: String,
    engine: Engine,
    buttons: u8,
    pointer: (u16, u16),
    cursor: (f64, f64),
    caps: bool,
    fullscreen: bool,
    bar_shown: bool,
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

/// Height in pixels of the full-screen overlay toolbar (Remmina-style: reveals when the pointer
/// touches the top edge, with buttons to leave full screen or disconnect).
const BAR_HEIGHT: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarAction {
    Windowed,
    Disconnect,
}

/// Which toolbar button, if any, the cursor is over (two right-aligned square buttons).
fn bar_action_at(cursor: (f64, f64), dw: usize) -> Option<BarAction> {
    if cursor.1 < 0.0 || cursor.1 >= BAR_HEIGHT as f64 || dw < 2 * BAR_HEIGHT {
        return None;
    }
    let x = cursor.0 as usize;
    if x >= dw.saturating_sub(BAR_HEIGHT) {
        Some(BarAction::Disconnect)
    } else if x >= dw.saturating_sub(2 * BAR_HEIGHT) {
        Some(BarAction::Windowed)
    } else {
        None
    }
}

fn fill_rect(buf: &mut [u32], dw: usize, x0: usize, y0: usize, w: usize, h: usize, color: u32) {
    for y in y0..y0 + h {
        let row = y * dw;
        for x in x0..x0 + w {
            if let Some(px) = buf.get_mut(row + x) {
                *px = color;
            }
        }
    }
}

fn draw_cross(buf: &mut [u32], dw: usize, x: usize, y: usize, size: usize, color: u32) {
    for i in 0..size {
        if let Some(px) = buf.get_mut((y + i) * dw + x + i) {
            *px = color;
        }
        if let Some(px) = buf.get_mut((y + i) * dw + x + size - 1 - i) {
            *px = color;
        }
    }
}

fn draw_rect_outline(buf: &mut [u32], dw: usize, x: usize, y: usize, w: usize, h: usize, color: u32) {
    for i in 0..w {
        if let Some(px) = buf.get_mut(y * dw + x + i) {
            *px = color;
        }
        if let Some(px) = buf.get_mut((y + h - 1) * dw + x + i) {
            *px = color;
        }
    }
    for i in 0..h {
        if let Some(px) = buf.get_mut((y + i) * dw + x) {
            *px = color;
        }
        if let Some(px) = buf.get_mut((y + i) * dw + x + w - 1) {
            *px = color;
        }
    }
}

/// Draws text with the public-domain 8x8 bitmap font, stopping at `max_x` or the buffer edge.
fn draw_text(buf: &mut [u32], dw: usize, x0: usize, y: usize, text: &str, color: u32, max_x: usize) {
    use font8x8::UnicodeFonts;
    let mut x = x0;
    for ch in text.chars() {
        if x + 8 > max_x {
            break;
        }
        if let Some(glyph) = font8x8::BASIC_FONTS.get(ch) {
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8 {
                    if (bits >> col) & 1 != 0
                        && let Some(px) = buf.get_mut((y + row) * dw + x + col)
                    {
                        *px = color;
                    }
                }
            }
        }
        x += 8;
    }
}

/// Draws the overlay toolbar: a dark strip with the connection name on the left and, on the right,
/// a "leave full screen" (rectangle) and a red "disconnect" (X) button, the hovered one highlighted.
fn draw_bar(buf: &mut [u32], dw: usize, dh: usize, hover: Option<BarAction>, title: &str) {
    let bar_h = BAR_HEIGHT.min(dh);
    fill_rect(buf, dw, 0, 0, dw, bar_h, 0x1c_1c_1c);
    if dw < 2 * BAR_HEIGHT {
        return;
    }
    let disconnect = dw - BAR_HEIGHT;
    let windowed = dw - 2 * BAR_HEIGHT;
    draw_text(buf, dw, 10, bar_h.saturating_sub(8) / 2, title, 0xdd_dd_dd, windowed.saturating_sub(8));
    if hover == Some(BarAction::Disconnect) {
        fill_rect(buf, dw, disconnect, 0, BAR_HEIGHT, bar_h, 0x99_22_22);
    }
    if hover == Some(BarAction::Windowed) {
        fill_rect(buf, dw, windowed, 0, BAR_HEIGHT, bar_h, 0x33_33_33);
    }
    draw_cross(buf, dw, disconnect + 11, 10, 14, 0xff_66_66);
    draw_rect_outline(buf, dw, windowed + 10, 11, 16, 13, 0xdd_dd_dd);
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
    fn new(shared: Arc<Shared>, backend: Backend, title: String) -> Self {
        App {
            shared,
            backend,
            title,
            engine: Engine::new(Profile::mac_default()),
            buttons: 0,
            pointer: (0, 0),
            cursor: (0.0, 0.0),
            caps: false,
            fullscreen: false,
            bar_shown: false,
            window: None,
            surface: None,
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

        let fb = self.shared.framebuffer.lock().unwrap();
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
            draw_bar(&mut buffer, dw, dh, bar_action_at(self.cursor, dw), &self.title);
        }
        let _ = buffer.present();
    }

    fn send_host_events(&mut self, events: Vec<HostEvent>) {
        let caps = self.caps;
        match &mut self.backend {
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
        let key = keys::to_key(code);
        let down = event.state == ElementState::Pressed;
        if key == aktivestream_keymap::Key::CapsLock && down {
            self.caps = !self.caps;
        }
        let output = self.engine.handle_key(KeyInput { key, down });
        match output.local {
            Some(LocalAction::ToggleFullscreen) => self.toggle_fullscreen(),
            Some(LocalAction::Disconnect) => event_loop.exit(),
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
        let fb = self.shared.framebuffer.lock().unwrap();
        let (sw, sh) = (fb.width.max(1), fb.height.max(1));
        let fx = (x.max(0.0) as u32 * sw / size.width.max(1)).min(sw - 1);
        let fy = (y.max(0.0) as u32 * sh / size.height.max(1)).min(sh - 1);
        (fx as u16, fy as u16)
    }

    fn send_pointer(&mut self) {
        let (x, y) = self.pointer;
        let buttons = self.buttons;
        let (fw, fh) = {
            let fb = self.shared.framebuffer.lock().unwrap();
            (fb.width.max(1) as f32, fb.height.max(1) as f32)
        };
        match &mut self.backend {
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
        let over_bar = self.fullscreen && y < BAR_HEIGHT as f64;
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
                match bar_action_at(self.cursor, dw) {
                    Some(BarAction::Windowed) => self.toggle_fullscreen(),
                    Some(BarAction::Disconnect) => event_loop.exit(),
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
        match &mut self.backend {
            Backend::Rfb { writer, .. } => {
                let (bit, count) = if notches >= 0 { (WHEEL_UP, notches) } else { (WHEEL_DOWN, -notches) };
                for _ in 0..count {
                    let _ = writer.pointer_event(buttons | bit, x, y);
                    let _ = writer.pointer_event(buttons, x, y);
                }
            }
            Backend::Native { tx, .. } => {
                // The native host takes pixel scroll deltas; a notch is ~120px (up positive).
                let _ = tx.send(Input::Scroll { dx: 0.0, dy: notches as f32 * 120.0 });
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
        if let Some(error) = self.shared.error.lock().unwrap().take() {
            eprintln!("{error}");
            event_loop.exit();
            return;
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
    draw_bar(&mut buf, w, h, Some(BarAction::Disconnect), "studio - Mac Studio");

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
    let target = args
        .first()
        .cloned()
        .ok_or("usage: aktivestream [--native] [--tunnel <ssh>] <host-or-name> [port] | --list")?;
    let title = target.clone();
    let port_arg: Option<u16> = args.get(1).and_then(|p| p.parse().ok());

    // Resolve a saved connection by name; otherwise treat the argument as a host address.
    let saved = config::find(&target);
    let host = saved.as_ref().map_or(target, |c| c.host.clone());
    let use_native = native_flag || saved.as_ref().is_some_and(|c| c.mode == config::Mode::Native);
    let port = port_arg.or(saved.as_ref().map(|c| c.port)).unwrap_or(if use_native { 35390 } else { 5900 });
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

    // RFB path, optionally through an SSH tunnel. Keep the tunnel alive for the whole session.
    let _tunnel: Option<tunnel::Tunnel>;
    let (rfb_host, rfb_port) = if let Some(spec) = &tunnel {
        let active = tunnel::open(spec, &host, port)?;
        let local_port = active.local_port;
        _tunnel = Some(active);
        ("127.0.0.1".to_string(), local_port)
    } else {
        _tunnel = None;
        (host, port)
    };
    let username = saved_user.or_else(|| std::env::var("RFB_USER").ok());
    let password = std::env::var("RFB_PASSWORD").unwrap_or_default();
    let options = ConnectOptions::new(rfb_host, rfb_port, username, password);

    // Headless verification: decode the first frame to a PNG and exit, no display server needed.
    if let Ok(path) = std::env::var("AKTIVE_SNAPSHOT") {
        return run_snapshot(options, &path);
    }
    // Headless input check: move the remote pointer to known framebuffer coordinates, so a
    // watcher on the Mac can confirm the client->host input channel over RFB.
    if std::env::var("AKTIVE_INPUT_TEST").is_ok() {
        return run_input_test(options);
    }

    let (info, reader, writer) = connect(&options).map_err(|e| format!("connect: {e}"))?;
    eprintln!("connected to {} ({}x{})", info.name, info.width, info.height);
    let shared = Arc::new(Shared::new(info.width as u32, info.height as u32));

    let event_loop = EventLoop::<()>::with_user_event().build().map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let reader_shared = shared.clone();
    thread::Builder::new()
        .name("rfb-reader".into())
        .spawn(move || run_reader(reader_shared, proxy, reader))
        .map_err(|e| e.to_string())?;

    let mut app = App::new(shared, Backend::Rfb { writer, translator: RfbTranslator::new() }, title);
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}

#[cfg(test)]
mod bar_tests {
    use super::*;

    #[test]
    fn draw_bar_fills_strip_and_hovered_button() {
        let (w, h) = (200usize, 50usize);
        let mut buf = vec![0u32; w * h];
        draw_bar(&mut buf, w, h, Some(BarAction::Disconnect), "studio - Mac Studio");
        assert_eq!(buf[0], 0x1c_1c_1c, "toolbar background");
        assert_eq!(buf[BAR_HEIGHT * w], 0, "below the toolbar is untouched");
        assert_eq!(buf[2 * w + (w - BAR_HEIGHT + 2)], 0x99_22_22, "disconnect hover highlight");
    }

    #[test]
    fn bar_hit_testing() {
        let dw = 400;
        assert_eq!(bar_action_at((399.0, 5.0), dw), Some(BarAction::Disconnect));
        assert_eq!(bar_action_at((340.0, 5.0), dw), Some(BarAction::Windowed));
        assert_eq!(bar_action_at((10.0, 5.0), dw), None);
        assert_eq!(bar_action_at((399.0, 50.0), dw), None);
    }
}
