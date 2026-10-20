//! Diagnostics for `--exit-after` / `--screenshot`: session statistics and a PNG of the
//! rendered client area, so rendering can be checked without anyone at the keyboard.

use std::path::Path;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub updates: u64,
    pub update_pixels: u64,
    pub paints: u64,
    pub paint_time: Duration,
    pub renderer: &'static str,
}

impl Stats {
    pub fn report(&self, elapsed: Duration) -> String {
        let secs = elapsed.as_secs_f64().max(1e-9);
        let avg_paint_ms =
            if self.paints == 0 { 0.0 } else { self.paint_time.as_secs_f64() * 1000.0 / self.paints as f64 };
        format!(
            "session {secs:.1}s ({}): {} updates ({:.1}/s), {:.1} Mpixel updated ({:.2} Mpixel/s), {} frames, {avg_paint_ms:.2} ms avg frame",
            self.renderer,
            self.updates,
            self.updates as f64 / secs,
            self.update_pixels as f64 / 1e6,
            self.update_pixels as f64 / 1e6 / secs,
            self.paints,
        )
    }
}

pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}

pub fn capture_client(hwnd: HWND, path: &Path) -> Result<(), String> {
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }.map_err(|e| e.to_string())?;
    let (w, h) = (client.right - client.left, client.bottom - client.top);
    if w <= 0 || h <= 0 {
        return Err("window has no client area".into());
    }
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
    let rgba = unsafe {
        let window_dc = GetDC(Some(hwnd));
        let memory_dc = CreateCompatibleDC(Some(window_dc));
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(window_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0);
        let result = bitmap.map_err(|e| e.to_string()).and_then(|bitmap| {
            let previous = SelectObject(memory_dc, bitmap.into());
            let copied = BitBlt(memory_dc, 0, 0, w, h, Some(window_dc), 0, 0, SRCCOPY);
            let pixels = std::slice::from_raw_parts(bits as *const u32, (w * h) as usize);
            let rgba: Vec<u8> = pixels.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]).collect();
            SelectObject(memory_dc, previous);
            let _ = DeleteObject(bitmap.into());
            copied.map(|_| rgba).map_err(|e| e.to_string())
        });
        let _ = DeleteDC(memory_dc);
        ReleaseDC(Some(hwnd), window_dc);
        result?
    };
    write_png(path, w as u32, h as u32, &rgba)
}
