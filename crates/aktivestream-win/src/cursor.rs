//! Local cursor from the server's Cursor pseudo-encoding, scaled with the view so it matches
//! what the Mac shows.

use aktivestream_rfb::CursorShape;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyCursor, HCURSOR, HICON, ICONINFO};

pub struct Cursor(pub HCURSOR);

impl Drop for Cursor {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyCursor(self.0);
        }
    }
}

/// Builds a cursor scaled by `scale` (view width / framebuffer width), nearest neighbour.
pub fn create(shape: &CursorShape, scale: f64) -> Option<Cursor> {
    if shape.width == 0 || shape.height == 0 {
        return None;
    }
    let scale = scale.clamp(0.25, 1.0);
    let w = ((f64::from(shape.width) * scale).round() as i32).max(1);
    let h = ((f64::from(shape.height) * scale).round() as i32).max(1);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let mut bits = std::ptr::null_mut();
        let color: HBITMAP = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dst = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
        for y in 0..h {
            let sy = ((f64::from(y) / scale) as usize).min(usize::from(shape.height) - 1);
            for x in 0..w {
                let sx = ((f64::from(x) / scale) as usize).min(usize::from(shape.width) - 1);
                let argb = shape.pixels[sy * usize::from(shape.width) + sx];
                // 32-bit cursors use premultiplied alpha; the server's alpha is 0 or 255.
                dst[(y * w + x) as usize] = if argb >> 24 == 0 { 0 } else { argb | 0xff00_0000 };
            }
        }
        let mask = CreateBitmap(w, h, 1, 1, None);
        let icon = ICONINFO {
            fIcon: false.into(),
            xHotspot: (f64::from(shape.hotspot_x) * scale) as u32,
            yHotspot: (f64::from(shape.hotspot_y) * scale) as u32,
            hbmMask: mask,
            hbmColor: color,
        };
        let created: windows::core::Result<HICON> = CreateIconIndirect(&icon);
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        created.ok().map(|h| Cursor(HCURSOR(h.0)))
    }
}
