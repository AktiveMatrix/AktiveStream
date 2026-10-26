//! The client toolbar, drawn directly onto a `0x00RRGGBB` pixel buffer so every client can
//! composite it with its own renderer (Linux/macOS softbuffer, Windows Direct3D 11, …).
//! Platform-neutral: layout, hit-testing and drawing only. It is a persistent strip across the top
//! in windowed mode (the remote view sits below it) and, in full screen, the same strip revealed
//! when the pointer touches the top edge. The connection name sits on the left; on the right are
//! capture, enter/leave-full-screen and disconnect buttons.

use crate::draw;

/// Height in pixels of the toolbar.
pub const BAR_HEIGHT: usize = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    /// Save a screenshot of the remote screen (file + clipboard).
    Capture,
    /// Enter or leave full screen (the icon reflects the current state).
    Fullscreen,
    /// Close the session.
    Disconnect,
}

/// The right-aligned buttons, left to right. Each is a [`BAR_HEIGHT`]-wide square.
const BUTTONS: [BarAction; 3] = [BarAction::Capture, BarAction::Fullscreen, BarAction::Disconnect];

/// Left edge (window x) of button `i` (0 = leftmost of the group), right-aligned in width `dw`.
fn button_x(dw: usize, i: usize) -> usize {
    dw - (BUTTONS.len() - i) * BAR_HEIGHT
}

/// Which toolbar button, if any, the cursor (window pixels) is over.
pub fn bar_action_at(cursor: (f64, f64), dw: usize) -> Option<BarAction> {
    if cursor.1 < 0.0 || cursor.1 >= BAR_HEIGHT as f64 || cursor.0 < 0.0 || dw < BUTTONS.len() * BAR_HEIGHT {
        return None;
    }
    let x = cursor.0 as usize;
    if x >= dw {
        return None;
    }
    let from_right = (dw - 1 - x) / BAR_HEIGHT; // 0 = rightmost square
    if from_right >= BUTTONS.len() {
        return None;
    }
    Some(BUTTONS[BUTTONS.len() - 1 - from_right])
}

/// Draws the toolbar into the top of `buf` (a `dw`x`dh` frame of `0x00RRGGBB` pixels): a dark strip
/// with `title` on the left and, on the right, capture / full-screen / disconnect buttons, the
/// hovered one highlighted. `fullscreen` picks the full-screen button's icon (leave vs enter).
pub fn draw_bar(buf: &mut [u32], dw: usize, dh: usize, hover: Option<BarAction>, title: &str, fullscreen: bool) {
    let bar_h = BAR_HEIGHT.min(dh);
    draw::fill_rect(buf, dw, 0, 0, dw, bar_h, 0x1c_1c_1c);
    if dw < BUTTONS.len() * BAR_HEIGHT {
        return;
    }
    let title_w = button_x(dw, 0).saturating_sub(8);
    draw::text(buf, dw, 10, bar_h.saturating_sub(8) / 2, title, 0xdd_dd_dd, title_w, 1);
    for (i, action) in BUTTONS.iter().enumerate() {
        let x0 = button_x(dw, i);
        if hover == Some(*action) {
            let bg = if *action == BarAction::Disconnect { 0x99_22_22 } else { 0x33_33_33 };
            draw::fill_rect(buf, dw, x0, 0, BAR_HEIGHT, bar_h, bg);
        }
        draw_icon(buf, dw, x0, *action, fullscreen);
    }
}

/// Draws one button's icon centred in its `BAR_HEIGHT`-wide square at `x0`.
fn draw_icon(buf: &mut [u32], dw: usize, x0: usize, action: BarAction, fullscreen: bool) {
    const FG: u32 = 0xdd_dd_dd;
    match action {
        BarAction::Capture => {
            // A camera: a small nub, a body and a lens.
            draw::fill_rect(buf, dw, x0 + 13, 9, 6, 3, FG);
            draw::rect_outline(buf, dw, x0 + 9, 12, 18, 14, FG);
            draw::rect_outline(buf, dw, x0 + 15, 16, 6, 6, 0xaa_aa_aa);
        }
        BarAction::Fullscreen if fullscreen => {
            // Leave full screen: two offset rectangles, the conventional "restore" glyph.
            draw::rect_outline(buf, dw, x0 + 9, 11, 13, 10, FG);
            draw::rect_outline(buf, dw, x0 + 13, 15, 13, 10, FG);
        }
        BarAction::Fullscreen => {
            // Enter full screen: a single screen rectangle.
            draw::rect_outline(buf, dw, x0 + 10, 12, 16, 12, FG);
        }
        BarAction::Disconnect => draw::cross(buf, dw, x0 + 11, 10, 14, 0xff_66_66),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_bar_fills_strip_and_hovered_button() {
        let (w, h) = (300usize, 50usize);
        let mut buf = vec![0u32; w * h];
        draw_bar(&mut buf, w, h, Some(BarAction::Disconnect), "studio - Mac Studio", false);
        assert_eq!(buf[0], 0x1c_1c_1c, "toolbar background");
        assert_eq!(buf[BAR_HEIGHT * w], 0, "below the toolbar is untouched");
        assert_eq!(buf[2 * w + (w - BAR_HEIGHT + 2)], 0x99_22_22, "disconnect hover highlight");
    }

    #[test]
    fn bar_hit_testing_three_buttons() {
        let dw = 400;
        assert_eq!(bar_action_at((399.0, 5.0), dw), Some(BarAction::Disconnect));
        assert_eq!(bar_action_at((399.0 - BAR_HEIGHT as f64, 5.0), dw), Some(BarAction::Fullscreen));
        assert_eq!(bar_action_at((399.0 - 2.0 * BAR_HEIGHT as f64, 5.0), dw), Some(BarAction::Capture));
        assert_eq!(bar_action_at((10.0, 5.0), dw), None, "title area is not a button");
        assert_eq!(bar_action_at((399.0, BAR_HEIGHT as f64), dw), None, "below the strip");
    }

    #[test]
    fn narrow_bar_has_no_buttons() {
        assert_eq!(bar_action_at((20.0, 5.0), 2 * BAR_HEIGHT), None);
    }

    #[test]
    fn fullscreen_flag_changes_only_the_fullscreen_icon() {
        // The restore glyph (leave full screen) paints a pixel the enter glyph leaves blank, so the
        // two states render differently without touching the other buttons.
        let (w, h) = (300usize, BAR_HEIGHT);
        let fs_x0 = button_x(w, 1);
        let mut enter = vec![0u32; w * h];
        let mut leave = vec![0u32; w * h];
        draw_bar(&mut enter, w, h, None, "x", false);
        draw_bar(&mut leave, w, h, None, "x", true);
        assert_ne!(enter[..], leave[..], "the full-screen icon differs by state");
        // The capture button (to the left of the full-screen one) is identical in both.
        let cap = button_x(w, 0);
        for y in 0..h {
            for x in cap..fs_x0 {
                assert_eq!(enter[y * w + x], leave[y * w + x], "capture icon unchanged");
            }
        }
    }
}
