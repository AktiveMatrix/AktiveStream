//! The full-screen overlay toolbar, drawn directly onto a `0x00RRGGBB` pixel buffer so every
//! client can composite it with its own renderer (Linux softbuffer, Windows Direct3D 11, …).
//! Platform-neutral: layout, hit-testing and drawing only. Remmina-style — revealed when the
//! pointer touches the top edge in full screen, with the connection name on the left and buttons
//! to leave full screen or disconnect on the right.

use font8x8::UnicodeFonts;

/// Height in pixels of the overlay toolbar.
pub const BAR_HEIGHT: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    Windowed,
    Disconnect,
}

/// Which toolbar button, if any, the cursor (window pixels) is over (two right-aligned squares).
pub fn bar_action_at(cursor: (f64, f64), dw: usize) -> Option<BarAction> {
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

/// Draws the overlay toolbar into the top of `buf` (a `dw`x`dh` frame of `0x00RRGGBB` pixels): a
/// dark strip with `title` on the left and, on the right, a "leave full screen" (rectangle) and a
/// red "disconnect" (X) button, the hovered one highlighted.
pub fn draw_bar(buf: &mut [u32], dw: usize, dh: usize, hover: Option<BarAction>, title: &str) {
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

#[cfg(test)]
mod tests {
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
