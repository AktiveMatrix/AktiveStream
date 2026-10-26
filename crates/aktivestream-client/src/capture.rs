//! Screenshot of the remote screen: encode the native-resolution framebuffer to PNG, plus the
//! default save path. The clipboard handoff is per-client (platform APIs); this module is the
//! shared, platform-neutral part, so every client writes identical files from the same pixels.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// RGBA8 bytes (row-major, opaque) from a `0x00RRGGBB` framebuffer, for PNG and the clipboard. A
/// buffer shorter than `w*h` is padded with black; extra trailing pixels are ignored.
pub fn to_rgba(pixels: &[u32], w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let p = pixels.get(i).copied().unwrap_or(0);
        out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xff]);
    }
    out
}

/// Encodes the framebuffer as an opaque 8-bit RGBA PNG.
pub fn encode_png(pixels: &[u32], w: usize, h: usize) -> Result<Vec<u8>, String> {
    let rgba = to_rgba(pixels, w, h);
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, w as u32, h as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// `<Pictures>/AktiveStream/<name>-YYYYMMDD-HHMMSS.png` (UTC), with `name` reduced to a safe slug.
/// The caller creates the parent directory and writes the file.
pub fn default_path(name: &str) -> PathBuf {
    pictures_dir().join("AktiveStream").join(format!("{}-{}.png", slug(name), timestamp(SystemTime::now())))
}

fn pictures_dir() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.map(|h| PathBuf::from(h).join("Pictures")).unwrap_or_else(std::env::temp_dir)
}

/// Keeps ASCII letters and digits; every maximal run of anything else becomes a single '-' (so a
/// meaningful internal dash like `STAR-LT-MBP` survives). Empty -> "capture".
fn slug(name: &str) -> String {
    let mut s = String::new();
    let mut dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
            dash = false;
        } else if !dash {
            s.push('-');
            dash = true;
        }
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "capture".to_string() } else { s }
}

/// `YYYYMMDD-HHMMSS` in UTC for `t`.
fn timestamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}")
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian. (Howard Hinnant,
/// "chrono-Compatible Low-Level Date Algorithms".)
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = yoe as i64 + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rgba_is_opaque_and_channel_ordered() {
        let rgba = to_rgba(&[0x00ff_0000, 0x0000_00ff], 2, 1);
        assert_eq!(rgba, vec![255, 0, 0, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn short_buffer_pads_with_black() {
        assert_eq!(to_rgba(&[], 1, 1), vec![0, 0, 0, 255]);
    }

    #[test]
    fn png_has_signature_and_decodes_to_size() {
        let png = encode_png(&[0x00ab_cdef; 4], 2, 2).unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']);
        let decoder = png::Decoder::new(&png[..]);
        let reader = decoder.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (2, 2));
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("studio - Mac Studio"), "studio-Mac-Studio");
        assert_eq!(slug("  "), "capture");
        assert_eq!(slug("a/b:c*d"), "a-b-c-d");
    }

    #[test]
    fn timestamp_is_utc_civil_time() {
        assert_eq!(timestamp(UNIX_EPOCH), "19700101-000000");
        // 1_700_000_000 is 2023-11-14 22:13:20 UTC.
        assert_eq!(timestamp(UNIX_EPOCH + Duration::from_secs(1_700_000_000)), "20231114-221320");
    }

    #[test]
    fn default_path_lands_under_aktivestream() {
        let p = default_path("mac - studio");
        assert_eq!(p.extension().unwrap(), "png");
        assert_eq!(p.parent().unwrap().file_name().unwrap(), "AktiveStream");
        assert!(p.file_name().unwrap().to_str().unwrap().starts_with("mac-studio-"));
    }
}
