//! Native path (Linux): the QUIC/TLS transport to the Swift host and a software/VA-API video
//! decoder via ffmpeg. This module is the performance path (hardware video instead of RFB's
//! lossless CPU encodings). Milestone 3a: a headless decode-to-PNG to prove QUIC + HEVC decode on
//! Linux; the windowed path and native input build on it. Linux-only (ffmpeg does not build on
//! Windows), so the whole module is `cfg(target_os = "linux")`.

use std::net::ToSocketAddrs;
use std::sync::Arc;

use aktivestream_keymap::native::NativeInput;
use aktivestream_proto::{DEFAULT_PORT, Input, Message, connect, read_message, write_input};
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling::{Context as Scaler, Flags};
use ffmpeg::util::frame::video::Video;
use ffmpeg_next as ffmpeg;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use winit::event_loop::{EventLoop, EventLoopProxy};

use crate::{App, Backend, Shared};

/// Connects over QUIC, decodes the first video frame with ffmpeg, writes it to `path`, and exits.
pub fn run_native_snapshot(host: String, password: String, path: String) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(native_snapshot(host, password, path))
}

async fn native_snapshot(host: String, password: String, path: String) -> Result<(), String> {
    let addr = if host.contains(':') { host } else { format!("{host}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("could not resolve host")?;

    let connection = connect(socket, None).await.map_err(|e| e.to_string())?;
    let control = connection.control("aktivestream-linux", &password).await.map_err(|e| e.to_string())?;
    let (info, _send, mut recv) = control.into_parts();
    eprintln!("connected to {} over QUIC", info.name);

    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut decoder: Option<ffmpeg::decoder::Video> = None;

    loop {
        match read_message(&mut recv).await.map_err(|e| e.to_string())? {
            Some(Message::VideoStart(start)) => {
                let id = if start.codec == 1 { ffmpeg::codec::Id::H264 } else { ffmpeg::codec::Id::HEVC };
                let codec = ffmpeg::decoder::find(id).ok_or("no ffmpeg decoder for the stream codec")?;
                let ctx = ffmpeg::codec::context::Context::new_with_codec(codec);
                decoder = Some(ctx.decoder().video().map_err(|e| e.to_string())?);
                eprintln!("video start: {}x{} codec {}", start.width, start.height, start.codec);
            }
            Some(Message::VideoFrame(frame)) => {
                let Some(dec) = decoder.as_mut() else { continue };
                let packet = ffmpeg::codec::packet::Packet::copy(&frame.data);
                dec.send_packet(&packet).map_err(|e| e.to_string())?;
                let mut yuv = Video::empty();
                if dec.receive_frame(&mut yuv).is_ok() {
                    write_frame_png(&yuv, &path)?;
                    eprintln!("wrote native snapshot {path} ({}x{})", yuv.width(), yuv.height());
                    return Ok(());
                }
            }
            Some(_) => {}
            None => return Err("stream ended before a video frame".into()),
        }
    }
}

/// Converts a decoded (YUV) frame to RGB24 with swscale and writes it to a PNG.
fn write_frame_png(frame: &Video, path: &str) -> Result<(), String> {
    let (w, h) = (frame.width(), frame.height());
    let mut scaler =
        Scaler::get(frame.format(), w, h, Pixel::RGB24, w, h, Flags::BILINEAR).map_err(|e| e.to_string())?;
    let mut rgb = Video::empty();
    scaler.run(frame, &mut rgb).map_err(|e| e.to_string())?;

    let data = rgb.data(0);
    let stride = rgb.stride(0);
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let row_bytes = (w * 3) as usize;
    let mut out = Vec::with_capacity(row_bytes * h as usize);
    for y in 0..h as usize {
        let start = y * stride;
        out.extend_from_slice(&data[start..start + row_bytes]);
    }
    writer.write_image_data(&out).map_err(|e| e.to_string())
}

/// Windowed native client: a winit window (shared with the RFB path) fed by a background QUIC +
/// ffmpeg decode session, with input sent as native `Input` messages (injected by the host with
/// CGEvent, so it works even on a headless Mac).
pub fn run_native(host: String, password: String, title: String) -> Result<(), String> {
    let shared = Arc::new(Shared::new(0, 0));
    let event_loop = EventLoop::<()>::with_user_event().build().map_err(|e| e.to_string())?;
    let proxy = event_loop.create_proxy();
    let (input_tx, input_rx) = unbounded_channel::<Input>();

    let session_shared = shared.clone();
    std::thread::Builder::new()
        .name("native-session".into())
        .spawn(move || match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(runtime) => runtime.block_on(native_session(host, password, session_shared, proxy, input_rx)),
            Err(e) => eprintln!("native runtime: {e}"),
        })
        .map_err(|e| e.to_string())?;

    let backend = Backend::Native { tx: input_tx, translator: NativeInput::new() };
    let mut app = App::new(shared, backend, title);
    event_loop.run_app(&mut app).map_err(|e| e.to_string())
}

async fn native_session(
    host: String,
    password: String,
    shared: Arc<Shared>,
    proxy: EventLoopProxy<()>,
    mut input_rx: UnboundedReceiver<Input>,
) {
    if let Err(e) = native_session_inner(host, password, &shared, &proxy, &mut input_rx).await {
        *shared.error.lock().unwrap() = Some(e);
        let _ = proxy.send_event(());
    }
}

async fn native_session_inner(
    host: String,
    password: String,
    shared: &Arc<Shared>,
    proxy: &EventLoopProxy<()>,
    input_rx: &mut UnboundedReceiver<Input>,
) -> Result<(), String> {
    let addr = if host.contains(':') { host } else { format!("{host}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("could not resolve host")?;
    let connection = connect(socket, None).await.map_err(|e| e.to_string())?;
    let control = connection.control("aktivestream-linux", &password).await.map_err(|e| e.to_string())?;
    let (info, mut send, mut recv) = control.into_parts();
    eprintln!("connected to {} over QUIC", info.name);

    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut decoder: Option<ffmpeg::decoder::Video> = None;
    let mut scaler: Option<Scaler> = None;

    loop {
        tokio::select! {
            message = read_message(&mut recv) => {
                match message.map_err(|e| e.to_string())? {
                    Some(Message::VideoStart(start)) => {
                        let id = if start.codec == 1 { ffmpeg::codec::Id::H264 } else { ffmpeg::codec::Id::HEVC };
                        let codec = ffmpeg::decoder::find(id).ok_or("no ffmpeg decoder for the stream codec")?;
                        let ctx = ffmpeg::codec::context::Context::new_with_codec(codec);
                        decoder = Some(ctx.decoder().video().map_err(|e| e.to_string())?);
                        scaler = None;
                        eprintln!("native video start: {}x{} codec {}", start.width, start.height, start.codec);
                    }
                    Some(Message::VideoFrame(frame)) => {
                        if let Some(dec) = decoder.as_mut() {
                            let packet = ffmpeg::codec::packet::Packet::copy(&frame.data);
                            if dec.send_packet(&packet).is_ok() {
                                let mut yuv = Video::empty();
                                while dec.receive_frame(&mut yuv).is_ok() {
                                    present_frame(&yuv, &mut scaler, shared);
                                    let _ = proxy.send_event(());
                                }
                            }
                        }
                    }
                    Some(_) => {}
                    None => return Ok(()),
                }
            }
            Some(input) = input_rx.recv() => {
                write_input(&mut send, &input).await.map_err(|e| e.to_string())?;
            }
        }
    }
}

/// Headless native input check: send pointer moves to known normalized positions over QUIC so a
/// watcher on the Mac (/tmp/mousepos) can confirm the native input path — which, unlike RFB, the
/// host applies with CGEvent even on a headless Mac.
pub fn run_native_input_test(host: String, password: String) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(native_input_test_inner(host, password))
}

async fn native_input_test_inner(host: String, password: String) -> Result<(), String> {
    let addr = if host.contains(':') { host } else { format!("{host}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("could not resolve host")?;
    let connection = connect(socket, None).await.map_err(|e| e.to_string())?;
    let control = connection.control("aktivestream-linux", &password).await.map_err(|e| e.to_string())?;
    let (info, mut send, mut recv) = control.into_parts();
    eprintln!("connected to {} over QUIC", info.name);
    tokio::spawn(async move { while matches!(read_message(&mut recv).await, Ok(Some(_))) {} });

    for (x, y) in [(0.5f32, 0.5f32), (0.25, 0.25), (0.8, 0.3), (0.5, 0.85)] {
        write_input(&mut send, &Input::Pointer { buttons: 0, x, y }).await.map_err(|e| e.to_string())?;
        eprintln!("pointer -> ({x}, {y})");
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
    }
    connection.close().await;
    Ok(())
}

/// Benchmarks the native pipeline: decodes every frame the host sends (the 150 synthetic frames
/// when no display is available) and reports decode fps and the swscale cost, to see where the
/// time goes on this hardware before optimizing (GPU present vs VA-API decode).
pub fn run_native_bench(host: String, password: String) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(native_bench_inner(host, password))
}

async fn native_bench_inner(host: String, password: String) -> Result<(), String> {
    let addr = if host.contains(':') { host } else { format!("{host}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("could not resolve host")?;
    let connection = connect(socket, None).await.map_err(|e| e.to_string())?;
    let control = connection.control("aktivestream-linux", &password).await.map_err(|e| e.to_string())?;
    let (info, _send, mut recv) = control.into_parts();
    eprintln!("connected to {} over QUIC", info.name);
    ffmpeg::init().map_err(|e| e.to_string())?;

    let mut decoder: Option<ffmpeg::decoder::Video> = None;
    let mut scaler: Option<Scaler> = None;
    let (mut received, mut decoded) = (0u32, 0u32);
    let mut first: Option<std::time::Instant> = None;
    let mut scale_nanos = 0u128;

    while let Some(message) = read_message(&mut recv).await.map_err(|e| e.to_string())? {
        match message {
            Message::VideoStart(start) => {
                let id = if start.codec == 1 { ffmpeg::codec::Id::H264 } else { ffmpeg::codec::Id::HEVC };
                let codec = ffmpeg::decoder::find(id).ok_or("no ffmpeg decoder for the stream codec")?;
                decoder = Some(
                    ffmpeg::codec::context::Context::new_with_codec(codec)
                        .decoder()
                        .video()
                        .map_err(|e| e.to_string())?,
                );
                scaler = None;
                eprintln!("native video start: {}x{} codec {}", start.width, start.height, start.codec);
            }
            Message::VideoFrame(frame) => {
                received += 1;
                first.get_or_insert_with(std::time::Instant::now);
                if let Some(dec) = decoder.as_mut() {
                    let packet = ffmpeg::codec::packet::Packet::copy(&frame.data);
                    if dec.send_packet(&packet).is_ok() {
                        let mut yuv = Video::empty();
                        while dec.receive_frame(&mut yuv).is_ok() {
                            decoded += 1;
                            let t = std::time::Instant::now();
                            if scaler.is_none() {
                                scaler = Scaler::get(
                                    yuv.format(),
                                    yuv.width(),
                                    yuv.height(),
                                    Pixel::BGRA,
                                    yuv.width(),
                                    yuv.height(),
                                    Flags::BILINEAR,
                                )
                                .ok();
                            }
                            if let Some(sc) = scaler.as_mut() {
                                let mut bgra = Video::empty();
                                let _ = sc.run(&yuv, &mut bgra);
                            }
                            scale_nanos += t.elapsed().as_nanos();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let secs = first.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0).max(1e-6);
    let scale_ms = scale_nanos as f64 / 1e6;
    println!(
        "received {received}, decoded {decoded} frames in {secs:.2}s = {:.1} fps decode; swscale {scale_ms:.0}ms total, {:.2}ms/frame",
        decoded as f64 / secs,
        scale_ms / decoded.max(1) as f64
    );
    Ok(())
}

/// Converts a decoded (YUV) frame to the shared framebuffer's `0x00RRGGBB` pixels via swscale.
fn present_frame(yuv: &Video, scaler: &mut Option<Scaler>, shared: &Shared) {
    let (w, h) = (yuv.width(), yuv.height());
    if scaler.is_none() {
        match Scaler::get(yuv.format(), w, h, Pixel::BGRA, w, h, Flags::BILINEAR) {
            Ok(s) => *scaler = Some(s),
            Err(_) => return,
        }
    }
    let Some(sc) = scaler.as_mut() else { return };
    let mut bgra = Video::empty();
    if sc.run(yuv, &mut bgra).is_err() {
        return;
    }
    let data = bgra.data(0);
    let stride = bgra.stride(0);

    let mut fb = shared.framebuffer.lock().unwrap();
    if fb.width != w || fb.height != h {
        fb.resize(w, h);
    }
    let wu = w as usize;
    for row in 0..h as usize {
        let src = row * stride;
        let dst = &mut fb.pixels[row * wu..row * wu + wu];
        for (px, chunk) in dst.iter_mut().zip(data[src..src + wu * 4].as_chunks::<4>().0) {
            *px = u32::from_le_bytes(*chunk);
        }
    }
}
