# Validation

## Offline

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- `aktivestream-keymap`: unit tests for every conversion, modifier union, override lifetime,
  auto-repeat, system-shortcut routing, mouse rules, release-all and key-symbol translation.
- `aktivestream-rfb`: decoder tests against a test-only encoder; a VNC DES known-answer test;
  an in-process fake server for the Apple DH and VNC auth handshakes and the update flow.
- `aktivestream-video`: the `decode_file` example decodes an Annex B HEVC file and saves PNGs.

## Live, against a Mac

Point the client at a Mac you control:

```powershell
# VNC path (macOS Screen Sharing on 5900):
aktivestream.exe <mac-host>

# Native path (the host agent on 35390):
aktivestream.exe --native <mac-host>

# RFB protocol smoke + throughput (needs MAC_HOST / MAC_USER / MAC_PASSWORD env vars):
cargo run -p aktivestream-rfb --example smoke --release
```

## Manual client checks

1. The window shows the Mac desktop scaled to fit; the cursor tracks without lag.
2. In a text editor: type letters, Shift letters, digits and symbols; Left Ctrl+A/C/V work;
   Right Ctrl+C in a terminal interrupts; Alt+E then E gives `é`.
3. Ctrl+Tab switches tabs; Ctrl+Alt+Tab switches apps; Alt+F4 quits the app.
4. Ctrl+Alt+F11 toggles full screen; in full screen the Win key acts as Command, and Windows
   Magnifier (Win+Plus / Minus / Esc) still works locally.
5. Switching away from the window leaves no stuck modifiers on the Mac.
