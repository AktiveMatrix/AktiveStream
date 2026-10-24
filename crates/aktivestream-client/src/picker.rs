//! The connection picker: a full-window list of saved connections drawn to a `0x00RRGGBB` buffer,
//! shown when a client starts with no target. Platform-neutral layout, drawing and hit-testing;
//! each client composites it to its surface and launches the connection the user chooses.

use crate::config::{Connection, Mode};
use crate::draw;

const PAD: usize = 24;
const TITLE_SCALE: usize = 3; // 24px tall.
const ROW_SCALE: usize = 2; // 16px tall — larger than the toolbar for readability.
/// Top edge of the first connection row.
pub const TOP: usize = 88;
/// Height of each connection row.
pub const ROW_H: usize = 44;

const BG: u32 = 0x16_16_18;
const SELECTED_BG: u32 = 0x24_34_50;
const ACCENT: u32 = 0x5a_8a_ff;

/// The connection row (0-based) at `cursor`, if the cursor is over one of `n` rows within `dh`.
pub fn row_at(cursor: (f64, f64), dh: usize, n: usize) -> Option<usize> {
    if cursor.1 < TOP as f64 || cursor.0 < 0.0 {
        return None;
    }
    let idx = (cursor.1 as usize - TOP) / ROW_H;
    if idx < n && TOP + (idx + 1) * ROW_H <= dh { Some(idx) } else { None }
}

/// Draws the picker: a title, a key hint, and one row per connection (name on the left, host:port
/// and mode on the right), with `selected` highlighted. An empty list shows how to add one.
pub fn draw_picker(buf: &mut [u32], dw: usize, dh: usize, entries: &[Connection], selected: usize) {
    draw::fill_rect(buf, dw, 0, 0, dw, dh, BG);
    draw::text(buf, dw, PAD, PAD, "AktiveStream", 0xf0_f0_f2, dw.saturating_sub(PAD), TITLE_SCALE);
    draw::text(
        buf,
        dw,
        PAD,
        PAD + 8 * TITLE_SCALE + 8,
        "UP/DOWN select    ENTER connect    ESC quit",
        0x80_80_86,
        dw.saturating_sub(PAD),
        1,
    );
    if entries.is_empty() {
        draw::text(buf, dw, PAD, TOP, "No saved connections yet.", 0xaa_aa_aa, dw.saturating_sub(PAD), ROW_SCALE);
        draw::text(
            buf,
            dw,
            PAD,
            TOP + 8 * ROW_SCALE + 10,
            "Run  aktivestream <host>  once to save one to the phone book.",
            0x80_80_86,
            dw.saturating_sub(PAD),
            1,
        );
        return;
    }
    for (i, c) in entries.iter().enumerate() {
        let ry = TOP + i * ROW_H;
        if ry + ROW_H > dh {
            break;
        }
        if i == selected {
            draw::fill_rect(buf, dw, 0, ry, dw, ROW_H, SELECTED_BG);
            draw::fill_rect(buf, dw, 0, ry, 5, ROW_H, ACCENT);
        }
        let ty = ry + ROW_H.saturating_sub(8 * ROW_SCALE) / 2;
        let name_color = if i == selected { 0xff_ff_ff } else { 0xcc_cc_cc };
        // host:port is right-aligned, the mode tag sits just left of it, and the name fills the
        // space remaining on the left (clipped so it never collides with the mode).
        let hostport = format!("{}:{}", c.host, c.port);
        let hp_x = dw.saturating_sub(PAD).saturating_sub(draw::text_width(&hostport, ROW_SCALE));
        let mode = match c.mode {
            Mode::Native => "native",
            Mode::Rfb => "vnc",
        };
        let mode_x = hp_x.saturating_sub(16).saturating_sub(draw::text_width(mode, 1));
        draw::text(buf, dw, PAD, ty, &c.name, name_color, mode_x.saturating_sub(12), ROW_SCALE);
        draw::text(buf, dw, mode_x, ry + ROW_H.saturating_sub(8) / 2, mode, 0x7a_8a_aa, hp_x, 1);
        draw::text(buf, dw, hp_x, ty, &hostport, 0xc8_c8_cc, dw.saturating_sub(PAD), ROW_SCALE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(name: &str) -> Connection {
        Connection {
            name: name.into(),
            host: "h".into(),
            port: 5900,
            mode: Mode::Rfb,
            username: None,
            tunnel: None,
            generic: false,
        }
    }

    #[test]
    fn row_hit_testing() {
        let dh = TOP + ROW_H * 3 + 10;
        assert_eq!(row_at((10.0, (TOP + 2) as f64), dh, 3), Some(0));
        assert_eq!(row_at((10.0, (TOP + ROW_H + 2) as f64), dh, 3), Some(1));
        assert_eq!(row_at((10.0, 10.0), dh, 3), None, "above the first row");
        assert_eq!(row_at((10.0, (TOP + 2) as f64), dh, 0), None, "no rows");
    }

    #[test]
    fn draws_selected_highlight() {
        let (w, h) = (400usize, TOP + ROW_H * 2 + 20);
        let mut buf = vec![0u32; w * h];
        let entries = [conn("alpha"), conn("beta")];
        draw_picker(&mut buf, w, h, &entries, 1);
        assert_eq!(buf[0], BG, "background");
        let row1 = TOP + ROW_H; // the selected (second) row
        assert_eq!(buf[(row1 + 2) * w + 2], ACCENT, "accent bar on the selected row");
        assert_eq!(buf[(row1 + 2) * w + 100], SELECTED_BG, "selected row background");
    }
}
