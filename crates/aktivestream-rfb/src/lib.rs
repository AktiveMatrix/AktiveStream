//! RFB (VNC) client for macOS Screen Sharing.
//!
//! Scope is what Apple's built-in server actually speaks (measured, see
//! docs/mac-screen-sharing.md): protocol `RFB 003.889` answered as 3.8, security type 30
//! (Apple Diffie-Hellman with the Mac login) and type 2 (legacy VNC password), the ZRLE and
//! Zlib encodings plus Raw and CopyRect, and the Cursor, DesktopSize and LastRect
//! pseudo-encodings. Pixels are always requested as 32 bpp little-endian `0x00RRGGBB`, which
//! is also the layout of a Windows 32-bit DIB.
//!
//! The public items in this file are the contract the Windows client builds against.
//! Bodies marked `todo!()` are filled in by the RFB implementation; signatures change only
//! together with `crates/aktivestream-win`.

use std::collections::{BTreeMap, VecDeque};
use std::io::BufReader;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod auth;
mod decode;
mod ra2;
mod session;
mod wire;

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("server speaks unsupported protocol {0:?}")]
    Protocol(String),
    #[error("server offered no usable security type (offered {0:?})")]
    NoSecurityType(Vec<u8>),
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("security type {0} needs a user name")]
    UsernameRequired(u8),
    #[error("malformed server data: {0}")]
    Malformed(String),
    #[error("server sent unknown message type {0}")]
    UnknownMessage(u8),
    #[error("server used encoding {0} that was not requested")]
    UnexpectedEncoding(i32),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Encodings and pseudo-encodings this client implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Encoding {
    Raw = 0,
    CopyRect = 1,
    Zlib = 6,
    Zrle = 16,
    Cursor = -239,
    DesktopSize = -223,
    LastRect = -224,
}

impl Encoding {
    /// Preference order sent in SetEncodings. Apple's server answers only ZRLE or Zlib in
    /// compressed form, and falls back to Raw for everything else.
    pub const DEFAULT: &'static [Encoding] = &[
        Encoding::Zrle,
        Encoding::Zlib,
        Encoding::CopyRect,
        Encoding::Raw,
        Encoding::Cursor,
        Encoding::DesktopSize,
        Encoding::LastRect,
    ];
}

/// Security types the client knows. Apple also offers 33, 35 and 36 (not implemented yet,
/// see docs/apple-security-types.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SecurityType {
    None = 1,
    VncAuth = 2,
    /// RealVNC RSA-AES (type 5): RSA key exchange, then an AES-128-EAX encrypted session.
    RsaAes = 5,
    AppleDh = 30,
}

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    /// Mac account name for [`SecurityType::AppleDh`]. Without it only VNC auth is tried.
    pub username: Option<String>,
    /// Mac account password (Apple DH) or VNC password (VNC auth, first 8 bytes used).
    pub password: String,
    /// Security types to try, in preference order, among those the server offers.
    pub security: Vec<SecurityType>,
    pub encodings: Vec<Encoding>,
    /// Ask the server to keep other clients connected.
    pub shared: bool,
    /// Send Apple's "auto pasteboard" enable (message type 0x15) after setup, so the server
    /// pushes clipboard changes and accepts ClientCutText. Note: macOS only delivers these
    /// to the pasteboard of the user who owns the console; on a headless Mac whose console is
    /// held by `loginwindow` it is a no-op (see docs/mac-screen-sharing.md). Default true.
    pub sync_clipboard: bool,
    /// Send Apple Screen Sharing-only extensions (currently the auto-pasteboard enable, client
    /// message 0x15). A generic server (RealVNC, wayvnc, TigerVNC, ...) drops the connection on an
    /// unknown message, so set this false for anything that is not a Mac. Default true.
    pub apple_extensions: bool,
    pub connect_timeout: Duration,
}

impl ConnectOptions {
    pub fn new(host: impl Into<String>, port: u16, username: Option<String>, password: impl Into<String>) -> Self {
        ConnectOptions {
            host: host.into(),
            port,
            username,
            password: password.into(),
            security: vec![SecurityType::AppleDh, SecurityType::VncAuth, SecurityType::RsaAes, SecurityType::None],
            encodings: Encoding::DEFAULT.to_vec(),
            shared: true,
            sync_clipboard: true,
            apple_extensions: true,
            connect_timeout: Duration::from_secs(10),
        }
    }
}

/// The server's pixel format as sent in ServerInit (informational; the client always
/// switches to 32 bpp `0x00RRGGBB`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// Version string the server announced, e.g. `RFB 003.889`.
    pub protocol: String,
    pub security: SecurityType,
    pub width: u16,
    pub height: u16,
    pub name: String,
    pub server_pixel_format: PixelFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// Remote screen contents, one `0x00RRGGBB` pixel per `u32`, rows top to bottom with no
/// padding (`stride == width`).
#[derive(Debug, Clone, Default)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: u32, height: u32) -> Framebuffer {
        Framebuffer { width, height, pixels: vec![0; width as usize * height as usize] }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels.resize(width as usize * height as usize, 0);
    }
}

/// A cursor image from the Cursor pseudo-encoding. `pixels` are `0xAARRGGBB`, alpha 0 or
/// 255 from the server's bitmask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorShape {
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    /// A FramebufferUpdate was applied to the framebuffer; `dirty` lists the changed areas.
    Update {
        dirty: Vec<Rect>,
    },
    /// The remote desktop changed size; the framebuffer has been resized and must be
    /// repainted in full when the next update arrives.
    Resized {
        width: u16,
        height: u16,
    },
    Cursor(CursorShape),
    Bell,
    /// ServerCutText, decoded from Latin-1.
    CutText(String),
}

/// Connects, authenticates, exchanges ClientInit/ServerInit, sets the pixel format and
/// encodings, and asks for a first full update.
pub fn connect(options: &ConnectOptions) -> Result<(ServerInfo, SessionReader, SessionWriter)> {
    session::connect(options)
}

/// Counters for measuring a session (see `examples/smoke.rs`).
#[derive(Debug, Clone, Default)]
pub struct ReaderStats {
    /// Bytes received from the socket since connect (after the handshake).
    pub bytes_read: u64,
    /// FramebufferUpdate messages processed.
    pub updates: u64,
    /// Rectangles seen, keyed by encoding number (pseudo-encodings included).
    pub rects: BTreeMap<i32, u64>,
    /// Time spent inflating, decoding and writing pixels (socket waits excluded).
    pub decode_time: Duration,
}

/// Reading half of a session. Owns the decoder state (persistent zlib streams).
///
/// After each FramebufferUpdate the reader itself sends the next incremental
/// FramebufferUpdateRequest, so the update pipeline never waits on the UI thread. Reader
/// and [`SessionWriter`] share the socket's write half behind a mutex; every message is
/// serialized first and written with one `write_all` under that lock.
pub struct SessionReader {
    input: BufReader<wire::CountingReader<session::ReadHalf>>,
    out: Arc<Mutex<session::WriteHalf>>,
    /// Current remote desktop size (changes with DesktopSize).
    width: u16,
    height: u16,
    /// LastRect was requested, so a rect count of 0xFFFF means "until LastRect".
    last_rect: bool,
    zrle: decode::Inflater,
    zlib: decode::Inflater,
    payload: Vec<u8>,
    inflated: Vec<u8>,
    scratch: Vec<u32>,
    pending: VecDeque<ServerEvent>,
    stats: ReaderStats,
    /// One update's rects, read in full before decoding starts (see `session::Op`).
    ops: Vec<session::Op>,
    /// Payload buffers for `ops`, reused across updates.
    bufs: Vec<Vec<u8>>,
}

impl SessionReader {
    /// Blocks for the next server message and applies it. Socket reads happen without
    /// holding `framebuffer`; the lock is taken only while pixels are written, so a UI
    /// thread can paint from the same framebuffer between messages.
    ///
    /// The framebuffer is resized to the remote size before pixels are written if it does
    /// not match (for example a fresh `Framebuffer::default()`), and rects reaching past its
    /// edge are clipped. Rects too large to be real return [`Error::Malformed`].
    pub fn next_event(&mut self, framebuffer: &Mutex<Framebuffer>) -> Result<ServerEvent> {
        self.next_event_impl(framebuffer)
    }

    /// Current remote desktop size.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    pub fn stats(&self) -> ReaderStats {
        self.stats_impl()
    }
}

/// Writing half of a session. `TCP_NODELAY` is set; each call writes one message. Cloneable so
/// a side channel (e.g. a clipboard poller) can send too; the shared lock keeps messages whole.
#[derive(Clone)]
pub struct SessionWriter {
    out: Arc<Mutex<session::WriteHalf>>,
}

impl SessionWriter {
    pub fn key_event(&mut self, keysym: u32, down: bool) -> Result<()> {
        session::send(&self.out, &wire::key_event(keysym, down))
    }

    /// `buttons` is the RFB mask: 1 left, 2 middle, 4 right, 8/16 wheel up/down,
    /// 32/64 wheel left/right.
    pub fn pointer_event(&mut self, buttons: u8, x: u16, y: u16) -> Result<()> {
        session::send(&self.out, &wire::pointer_event(buttons, x, y))
    }

    /// ClientCutText. Characters outside Latin-1 are replaced with `?`.
    pub fn cut_text(&mut self, text: &str) -> Result<()> {
        session::send(&self.out, &wire::cut_text(text))
    }

    pub fn request_update(&mut self, incremental: bool, area: Rect) -> Result<()> {
        session::send(&self.out, &wire::update_request(incremental, area))
    }

    /// Closes the connection; the reader's next read fails.
    pub fn shutdown(&self) {
        let s = self.out.lock().unwrap_or_else(|e| e.into_inner());
        s.shutdown();
    }
}
