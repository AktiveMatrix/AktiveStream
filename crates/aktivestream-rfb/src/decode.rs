//! Pixel decoders. Everything here works on byte slices already read from the socket and
//! writes into caller-owned buffers, so the framebuffer lock is only held for blits.

use flate2::{Decompress, FlushDecompress, Status};

use crate::{CursorShape, Error, Framebuffer, Rect, Result};

fn malformed(what: impl Into<String>) -> Error {
    Error::Malformed(what.into())
}

/// One persistent zlib stream (ZRLE and Zlib each keep their own for the whole session).
pub(crate) struct Inflater {
    z: Decompress,
}

impl Inflater {
    pub(crate) fn new() -> Inflater {
        Inflater { z: Decompress::new(true) }
    }

    /// Inflates all of `input` into `out` (cleared first), growing it as needed.
    pub(crate) fn inflate_all(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        let mut consumed = 0usize;
        loop {
            if out.capacity() - out.len() < 32 * 1024 {
                out.reserve((input.len() * 4).max(64 * 1024));
            }
            let (in0, out0) = (self.z.total_in(), self.z.total_out());
            let status = self
                .z
                .decompress_vec(&input[consumed..], out, FlushDecompress::None)
                .map_err(|e| malformed(format!("zlib: {e}")))?;
            let used = (self.z.total_in() - in0) as usize;
            let produced = (self.z.total_out() - out0) as usize;
            consumed += used;
            let room_left = out.len() < out.capacity();
            match status {
                Status::StreamEnd => return Ok(()),
                _ if consumed >= input.len() && room_left => return Ok(()),
                Status::BufError if used == 0 && produced == 0 && room_left => {
                    return Err(malformed("zlib stream stalled"));
                }
                _ => {}
            }
        }
    }

    /// Inflates `input` into exactly `expected` bytes of `out`.
    pub(crate) fn inflate_exact(&mut self, input: &[u8], out: &mut Vec<u8>, expected: usize) -> Result<()> {
        self.inflate_all(input, out)?;
        if out.len() != expected {
            return Err(malformed(format!("zlib rect inflated to {} bytes, expected {expected}", out.len())));
        }
        Ok(())
    }
}

/// Pixel rects larger than this cannot come from a real server (u16 sides allow 16 GiB).
const MAX_RECT_PIXELS: usize = 64 << 20;

/// Rejects rects whose payload could not be real, before anything is allocated for them.
pub(crate) fn check_size(r: Rect) -> Result<()> {
    if r.w as usize * r.h as usize > MAX_RECT_PIXELS {
        return Err(malformed(format!("rect {r:?} is impossibly large")));
    }
    Ok(())
}

/// The visible part of `r` in a `width` x `height` framebuffer, or `None` if it is empty.
pub(crate) fn clip(r: Rect, width: u32, height: u32) -> Option<Rect> {
    let w = (r.w as u32).min(width.saturating_sub(r.x as u32));
    let h = (r.h as u32).min(height.saturating_sub(r.y as u32));
    (w > 0 && h > 0).then_some(Rect { x: r.x, y: r.y, w: w as u16, h: h as u16 })
}

/// Writes 32 bpp little-endian pixels (`r.w * r.h * 4` bytes) into the framebuffer, clipped
/// to it. Returns the area actually written.
pub(crate) fn put_raw(fb: &mut Framebuffer, r: Rect, bytes: &[u8]) -> Option<Rect> {
    let vis = clip(r, fb.width, fb.height)?;
    let (src_stride, stride, vw) = (r.w as usize * 4, fb.width as usize, vis.w as usize);
    for (row, src) in bytes.chunks_exact(src_stride).take(vis.h as usize).enumerate() {
        let start = (r.y as usize + row) * stride + r.x as usize;
        let dst = &mut fb.pixels[start..start + vw];
        for (d, s) in dst.iter_mut().zip(src.as_chunks::<4>().0) {
            *d = u32::from_le_bytes([s[0], s[1], s[2], 0]);
        }
    }
    Some(vis)
}

/// Copies an `r.w * r.h` pixel buffer into the framebuffer at `r`, clipped to it. Returns
/// the area actually written.
pub(crate) fn blit(fb: &mut Framebuffer, r: Rect, src: &[u32]) -> Option<Rect> {
    let vis = clip(r, fb.width, fb.height)?;
    let (src_stride, stride, vw) = (r.w as usize, fb.width as usize, vis.w as usize);
    for (row, line) in src.chunks_exact(src_stride).take(vis.h as usize).enumerate() {
        let start = (r.y as usize + row) * stride + r.x as usize;
        fb.pixels[start..start + vw].copy_from_slice(&line[..vw]);
    }
    Some(vis)
}

/// CopyRect: moves the area at (`sx`, `sy`) to `r`, overlap-safe, clipped so that both
/// source and destination lie inside the framebuffer. Returns the area actually written.
pub(crate) fn copy_rect(fb: &mut Framebuffer, r: Rect, sx: u16, sy: u16) -> Option<Rect> {
    let dst = clip(r, fb.width, fb.height)?;
    let src = clip(Rect { x: sx, y: sy, w: dst.w, h: dst.h }, fb.width, fb.height)?;
    let (w, h, stride) = (src.w as usize, src.h as usize, fb.width as usize);
    let mut copy_row = |row: usize| {
        let from = (sy as usize + row) * stride + sx as usize;
        let to = (r.y as usize + row) * stride + r.x as usize;
        fb.pixels.copy_within(from..from + w, to);
    };
    if r.y > sy {
        (0..h).rev().for_each(&mut copy_row);
    } else {
        (0..h).for_each(&mut copy_row);
    }
    Some(Rect { x: r.x, y: r.y, w: src.w, h: src.h })
}

/// Cursor pseudo-encoding payload: `w * h` 32 bpp pixels then a 1-bit mask, rows padded to
/// whole bytes, most significant bit first.
pub(crate) fn cursor(r: Rect, data: &[u8]) -> Result<CursorShape> {
    let (w, h) = (r.w as usize, r.h as usize);
    let mask_row = w.div_ceil(8);
    if data.len() != w * h * 4 + mask_row * h {
        return Err(malformed("cursor payload size"));
    }
    let (pix, mask) = data.split_at(w * h * 4);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let p = &pix[(y * w + x) * 4..][..4];
            let opaque = mask[y * mask_row + x / 8] & (0x80 >> (x % 8)) != 0;
            let alpha = if opaque { 0xFF00_0000 } else { 0 };
            pixels.push(alpha | u32::from_le_bytes([p[0], p[1], p[2], 0]));
        }
    }
    Ok(CursorShape { hotspot_x: r.x, hotspot_y: r.y, width: r.w, height: r.h, pixels })
}

/// Cursor over a ZRLE payload with bounds-checked reads.
struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bytes<'a> {
    #[inline]
    fn u8(&mut self) -> Result<u8> {
        let b = *self.data.get(self.pos).ok_or_else(|| malformed("ZRLE data truncated"))?;
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let end = end.ok_or_else(|| malformed("ZRLE data truncated"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// A 3-byte compressed pixel (least significant bytes of 0x00RRGGBB, little-endian).
    #[inline]
    fn cpixel(&mut self) -> Result<u32> {
        let s = self.take(3)?;
        Ok(cpixel(s))
    }

    /// ZRLE run length: 1 + sum of bytes up to and including the first byte below 255.
    #[inline]
    fn run_length(&mut self) -> Result<usize> {
        let mut n = 1usize;
        loop {
            let b = self.u8()?;
            n += b as usize;
            if b != 255 {
                return Ok(n);
            }
        }
    }
}

#[inline]
fn cpixel(s: &[u8]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16
}

/// Fills `n` pixels of a `tw`-wide tile starting at linear tile position `*pos`.
#[inline]
fn fill_run(out: &mut [u32], base: usize, stride: usize, tw: usize, pos: &mut usize, mut n: usize, colour: u32) {
    while n > 0 {
        let (row, col) = (*pos / tw, *pos % tw);
        let k = n.min(tw - col);
        let start = base + row * stride + col;
        out[start..start + k].fill(colour);
        *pos += k;
        n -= k;
    }
}

fn read_palette(b: &mut Bytes<'_>, size: usize, palette: &mut [u32; 128]) -> Result<()> {
    let s = b.take(size * 3)?;
    for (p, c) in palette.iter_mut().zip(s.as_chunks::<3>().0) {
        *p = cpixel(c);
    }
    Ok(())
}

/// Decodes an inflated ZRLE rectangle (`w x h`) into `out`, row-major with stride `w`.
pub(crate) fn zrle(data: &[u8], w: usize, h: usize, out: &mut Vec<u32>) -> Result<()> {
    out.clear();
    out.resize(w * h, 0);
    let mut b = Bytes { data, pos: 0 };
    let mut palette = [0u32; 128];
    for ty in (0..h).step_by(64) {
        let th = (h - ty).min(64);
        for tx in (0..w).step_by(64) {
            let tw = (w - tx).min(64);
            let base = ty * w + tx;
            let sub = b.u8()?;
            match sub {
                0 => {
                    let s = b.take(tw * th * 3)?;
                    for (row, line) in s.chunks_exact(tw * 3).enumerate() {
                        let start = base + row * w;
                        for (d, c) in out[start..start + tw].iter_mut().zip(line.as_chunks::<3>().0) {
                            *d = cpixel(c);
                        }
                    }
                }
                1 => {
                    let colour = b.cpixel()?;
                    for row in 0..th {
                        let start = base + row * w;
                        out[start..start + tw].fill(colour);
                    }
                }
                2..=16 => {
                    let size = sub as usize;
                    read_palette(&mut b, size, &mut palette)?;
                    let bits = match size {
                        2 => 1,
                        3..=4 => 2,
                        _ => 4,
                    };
                    let mask = (1u8 << bits) - 1;
                    let row_bytes = (tw * bits).div_ceil(8);
                    let packed = b.take(row_bytes * th)?;
                    for (row, line) in packed.chunks_exact(row_bytes).enumerate() {
                        let start = base + row * w;
                        for (col, d) in out[start..start + tw].iter_mut().enumerate() {
                            let bit = col * bits;
                            let idx = (line[bit / 8] >> (8 - bits - bit % 8)) & mask;
                            if idx as usize >= size {
                                return Err(malformed("ZRLE packed palette index out of range"));
                            }
                            *d = palette[idx as usize];
                        }
                    }
                }
                128 => {
                    let total = tw * th;
                    let mut pos = 0usize;
                    while pos < total {
                        let colour = b.cpixel()?;
                        let n = b.run_length()?;
                        if n > total - pos {
                            return Err(malformed("ZRLE run overflows tile"));
                        }
                        fill_run(out, base, w, tw, &mut pos, n, colour);
                    }
                }
                130..=255 => {
                    let size = sub as usize - 128;
                    read_palette(&mut b, size, &mut palette)?;
                    let total = tw * th;
                    let mut pos = 0usize;
                    while pos < total {
                        let v = b.u8()?;
                        let idx = (v & 0x7F) as usize;
                        if idx >= size {
                            return Err(malformed("ZRLE palette RLE index out of range"));
                        }
                        let n = if v & 0x80 != 0 { b.run_length()? } else { 1 };
                        if n > total - pos {
                            return Err(malformed("ZRLE run overflows tile"));
                        }
                        fill_run(out, base, w, tw, &mut pos, n, palette[idx]);
                    }
                }
                _ => return Err(malformed(format!("ZRLE subencoding {sub}"))),
            }
        }
    }
    if b.pos != data.len() {
        return Err(malformed(format!("ZRLE rect left {} unused bytes", data.len() - b.pos)));
    }
    Ok(())
}
