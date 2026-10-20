use super::encode::{Deflater, Tile, image_for, raw_bytes, zrle_tiles};
use crate::decode::{self, Inflater};
use crate::{Framebuffer, Rect};

fn roundtrip(w: usize, h: usize, modes: &[Tile], colours: &[usize], z: &mut Deflater, inf: &mut Inflater) {
    let mode = |i: usize| modes[i % modes.len()];
    let count = |i: usize| colours[i % colours.len()];
    let pixels = image_for(w, h, mode, count);
    let compressed = z.chunk(&zrle_tiles(&pixels, w, h, mode));
    let mut inflated = Vec::new();
    inf.inflate_all(&compressed, &mut inflated).unwrap();
    let mut out = Vec::new();
    decode::zrle(&inflated, w, h, &mut out).unwrap();
    assert_eq!(out, pixels, "{w}x{h} modes {modes:?}");
}

#[test]
fn zrle_every_subencoding_and_edge_tiles() {
    // One persistent stream across rects, as on the wire.
    let (mut z, mut inf) = (Deflater::new(), Inflater::new());
    // 150x70: tiles 64/64/22 wide, 64/6 tall.
    roundtrip(
        150,
        70,
        &[Tile::Raw, Tile::Solid, Tile::Packed, Tile::Packed, Tile::Packed, Tile::PlainRle],
        &[2, 2, 2, 4, 9, 2],
        &mut z,
        &mut inf,
    );
    // 1-, 2- and 4-bit packed palettes on full and partial tiles.
    for colours in [2, 3, 4, 5, 16] {
        roundtrip(150, 70, &[Tile::Packed], &[colours], &mut z, &mut inf);
    }
    // Palette RLE with small and large palettes, plain RLE, raw; 2-pixel and 1-row edges.
    roundtrip(
        130,
        65,
        &[Tile::PaletteRle, Tile::PaletteRle, Tile::PlainRle, Tile::Raw, Tile::Solid, Tile::PlainRle],
        &[5, 100, 2, 2, 2, 2],
        &mut z,
        &mut inf,
    );
    roundtrip(64, 64, &[Tile::PaletteRle], &[127], &mut z, &mut inf);
    roundtrip(1, 1, &[Tile::Raw], &[2], &mut z, &mut inf);
    roundtrip(200, 3, &[Tile::PlainRle], &[2], &mut z, &mut inf);
}

#[test]
fn zrle_long_runs_cross_rows() {
    // A solid-colour plain RLE tile is one run of 4096 pixels: 4095 = 16 * 255 + 15,
    // so 17 length bytes after the subencoding byte and the colour.
    let (mut z, mut inf) = (Deflater::new(), Inflater::new());
    let pixels = vec![0x0012_3456u32; 64 * 64];
    let compressed = z.chunk(&zrle_tiles(&pixels, 64, 64, |_| Tile::PlainRle));
    let mut inflated = Vec::new();
    inf.inflate_all(&compressed, &mut inflated).unwrap();
    assert_eq!(inflated.len(), 1 + 3 + 17);
    let mut out = Vec::new();
    decode::zrle(&inflated, 64, 64, &mut out).unwrap();
    assert_eq!(out, pixels);
}

#[test]
fn zrle_rejects_truncated_and_overflowing_data() {
    let mut out = Vec::new();
    assert!(decode::zrle(&[0, 1, 2], 4, 4, &mut out).is_err(), "truncated raw tile");
    // Plain RLE run of 20 pixels in a 4x4 tile.
    assert!(decode::zrle(&[128, 1, 2, 3, 19], 4, 4, &mut out).is_err(), "run overflows tile");
    assert!(decode::zrle(&[17], 4, 4, &mut out).is_err(), "unused subencoding");
    // Solid tile plus a stray byte.
    assert!(decode::zrle(&[1, 1, 2, 3, 0], 4, 4, &mut out).is_err(), "trailing bytes");
}

#[test]
fn zlib_rects_share_one_stream() {
    let (mut z, mut inf) = (Deflater::new(), Inflater::new());
    let mut out = Vec::new();
    for seed in 0..3u32 {
        let pixels: Vec<u32> = (0..37 * 23u32).map(|i| (i.wrapping_mul(2_654_435_761) ^ seed) & 0x00FF_FFFF).collect();
        let bytes = raw_bytes(&pixels);
        inf.inflate_exact(&z.chunk(&bytes), &mut out, bytes.len()).unwrap();
        assert_eq!(out, bytes);
    }
}

#[test]
fn large_rect_inflates_past_initial_buffer() {
    let (mut z, mut inf) = (Deflater::new(), Inflater::new());
    let pixels: Vec<u32> = (0..1024 * 768).map(|i| (i % 251) * 0x0001_0203).collect();
    let bytes = raw_bytes(&pixels);
    let mut out = Vec::new();
    inf.inflate_exact(&z.chunk(&bytes), &mut out, bytes.len()).unwrap();
    assert_eq!(out, bytes);
}

#[test]
fn raw_blit_and_copy_rect_clip_to_framebuffer() {
    let mut fb = Framebuffer::new(8, 4);
    // 4x2 rect at (6, 3): only 2x1 is visible.
    let r = Rect { x: 6, y: 3, w: 4, h: 2 };
    let px: Vec<u32> = (1..=8).collect();
    assert_eq!(decode::put_raw(&mut fb, r, &raw_bytes(&px)), Some(Rect { x: 6, y: 3, w: 2, h: 1 }));
    assert_eq!(&fb.pixels[3 * 8 + 6..], &[1, 2]);
    assert_eq!(decode::blit(&mut fb, Rect { x: 9, y: 0, w: 2, h: 2 }, &[0; 4]), None);
    assert_eq!(decode::blit(&mut fb, Rect { x: 0, y: 0, w: 4, h: 2 }, &px), Some(Rect { x: 0, y: 0, w: 4, h: 2 }));
    // Overlapping copy down-right by one pixel.
    let moved = decode::copy_rect(&mut fb, Rect { x: 1, y: 1, w: 4, h: 2 }, 0, 0);
    assert_eq!(moved, Some(Rect { x: 1, y: 1, w: 4, h: 2 }));
    assert_eq!(&fb.pixels[8..13], &[5, 1, 2, 3, 4]);
    assert_eq!(&fb.pixels[16..21], &[0, 5, 6, 7, 8]);
    assert!(decode::check_size(Rect { x: 0, y: 0, w: 65535, h: 65535 }).is_err());
}

#[test]
fn cursor_mask_becomes_alpha() {
    // 3x2 cursor, mask rows 0b101_____ and 0b010_____.
    let px: Vec<u32> = vec![0x00_11_22_33, 0x00_44_55_66, 0x00_77_88_99, 1, 2, 3];
    let mut data = raw_bytes(&px);
    data.extend_from_slice(&[0b1010_0000, 0b0100_0000]);
    let c = decode::cursor(Rect { x: 1, y: 2, w: 3, h: 2 }, &data).unwrap();
    assert_eq!((c.hotspot_x, c.hotspot_y, c.width, c.height), (1, 2, 3, 2));
    assert_eq!(c.pixels, vec![0xFF11_2233, 0x0044_5566, 0xFF77_8899, 1, 0xFF00_0002, 3]);
}
