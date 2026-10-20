//! Connects to the Swift host, receives its live HEVC video stream over QUIC, decodes it with
//! Media Foundation, and reports throughput (checkpoint 2c: full encode->QUIC->decode loop).
//! Saves a mid-stream frame as PNG to verify pixels. Usage: stream <host>[:port] [fingerprint]
//! Env: AS_PASSWORD (default "test").

use std::net::ToSocketAddrs;

use aktivestream_proto::{DEFAULT_PORT, Message, connect};
use aktivestream_video::{Codec, Decoder, init};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let target = args.next().ok_or("usage: stream <host>[:port] [fingerprint]")?;
    let expected = args.next();
    let password = std::env::var("AS_PASSWORD").unwrap_or_else(|_| "test".into());

    let addr = if target.contains(':') { target.clone() } else { format!("{target}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs()?.next().ok_or("could not resolve host")?;

    let connection = connect(socket, expected).await?;
    println!("connected, fingerprint {}", connection.fingerprint);
    let mut control = connection.control("aktivestream-stream-example", &password).await?;
    println!("handshake ok: host {:?}", control.info.name);

    init()?;
    let mut decoder: Option<Decoder> = None;
    let mut decoded = 0usize;
    let mut bytes = 0usize;
    let mut saved = false;
    let started = std::time::Instant::now();

    while let Some(message) = control.recv().await? {
        match message {
            Message::VideoStart(start) => {
                let codec = if start.codec == 1 { Codec::H264 } else { Codec::Hevc };
                decoder = Some(Decoder::new(codec, start.width, start.height)?);
                println!("video start: {}x{} codec={}", start.width, start.height, start.codec);
            }
            Message::VideoFrame(frame) => {
                bytes += frame.data.len();
                let Some(decoder) = decoder.as_mut() else { continue };
                for decoded_frame in decoder.decode(&frame.data)? {
                    decoded += 1;
                    if !saved && decoded >= 75 {
                        save_png(&decoded_frame.rgba, decoded_frame.width, decoded_frame.height)?;
                        saved = true;
                    }
                }
            }
            other => println!("ignoring {other:?}"),
        }
    }
    if let Some(mut decoder) = decoder {
        decoded += decoder.finish()?.len();
    }
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "decoded {decoded} frames ({:.1} KB received) in {:.2}s = {:.0} fps live over QUIC",
        bytes as f64 / 1024.0,
        seconds,
        decoded as f64 / seconds
    );
    connection.close().await;
    Ok(())
}

fn save_png(rgba: &[u8], width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/stream-frame.png");
    std::fs::create_dir_all(out.parent().unwrap())?;
    let file = std::fs::File::create(&out)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    println!("saved {}", out.display());
    Ok(())
}
