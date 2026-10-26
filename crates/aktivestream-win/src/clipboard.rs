//! Clipboard access: plain text (CF_UNICODETEXT) and screenshots (CF_DIB).

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;

fn open(hwnd: HWND) -> bool {
    // Another process may hold the clipboard for a moment.
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(hwnd)) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

pub fn read_text(hwnd: HWND) -> Option<String> {
    if !open(hwnd) {
        return None;
    }
    let text = unsafe {
        GetClipboardData(CF_UNICODETEXT).ok().and_then(|handle| {
            let global = HGLOBAL(handle.0);
            let ptr = GlobalLock(global) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(global);
            Some(text)
        })
    };
    unsafe {
        let _ = CloseClipboard();
    }
    text
}

pub fn write_text(hwnd: HWND, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    if !open(hwnd) {
        return false;
    }
    let ok = unsafe {
        let _ = EmptyClipboard();
        match GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            Ok(global) => {
                let ptr = GlobalLock(global) as *mut u16;
                if ptr.is_null() {
                    let _ = GlobalFree(Some(global));
                    false
                } else {
                    std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                    let _ = GlobalUnlock(global);
                    // On success the clipboard owns the memory.
                    if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(global.0))).is_ok() {
                        true
                    } else {
                        let _ = GlobalFree(Some(global));
                        false
                    }
                }
            }
            Err(_) => false,
        }
    };
    unsafe {
        let _ = CloseClipboard();
    }
    ok
}

/// Packs top-down RGBA pixels into a CF_DIB (a BITMAPINFOHEADER followed by bottom-up 32bpp BGRA
/// rows — the order most clipboard consumers expect). Pure, so it can be unit-tested.
fn dib_from_rgba(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let (wi, hi) = (w as usize, h as usize);
    let mut dib = Vec::with_capacity(40 + wi * hi * 4);
    let push = |dib: &mut Vec<u8>, v: u32| dib.extend_from_slice(&v.to_le_bytes());
    push(&mut dib, 40); // biSize
    push(&mut dib, w); // biWidth
    push(&mut dib, h); // biHeight (positive = bottom-up)
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    for _ in 0..5 {
        push(&mut dib, 0); // biCompression (BI_RGB), biSizeImage, x/y PPM, clrUsed/Important
    }
    push(&mut dib, 0); // biClrImportant (sixth u32; six trailing u32 fields in all)
    for y in (0..hi).rev() {
        let row = y * wi * 4;
        for x in 0..wi {
            let p = row + x * 4;
            let r = rgba.get(p).copied().unwrap_or(0);
            let g = rgba.get(p + 1).copied().unwrap_or(0);
            let b = rgba.get(p + 2).copied().unwrap_or(0);
            let a = rgba.get(p + 3).copied().unwrap_or(0xff);
            dib.extend_from_slice(&[b, g, r, a]);
        }
    }
    dib
}

/// Puts an image (top-down RGBA) on the clipboard as a device-independent bitmap.
pub fn write_image(hwnd: HWND, w: u32, h: u32, rgba: &[u8]) -> bool {
    if w == 0 || h == 0 {
        return false;
    }
    let dib = dib_from_rgba(w, h, rgba);
    if !open(hwnd) {
        return false;
    }
    let ok = unsafe {
        let _ = EmptyClipboard();
        match GlobalAlloc(GMEM_MOVEABLE, dib.len()) {
            Ok(global) => {
                let ptr = GlobalLock(global) as *mut u8;
                if ptr.is_null() {
                    let _ = GlobalFree(Some(global));
                    false
                } else {
                    std::ptr::copy_nonoverlapping(dib.as_ptr(), ptr, dib.len());
                    let _ = GlobalUnlock(global);
                    // On success the clipboard owns the memory.
                    if SetClipboardData(CF_DIB, Some(HANDLE(global.0))).is_ok() {
                        true
                    } else {
                        let _ = GlobalFree(Some(global));
                        false
                    }
                }
            }
            Err(_) => false,
        }
    };
    unsafe {
        let _ = CloseClipboard();
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::dib_from_rgba;

    #[test]
    fn dib_header_and_pixel_order() {
        // 1x1 red: a 40-byte header, then one bottom-up BGRA pixel.
        let dib = dib_from_rgba(1, 1, &[255, 0, 0, 255]);
        assert_eq!(dib.len(), 44);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40); // biSize
        assert_eq!(u32::from_le_bytes(dib[4..8].try_into().unwrap()), 1); // biWidth
        assert_eq!(u32::from_le_bytes(dib[8..12].try_into().unwrap()), 1); // biHeight
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 32); // biBitCount
        assert_eq!(&dib[40..44], &[0, 0, 255, 255]); // B,G,R,A
    }

    #[test]
    fn dib_rows_are_bottom_up() {
        // Two rows: top red, bottom green. Bottom-up order puts green first.
        let dib = dib_from_rgba(1, 2, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(&dib[40..44], &[0, 255, 0, 255], "bottom row (green) first");
        assert_eq!(&dib[44..48], &[0, 0, 255, 255], "top row (red) second");
    }
}
