# AktiveStream

Low-latency remote desktop for driving a **Mac from a Windows or Linux PC** (a macOS client is
planned). Two paths share one client:

- **Native (recommended):** a small Swift agent on the Mac captures the screen with
  ScreenCaptureKit, hardware-encodes it with VideoToolbox (HEVC), and streams it over **QUIC
  (TLS 1.3)** to the client, which decodes on the GPU (Media Foundation / NVDEC) and presents
  with Direct3D 11. This is what makes it smooth at full resolution.
- **VNC fallback:** a from-scratch RFB client for macOS's built-in Screen Sharing — no Mac
  install, logs in with the Mac account, with a PC→Mac keyboard mapping that makes the whole
  keyboard behave (Left Ctrl = ⌘, Right Ctrl = ⌃, Alt = ⌥, and the usual shortcut conversions).

> **Status: alpha.** The native viewer works end to end — live desktop and hardware video over
> QUIC, with keyboard and mouse control (the host injects input with CGEvent). A **Linux client**
> (X11/Wayland) runs both paths too, with a connection phone book, an SSH tunnel for off-LAN
> access, and a full-screen toolbar. Realtime pacing, cursor and audio are in progress; the VNC
> client is usable today.

## Why

Apple's built-in Screen Sharing encodes losslessly on the CPU, so it tops out at a few full
frames per second at Retina resolution. A hardware video encoder on the Mac is dozens of times
faster — the same approach other hardware-video remote desktops use — so AktiveStream streams a
4K-class desktop smoothly while keeping keystrokes responsive.

## Workspace

| Crate / component | Role |
|---|---|
| `aktivestream-win` | Windows client (`aktivestream.exe`): window, Direct3D 11, keyboard hook, VNC + native modes, connection phone book, SSH tunnel |
| `aktivestream-linux` | Linux client (X11/Wayland): winit + softbuffer, VNC + native (ffmpeg decode) modes, phone book, SSH tunnel, full-screen toolbar |
| `aktivestream-client` | Shared, platform-neutral client logic: the connection phone book and SSH tunnel (so clients stay in parity) |
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

**Linux client** (Rust). The VNC client needs only the GUI dev packages; the native path is
behind a `native` feature that pulls ffmpeg (bindings pinned to ffmpeg 8):

```sh
# VNC client (works on any Linux):
sudo apt install pkg-config libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libx11-dev libxcb1-dev libxrandr-dev libxi-dev libxcursor-dev libxcb-shm0-dev
cargo build -p aktivestream-linux --release                     # target/release/aktivestream

# Native QUIC + hardware-decode path (needs ffmpeg 8 dev libraries):
sudo apt install libva-dev libavcodec-dev libavformat-dev libavutil-dev libswscale-dev
cargo build -p aktivestream-linux --release --features native
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

The Windows binary is `aktivestream.exe`, the Linux binary is `aktivestream`; both take the same
arguments:

```sh
aktivestream --native <mac-host>         # native QUIC + hardware decode (UDP 35390)
aktivestream <mac-host>                  # VNC fallback (macOS Screen Sharing, 5900)
aktivestream --list                      # list saved connections (the phone book)
aktivestream <saved-name>                # connect by a saved connection name
aktivestream --tunnel user@jump <mac>    # VNC through an ssh -L forward (off-LAN; Tailscale needs none)
```

Saved connections live in `~/.config/aktivestream/connections` (or `%APPDATA%\aktivestream\connections`
on Windows): one per line, `name  host  port  mode(rfb|native)  [user]  [tunnel=ssh]`.

On the Linux client in full screen, move the pointer to the top edge for a toolbar (connection
name, leave full screen, disconnect).

## Interoperability note

The VNC client speaks the open RFB protocol (RFC 6143) plus the behaviour of macOS's Screen
Sharing server, observed by connecting to a Mac you control with your own credentials. It
contains no third-party code; the macOS-specific details in `docs/` are behavioural
observations for interoperability.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
