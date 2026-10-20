//! Big-endian reads and client message serialization.

use std::io::{self, Read};

use crate::{Encoding, Error, Rect, Result};

pub(crate) fn read_u8<R: Read>(r: &mut R) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

pub(crate) fn read_u16<R: Read>(r: &mut R) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

pub(crate) fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

pub(crate) fn read_i32<R: Read>(r: &mut R) -> io::Result<i32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(i32::from_be_bytes(b))
}

/// Reads exactly `len` bytes into `buf`, reusing its allocation.
pub(crate) fn read_into<R: Read>(r: &mut R, buf: &mut Vec<u8>, len: usize) -> io::Result<()> {
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)
}

/// Upper bound for length-prefixed strings and payloads; anything larger is treated as a
/// desynchronized stream rather than allocated.
pub(crate) const MAX_PAYLOAD: usize = 256 << 20;

pub(crate) fn checked_len(len: u32, what: &str) -> Result<usize> {
    let len = len as usize;
    if len > MAX_PAYLOAD {
        return Err(Error::Malformed(format!("{what} length {len} exceeds limit")));
    }
    Ok(len)
}

/// A u32-length-prefixed string (reason strings, desktop name).
pub(crate) fn read_string<R: Read>(r: &mut R) -> Result<String> {
    let len = checked_len(read_u32(r)?, "string")?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Wraps a reader and counts bytes taken from it.
pub(crate) struct CountingReader<R> {
    pub(crate) inner: R,
    pub(crate) count: u64,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        Ok(n)
    }
}

/// SetPixelFormat: 32 bpp, depth 24, little-endian, true colour, 0x00RRGGBB.
pub(crate) fn set_pixel_format() -> [u8; 20] {
    [
        0, 0, 0, 0, // type, padding
        32, 24, 0, 1, // bpp, depth, big-endian, true-colour
        0, 255, 0, 255, 0, 255, // red/green/blue max
        16, 8, 0, // red/green/blue shift
        0, 0, 0, // padding
    ]
}

pub(crate) fn set_encodings(encodings: &[Encoding]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + 4 * encodings.len());
    m.extend_from_slice(&[2, 0]);
    m.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for &e in encodings {
        m.extend_from_slice(&(e as i32).to_be_bytes());
    }
    m
}

pub(crate) fn update_request(incremental: bool, area: Rect) -> [u8; 10] {
    let mut m = [0u8; 10];
    m[0] = 3;
    m[1] = incremental as u8;
    m[2..4].copy_from_slice(&area.x.to_be_bytes());
    m[4..6].copy_from_slice(&area.y.to_be_bytes());
    m[6..8].copy_from_slice(&area.w.to_be_bytes());
    m[8..10].copy_from_slice(&area.h.to_be_bytes());
    m
}

pub(crate) fn key_event(keysym: u32, down: bool) -> [u8; 8] {
    let mut m = [0u8; 8];
    m[0] = 4;
    m[1] = down as u8;
    m[4..8].copy_from_slice(&keysym.to_be_bytes());
    m
}

pub(crate) fn pointer_event(buttons: u8, x: u16, y: u16) -> [u8; 6] {
    let mut m = [0u8; 6];
    m[0] = 5;
    m[1] = buttons;
    m[2..4].copy_from_slice(&x.to_be_bytes());
    m[4..6].copy_from_slice(&y.to_be_bytes());
    m
}

/// Apple "auto pasteboard" enable (`RFBAutoPasteboardMessage`, message type 0x15): an 8-byte
/// message `{u8 type; u8 pad; u16 command; u32 reserved}`. Command 1 enables pushing the
/// pasteboard to the viewer; the server accepts 1 or 2 and ignores anything else.
pub(crate) fn set_autopasteboard(command: u16) -> [u8; 8] {
    let mut m = [0u8; 8];
    m[0] = 0x15;
    m[2..4].copy_from_slice(&command.to_be_bytes());
    m
}

pub(crate) fn cut_text(text: &str) -> Vec<u8> {
    let latin1: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
    let mut m = Vec::with_capacity(8 + latin1.len());
    m.extend_from_slice(&[6, 0, 0, 0]);
    m.extend_from_slice(&(latin1.len() as u32).to_be_bytes());
    m.extend_from_slice(&latin1);
    m
}

/// Decodes Latin-1 bytes (ServerCutText) to a string.
pub(crate) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autopasteboard_enable_is_eight_bytes() {
        assert_eq!(set_autopasteboard(1), [0x15, 0, 0, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn cut_text_frames_latin1_with_length() {
        assert_eq!(cut_text("hi"), [6, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']);
        // Characters beyond Latin-1 become '?'.
        assert_eq!(&cut_text("™")[8..], b"?");
    }
}
