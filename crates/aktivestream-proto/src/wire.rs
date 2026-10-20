//! Message framing for the control stream: each message is `u32 len` (little-endian) followed
//! by `len` bytes whose first byte is the message kind. See `docs/phase2-protocol.md`.

use quinn::{RecvStream, SendStream};

use crate::{Error, Result};

pub const PROTO_VERSION: u32 = 1;

/// Message kinds (control stream + the checkpoint-1b synthetic frame).
pub mod kind {
    pub const ERROR: u8 = 0;
    pub const HELLO: u8 = 1;
    pub const HELLO_OK: u8 = 2;
    pub const AUTH: u8 = 3;
    pub const AUTH_OK: u8 = 4;
    pub const TEST_FRAME: u8 = 20;
    pub const VIDEO_FRAME: u8 = 21;
    pub const VIDEO_START: u8 = 22;
    pub const INPUT_KEY: u8 = 30;
    pub const INPUT_POINTER: u8 = 31;
    pub const INPUT_SCROLL: u8 = 32;
}

/// Largest single control/frame message accepted (a raw test frame can be a few MB).
const MAX_MESSAGE: usize = 64 << 20;

pub async fn write_msg(send: &mut SendStream, kind: u8, body: &[u8]) -> Result<()> {
    let len = (body.len() + 1) as u32;
    send.write_all(&len.to_le_bytes()).await.map_err(io_err)?;
    send.write_all(&[kind]).await.map_err(io_err)?;
    send.write_all(body).await.map_err(io_err)?;
    Ok(())
}

/// Reads one framed message, returning its kind and body.
pub async fn read_msg(recv: &mut RecvStream) -> Result<(u8, Vec<u8>)> {
    read_msg_opt(recv).await?.ok_or_else(|| Error::Protocol("stream ended".into()))
}

/// Like [`read_msg`] but returns `None` when the stream was closed cleanly at a message
/// boundary (the host finished sending).
pub async fn read_msg_opt(recv: &mut RecvStream) -> Result<Option<(u8, Vec<u8>)>> {
    let mut len_buf = [0u8; 4];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(read_err(e)),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err(Error::Protocol(format!("message length {len} out of range")));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.map_err(read_err)?;
    let kind = buf[0];
    buf.remove(0);
    Ok(Some((kind, buf)))
}

fn io_err(e: quinn::WriteError) -> Error {
    Error::Io(std::io::Error::other(e))
}

fn read_err(e: quinn::ReadExactError) -> Error {
    Error::Io(std::io::Error::other(e))
}

/// A little-endian reader over a message body.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
        match end {
            Some(end) => {
                let slice = &self.buf[self.pos..end];
                self.pos = end;
                Ok(slice)
            }
            None => Err(Error::Protocol("message truncated".into())),
        }
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// A `u8`-length-prefixed UTF-8 string.
    pub fn short_string(&mut self) -> Result<String> {
        let n = self.u8()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let slice = &self.buf[self.pos..];
        self.pos = self.buf.len();
        slice
    }
}

/// Builds a message body with little-endian fields.
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer::default()
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn short_string(&mut self, s: &str) -> &mut Self {
        let bytes = s.as_bytes();
        let n = bytes.len().min(255);
        self.u8(n as u8);
        self.buf.extend_from_slice(&bytes[..n]);
        self
    }

    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    pub fn finish(&self) -> &[u8] {
        &self.buf
    }
}
