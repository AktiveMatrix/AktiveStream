//! Software-drawing primitives shared by the client overlays (the toolbar and the connection
//! picker): direct writes to a `0x00RRGGBB` pixel buffer, with the public-domain 8x8 bitmap font
//! for text. No platform dependencies, so every client composites the same way.

use font8x8::UnicodeFonts;

/// Fills a rectangle, clipped to the buffer.
pub fn fill_rect(buf: &mut [u32], dw: usize, x0: usize, y0: usize, w: usize, h: usize, color: u32) {
    for y in y0..y0 + h {
        let row = y * dw;
        for x in x0..x0 + w {
            if let Some(px) = buf.get_mut(row + x) {
                *px = color;
            }
        }
    }
}

/// Draws a 1px rectangle outline.
pub fn rect_outline(buf: &mut [u32], dw: usize, x: usize, y: usize, w: usize, h: usize, color: u32) {
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

/// Draws a diagonal cross (an X) in a `size`x`size` box.
pub fn cross(buf: &mut [u32], dw: usize, x: usize, y: usize, size: usize, color: u32) {
    for i in 0..size {
        if let Some(px) = buf.get_mut((y + i) * dw + x + i) {
            *px = color;
        }
        if let Some(px) = buf.get_mut((y + i) * dw + x + size - 1 - i) {
            *px = color;
        }
    }
}

/// Draws `text` with the 8x8 bitmap font, each glyph pixel scaled to a `scale`x`scale` block
/// (scale 1 = 8px tall, 2 = 16px, …), stopping at `max_x` or the buffer edge. Returns the x
/// just past the last glyph drawn.
#[allow(clippy::too_many_arguments)]
pub fn text(buf: &mut [u32], dw: usize, x0: usize, y: usize, s: &str, color: u32, max_x: usize, scale: usize) -> usize {
    let scale = scale.max(1);
    let advance = 8 * scale;
    let mut x = x0;
    for ch in s.chars() {
        if x + advance > max_x {
            break;
        }
        if let Some(glyph) = font8x8::BASIC_FONTS.get(ch) {
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8 {
                    if (bits >> col) & 1 == 0 {
                        continue;
                    }
                    for dy in 0..scale {
                        let py = y + row * scale + dy;
                        let base = py * dw + x + col * scale;
                        for dx in 0..scale {
                            if let Some(px) = buf.get_mut(base + dx) {
                                *px = color;
                            }
                        }
                    }
                }
            }
        }
        x += advance;
    }
    x
}

/// Width in pixels that `text` would occupy at `scale` (8px per glyph, times the scale).
pub fn text_width(s: &str, scale: usize) -> usize {
    s.chars().count() * 8 * scale.max(1)
}

/// Copies a `sw`x`sh` source buffer of `0x00RRGGBB` pixels into `dst` (a `dw`-wide frame) with its
/// top-left at (`x`, `y`), clipped to the destination. Used to composite a pre-rendered overlay
/// panel (the stats readout) onto a client's frame.
#[allow(clippy::too_many_arguments)]
pub fn blit(dst: &mut [u32], dw: usize, src: &[u32], sw: usize, sh: usize, x: usize, y: usize) {
    for row in 0..sh {
        let dy = y + row;
        let dst_base = dy * dw + x;
        let src_base = row * sw;
        for col in 0..sw {
            if x + col >= dw {
                break; // Stay on this row; do not wrap into the next.
            }
            if let (Some(px), Some(&s)) = (dst.get_mut(dst_base + col), src.get(src_base + col)) {
                *px = s;
            }
        }
    }
}

/// Builds a tight dark panel (bg `0x12121a`, border `0x333340`) holding one left-aligned line of
/// text at `scale`, sized to its content. Returns `(pixels, width, height)` for the client to
/// composite — used for the transient capture confirmation toast, matching the stats readout's look.
pub fn label_panel(s: &str, scale: usize) -> (Vec<u32>, usize, usize) {
    const PAD: usize = 10;
    let w = text_width(s, scale) + 2 * PAD;
    let h = 2 * PAD + 8 * scale.max(1);
    let mut buf = vec![0u32; w * h];
    fill_rect(&mut buf, w, 0, 0, w, h, 0x12_12_1a);
    rect_outline(&mut buf, w, 0, 0, w, h, 0x33_33_40);
    text(&mut buf, w, PAD, PAD, s, 0xdd_dd_dd, w.saturating_sub(PAD), scale);
    (buf, w, h)
}
