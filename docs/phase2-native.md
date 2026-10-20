# Phase 2: native host agent (design notes)

Goal: fluid 60 fps performance (smooth 60 fps at full Retina resolution, tens of
milliseconds of latency) between this Mac and the AktiveStream clients. Apple's Screen
Sharing server cannot get there with third-party clients: it only sends lossless ZRLE or
Zlib encoded on the CPU (`mac-screen-sharing.md`). Fluid's speed comes from a host agent
that captures the screen and encodes it with the hardware video encoder; AktiveStream needs
its own.

Status: not started. Nothing here is implemented or measured yet.

## Host (`aktivestream-host`, macOS, Rust with objc2 framework bindings)

- **Capture:** ScreenCaptureKit (`SCStream`) delivering IOSurface-backed `CVPixelBuffer`s
  at display refresh, only on change. Needs Screen Recording permission.
- **Encode:** VideoToolbox `VTCompressionSession`, HEVC Main (fall back to H.264),
  real-time mode, no B-frames, low-latency rate control, keyframes on demand
  (client request or packet loss). The M5 Max media engine handles 3456x2234 at 60 fps.
- **Input:** `CGEventPost` with native virtual keycodes and modifier flags from
  `Key::mac_virtual_keycode`, so keys no longer pass through keysym translation. Needs
  Accessibility permission. Mouse with absolute coordinates, high-resolution scroll deltas.
- **Cursor:** send cursor shape and position separately so the client draws it locally.
- **Audio (later):** ScreenCaptureKit audio capture, Opus.
- **Run as:** a LaunchAgent in the user session.

## Transport (`aktivestream-proto`)

- QUIC (quinn) over UDP: TLS 1.3 gives encryption and server authentication for free;
  the host keeps a self-signed certificate and the client pins its fingerprint on first
  use, like SSH.
- A video stream (one access unit per QUIC stream or datagrams with sequence numbers),
  a reliable control stream for input, cursor, clipboard (UTF-8) and keyframe requests.
- Authentication: the Mac account password checked by the host, or a pairing key.

## Client side

- **Windows:** Media Foundation hardware decoder (NVDEC on the RTX 3090 Ti) into D3D11
  textures, a D3D11 swap chain for presentation (the GDI renderer from Phase 1 is replaced
  by a renderer trait with a D3D11 implementation).
- **Linux/macOS clients:** VA-API/NVDEC and VideoToolbox decoders respectively.

## Measured: encode throughput (2026-10-19, `aktivestream-host enctest`, synthetic frames)

M5 Max VideoToolbox HEVC, 1920x1200, release build: **226 fps**, ~76 Mbit/s for a worst-case
high-entropy moving pattern, single IDR. Apple's lossless VNC does ~4.8 full frames/s at that
size (~11 Mpixel/s), so hardware HEVC is ~47x faster, with large headroom over 60 fps. Real
desktop content (mostly static) compresses far better and encodes faster. This validates the
Phase 2 performance thesis: the ceiling is Apple's CPU encoder in the VNC path, not the Mac.
(The enctest's per-frame "latency" is inflated by batch submission; realtime-paced latency is
much lower and is the next thing to measure end to end.)

## Open questions to measure first

- End-to-end latency budget: capture, encode, network and decode times per frame on this
  hardware (realtime-paced, not batched).
- HEVC 4:2:0 text sharpness at Retina size vs. 4:4:4 (VideoToolbox HEVC 4:4:4 support on M5).
- Whether to reverse-engineer Apple's own high-performance Screen Sharing mode instead
  (Mac-to-Mac only today; would not need our agent, but no Windows decoder exists for it).
