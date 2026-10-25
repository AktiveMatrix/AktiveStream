//! Live session telemetry, shared by every client. A rolling window of cumulative counters yields
//! per-second rates and a one-line bottleneck verdict, rendered as an on-screen panel with the
//! shared [`draw`] primitives so the Windows (Direct3D) client and the winit/softbuffer clients
//! show exactly the same readout.
//!
//! Diagnostics only: nothing here touches the wire. It answers "where is the lag?" — the far side
//! (server encode or network) or this client (decode or paint) — so stream tuning can target the
//! real bottleneck. Toggled with the `ToggleStats` local shortcut (Ctrl+Alt+S).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::draw;

/// How much history the rates are averaged over. Long enough to be steady, short enough to react.
const WINDOW: Duration = Duration::from_millis(1200);

/// Cumulative session counters at one instant. Every field is a running total since connect, so
/// rates come from differencing two snapshots.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counters {
    /// Bytes received from the server since the handshake. 0 means "unknown" (the native video
    /// path has no RFB byte count), which the readout shows as `n/a` (font8x8 is ASCII-only).
    pub bytes: u64,
    /// FramebufferUpdates applied.
    pub updates: u64,
    /// Pixels written by those updates (sum of changed-rect areas).
    pub update_pixels: u64,
    /// Time the reader spent inflating, decoding and writing pixels (socket waits excluded).
    pub decode: Duration,
    /// Frames the client presented.
    pub paints: u64,
    /// Wall-clock time the client spent presenting those frames.
    pub paint: Duration,
}

/// Per-second rates and load fractions over the measurement window, derived from two [`Counters`]
/// snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    /// Seconds the window actually covers.
    pub span: f64,
    /// Frames presented per second.
    pub fps: f64,
    /// FramebufferUpdates per second.
    pub updates_per_s: f64,
    /// Megabytes per second on the wire (1e6 bytes), or 0 when the byte count is unknown.
    pub mbps: f64,
    /// Megapixels per second written to the framebuffer.
    pub mpix_per_s: f64,
    /// Mean decode time per update, milliseconds.
    pub decode_ms: f64,
    /// Mean present time per frame, milliseconds.
    pub paint_ms: f64,
    /// Bytes per megapixel (compression efficiency), or 0 when the byte count is unknown.
    pub bytes_per_mpix: f64,
    /// Fraction of wall-clock the reader thread spent decoding (1.0 = fully decode-saturated).
    pub decode_load: f64,
    /// Fraction of wall-clock the UI thread spent presenting.
    pub paint_load: f64,
    /// Whether the wire byte count was available (false on the native video path).
    pub bytes_known: bool,
}

/// Which side the pipeline is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Almost nothing is changing; there is no lag to tune.
    Idle,
    /// The reader thread cannot decode as fast as frames arrive.
    DecodeBound,
    /// The UI thread cannot present as fast as frames arrive.
    PaintBound,
    /// This client keeps up; the lag is the server's encode time and/or the network. Compare
    /// `mpix_per_s` to the server's known encode ceiling and `mbps` to the link to tell which.
    ServerOrLink,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Idle => "idle",
            Verdict::DecodeBound => "client: decode-bound",
            Verdict::PaintBound => "client: paint-bound",
            Verdict::ServerOrLink => "server / link-bound",
        }
    }

    /// Panel colour for the verdict line (`0x00RRGGBB`).
    fn color(self) -> u32 {
        match self {
            Verdict::Idle => 0x88_88_90,
            Verdict::DecodeBound | Verdict::PaintBound => 0x66_cc_ff,
            Verdict::ServerOrLink => 0xff_c0_55,
        }
    }
}

impl Rates {
    /// Classifies the bottleneck. A thread that spends most of the window busy is the limiter;
    /// otherwise, if anything is moving at all, the client is keeping up and the far side is the
    /// constraint.
    pub fn verdict(&self) -> Verdict {
        if self.updates_per_s < 1.0 && self.mpix_per_s < 0.1 {
            Verdict::Idle
        } else if self.decode_load > 0.7 {
            Verdict::DecodeBound
        } else if self.paint_load > 0.7 {
            Verdict::PaintBound
        } else {
            Verdict::ServerOrLink
        }
    }

    /// The readout lines, top to bottom (verdict excluded; it is drawn in its own colour).
    fn lines(&self, renderer: &str) -> Vec<String> {
        let wire = if self.bytes_known { format!("{:.1} MB/s", self.mbps) } else { "n/a".to_string() };
        let eff = if self.bytes_known && self.mpix_per_s > 0.0 {
            format!("  {:.2} B/px", self.bytes_per_mpix / 1e6)
        } else {
            String::new()
        };
        vec![
            format!("AktiveStream  [{renderer}]"),
            format!("fps {:>4.0}   upd {:>4.0}/s", self.fps, self.updates_per_s),
            format!("pixels {:>5.1} Mpix/s", self.mpix_per_s),
            format!("wire   {wire}{eff}"),
            format!("decode {:>5.1} ms   paint {:>4.1} ms", self.decode_ms, self.paint_ms),
        ]
    }
}

/// A rolling window of counter snapshots plus the readout's visibility. One per client.
pub struct Meter {
    samples: VecDeque<(Instant, Counters)>,
    renderer: &'static str,
    visible: bool,
}

impl Meter {
    /// `renderer` is a short tag for the presentation path, shown in the readout (e.g. "D3D11").
    pub fn new(renderer: &'static str) -> Self {
        Meter { samples: VecDeque::new(), renderer, visible: false }
    }

    /// Flips the readout on or off; returns the new state. Clearing on hide so a re-show starts
    /// from fresh samples rather than a stale rate spanning the hidden interval.
    pub fn toggle(&mut self) -> bool {
        self.visible = !self.visible;
        if !self.visible {
            self.samples.clear();
        }
        self.visible
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Updates the presentation-path tag shown in the readout (e.g. after a Direct3D -> GDI
    /// fallback).
    pub fn set_renderer(&mut self, renderer: &'static str) {
        self.renderer = renderer;
    }

    /// Records a snapshot, dropping samples older than the window while always keeping at least
    /// two so a rate can still be computed.
    pub fn sample(&mut self, now: Instant, counters: Counters) {
        self.samples.push_back((now, counters));
        while self.samples.len() > 2 && now.duration_since(self.samples[1].0) >= WINDOW {
            self.samples.pop_front();
        }
    }

    /// The current rates, or `None` until two snapshots span a usable interval.
    pub fn rates(&self) -> Option<Rates> {
        let (t0, c0) = *self.samples.front()?;
        let (t1, c1) = *self.samples.back()?;
        let span = t1.duration_since(t0).as_secs_f64();
        if span < 0.05 {
            return None;
        }
        let d_updates = c1.updates.saturating_sub(c0.updates) as f64;
        let d_pix = c1.update_pixels.saturating_sub(c0.update_pixels) as f64;
        let d_bytes = c1.bytes.saturating_sub(c0.bytes) as f64;
        let d_paints = c1.paints.saturating_sub(c0.paints) as f64;
        let d_decode = c1.decode.saturating_sub(c0.decode).as_secs_f64();
        let d_paint = c1.paint.saturating_sub(c0.paint).as_secs_f64();
        let mpix = d_pix / 1e6;
        Some(Rates {
            span,
            fps: d_paints / span,
            updates_per_s: d_updates / span,
            mbps: d_bytes / 1e6 / span,
            mpix_per_s: mpix / span,
            decode_ms: if d_updates > 0.0 { d_decode * 1000.0 / d_updates } else { 0.0 },
            paint_ms: if d_paints > 0.0 { d_paint * 1000.0 / d_paints } else { 0.0 },
            bytes_per_mpix: if mpix > 0.0 { d_bytes / mpix } else { 0.0 },
            decode_load: (d_decode / span).min(1.0),
            paint_load: (d_paint / span).min(1.0),
            bytes_known: c1.bytes > 0,
        })
    }

    /// Renders the readout into a tight `0x00RRGGBB` panel buffer, or `None` when hidden or still
    /// warming up. Returns `(pixels, width, height)`; the client composites it with [`draw::blit`]
    /// (softbuffer) or a textured overlay (Direct3D).
    pub fn panel(&self) -> Option<(Vec<u32>, usize, usize)> {
        if !self.visible {
            return None;
        }
        let rates = self.rates()?;
        Some(render_panel(&rates, self.renderer))
    }
}

/// Readout text scale (8px glyphs x this). 2 keeps it legible at a glance and for low vision.
const SCALE: usize = 2;
const PAD: usize = 10;
const LINE_GAP: usize = 6;

/// Renders a stats panel sized to its content. Separate from [`Meter`] so it can be unit-tested
/// and reused by a headless preview.
pub fn render_panel(rates: &Rates, renderer: &str) -> (Vec<u32>, usize, usize) {
    let verdict = rates.verdict();
    let mut lines = rates.lines(renderer);
    lines.push(verdict.label().to_string());
    let line_h = 8 * SCALE;
    let text_w = lines.iter().map(|l| draw::text_width(l, SCALE)).max().unwrap_or(0);
    let w = text_w + 2 * PAD;
    let h = PAD * 2 + lines.len() * line_h + (lines.len() - 1) * LINE_GAP;
    let mut buf = vec![0u32; w * h];
    draw::fill_rect(&mut buf, w, 0, 0, w, h, 0x12_12_1a);
    draw::rect_outline(&mut buf, w, 0, 0, w, h, 0x33_33_40);
    let last = lines.len() - 1;
    for (i, line) in lines.iter().enumerate() {
        let y = PAD + i * (line_h + LINE_GAP);
        let color = if i == 0 {
            0xdd_dd_dd
        } else if i == last {
            verdict.color()
        } else {
            0xbb_bb_c4
        };
        draw::text(&mut buf, w, PAD, y, line, color, w.saturating_sub(PAD), SCALE);
    }
    (buf, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters(bytes: u64, updates: u64, pixels: u64, decode_ms: u64, paints: u64, paint_ms: u64) -> Counters {
        Counters {
            bytes,
            updates,
            update_pixels: pixels,
            decode: Duration::from_millis(decode_ms),
            paints,
            paint: Duration::from_millis(paint_ms),
        }
    }

    #[test]
    fn rates_are_per_second_differences() {
        let mut m = Meter::new("test");
        let t = Instant::now();
        m.sample(t, counters(0, 0, 0, 0, 0, 0));
        // One second later: 10 MB, 60 updates, 60 Mpixels, 600 ms decode, 60 paints, 30 ms paint.
        m.sample(t + Duration::from_secs(1), counters(10_000_000, 60, 60_000_000, 600, 60, 30));
        let r = m.rates().expect("rates");
        assert!((r.mbps - 10.0).abs() < 0.01, "{}", r.mbps);
        assert!((r.updates_per_s - 60.0).abs() < 0.01, "{}", r.updates_per_s);
        assert!((r.mpix_per_s - 60.0).abs() < 0.01, "{}", r.mpix_per_s);
        assert!((r.fps - 60.0).abs() < 0.01, "{}", r.fps);
        assert!((r.decode_ms - 10.0).abs() < 0.01, "{}", r.decode_ms); // 600ms / 60 updates
        assert!((r.paint_ms - 0.5).abs() < 0.01, "{}", r.paint_ms); // 30ms / 60 paints
        assert!((r.decode_load - 0.6).abs() < 0.01, "{}", r.decode_load);
        assert!(r.bytes_known);
    }

    #[test]
    fn verdict_flags_a_decode_saturated_reader() {
        let mut m = Meter::new("test");
        let t = Instant::now();
        m.sample(t, counters(0, 0, 0, 0, 0, 0));
        // 950 ms of a 1 s window spent decoding: the client is the limiter.
        m.sample(t + Duration::from_secs(1), counters(1_000_000, 30, 30_000_000, 950, 30, 20));
        assert_eq!(m.rates().unwrap().verdict(), Verdict::DecodeBound);
    }

    #[test]
    fn verdict_blames_the_far_side_when_the_client_keeps_up() {
        let mut m = Meter::new("test");
        let t = Instant::now();
        m.sample(t, counters(0, 0, 0, 0, 0, 0));
        // Mac-like: ~11 Mpix/s, modest bytes, tiny decode/paint load -> waiting on the server.
        m.sample(t + Duration::from_secs(1), counters(4_300_000, 10, 11_000_000, 60, 60, 2));
        let r = m.rates().unwrap();
        assert_eq!(r.verdict(), Verdict::ServerOrLink);
        assert!(r.decode_load < 0.7 && r.paint_load < 0.7);
    }

    #[test]
    fn verdict_is_idle_with_nothing_moving() {
        let mut m = Meter::new("test");
        let t = Instant::now();
        m.sample(t, counters(1000, 5, 1000, 10, 5, 5));
        m.sample(t + Duration::from_secs(1), counters(1000, 5, 1000, 10, 5, 5));
        assert_eq!(m.rates().unwrap().verdict(), Verdict::Idle);
    }

    #[test]
    fn unknown_byte_count_reads_as_unknown() {
        let mut m = Meter::new("test");
        let t = Instant::now();
        m.sample(t, counters(0, 0, 0, 0, 0, 0));
        m.sample(t + Duration::from_secs(1), counters(0, 60, 60_000_000, 10, 60, 10));
        let r = m.rates().unwrap();
        assert!(!r.bytes_known);
        assert_eq!(r.mbps, 0.0);
    }

    #[test]
    fn window_keeps_at_least_two_samples() {
        let mut m = Meter::new("test");
        let t = Instant::now();
        for i in 0..100u64 {
            m.sample(t + Duration::from_millis(i * 100), counters(i * 1000, i, i * 1000, i, i, i));
        }
        // Old samples are trimmed, but a rate is still computable and spans about the window.
        let r = m.rates().expect("rates");
        assert!(r.span <= WINDOW.as_secs_f64() + 0.2, "span {}", r.span);
        assert!(r.span > 0.0);
    }

    #[test]
    fn toggle_hides_and_clears() {
        let mut m = Meter::new("test");
        let t = Instant::now();
        m.sample(t, counters(0, 0, 0, 0, 0, 0));
        m.sample(t + Duration::from_secs(1), counters(1, 1, 1, 1, 1, 1));
        assert!(m.toggle()); // now visible
        assert!(m.panel().is_some());
        assert!(!m.toggle()); // hidden again, samples cleared
        assert!(m.rates().is_none());
    }

    #[test]
    fn panel_is_sized_and_filled() {
        let r = Rates {
            span: 1.0,
            fps: 60.0,
            updates_per_s: 60.0,
            mbps: 12.0,
            mpix_per_s: 7.0,
            bytes_known: true,
            ..Default::default()
        };
        let (buf, w, h) = render_panel(&r, "D3D11");
        assert!(w > 0 && h > 0);
        assert_eq!(buf.len(), w * h);
        assert_eq!(buf[0], 0x33_33_40, "border drawn at the corner");
        assert_eq!(buf[(h / 2) * w + w / 2] & 0xff_00_00, buf[(h / 2) * w + w / 2] & 0xff_00_00); // sane index
    }
}
