//! Decodes a length-prefixed Annex B HEVC file (u32 LE length + access unit, repeated) with
//! Media Foundation and saves each decoded frame as a PNG.
//! Usage: decode_file <input.h265f> <output_dir>

use aktivestream_video::{Codec, DecodedFrame, Decoder, init};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or("usage: decode_file <input.h265f> <output_dir> [width height]")?;
    let out_dir = args.next().unwrap_or_else(|| ".".into());
    let width: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(320);
    let height: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(240);
    std::fs::create_dir_all(&out_dir)?;

    let data = std::fs::read(&input)?;
    init()?;
    let mut decoder = Decoder::new(Codec::Hevc, width, height)?;

    let mut frames: Vec<DecodedFrame> = Vec::new();
    let mut pos = 0;
    let started = std::time::Instant::now();
    while pos + 4 <= data.len() {
        let len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + len > data.len() {
            break;
        }
        frames.extend(decoder.decode(&data[pos..pos + len])?);
        pos += len;
    }
    frames.extend(decoder.finish()?);
    let elapsed = started.elapsed();

    println!(
        "decoded {} frames in {:.1} ms ({:.0} fps)",
        frames.len(),
        elapsed.as_secs_f64() * 1000.0,
        frames.len() as f64 / elapsed.as_secs_f64()
    );
    for (i, frame) in frames.iter().enumerate() {
        let path = format!("{out_dir}/decoded_{i:03}.png");
        let file = std::fs::File::create(&path)?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&frame.rgba)?;
    }
    if let Some(f) = frames.first() {
        println!("first frame {}x{}, saved {}/decoded_000.png", f.width, f.height, out_dir);
    }
    Ok(())
}
