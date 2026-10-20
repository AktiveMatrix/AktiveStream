//! Keyboard profile engine for AktiveStream.
//!
//! The platform layer reports physical PC keys as [`KeyInput`]. The [`Engine`] applies a
//! [`Profile`] (by default [`Profile::mac_default`], a sensible PC-to-Mac mapping
//! Desktop's built-in "macOS" profile) and returns [`HostEvent`]s in Mac terms: Command,
//! Control, Option, Shift and positional keys. Backends turn those into wire events:
//! [`RfbTranslator`] produces RFB keysyms for macOS Screen Sharing.
//!
//! The public items in this file are the contract the Windows client builds against; the
//! implementation lives in the private modules. Signatures change only together with
//! `crates/aktivestream-win`.

use bitflags::bitflags;

mod engine;
mod key;
mod keysym;
pub mod native;
mod profile;

/// A physical key on a PC keyboard, named by position on a US layout.
///
/// Characters are decided by the Mac's keyboard layout, not the PC's, so keys are
/// positional: `Key::Q` is the key left of `W` whatever the PC layout prints on it.
#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    /// `` ` `` / `~`
    Grave,
    Minus,
    Equal,
    LeftBracket,
    RightBracket,
    Backslash,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
    /// The extra key left of `Z` on ISO keyboards.
    IntlBackslash,
    Space,
    Enter,
    Tab,
    Backspace,
    Escape,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
    F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24,
    PrintScreen,
    ScrollLock,
    Pause,
    CapsLock,
    NumLock,
    Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
    NumpadDecimal,
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadEnter,
    LShift,
    RShift,
    LCtrl,
    RCtrl,
    LAlt,
    RAlt,
    LWin,
    RWin,
    /// The context-menu key.
    Apps,
    VolumeMute,
    VolumeDown,
    VolumeUp,
    MediaPlayPause,
    MediaNext,
    MediaPrev,
    /// Any Windows virtual-key code without a mapping above.
    Other(u16),
}

impl Key {
    /// Maps a Windows low-level keyboard hook event (`KBDLLHOOKSTRUCT.vkCode`, `scanCode`,
    /// and `LLKHF_EXTENDED`) to a key. Low-level hooks already report sided modifiers
    /// (`VK_LSHIFT`, `VK_RCONTROL`, ...); `VK_RETURN` with the extended flag is the numpad
    /// Enter. Numpad keys with Num Lock off arrive as navigation keys and stay navigation keys.
    /// Extended `VK_LSHIFT`/`VK_RSHIFT` transitions are the synthetic shifts Windows wraps
    /// around numpad navigation keys when Num Lock is on; they map to `Other` and pass
    /// through untouched.
    pub fn from_windows(vk: u16, scan: u16, extended: bool) -> Key {
        key::from_windows(vk, scan, extended)
    }

    /// The Carbon `kVK_*` virtual keycode at this key's position on an ANSI Mac keyboard,
    /// for backends that inject Mac key events directly. PC-only keys follow Apple's PC
    /// keyboard convention (Print Screen = F13, Scroll Lock = F14, Pause = F15,
    /// Num Lock = keypad Clear, Insert = Help). `None` for keys with no Mac keycode
    /// (F21–F24, media transport keys, `Other`).
    pub fn mac_virtual_keycode(self) -> Option<u16> {
        key::mac_virtual_keycode(self)
    }

    /// True for the eight sided modifier keys.
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            Key::LShift | Key::RShift | Key::LCtrl | Key::RCtrl | Key::LAlt | Key::RAlt | Key::LWin | Key::RWin
        )
    }

    /// Caps Lock, Num Lock and Scroll Lock toggle local state and are never sent to the host.
    pub fn is_lock(self) -> bool {
        matches!(self, Key::CapsLock | Key::NumLock | Key::ScrollLock)
    }
}

bitflags! {
    /// Physical modifiers as a profile matches them: either side counts.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct PhysMods: u8 {
        const SHIFT = 1;
        const CTRL = 2;
        const ALT = 4;
        const WIN = 8;
    }
}

bitflags! {
    /// Modifiers held on the Mac.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct HostMods: u8 {
        const SHIFT = 1;
        const CONTROL = 2;
        const OPTION = 4;
        const COMMAND = 8;
    }
}

/// One Mac modifier key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostMod {
    Shift,
    Control,
    Option,
    Command,
}

impl HostMod {
    /// The matching [`HostMods`] flag.
    pub fn flag(self) -> HostMods {
        match self {
            HostMod::Shift => HostMods::SHIFT,
            HostMod::Control => HostMods::CONTROL,
            HostMod::Option => HostMods::OPTION,
            HostMod::Command => HostMods::COMMAND,
        }
    }

    /// Carbon `kVK_*` code of the left-hand key for this modifier.
    pub fn mac_virtual_keycode(self) -> u16 {
        match self {
            HostMod::Shift => 0x38,
            HostMod::Control => 0x3B,
            HostMod::Option => 0x3A,
            HostMod::Command => 0x37,
        }
    }
}

/// "When I press `when` + `key`, send `send` + `send_key`", for shortcut remapping.
/// `when` must match the held physical modifiers exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    pub when: PhysMods,
    pub key: Key,
    pub send: HostMods,
    pub send_key: Key,
}

/// Client-side actions bound to key chords. They take priority over conversions and are
/// never sent to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalAction {
    ToggleFullscreen,
    ToggleSystemShortcuts,
    Disconnect,
    /// Hand the key just pressed to Windows together with a Win key (one of
    /// [`Profile::windows_chords`] while the Win key itself is going to the host). The
    /// platform layer replays Win + this key to Windows; the engine has already consumed
    /// the physical key and released the Win key's host Command.
    PassChordToWindows,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShortcut {
    pub when: PhysMods,
    pub key: Key,
    pub action: LocalAction,
}

/// When Windows system shortcuts (the Win key, Alt+Tab, Alt+Shift+Tab, Alt+Esc, Ctrl+Esc)
/// go to the host instead of staying on the PC. Routes Windows system shortcuts to the host or keeps them local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SystemShortcutMode {
    DontSend,
    OnConnect,
    #[default]
    InFullscreen,
}

/// Mouse rewrites tied to modifiers (Ctrl+click = right click, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseRules {
    /// Ctrl + left click sends a right click.
    pub ctrl_click_is_right_click: bool,
    /// Ctrl + Win + left click sends a middle click.
    pub ctrl_win_click_is_middle_click: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    /// Host modifier each physical modifier key sends when no conversion is active.
    pub modifier_map: Vec<(Key, HostMod)>,
    pub conversions: Vec<Conversion>,
    pub local_shortcuts: Vec<LocalShortcut>,
    pub system_shortcuts: SystemShortcutMode,
    pub mouse: MouseRules,
    /// Win-key chords that always stay on the PC, even while system shortcuts go to the
    /// host: the Windows Magnifier shortcuts, so magnification keeps working in full screen.
    /// (Kept on the PC so client-side magnification always works.)
    pub windows_chords: Vec<(PhysMods, Key)>,
}

impl Profile {
    /// The default PC-to-Mac mapping (see docs/keyboard-profile.md): Left Ctrl = Command, Right Ctrl = Control,
    /// Alt = Option, Win = Command, plus the default key conversions and client shortcuts.
    pub fn mac_default() -> Profile {
        profile::mac_default()
    }
}

/// A physical key transition reported by the platform layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    pub down: bool,
}

/// Whether the platform layer swallows the PC key event or lets Windows handle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Consume,
    PassToOs,
}

/// A host-side key transition, in Mac terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEvent {
    Mod { m: HostMod, down: bool },
    Key { key: Key, down: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub disposition: Disposition,
    /// Events to send to the host, in order.
    pub host: Vec<HostEvent>,
    pub local: Option<LocalAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Applies a [`Profile`] to a stream of physical key events.
///
/// Invariants: every host key or modifier the engine reports down is reported up exactly
/// once (on the matching physical release, on a conversion ending, or from
/// [`Engine::release_all`]); a key's up event names the same host key as its down event
/// even if modifiers changed in between; auto-repeat (a down for a key already held) repeats
/// only non-modifier keys.
///
/// Routing to Windows: lock keys and `Key::Other` always pass through with no host events.
/// While system shortcuts stay on the PC, Shift, Ctrl and Alt pass through *and* send their
/// host modifiers (so Windows still sees Alt+Tab, Ctrl+Esc and friends), Win keys pass
/// through with no host effect, and Alt+Tab, Alt+Shift+Tab, Alt+Esc, Ctrl+Esc,
/// Ctrl+Shift+Esc and any key pressed while such a Win key is held go to Windows only.
/// While system shortcuts go to the host, every key is consumed. A key's release always
/// gets the disposition its press got. Because Alt can reach Windows, the client's window
/// procedure must swallow `WM_SYSKEYDOWN`/`WM_SYSKEYUP` and `SC_KEYMENU` so a lone Alt does
/// not open the system menu.
#[derive(Debug)]
pub struct Engine {
    profile: Profile,
    state: engine::State,
}

impl Engine {
    pub fn new(profile: Profile) -> Engine {
        Engine { profile, state: engine::State::default() }
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Tells the engine whether the session window is full screen; with
    /// [`SystemShortcutMode::InFullscreen`] this decides where system shortcuts go.
    /// A change also cancels a [`LocalAction::ToggleSystemShortcuts`] toggle.
    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        self.state.set_fullscreen(fullscreen);
    }

    /// Whether system shortcuts currently go to the host (mode, full-screen state, and the
    /// [`LocalAction::ToggleSystemShortcuts`] toggle combined).
    pub fn system_shortcuts_to_host(&self) -> bool {
        self.state.system_shortcuts_to_host(&self.profile)
    }

    /// Local shortcuts are reported in [`Output::local`] and the engine applies
    /// `ToggleSystemShortcuts` itself; the client performs `ToggleFullscreen` (then calls
    /// [`Engine::set_fullscreen`]) and `Disconnect`.
    pub fn handle_key(&mut self, input: KeyInput) -> Output {
        self.state.handle_key(&self.profile, input)
    }

    /// Rewrites a mouse button for the held modifiers and returns `(host events, button to
    /// send)`.
    ///
    /// On press, send the returned events *before* the button-down; for a Ctrl+click they
    /// release the Ctrl-derived host modifiers so the click arrives as a plain right click
    /// (Ctrl+Win+click: middle click). On release, call with the same `button`, send the
    /// returned button's button-up *first*, then the returned events, which restore those
    /// modifiers if their keys are still held.
    pub fn mouse_button(&mut self, button: MouseButton, down: bool) -> (Vec<HostEvent>, MouseButton) {
        self.state.mouse_button(&self.profile, button, down)
    }

    /// Releases everything currently held on the host, for focus loss or disconnect, and
    /// forgets held physical keys.
    pub fn release_all(&mut self) -> Vec<HostEvent> {
        self.state.release_all()
    }
}

/// Turns [`HostEvent`]s into RFB `KeyEvent` keysyms for macOS Screen Sharing.
///
/// Measured on macOS 26.6.2 (docs/mac-screen-sharing.md): Super_L = Command,
/// Meta_L = Option, Control_L = Control (Control_R does nothing), Shift_L = Shift.
/// Printable keys are sent as their US-layout keysym, shifted when host Shift is held
/// (or Caps Lock is on, for letters); a key's release always repeats the keysym its press sent.
#[derive(Debug, Default)]
pub struct RfbTranslator {
    state: keysym::Translator,
}

impl RfbTranslator {
    pub fn new() -> RfbTranslator {
        RfbTranslator::default()
    }

    /// Local Caps Lock state, read by the platform layer before each key event.
    pub fn set_caps_lock(&mut self, on: bool) {
        self.state.set_caps_lock(on);
    }

    /// Returns `(keysym, down)` pairs for one host event. Keys with no keysym return nothing.
    pub fn translate(&mut self, event: HostEvent) -> Vec<(u32, bool)> {
        self.state.translate(event)
    }
}
