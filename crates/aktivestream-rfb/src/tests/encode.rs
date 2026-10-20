//! Test-only encoders: produce ZRLE and Zlib payloads with a chosen subencoding per tile.

use flate2::{Compress, Compression, FlushCompress};

/// How the encoder writes one 64x64 (or edge) tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tile {
    Raw,
    Solid,
    Packed,
    PlainRle,
    PaletteRle,
}

/// One persistent zlib stream, flushed with Z_SYNC_FLUSH per rect like a real server.
pub(crate) struct Deflater(Compress);

impl Deflater {
    pub(crate) fn new() -> Deflater {
        Deflater(Compress::new(Compression::default(), true))
    }

    pub(crate) fn chunk(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 1024);
        let mut consumed = 0usize;
        loop {
            if out.capacity() - out.len() < 1024 {
                out.reserve(out.capacity().max(4096));
            }
            let before = self.0.total_in();
            self.0.compress_vec(&data[consumed..], &mut out, FlushCompress::Sync).unwrap();
            consumed += (self.0.total_in() - before) as usize;
            if consumed == data.len() && out.len() < out.capacity() {
                return out;
            }
        }
    }
}

fn push_cpixel(out: &mut Vec<u8>, p: u32) {
    out.extend_from_slice(&p.to_le_bytes()[..3]);
}

fn push_run(out: &mut Vec<u8>, n: usize) {
    let mut rest = n - 1;
    while rest >= 255 {
        out.push(255);
        rest -= 255;
    }
    out.push(rest as u8);
}

fn palette_of(pixels: &[u32]) -> Vec<u32> {
    let mut palette = Vec::new();
    for &p in pixels {
        if !palette.contains(&p) {
            palette.push(p);
        }
    }
    palette
}

fn runs(pixels: &[u32]) -> Vec<(u32, usize)> {
    let mut out: Vec<(u32, usize)> = Vec::new();
    for &p in pixels {
        match out.last_mut() {
            Some((c, n)) if *c == p => *n += 1,
            _ => out.push((p, 1)),
        }
    }
    out
}

fn encode_tile(out: &mut Vec<u8>, tile: &[u32], tw: usize, mode: Tile) {
    match mode {
        Tile::Raw => {
            out.push(0);
            tile.iter().for_each(|&p| push_cpixel(out, p));
        }
        Tile::Solid => {
            assert!(tile.iter().all(|&p| p == tile[0]), "solid tile must be one colour");
            out.push(1);
            push_cpixel(out, tile[0]);
        }
        Tile::Packed => {
            let palette = palette_of(tile);
            assert!((2..=16).contains(&palette.len()), "packed tile needs 2..=16 colours");
            out.push(palette.len() as u8);
            palette.iter().for_each(|&p| push_cpixel(out, p));
            let bits = match palette.len() {
                2 => 1,
                3..=4 => 2,
                _ => 4,
            };
            for row in tile.chunks_exact(tw) {
                let mut byte = 0u8;
                let mut used = 0;
                for &p in row {
                    let idx = palette.iter().position(|&c| c == p).unwrap() as u8;
                    byte |= idx << (8 - bits - used);
                    used += bits;
                    if used == 8 {
                        out.push(byte);
                        byte = 0;
                        used = 0;
                    }
                }
                if used > 0 {
                    out.push(byte);
                }
            }
        }
        Tile::PlainRle => {
            out.push(128);
            for (colour, n) in runs(tile) {
                push_cpixel(out, colour);
                push_run(out, n);
            }
        }
        Tile::PaletteRle => {
            let palette = palette_of(tile);
            assert!((2..=127).contains(&palette.len()), "palette RLE tile needs 2..=127 colours");
            out.push(128 + palette.len() as u8);
            palette.iter().for_each(|&p| push_cpixel(out, p));
            for (colour, n) in runs(tile) {
                let idx = palette.iter().position(|&c| c == colour).unwrap() as u8;
                if n == 1 {
                    out.push(idx);
                } else {
                    out.push(idx | 0x80);
                    push_run(out, n);
                }
            }
        }
    }
}

/// Uncompressed ZRLE tile stream for a `w x h` rect, tile `i` (row-major) written as `modes(i)`.
pub(crate) fn zrle_tiles(pixels: &[u32], w: usize, h: usize, modes: impl Fn(usize) -> Tile) -> Vec<u8> {
    let mut out = Vec::new();
    let mut index = 0;
    for ty in (0..h).step_by(64) {
        let th = (h - ty).min(64);
        for tx in (0..w).step_by(64) {
            let tw = (w - tx).min(64);
            let mut tile = Vec::with_capacity(tw * th);
            for row in 0..th {
                let start = (ty + row) * w + tx;
                tile.extend_from_slice(&pixels[start..start + tw]);
            }
            encode_tile(&mut out, &tile, tw, modes(index));
            index += 1;
        }
    }
    out
}

/// Fills a `w x h` image so tile `i` suits `modes(i)`: one colour for solid tiles,
/// `colours(i)` colours for palette tiles, arbitrary colours for raw and plain RLE.
pub(crate) fn image_for(
    w: usize,
    h: usize,
    modes: impl Fn(usize) -> Tile,
    colours: impl Fn(usize) -> usize,
) -> Vec<u32> {
    let mut pixels = vec![0u32; w * h];
    let mut index = 0;
    for ty in (0..h).step_by(64) {
        let th = (h - ty).min(64);
        for tx in (0..w).step_by(64) {
            let tw = (w - tx).min(64);
            let seed = (index as u32 + 1).wrapping_mul(0x9E37_79B9);
            for row in 0..th {
                for col in 0..tw {
                    let (x, y) = (tx + col, ty + row);
                    let v = match modes(index) {
                        Tile::Solid => seed,
                        Tile::Packed | Tile::PaletteRle => {
                            // Runs of 3 so palette RLE sees both single pixels and runs.
                            let k = colours(index);
                            let c = ((x / 3) * 7 + y * 3 + (x % 3 == 0) as usize) % k;
                            seed ^ (c as u32).wrapping_mul(0x0101_0F0F)
                        }
                        Tile::PlainRle => seed ^ ((((x / 5) + y) % 4) as u32 * 0x0011_2233),
                        Tile::Raw => {
                            seed ^ ((x as u32).wrapping_mul(0x00A3_1F07) ^ (y as u32).wrapping_mul(0x0071_9E11))
                        }
                    };
                    pixels[y * w + x] = v & 0x00FF_FFFF;
                }
            }
            index += 1;
        }
    }
    pixels
}

/// 32 bpp little-endian bytes for raw-style payloads.
pub(crate) fn raw_bytes(pixels: &[u32]) -> Vec<u8> {
    pixels.iter().flat_map(|p| p.to_le_bytes()).collect()
}
