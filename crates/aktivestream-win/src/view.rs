//! Where the remote framebuffer sits in the window: scaled to fit with the aspect ratio kept,
//! never enlarged, centred.

use aktivestream_rfb::Rect;
use windows::Win32::Foundation::RECT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct View {
    pub fb_w: i32,
    pub fb_h: i32,
    /// Destination rectangle in client coordinates.
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl View {
    /// Fits the framebuffer to the client area, aspect ratio kept, centred. When `allow_upscale`
    /// is set, a framebuffer smaller than the window is enlarged to fill it (a 1080p native stream
    /// in a maximized window); otherwise it is shown at 1:1, centred, as it was before.
    pub fn fit_scaled(fb_w: u32, fb_h: u32, client_w: i32, client_h: i32, allow_upscale: bool) -> View {
        let (fb_w, fb_h) = (fb_w as i32, fb_h as i32);
        if fb_w <= 0 || fb_h <= 0 || client_w <= 0 || client_h <= 0 {
            return View { fb_w, fb_h, ..View::default() };
        }
        let (w, h) = if fb_w <= client_w && fb_h <= client_h && !allow_upscale {
            (fb_w, fb_h)
        } else if i64::from(client_w) * i64::from(fb_h) <= i64::from(client_h) * i64::from(fb_w) {
            (client_w, (i64::from(client_w) * i64::from(fb_h) / i64::from(fb_w)) as i32)
        } else {
            ((i64::from(client_h) * i64::from(fb_w) / i64::from(fb_h)) as i32, client_h)
        };
        View { fb_w, fb_h, x: (client_w - w) / 2, y: (client_h - h) / 2, w, h }
    }

    /// Fit without ever enlarging past 1:1. Used for the initial window size.
    pub fn fit(fb_w: u32, fb_h: u32, client_w: i32, client_h: i32) -> View {
        Self::fit_scaled(fb_w, fb_h, client_w, client_h, false)
    }

    pub fn is_one_to_one(&self) -> bool {
        self.w == self.fb_w && self.h == self.fb_h
    }

    /// Client point to framebuffer pixel, clamped to the framebuffer.
    pub fn fb_point(&self, cx: i32, cy: i32) -> (u16, u16) {
        if self.w <= 0 || self.h <= 0 {
            return (0, 0);
        }
        let fx = (i64::from(cx - self.x) * i64::from(self.fb_w) / i64::from(self.w)).clamp(0, i64::from(self.fb_w - 1));
        let fy = (i64::from(cy - self.y) * i64::from(self.fb_h) / i64::from(self.h)).clamp(0, i64::from(self.fb_h - 1));
        (fx as u16, fy as u16)
    }

    fn fb_x_to_client(&self, fx: i64, round_up: bool) -> i32 {
        let num = fx * i64::from(self.w);
        let d = i64::from(self.fb_w);
        self.x + if round_up { (num + d - 1) / d } else { num / d } as i32
    }

    fn fb_y_to_client(&self, fy: i64, round_up: bool) -> i32 {
        let num = fy * i64::from(self.h);
        let d = i64::from(self.fb_h);
        self.y + if round_up { (num + d - 1) / d } else { num / d } as i32
    }

    /// Client area covering a framebuffer rectangle, with a pixel of slack for the filter.
    pub fn fb_rect_to_client(&self, r: Rect) -> RECT {
        if self.fb_w <= 0 || self.fb_h <= 0 {
            return RECT::default();
        }
        RECT {
            left: self.fb_x_to_client(i64::from(r.x), false) - 1,
            top: self.fb_y_to_client(i64::from(r.y), false) - 1,
            right: self.fb_x_to_client(i64::from(r.x) + i64::from(r.w), true) + 1,
            bottom: self.fb_y_to_client(i64::from(r.y) + i64::from(r.h), true) + 1,
        }
    }

    /// For a client area to repaint, the framebuffer source rectangle and the exact
    /// destination it maps to (whole source pixels, so neighbouring repaints line up).
    pub fn paint_mapping(&self, paint: RECT) -> Option<(Rect, RECT)> {
        let left = paint.left.max(self.x);
        let top = paint.top.max(self.y);
        let right = paint.right.min(self.x + self.w);
        let bottom = paint.bottom.min(self.y + self.h);
        if left >= right || top >= bottom {
            return None;
        }
        let fw = i64::from(self.fb_w);
        let fh = i64::from(self.fb_h);
        let (vw, vh) = (i64::from(self.w), i64::from(self.h));
        let sx0 = (i64::from(left - self.x) * fw / vw).clamp(0, fw);
        let sy0 = (i64::from(top - self.y) * fh / vh).clamp(0, fh);
        let sx1 = ((i64::from(right - self.x) * fw + vw - 1) / vw).clamp(0, fw);
        let sy1 = ((i64::from(bottom - self.y) * fh + vh - 1) / vh).clamp(0, fh);
        if sx0 >= sx1 || sy0 >= sy1 {
            return None;
        }
        let source = Rect { x: sx0 as u16, y: sy0 as u16, w: (sx1 - sx0) as u16, h: (sy1 - sy0) as u16 };
        let dest = RECT {
            left: self.fb_x_to_client(sx0, false),
            top: self.fb_y_to_client(sy0, false),
            right: self.fb_x_to_client(sx1, false),
            bottom: self.fb_y_to_client(sy1, false),
        };
        Some((source, dest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_retina_mac_into_4k_work_area() {
        let v = View::fit(3456, 2234, 3840, 2100);
        assert_eq!(v.h, 2100);
        assert_eq!(v.w, 3248);
        assert_eq!(v.x, (3840 - 3248) / 2);
        assert!(!v.is_one_to_one());
    }

    #[test]
    fn small_framebuffer_is_not_enlarged() {
        let v = View::fit(800, 600, 1920, 1080);
        assert!(v.is_one_to_one());
        assert_eq!((v.x, v.y), (560, 240));
    }

    #[test]
    fn upscale_fills_a_larger_window_keeping_aspect() {
        // 1080p native stream maximized on a 1440p monitor: enlarge to fill, aspect kept, centred.
        let v = View::fit_scaled(1920, 1080, 2560, 1440, true);
        assert_eq!((v.w, v.h), (2560, 1440));
        assert_eq!((v.x, v.y), (0, 0));
        assert!(!v.is_one_to_one());
        // A 16:9 stream in a 16:10 window is letterboxed, never stretched.
        let v = View::fit_scaled(1920, 1080, 1920, 1200, true);
        assert_eq!(v.w, 1920);
        assert_eq!(v.h, 1080);
        assert_eq!(v.y, 60);
    }

    #[test]
    fn point_mapping_round_trips_corners() {
        let v = View::fit(3456, 2234, 1728, 1117);
        assert_eq!(v.fb_point(v.x, v.y), (0, 0));
        assert_eq!(v.fb_point(v.x + v.w + 50, v.y + v.h + 50), (3455, 2233));
    }

    #[test]
    fn paint_mapping_covers_requested_area() {
        let v = View::fit(3456, 2234, 1728, 1117);
        let paint = RECT { left: 100, top: 50, right: 300, bottom: 200 };
        let (src, dst) = v.paint_mapping(paint).unwrap();
        assert!(dst.left <= paint.left && dst.top <= paint.top);
        assert!(dst.right >= paint.right && dst.bottom >= paint.bottom);
        assert!(src.w > 0 && src.h > 0);
    }
}
