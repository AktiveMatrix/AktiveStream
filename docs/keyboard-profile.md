# Keyboard mapping (PC to Mac)

A Windows keyboard has no Command or Option key, and macOS Screen Sharing maps VNC key
symbols to Mac modifiers in a fixed way (see `mac-screen-sharing.md`). AktiveStream's default
profile bridges the two so a PC keyboard drives a Mac naturally. The mapping choices are
functional: there is essentially one sensible way to give a PC user Command, Control and
Option, and to make the familiar Ctrl-based shortcuts do the Mac's Command-based ones.

## Base layout

| PC key | Mac key |
|---|---|
| Left Ctrl | Command |
| Right Ctrl | Control |
| Left / Right Alt | Option |
| Left / Right Win | Command (when system shortcuts go to the Mac) |
| Left / Right Shift | Shift |

So Left Ctrl+C copies (⌘C) and Right Ctrl+C sends ^C in a terminal — the two things a PC user
needs most.

## Key conversions

So that muscle-memory PC shortcuts do the right Mac thing (physical modifiers must match
exactly; either side counts):

| You press | Mac receives |
|---|---|
| Ctrl+Alt+Tab | Cmd+Tab (switch apps) |
| Alt+Left / Alt+Right | Cmd+Left / Cmd+Right |
| Ctrl+Tab, Ctrl+Shift+Tab | Control+Tab, Control+Shift+Tab (unchanged, for tabs) |
| Alt+F4 | Cmd+Q (quit) |
| Ctrl+F4 | Cmd+W (close) |
| Ctrl+Y | Cmd+Shift+Z (redo) |
| Ctrl+Win+F | Control+Cmd+F (full screen) |
| Ctrl+Left / Ctrl+Right | Control+Left / Control+Right (Spaces) |

## Client shortcuts (handled locally, not sent)

| Chord | Action |
|---|---|
| Ctrl+Alt+F11 | Toggle full screen |
| Ctrl+Alt+Left | Toggle "send Windows shortcuts to the Mac" |
| Ctrl+Alt+W | Disconnect |

## System shortcuts

"Send Windows Shortcuts" defaults to **In Fullscreen**: in full screen the Win key, Alt+Tab,
Alt+Shift+Tab, Alt+Esc and Ctrl+Esc go to the Mac; in a window they stay on the PC. Ctrl+Alt+Del
and Win+L are always handled by Windows.

## Option-layer characters

Option+key types the Mac US layout's Option-layer character (é via the Option+E dead key, •, ™,
©, –, ≠, …), and Shift+Option its shifted layer; dead keys compose with the next letter. With
Command or Control also held, Option stays a shortcut modifier (so Option+Cmd+8 toggles macOS
Zoom). This is needed because macOS Screen Sharing types each key symbol's own character and
ignores Option when choosing it, so the Option layer is applied on the client.

## Accessibility

- **macOS Zoom** keys are produced correctly (Left Ctrl+Alt+8 / = / - reach the Mac as
  Option+Cmd+8 / = / -), but macOS Zoom runs in the user's GUI session, which Screen Sharing
  input does not reach on a headless, remotely-accessed Mac. The reliable magnification path
  there is **Windows Magnifier** on the PC side.
- **Windows Magnifier** shortcuts (Win+Plus, Win+Minus, Win+Esc) are kept on the PC even in
  full screen, so magnification of the client window always works.

## Windows quirks

- Low-level keyboard hooks report the real Right Shift with the extended flag set, the same
  flag Windows puts on the synthetic shifts it wraps around numpad keys when Num Lock is on, so
  only extended *Left* Shift events are treated as synthetic.
- Caps Lock, Num Lock and Scroll Lock toggle locally and are not sent; letters are sent
  uppercased when Caps Lock is on.
