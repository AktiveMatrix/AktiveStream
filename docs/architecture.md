# Architecture

AktiveStream is a remote-desktop client for driving a Mac from other machines with the same
keyboard behaviour as a hardware-video remote desktop. It is built in two phases that share the
client and keyboard layers.

```
             Phase 1 (now)                                  Phase 2 (planned)
 PC keys -> aktivestream-keymap ---- RfbTranslator ----> keysyms -> macOS Screen Sharing (RFB, port 5900)
            (PC-to-Mac key profile) \                                 [Apple's server, no install]
                                  \-- mac_virtual_keycode ------> Mac kVK codes -> aktivestream-host (QUIC)
                                                                   [our agent: ScreenCaptureKit +
 screen <- platform shell <- aktivestream-rfb (ZRLE/Zlib/Raw/CopyRect)    VideoToolbox HEVC + CGEvent]
          (aktivestream-win)  <- aktivestream-native (HEVC decode, planned)
```

## Crates

| Crate | Role | Platform |
|---|---|---|
| `aktivestream-keymap` | Physical keys to Mac host events. `Engine` applies a `Profile` (default `Profile::mac_default`); `RfbTranslator` turns host events into RFB keysyms; `Key::mac_virtual_keycode` serves the native backend. | neutral |
| `aktivestream-rfb` | RFB client for Apple's server: handshake, security types 30 and 2, ZRLE/Zlib/Raw/CopyRect, Cursor/DesktopSize/LastRect, client messages. Split into a reader (decodes into a shared `Framebuffer`, pipelines update requests) and a writer. | neutral |
| `aktivestream-win` | Windows shell, binary `aktivestream.exe`: Win32 window, GDI rendering, low-level keyboard hook, mouse, clipboard, Credential Manager. | Windows |

Planned: `aktivestream-host` (macOS agent), `aktivestream-proto` (native wire protocol), and
Linux and macOS client shells (see Roadmap).

## Data flow in `aktivestream-win`

Four threads:

- **Keyboard-hook thread** (time-critical priority) owns the low-level keyboard hook. Every
  key on the PC passes through it, so it does only the keymap step under a short mutex
  (`KeyCtx`: `Engine` + `RfbTranslator`) and a channel send; it never waits for rendering.
  Keys are consumed or passed to Windows according to the engine's `Disposition`, which is
  how the Win key, Alt+Tab and Alt+F4 are captured.
- **UI thread** owns the window and the Direct3D 11 renderer (`render.rs`): the framebuffer
  lives in a GPU texture, dirty rectangles are uploaded as they arrive, and a pixel shader
  scales with an exact area filter (about 1.5 ms per frame on the RTX 3090 Ti). GDI
  `StretchDIBits` (HALFTONE) remains as a fallback when Direct3D is unavailable.
- **Reader thread** loops on `SessionReader::next_event`. Each FramebufferUpdate is read in
  full, the next incremental update is requested at once, and only then decoded into the
  shared `Mutex<Framebuffer>`, so Apple's slow encoder works on the next update while this
  one decodes. Dirty rectangles are merged and posted to the window as one message.
- **Writer thread** drains a channel of key, pointer and clipboard messages and writes them
  with `SessionWriter`, merging consecutive pointer moves. Nothing on the UI or hook thread
  blocks on the network.

On focus loss the engine's `release_all` releases every key and modifier held on the Mac.

## Keyboard model

The engine works in Mac terms (Command, Control, Option, Shift plus positional keys), so a
backend never needs to know about Windows. Characters are decided by the Mac's layout: keys
are positional, named by their US-layout position. See `keyboard-profile.md` for the
profile and `mac-screen-sharing.md` for why Command is sent as `Super_L` and Option as
`Meta_L`.

## Roadmap

1. **Phase 1, RFB client (in progress):** Windows client for Apple's built-in Screen
   Sharing with the PC-to-Mac keyboard profile.
2. **Apple security types 33/35/36:** reverse-engineer and implement Apple's encrypted
   session types so Phase 1 traffic is encrypted (`apple-security-types.md`).
3. **Phase 2, native host agent:** fluid, low-latency performance via hardware video encoding on
   the Mac (`phase2-native.md`).
4. **Linux and macOS clients:** the user wants clients on all three desktops so the
   experience stays in sync. The keymap and protocol crates are platform-neutral for this
   reason; each client adds only a shell (window, key capture, rendering) plus a
   `Key::from_*` table (Linux evdev codes, macOS kVK codes). Notes:
   - Linux: X11 can grab the keyboard; on Wayland, full capture needs the compositor's
     keyboard-shortcuts-inhibit protocol (GNOME/KDE support it) or evdev access.
   - macOS client: needs Accessibility/Input Monitoring permission to capture Cmd+Tab and
     similar system shortcuts with an event tap. Its profile is the identity layout
     (Mac keys already match).
