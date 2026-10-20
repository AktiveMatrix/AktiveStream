# AktiveStream

Low-latency remote desktop for driving a **Mac from a Windows PC** (with Linux and macOS
clients planned). Two paths share one client:

- **Native (recommended):** a small Swift agent on the Mac captures the screen with
  ScreenCaptureKit, hardware-encodes it with VideoToolbox (HEVC), and streams it over **QUIC
  (TLS 1.3)** to the client, which decodes on the GPU (Media Foundation / NVDEC) and presents
  with Direct3D 11. This is what makes it smooth at full resolution.
- **VNC fallback:** a from-scratch RFB client for macOS's built-in Screen Sharing — no Mac
  install, logs in with the Mac account, with a PC→Mac keyboard mapping that makes the whole
  keyboard behave (Left Ctrl = ⌘, Right Ctrl = ⌃, Alt = ⌥, and the usual shortcut conversions).

> **Status: alpha.** The native viewer works end to end (live desktop, hardware video over
> QUIC). Keyboard/mouse control over the native path, realtime pacing, cursor and audio are in
> progress. The VNC client is usable today.

## Why

Apple's built-in Screen Sharing encodes losslessly on the CPU, so it tops out at a few full
frames per second at Retina resolution. A hardware video encoder on the Mac is dozens of times
faster — the same approach other hardware-video remote desktops use — so AktiveStream streams a
4K-class desktop smoothly while keeping keystrokes responsive.

## Workspace

| Crate / component | Role |
|---|---|
| `aktivestream-win` | Windows client (`aktivestream.exe`): window, Direct3D 11, keyboard hook, VNC + native modes |
| `aktivestream-proto` | Native transport: QUIC/TLS client, trust-on-first-use certificate pinning, wire messages |
| `aktivestream-video` | Windows HEVC/H.264 decode via Media Foundation (NVDEC) |
| `aktivestream-keymap` | PC-key → Mac-event engine and key-symbol translation (platform-neutral) |
| `aktivestream-rfb` | RFB/VNC client for macOS Screen Sharing (platform-neutral) |
| `host/` | Swift native host agent (ScreenCaptureKit + VideoToolbox + QUIC) |

See `docs/` for the architecture, the native protocol, and the measured behaviour of macOS
Screen Sharing.

## Build

**Windows client** (Rust, MSVC toolchain):

```powershell
cargo build --release          # produces target/release/aktivestream.exe
cargo test --workspace
```

**Mac host agent** (Swift, macOS 14+):

```sh
cd host && swift build -c release
```

For a persistent, permission-stable agent, package it as a signed `.app` with
`host/packaging/make-bundle.sh` (set your own Developer ID), install it, and load the
LaunchAgent. The agent needs **Screen Recording** permission (and **Accessibility** for input),
granted once in System Settings — the same one-time grant any screen-capture tool needs.

## Usage

```powershell
# Native (QUIC + HEVC), connects to the host agent on UDP 35390:
aktivestream.exe --native <mac-host>

# VNC fallback, connects to macOS Screen Sharing on 5900:
aktivestream.exe <mac-host>
```

## Interoperability note

The VNC client speaks the open RFB protocol (RFC 6143) plus the behaviour of macOS's Screen
Sharing server, observed by connecting to a Mac you control with your own credentials. It
contains no third-party code; the macOS-specific details in `docs/` are behavioural
observations for interoperability.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
