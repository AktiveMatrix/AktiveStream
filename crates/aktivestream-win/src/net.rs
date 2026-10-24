//! Reader and writer threads. Neither the UI thread nor the keyboard hook ever touches the
//! socket: input goes through a channel to the writer, and the reader posts coalesced
//! notifications to the window.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;

use aktivestream_rfb::{CursorShape, Framebuffer, Rect, ServerEvent, SessionReader, SessionWriter};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_FRAME: u32 = WM_APP + 1;
pub const WM_APP_RESIZED: u32 = WM_APP + 2;
pub const WM_APP_CURSOR: u32 = WM_APP + 3;
pub const WM_APP_CUT_TEXT: u32 = WM_APP + 4;
pub const WM_APP_DISCONNECTED: u32 = WM_APP + 5;

/// Messages for the writer thread.
pub enum Out {
    Key(u32, bool),
    Pointer(u8, u16, u16),
    CutText(String),
}

/// State shared between the reader thread and the UI thread.
pub struct Shared {
    pub framebuffer: Mutex<Framebuffer>,
    /// Union of areas changed since the UI last looked.
    pub dirty: Mutex<Option<Rect>>,
    frame_posted: AtomicBool,
    pub cursor: Mutex<Option<CursorShape>>,
    pub cut_text: Mutex<Option<String>>,
    pub error: Mutex<Option<String>>,
    pub updates: AtomicU64,
    pub update_pixels: AtomicU64,
    hwnd: AtomicIsize,
}

impl Shared {
    pub fn new(width: u16, height: u16) -> Shared {
        Shared {
            framebuffer: Mutex::new(Framebuffer::new(width.into(), height.into())),
            dirty: Mutex::new(None),
            frame_posted: AtomicBool::new(false),
            cursor: Mutex::new(None),
            cut_text: Mutex::new(None),
            error: Mutex::new(None),
            updates: AtomicU64::new(0),
            update_pixels: AtomicU64::new(0),
            hwnd: AtomicIsize::new(0),
        }
    }

    pub fn set_hwnd(&self, hwnd: HWND) {
        self.hwnd.store(hwnd.0 as isize, Ordering::Release);
    }

    fn post(&self, message: u32) {
        let hwnd = HWND(self.hwnd.load(Ordering::Acquire) as *mut _);
        if !hwnd.is_invalid() {
            unsafe {
                let _ = PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Called by the UI when it handles WM_APP_FRAME, before taking the dirty area.
    pub fn frame_seen(&self) {
        self.frame_posted.store(false, Ordering::Release);
    }

    /// Writes a decoded RGBA frame into the framebuffer and schedules a repaint (the native
    /// path). RGBA bytes are packed into the framebuffer's `0x00RRGGBB` (BGRA in memory) layout.
    pub fn present_decoded(&self, width: u32, height: u32, rgba: &[u8]) {
        let resized = {
            let mut fb = self.framebuffer.lock().unwrap();
            let resized = fb.width != width || fb.height != height;
            if resized {
                fb.resize(width, height);
            }
            for (px, c) in fb.pixels.iter_mut().zip(rgba.as_chunks::<4>().0) {
                *px = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
            }
            resized
        };
        *self.dirty.lock().unwrap() = Some(Rect { x: 0, y: 0, w: width as u16, h: height as u16 });
        self.updates.fetch_add(1, Ordering::Relaxed);
        self.update_pixels.fetch_add(u64::from(width) * u64::from(height), Ordering::Relaxed);
        // A resolution change (the host switched display modes) resizes the framebuffer; make the
        // window re-fit the new size before the frame is drawn.
        if resized {
            self.post(WM_APP_RESIZED);
        }
        if !self.frame_posted.swap(true, Ordering::AcqRel) {
            self.post(WM_APP_FRAME);
        }
    }

    /// Reports a fatal connection error to the UI (also used by the native source).
    pub fn report_error(&self, error: String) {
        self.fail(error);
    }

    /// Closes the window on a clean end of stream (no error dialog).
    pub fn disconnect(&self) {
        self.post(WM_APP_DISCONNECTED);
    }

    fn fail(&self, error: String) {
        let mut slot = self.error.lock().unwrap();
        if slot.is_none() {
            *slot = Some(error);
        }
        drop(slot);
        self.post(WM_APP_DISCONNECTED);
    }
}

pub fn union(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (u32::from(a.x) + u32::from(a.w)).max(u32::from(b.x) + u32::from(b.w));
    let y1 = (u32::from(a.y) + u32::from(a.h)).max(u32::from(b.y) + u32::from(b.h));
    Rect { x: x0, y: y0, w: (x1 - u32::from(x0)) as u16, h: (y1 - u32::from(y0)) as u16 }
}

pub fn spawn_reader(shared: Arc<Shared>, mut reader: SessionReader) {
    thread::Builder::new()
        .name("rfb-reader".into())
        .spawn(move || {
            loop {
                match reader.next_event(&shared.framebuffer) {
                    Ok(ServerEvent::Update { dirty }) => {
                        if dirty.is_empty() {
                            continue;
                        }
                        let pixels: u64 = dirty.iter().map(|r| u64::from(r.w) * u64::from(r.h)).sum();
                        shared.updates.fetch_add(1, Ordering::Relaxed);
                        shared.update_pixels.fetch_add(pixels, Ordering::Relaxed);
                        let mut slot = shared.dirty.lock().unwrap();
                        let merged = dirty.into_iter().fold(*slot, |acc, r| Some(acc.map_or(r, |a| union(a, r))));
                        *slot = merged;
                        drop(slot);
                        if !shared.frame_posted.swap(true, Ordering::AcqRel) {
                            shared.post(WM_APP_FRAME);
                        }
                    }
                    Ok(ServerEvent::Resized { .. }) => shared.post(WM_APP_RESIZED),
                    Ok(ServerEvent::Cursor(shape)) => {
                        *shared.cursor.lock().unwrap() = Some(shape);
                        shared.post(WM_APP_CURSOR);
                    }
                    Ok(ServerEvent::CutText(text)) => {
                        *shared.cut_text.lock().unwrap() = Some(text);
                        shared.post(WM_APP_CUT_TEXT);
                    }
                    Ok(ServerEvent::Bell) => {}
                    Err(e) => {
                        shared.fail(format!("Connection lost: {e}"));
                        return;
                    }
                }
            }
        })
        .expect("spawn reader thread");
}

pub fn spawn_writer(shared: Arc<Shared>, mut writer: SessionWriter, rx: Receiver<Out>) {
    thread::Builder::new()
        .name("rfb-writer".into())
        .spawn(move || {
            let mut batch = Vec::new();
            while let Ok(first) = rx.recv() {
                batch.clear();
                batch.push(first);
                batch.extend(rx.try_iter());
                for (i, message) in batch.iter().enumerate() {
                    let result = match message {
                        // A move followed by another move with the same buttons is superseded.
                        Out::Pointer(buttons, _, _)
                            if matches!(batch.get(i + 1), Some(Out::Pointer(next, _, _)) if next == buttons) =>
                        {
                            continue;
                        }
                        Out::Pointer(buttons, x, y) => writer.pointer_event(*buttons, *x, *y),
                        Out::Key(keysym, down) => writer.key_event(*keysym, *down),
                        Out::CutText(text) => writer.cut_text(text),
                    };
                    if let Err(e) = result {
                        shared.fail(format!("Connection lost: {e}"));
                        return;
                    }
                }
            }
        })
        .expect("spawn writer thread");
}
