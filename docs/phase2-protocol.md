# Phase 2 protocol (AktiveStream native)

The Phase 2 path replaces VNC with a native Mac agent and a hardware-video transport, for
fluid, low-latency performance and to run in the user's GUI session (fixing the headless
clipboard/Zoom gate). Host is Swift; client is the existing Rust app. This file is the wire
contract both implement.

## Transport

- **QUIC over UDP** (TLS 1.3 built in). Host listens (default UDP **35390**); client dials.
  Host side: Network.framework `NWListener`/`NWConnection` with QUIC parameters. Client side:
  `quinn` (Rust).
- **ALPN**: `astream/1`.
- **Server identity**: the host holds a self-signed P-256 certificate, persisted in its
  Application Support dir. On first connect the client shows the certificate's SHA-256
  fingerprint and pins it (trust-on-first-use), like SSH known_hosts. Later connects require
  the same fingerprint or the client refuses.
- **Client auth**: after the QUIC/TLS handshake, the client proves it may connect with the
  Mac account password over the (already-encrypted) control stream (see Auth). No password
  ever crosses the wire in the clear or before TLS.

## Streams

QUIC streams, each a framed channel:

| Stream | Dir | Reliability | Carries |
|---|---|---|---|
| control | bi | reliable | Hello, Auth, DisplayList, SetDisplay, KeyframeRequest, Ping/Pong, Error |
| input | c→h | reliable, ordered | Key, Pointer, Scroll |
| clipboard | bi | reliable | ClipboardOffer, ClipboardData |
| video | h→c | unreliable datagrams + a reliable sideband | encoded frames (see Video) |

Each message: `u8 kind` then a kind-specific body. Multi-byte integers are **little-endian**
(unlike RFB; both ends are LE and it avoids byte-swaps on the hot path). Lengths are `u32`.

## Control messages

- `Hello {u8 kind=1; u32 proto_version; u8 client_name_len; client_name[]}` — client→host first.
- `HelloOk {kind=2; u32 proto_version; u8 host_name_len; host_name[]; u8 display_count}`.
- `Auth {kind=3; u16 token_len; token[]}` — client→host: an auth token = HKDF(exported TLS
  keying material "rtight-auth", Mac-password) so the password is bound to this TLS session
  and never replayable. Host recomputes and compares.
- `AuthOk {kind=4}` / `Error {kind=0; u16 msg_len; msg[]}`.
- `DisplayList {kind=5; [ {u32 id; u32 px_w; u32 px_h; u32 pt_w; u32 pt_h; u8 is_main} ]}`.
- `SetDisplay {kind=6; u32 id}` — pick which display to stream.
- `KeyframeRequest {kind=7}` — client lost sync / resized; host emits an IDR next frame.
- `Ping {kind=8; u64 t}` / `Pong {kind=9; u64 t}` — RTT + keepalive.

## Input messages (client→host)

The client already turns PC keys into Mac terms with `aktivestream-keymap`; for Phase 2 it uses
`Key::mac_virtual_keycode` + `HostMod::mac_virtual_keycode` and sends native keycodes, so no
keysym round-trip.

- `Key {kind=1; u16 keycode; u8 down; u32 mod_flags}` — `keycode` = Carbon kVK_*; `mod_flags`
  = CGEventFlags bits the host sets on the event.
- `Pointer {kind=2; u8 buttons; f32 x; f32 y}` — `x`,`y` in 0..1 of the streamed display;
  host maps to global points. Absolute, so no cursor drift.
- `Scroll {kind=3; f32 dx; f32 dy; u8 precise}` — high-resolution scroll deltas.

Host injects via `CGEvent` posted to the session event tap (needs Accessibility TCC).

## Video (host→client)

- Codec: **HEVC** (Main profile) from VideoToolbox, H.264 fallback. Real-time, low-latency
  rate control, no B-frames, IDR on demand.
- Each encoded frame → one `VideoFrame` sent as QUIC **datagrams** (fragmented if over the
  datagram limit) plus a small reliable `FrameMeta` on a sideband so loss is detectable:
  - `FrameMeta {kind=10; u64 frame_id; u32 total_len; u16 fragments; u8 is_idr; u64 capture_ns}`
  - `VideoFragment` datagram: `{u64 frame_id; u16 index; u16 count; bytes[]}`.
- Parameter sets (VPS/SPS/PPS) are prepended to each IDR (Annex B), so the decoder needs no
  out-of-band config. On missing fragments the client sends `KeyframeRequest` and drops the
  partial frame.
- Client decode: Media Foundation H.265/H.264 hardware decoder (NVDEC on the RTX 3090 Ti) into
  NV12, converted in the existing D3D11 renderer; the area-filter shader already scales it.
- Cursor: sent separately (the host hides the hardware cursor from capture and sends
  `CursorShape`/`CursorMove` on control) so the client draws a crisp local cursor — reuses the
  Phase 1 cursor path. (Phase 2 addition, message numbers TBD.)

## Clipboard (bidirectional, runs in the user session)

- On local change each side sends `ClipboardOffer {kind=1; u32 len; u8 is_text}`; the peer may
  request it, then `ClipboardData {kind=2; u32 len; bytes[]}` (UTF-8 for text). The host uses
  `NSPasteboard` in the user session, so this works headless — the thing Apple's VNC server
  could not do.

## Sessions and permissions

- The agent is a **LaunchAgent** in the user's GUI session (not a daemon), so ScreenCaptureKit,
  CGEvent and NSPasteboard act as the logged-in user — this is what bypasses the console-owner
  gate that blocks VNC input/clipboard on the headless Mac.
- TCC: needs **Screen Recording** (capture) and **Accessibility** (input), granted once to the
  signed bundle (Developer ID, Team TEAMID); the grant persists across restarts.
- Not started yet; see `phase2-native.md` for the component design and build/sign/launchd plan.
