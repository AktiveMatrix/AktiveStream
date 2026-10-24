//! Handshake and the server-message loop.

use std::collections::VecDeque;
use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::decode::{self, Inflater};
use crate::ra2::{self, Ra2Cipher, Ra2Reader, Ra2Writer};
use crate::wire::{self, CountingReader, read_i32, read_into, read_string, read_u8, read_u16, read_u32};
use crate::{
    ConnectOptions, Encoding, Error, Framebuffer, PixelFormat, ReaderStats, Rect, Result, SecurityType, ServerEvent,
    ServerInfo, SessionReader, SessionWriter, auth,
};

/// The reading half of a session's socket: plain, or RA2 AES-EAX framed. The RFB reader and the
/// handshake use it through [`Read`]; the stream cipher is transparent.
pub(crate) enum ReadHalf {
    Plain(TcpStream),
    Ra2(Ra2Reader<TcpStream>),
}

impl Read for ReadHalf {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            ReadHalf::Plain(s) => s.read(buf),
            ReadHalf::Ra2(r) => r.read(buf),
        }
    }
}

/// The writing half of a session's socket: plain, or RA2 AES-EAX framed.
pub(crate) enum WriteHalf {
    Plain(TcpStream),
    Ra2(Ra2Writer<TcpStream>),
}

impl Write for WriteHalf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            WriteHalf::Plain(s) => s.write(buf),
            WriteHalf::Ra2(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            WriteHalf::Plain(s) => s.flush(),
            WriteHalf::Ra2(w) => w.flush(),
        }
    }
}

impl WriteHalf {
    /// Closes the underlying socket in both directions, so the reader's next read fails.
    pub(crate) fn shutdown(&self) {
        let sock = match self {
            WriteHalf::Plain(s) => s,
            WriteHalf::Ra2(w) => w.get_ref(),
        };
        let _ = sock.shutdown(Shutdown::Both);
    }
}

pub(crate) fn lock(fb: &Mutex<Framebuffer>) -> MutexGuard<'_, Framebuffer> {
    fb.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn send(out: &Mutex<WriteHalf>, msg: &[u8]) -> Result<()> {
    let mut s = out.lock().unwrap_or_else(|e| e.into_inner());
    s.write_all(msg)?;
    Ok(())
}

fn open(options: &ConnectOptions) -> Result<TcpStream> {
    let mut last = None;
    for addr in (options.host.as_str(), options.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, options.connect_timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map(Error::Io).unwrap_or_else(|| Error::Protocol(format!("{} did not resolve", options.host))))
}

/// Parses `RFB xxx.yyy\n` into (trimmed string, minor version spoken).
fn parse_version(v: &[u8; 12]) -> Result<(String, u32)> {
    let text = String::from_utf8_lossy(v).into_owned();
    let digits = |s: &[u8]| -> Option<u32> { std::str::from_utf8(s).ok()?.parse().ok() };
    let ok = v.starts_with(b"RFB ") && v[7] == b'.' && v[11] == b'\n';
    let (major, minor) = match (ok, digits(&v[4..7]), digits(&v[8..11])) {
        (true, Some(major), Some(minor)) => (major, minor),
        _ => return Err(Error::Protocol(text)),
    };
    // RealVNC announces RFB 004.x / 005.x for its own extensions; it accepts a 3.8 reply and then
    // offers the RSA-AES (RA2) security types. Treat its versions, and any 3.8+, as 3.8.
    let spoken = match (major, minor) {
        (4 | 5, _) => 8,
        (3, m) if m >= 8 => 8,
        (3, 7) => 7,
        (3, _) => 3,
        _ => return Err(Error::Protocol(text)),
    };
    Ok((text.trim_end().to_owned(), spoken))
}

type Ra2Ciphers = (Ra2Cipher, Ra2Cipher);

fn negotiate_security(
    s: &mut &TcpStream,
    options: &ConnectOptions,
    minor: u32,
) -> Result<(SecurityType, Option<Ra2Ciphers>)> {
    let chosen = if minor == 3 {
        let t = read_u32(s)?;
        let st = match t {
            0 => return Err(Error::AuthFailed(read_string(s)?)),
            1 => SecurityType::None,
            2 => SecurityType::VncAuth,
            5 => SecurityType::RsaAes,
            _ => return Err(Error::NoSecurityType(vec![t as u8])),
        };
        if !options.security.contains(&st) {
            return Err(Error::NoSecurityType(vec![t as u8]));
        }
        st
    } else {
        let n = read_u8(s)? as usize;
        if n == 0 {
            return Err(Error::AuthFailed(read_string(s)?));
        }
        let mut offered = vec![0u8; n];
        s.read_exact(&mut offered)?;
        let usable = |t: SecurityType| {
            offered.contains(&(t as u8)) && (t != SecurityType::AppleDh || options.username.is_some())
        };
        let Some(t) = options.security.iter().copied().find(|&t| usable(t)) else {
            if options.username.is_none() && options.security.contains(&SecurityType::AppleDh) && offered.contains(&30)
            {
                return Err(Error::UsernameRequired(30));
            }
            return Err(Error::NoSecurityType(offered));
        };
        s.write_all(&[t as u8])?;
        t
    };

    let ciphers = match chosen {
        SecurityType::None => None,
        SecurityType::VncAuth => {
            auth::vnc_auth(s, &options.password)?;
            None
        }
        SecurityType::AppleDh => {
            let user = options.username.as_deref().ok_or(Error::UsernameRequired(30))?;
            auth::apple_dh(s, user, &options.password)?;
            None
        }
        SecurityType::RsaAes => {
            // RA2 may ask for a username (credential subtype 1); the handshake sends an empty one
            // if the server does not need it (subtype 2).
            let user = options.username.as_deref().unwrap_or("");
            Some(ra2::handshake(s, user, &options.password)?)
        }
    };

    // Plaintext types read SecurityResult here (3.8 always; older skip it for the None type). RA2
    // sends SecurityResult inside the encrypted stream, so the caller reads it once the cipher is set.
    if ciphers.is_none() && (minor >= 8 || chosen != SecurityType::None) && read_u32(s)? != 0 {
        let reason = if minor >= 8 { read_string(s).ok() } else { None };
        return Err(Error::AuthFailed(reason.unwrap_or_else(|| "server rejected the credentials".into())));
    }
    Ok((chosen, ciphers))
}

fn parse_pixel_format(p: &[u8; 16]) -> PixelFormat {
    PixelFormat {
        bits_per_pixel: p[0],
        depth: p[1],
        big_endian: p[2] != 0,
        true_colour: p[3] != 0,
        red_max: u16::from_be_bytes([p[4], p[5]]),
        green_max: u16::from_be_bytes([p[6], p[7]]),
        blue_max: u16::from_be_bytes([p[8], p[9]]),
        red_shift: p[10],
        green_shift: p[11],
        blue_shift: p[12],
    }
}

pub(crate) fn connect(options: &ConnectOptions) -> Result<(ServerInfo, SessionReader, SessionWriter)> {
    let stream = open(options)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(options.connect_timeout))?;
    stream.set_write_timeout(Some(options.connect_timeout))?;
    let mut s = &stream;

    let mut version = [0u8; 12];
    s.read_exact(&mut version)?;
    let (protocol, minor) = parse_version(&version)?;
    let reply: &[u8; 12] = match minor {
        8 => b"RFB 003.008\n",
        7 => b"RFB 003.007\n",
        _ => b"RFB 003.003\n",
    };
    s.write_all(reply)?;

    let (security, ciphers) = negotiate_security(&mut s, options, minor)?;

    // Handshake done: drop the connect read timeout so the message loop can block, then split the
    // socket into read/write halves (RA2 wraps each in its AES-EAX stream cipher).
    stream.set_read_timeout(None)?;
    let write_sock = stream.try_clone()?;
    let (mut read_half, mut write_half) = match ciphers {
        Some((c2s, s2c)) => {
            (ReadHalf::Ra2(Ra2Reader::new(stream, s2c)), WriteHalf::Ra2(Ra2Writer::new(write_sock, c2s)))
        }
        None => (ReadHalf::Plain(stream), WriteHalf::Plain(write_sock)),
    };

    // RA2 carries SecurityResult inside the encrypted stream; plaintext types already consumed it.
    if security == SecurityType::RsaAes && read_u32(&mut read_half)? != 0 {
        let reason = read_string(&mut read_half).ok();
        return Err(Error::AuthFailed(reason.unwrap_or_else(|| "server rejected the credentials".into())));
    }

    write_half.write_all(&[options.shared as u8])?;
    let width = read_u16(&mut read_half)?;
    let height = read_u16(&mut read_half)?;
    let mut pf = [0u8; 16];
    read_half.read_exact(&mut pf)?;
    let name = read_string(&mut read_half)?;

    let full = Rect { x: 0, y: 0, w: width, h: height };
    let mut setup = Vec::with_capacity(64);
    setup.extend_from_slice(&wire::set_pixel_format());
    setup.extend_from_slice(&wire::set_encodings(&options.encodings));
    // "Auto pasteboard" (client message 0x15) is an Apple Screen Sharing extension. Any other server
    // (RealVNC, wayvnc, ...) drops the connection on this unknown message, so send it only when the
    // caller says it is a Mac; `!= RsaAes` is a safety net for RealVNC, which is never a Mac.
    if options.sync_clipboard && options.apple_extensions && security != SecurityType::RsaAes {
        setup.extend_from_slice(&wire::set_autopasteboard(1));
    }
    setup.extend_from_slice(&wire::update_request(false, full));
    write_half.write_all(&setup)?;

    let out = Arc::new(Mutex::new(write_half));
    let info = ServerInfo { protocol, security, width, height, name, server_pixel_format: parse_pixel_format(&pf) };
    let reader = SessionReader {
        input: BufReader::with_capacity(1 << 20, CountingReader { inner: read_half, count: 0 }),
        out: Arc::clone(&out),
        width,
        height,
        last_rect: options.encodings.contains(&Encoding::LastRect),
        zrle: Inflater::new(),
        zlib: Inflater::new(),
        payload: Vec::new(),
        inflated: Vec::new(),
        scratch: Vec::new(),
        pending: VecDeque::new(),
        stats: ReaderStats::default(),
        ops: Vec::new(),
        bufs: Vec::new(),
    };
    Ok((info, reader, SessionWriter { out }))
}

/// One rect of a FramebufferUpdate, held between reading and decoding.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Op {
    /// Raw, Zlib or ZRLE pixels: rect, encoding, index into `SessionReader::bufs`.
    Pixels(Rect, i32, usize),
    Copy(Rect, u16, u16),
    Resize(u16, u16),
}

impl SessionReader {
    pub(crate) fn next_event_impl(&mut self, fb: &Mutex<Framebuffer>) -> Result<ServerEvent> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(ev);
            }
            match read_u8(&mut self.input)? {
                0 => self.read_update(fb)?,
                1 => {
                    // SetColourMapEntries: padding, first colour, count, then 6 bytes each.
                    let mut hdr = [0u8; 5];
                    self.input.read_exact(&mut hdr)?;
                    let n = u16::from_be_bytes([hdr[3], hdr[4]]) as usize;
                    read_into(&mut self.input, &mut self.payload, n * 6)?;
                }
                2 => return Ok(ServerEvent::Bell),
                3 => {
                    let mut pad = [0u8; 3];
                    self.input.read_exact(&mut pad)?;
                    let len = wire::checked_len(read_u32(&mut self.input)?, "ServerCutText")?;
                    read_into(&mut self.input, &mut self.payload, len)?;
                    return Ok(ServerEvent::CutText(wire::latin1(&self.payload)));
                }
                other => return Err(Error::UnknownMessage(other)),
            }
        }
    }

    /// Reads one FramebufferUpdate in full, requests the next one, then decodes. Apple's
    /// server spends hundreds of milliseconds encoding a large update, so asking before
    /// decoding lets it work on the next update while this one is decoded.
    fn read_update(&mut self, fb: &Mutex<Framebuffer>) -> Result<()> {
        let _pad = read_u8(&mut self.input)?;
        let count = read_u16(&mut self.input)?;
        let (start_w, start_h) = (self.width, self.height);
        let until_last_rect = self.last_rect && count == 0xFFFF;
        let mut ops = std::mem::take(&mut self.ops);
        ops.clear();
        let mut used = 0;
        let mut i = 0u32;
        while until_last_rect || i < count as u32 {
            i += 1;
            let r = Rect {
                x: read_u16(&mut self.input)?,
                y: read_u16(&mut self.input)?,
                w: read_u16(&mut self.input)?,
                h: read_u16(&mut self.input)?,
            };
            let enc = read_i32(&mut self.input)?;
            *self.stats.rects.entry(enc).or_default() += 1;
            decode::check_size(r)?;
            let (w, h) = (r.w as usize, r.h as usize);
            let len = match enc {
                e if e == Encoding::Raw as i32 => Some(w * h * 4),
                e if e == Encoding::Zlib as i32 => Some(wire::checked_len(read_u32(&mut self.input)?, "Zlib rect")?),
                e if e == Encoding::Zrle as i32 => Some(wire::checked_len(read_u32(&mut self.input)?, "ZRLE rect")?),
                _ => None,
            };
            if let Some(len) = len {
                if self.bufs.len() <= used {
                    self.bufs.push(Vec::new());
                }
                read_into(&mut self.input, &mut self.bufs[used], len)?;
                ops.push(Op::Pixels(r, enc, used));
                used += 1;
                continue;
            }
            match enc {
                e if e == Encoding::CopyRect as i32 => {
                    let sx = read_u16(&mut self.input)?;
                    let sy = read_u16(&mut self.input)?;
                    ops.push(Op::Copy(r, sx, sy));
                }
                e if e == Encoding::Cursor as i32 => {
                    let len = w * h * 4 + w.div_ceil(8) * h;
                    read_into(&mut self.input, &mut self.payload, len)?;
                    self.pending.push_back(ServerEvent::Cursor(decode::cursor(r, &self.payload)?));
                }
                e if e == Encoding::DesktopSize as i32 => {
                    self.width = r.w;
                    self.height = r.h;
                    ops.push(Op::Resize(r.w, r.h));
                }
                e if e == Encoding::LastRect as i32 => break,
                other => return Err(Error::UnexpectedEncoding(other)),
            }
        }
        self.stats.updates += 1;
        let full = Rect { x: 0, y: 0, w: self.width, h: self.height };
        send(&self.out, &wire::update_request(true, full))?;

        {
            let mut f = lock(fb);
            if f.width != start_w as u32 || f.height != start_h as u32 {
                f.resize(start_w as u32, start_h as u32);
            }
        }
        let t = Instant::now();
        let mut dirty = Vec::new();
        // Every write below clips against the framebuffer's size at the time of the write,
        // so a rect partly outside the screen updates only its visible part.
        for op in &ops {
            match *op {
                Op::Pixels(r, enc, buf) => {
                    let data = &self.bufs[buf];
                    let (w, h) = (r.w as usize, r.h as usize);
                    if enc == Encoding::Raw as i32 {
                        dirty.extend(decode::put_raw(&mut lock(fb), r, data));
                    } else if enc == Encoding::Zlib as i32 {
                        self.zlib.inflate_exact(data, &mut self.inflated, w * h * 4)?;
                        dirty.extend(decode::put_raw(&mut lock(fb), r, &self.inflated));
                    } else {
                        self.zrle.inflate_all(data, &mut self.inflated)?;
                        decode::zrle(&self.inflated, w, h, &mut self.scratch)?;
                        dirty.extend(decode::blit(&mut lock(fb), r, &self.scratch));
                    }
                }
                Op::Copy(r, sx, sy) => dirty.extend(decode::copy_rect(&mut lock(fb), r, sx, sy)),
                Op::Resize(w, h) => {
                    if !dirty.is_empty() {
                        self.pending.push_back(ServerEvent::Update { dirty: std::mem::take(&mut dirty) });
                    }
                    lock(fb).resize(w as u32, h as u32);
                    self.pending.push_back(ServerEvent::Resized { width: w, height: h });
                }
            }
        }
        self.stats.decode_time += t.elapsed();
        self.ops = ops;
        if !dirty.is_empty() {
            self.pending.push_back(ServerEvent::Update { dirty });
        }
        Ok(())
    }

    pub(crate) fn stats_impl(&self) -> ReaderStats {
        ReaderStats { bytes_read: self.input.get_ref().count, ..self.stats.clone() }
    }
}
