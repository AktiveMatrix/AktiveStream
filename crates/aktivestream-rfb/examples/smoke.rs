//! Live smoke test against a Mac running Screen Sharing.
//!
//! Reads MAC_HOST, MAC_USER and MAC_PASSWORD from the environment (the password is
//! never printed), connects with Apple DH, saves the first full frame as PNG under
//! `artifacts/`, measures 10 s of incremental updates, then 5 s of forced full refreshes.
//!
//!     cargo run -p aktivestream-rfb --release --example smoke

use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use aktivestream_rfb::{ConnectOptions, Framebuffer, ReaderStats, Rect, ServerEvent, connect};

const INCREMENTAL: u8 = 0;
const FULL_REFRESH: u8 = 1;

fn encoding_name(e: i32) -> String {
    match e {
        0 => "Raw".into(),
        1 => "CopyRect".into(),
        6 => "Zlib".into(),
        16 => "ZRLE".into(),
        -239 => "Cursor".into(),
        -223 => "DesktopSize".into(),
        -224 => "LastRect".into(),
        other => format!("enc {other}"),
    }
}

fn report(label: &str, before: &ReaderStats, after: &ReaderStats, secs: f64, frames: u64) {
    let bytes = after.bytes_read - before.bytes_read;
    let updates = after.updates - before.updates;
    let decode = after.decode_time - before.decode_time;
    println!("{label}: {secs:.1} s");
    println!("  updates: {updates} ({:.1}/s)", updates as f64 / secs);
    if frames > 0 {
        println!("  full frames: {frames} ({:.2}/s)", frames as f64 / secs);
    }
    println!("  received: {:.2} MB ({:.2} MB/s)", bytes as f64 / 1e6, bytes as f64 / 1e6 / secs);
    println!(
        "  decode time: {:.1} ms ({:.1}% of wall time)",
        decode.as_secs_f64() * 1e3,
        decode.as_secs_f64() / secs * 100.0
    );
    for (enc, n) in &after.rects {
        let n = n - before.rects.get(enc).copied().unwrap_or(0);
        if n > 0 {
            println!("  rects {}: {n}", encoding_name(*enc));
        }
    }
}

fn save_png(fb: &Framebuffer, path: &PathBuf, step: usize) -> Result<(), Box<dyn Error>> {
    let (w, h) = (fb.width as usize / step, fb.height as usize / step);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let p = fb.pixels[y * step * fb.width as usize + x * step];
            rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let host = std::env::var("MAC_HOST")?;
    let user = std::env::var("MAC_USER")?;
    let password = std::env::var("MAC_PASSWORD")?;
    let options = ConnectOptions::new(host, 5900, Some(user), password);

    let t0 = Instant::now();
    let (info, mut reader, mut writer) = connect(&options)?;
    println!(
        "protocol {}, security {:?}, {}x{}, name {:?}",
        info.protocol, info.security, info.width, info.height, info.name
    );
    println!("server pixel format {:?}", info.server_pixel_format);
    println!("connected in {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);

    let fb = Arc::new(Mutex::new(Framebuffer::new(info.width as u32, info.height as u32)));
    let total = info.width as u64 * info.height as u64;
    let t1 = Instant::now();
    let mut covered = 0u64;
    while covered < total {
        match reader.next_event(&fb)? {
            ServerEvent::Update { dirty } => covered += dirty.iter().map(|r| r.w as u64 * r.h as u64).sum::<u64>(),
            ServerEvent::Resized { width, height } => println!("resized to {width}x{height}"),
            ServerEvent::Cursor(c) => {
                println!("cursor {}x{} hotspot ({}, {})", c.width, c.height, c.hotspot_x, c.hotspot_y)
            }
            other => println!("event {other:?}"),
        }
    }
    let first = reader.stats();
    println!(
        "first full frame: {:.0} ms, {:.2} MB, decode {:.1} ms",
        t1.elapsed().as_secs_f64() * 1e3,
        first.bytes_read as f64 / 1e6,
        first.decode_time.as_secs_f64() * 1e3
    );
    report("first frame", &ReaderStats::default(), &first, t1.elapsed().as_secs_f64(), 0);

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts");
    std::fs::create_dir_all(&dir)?;
    {
        let fb = fb.lock().unwrap();
        save_png(&fb, &dir.join("smoke-first-frame.png"), 1)?;
        save_png(&fb, &dir.join("smoke-first-frame-quarter.png"), 4)?;
    }

    // Reader thread: counts, and in FULL_REFRESH mode asks for a new full frame as soon as
    // the previous one has fully arrived.
    let mode = Arc::new(AtomicU8::new(INCREMENTAL));
    let frames = Arc::new(AtomicU64::new(0));
    let stats = Arc::new(Mutex::new(reader.stats()));
    let full = Rect { x: 0, y: 0, w: info.width, h: info.height };
    {
        let (fb, mode, frames, stats) = (Arc::clone(&fb), Arc::clone(&mode), Arc::clone(&frames), Arc::clone(&stats));
        thread::spawn(move || {
            let mut covered = 0u64;
            let mut requested = false;
            loop {
                if mode.load(Ordering::Relaxed) == FULL_REFRESH && !requested {
                    writer.request_update(false, full).unwrap();
                    requested = true;
                    covered = 0;
                }
                let event = match reader.next_event(&fb) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("reader stopped: {e}");
                        return;
                    }
                };
                if let ServerEvent::Update { dirty } = &event {
                    covered += dirty.iter().map(|r| r.w as u64 * r.h as u64).sum::<u64>();
                    if requested && covered >= total {
                        frames.fetch_add(1, Ordering::Relaxed);
                        requested = false;
                    }
                }
                *stats.lock().unwrap() = reader.stats();
            }
        });
    }

    let before = stats.lock().unwrap().clone();
    let t = Instant::now();
    thread::sleep(Duration::from_secs(10));
    let after = stats.lock().unwrap().clone();
    report("incremental (screen as-is)", &before, &after, t.elapsed().as_secs_f64(), 0);

    mode.store(FULL_REFRESH, Ordering::Relaxed);
    let t = Instant::now();
    thread::sleep(Duration::from_secs(5));
    let done = stats.lock().unwrap().clone();
    report("forced full refresh", &after, &done, t.elapsed().as_secs_f64(), frames.load(Ordering::Relaxed));
    // Leave the reader blocked; the process exit closes the socket.
    std::process::exit(0);
}
