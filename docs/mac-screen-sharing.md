# macOS Screen Sharing: measured behaviour

The behaviour below was observed against macOS 26 (Apple Silicon) by connecting to a Mac with
its own credentials — interoperability observations for the RFB client, not from any
third-party source. Treat it as evidence for that build; re-measure after macOS updates.

## Handshake

| Item | Observed |
|---|---|
| Version string | `RFB 003.889` |
| Client reply that works | `RFB 003.008` |
| Security types offered | `[30, 33, 36, 2, 35]` |
| Type 30 (Apple DH) | works with the Mac account name + password |
| Type 2 (VNC auth) | offered because "VNC viewers may control screen with password" is on (`VNCLegacyConnectionsEnabled = 1`); password limited to 8 bytes |
| Types 33, 35, 36 | Apple-specific, not implemented yet (see `apple-security-types.md`) |
| ServerInit size / name | 3456 x 2234, `your-mac` (full Retina pixel size) |

`/Library/Preferences/com.apple.RemoteManagement` also has `allowInsecureDH = 1`, which is
what keeps type 30 on offer.

## Encodings

Probe: SetEncodings with exactly one encoding, then a non-incremental update of a 64x64 corner.

| Requested | Server answered with |
|---|---|
| Tight (7) | Raw (0) |
| ZRLE (16) | ZRLE (16) |
| Hextile (5) | Raw (0) |
| Zlib (6) | Zlib (6) |
| RRE (2) | Raw (0) |

Only ZRLE and Zlib come back compressed. Both are lossless and are encoded on the Mac's
CPU, which caps throughput no matter how fast the client decodes. This is the reason for
the Phase 2 native host agent (`phase2-native.md`).

## Keysym to Mac modifier mapping

Probe: connect with type 30, press one keysym, read `CGEventSource.flagsState(.combinedSessionState)`
over SSH while it is held, release. Modifier keysyms only.

| Keysym | Mac flags while held |
|---|---|
| `Control_L` 0xffe3 | Control (left) |
| `Control_R` 0xffe4 | **nothing** |
| `Shift_L` 0xffe1 | Shift (left) |
| `Shift_R` 0xffe2 | Shift (right) |
| `Alt_L` 0xffe9 / `Alt_R` 0xffea | **Command** (left / right) |
| `Meta_L` 0xffe7 / `Meta_R` 0xffe8 | **Option** (left / right) |
| `Super_L` 0xffeb / `Super_R` 0xffec | Command (left / right) |
| `Hyper_L` 0xffed | Command (left) |
| `Mode_switch` 0xff7e, `ISO_Level3_Shift` 0xfe03 | nothing |

Consequences:

- Option is reachable only through `Meta_*`. A client that sends `Alt_*` for the Alt key
  (most Windows VNC viewers) gives the user two Command keys and no Option key.
- Right Control must be sent as `Control_L`.
- AktiveStream sends Command as `Super_L`, Option as `Meta_L`, Control as `Control_L`.

The first probe run read `Shift_L` as "nothing"; a re-run with 0.9 s holds and three samples
showed Shift (left) consistently, so the first reading was a timing artefact. `Control_R`
stayed "nothing" in every sample.

## Throughput (2026-10-19, unlocked desktop 3456x2234, wired gigabit, ping < 1 ms)

Measured with a fresh session per variant, the first full update timed from
request to last byte.

| Variant | Time | Size |
|---|---|---|
| ZRLE 32 bpp | 705 ms | 4.30 MB |
| ZRLE 32 bpp + compress level 1 / 9 | 716 / 711 ms | 4.30 MB |
| Zlib 32 bpp | 709 ms | 3.92 MB |
| ZRLE 16 bpp | 647 ms | 2.00 MB |
| Zlib 16 bpp + compress level 1 | 635 ms | 1.49 MB |

The server sustains about 11 Mpixel/s whatever the client asks for: it ignores the
compression-level pseudo-encodings, and halving the bytes saves only about 8%. Transfer of
4.3 MB over gigabit is about 35 ms, so the time is capture and encoding on the Mac. Small
changes (typing) are cheap; large ones (scrolling, moving windows, full-screen redraws) cost
about 90 ms per Mpixel. Only a hardware video encoder (Phase 2) removes this ceiling.

Client side at the same size: full-frame ZRLE decode 57 ms with miniz_oxide, 43 ms (for a
larger 5.1 MB frame) with zlib-rs; full-frame repaint 58 ms with GDI HALFTONE, about 1.5 ms
per frame with the Direct3D 11 renderer.

## End-to-end results (keyboard, typed through the client)

Typed through `aktivestream.exe` into a raw-mode Terminal window on the Mac and checked byte for byte:

- Letters, Shift, digits, all US symbols, Return, Tab, Backspace, Escape, arrows, forward
  Delete, Shift+Tab, F1: correct.
- Right Ctrl+C / Right Ctrl+A arrive as `0x03` / `0x01` (Right Ctrl = Control).
- Modifiers while held: Left Ctrl = Command, Left/Right Alt = Option, Left Shift = Shift; in
  full screen the Win key = Command and the PC Start menu stays shut.
- Pointer: framebuffer pixel (x, y) lands on Mac point (x/2, y/2) on the 2x Retina display.
- Ctrl+F4 (Cmd+W) closed the test window; Ctrl+Alt+W disconnected.
- **Auto-repeat:** the Mac does not repeat a held key that arrives over VNC (one press held
  1.2 s typed one character); each KeyEvent down types once. Forwarding Windows' repeats is
  therefore correct and nothing doubles.
- **Option characters:** Apple's server types the keysym's own character and ignores Option
  for character generation: Option+E then E typed `ee`, Option+8 typed `8`, although the Mac
  reports Option held. Option still works as a shortcut modifier. The translator therefore
  applies the Mac US Option layer itself; with that, `é`, `ñ` (Latin-1 keysyms) and `•`, `™`
  (Unicode keysyms, 0x01000000 + code point) all type correctly, so the server accepts
  Unicode keysyms.
- **Single input session:** while another remote-desktop tool holds the Mac's console, macOS
  can report the console session locked (`CGSSessionScreenIsLocked`) with `loginwindow`
  frontmost, and Screen Sharing keystrokes do not reach the desktop even though its framebuffer
  still shows the live desktop. Only one tool can drive the keyboard at a time.
- **Clipboard:** macOS Screen Sharing gates clipboard sync behind an "auto pasteboard" opt-in.
  Observed client→server message types (little the standard RFB spec covers): **6** ClientCutText
  (plain text, `{u8 type; u8 pad[3]; u32 len}` + Latin-1), **0x15** an 8-byte auto-pasteboard
  enable (`{u8 type; u8 pad; u16 command; u32 reserved}`, command 1 turns on push), and **0x1f**
  a richer multi-format path (a zlib-compressed pasteboard blob). Server→client text returns as
  standard ServerCutText (type 3). AktiveStream sends the type-0x15 enable after setup and keeps
  the type-6 send and type-3 receive; a live session with it enabled stays stable.

  **But delivery is gated by console ownership.** The server's pasteboard handling targets the
  logged-in user's session, so on a headless Mac whose console is owned by `loginwindow` (not a
  logged-in user), CutText clears rather than sets the user pasteboard and no push arrives — the
  same session-ownership issue as keystrokes reaching a hidden lock screen. Clipboard works only
  when AktiveStream owns the console (interactive login, no competing remote session). Reliable
  headless clipboard belongs to the native agent, which owns `NSPasteboard` in the user session.
- Right Shift initially failed: a client-side bug, see `keyboard-profile.md` (Windows quirks).

## Headless (lid closed)

- The Mac runs lid-closed on a shelf. Closing the lid turns the internal display off, which
  triggered "require password after display off"; screen lock was turned off
  (`sysadminctl -screenLock off`) on 2026-10-19 so remote sessions are not interrupted.
- At the lock screen the server reports 1920x1200 instead of 3456x2234.
- Display modes include 1728x1080 points (3456x2160 pixels), which would show 1:1 in full
  screen on a 3840x2160 monitor, but switching modes with the lid closed failed
  (CGError 1014 from an SSH session, `launchctl asuser`, and an Aqua launchd job). Options:
  a 4K HDMI dummy plug, or a virtual display created by the Phase 2 host agent.
