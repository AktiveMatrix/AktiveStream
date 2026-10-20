//! Windows virtual-key codes in, Mac virtual keycodes out. Both tables are positional
//! (US/ANSI key positions).

use crate::Key;
use crate::Key::*;

const LETTERS: [Key; 26] = [A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z];
const DIGITS: [Key; 10] = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
const NUMPAD: [Key; 10] = [Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9];
const FUNCTION: [Key; 24] =
    [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24];

pub(crate) fn from_windows(vk: u16, scan: u16, extended: bool) -> Key {
    match vk {
        0x41..=0x5A => LETTERS[usize::from(vk - 0x41)],
        0x30..=0x39 => DIGITS[usize::from(vk - 0x30)],
        0x60..=0x69 => NUMPAD[usize::from(vk - 0x60)],
        0x70..=0x87 => FUNCTION[usize::from(vk - 0x70)],
        0x08 => Backspace,
        0x09 => Tab,
        0x0D if extended => NumpadEnter,
        0x0D => Enter,
        // With Num Lock on, Windows wraps numpad navigation keys in synthetic shift
        // transitions that carry the E0 (extended) prefix. They are not Shift presses.
        // Low-level hooks also report the real Right Shift as extended (measured: physical
        // scan code 0x36 arrives with LLKHF_EXTENDED), so only a Left Shift can be told apart.
        0x10 | 0xA0 if extended && scan != 0x36 => Other(vk),
        0x10 if scan == 0x36 => RShift,
        0x10 => LShift,
        0x11 if extended => RCtrl,
        0x11 => LCtrl,
        0x12 if extended => RAlt,
        0x12 => LAlt,
        0x13 => Pause,
        0x14 => CapsLock,
        0x1B => Escape,
        0x20 => Space,
        0x21 => PageUp,
        0x22 => PageDown,
        0x23 => End,
        0x24 => Home,
        0x25 => Left,
        0x26 => Up,
        0x27 => Right,
        0x28 => Down,
        0x2C => PrintScreen,
        0x2D => Insert,
        0x2E => Delete,
        0x5B => LWin,
        0x5C => RWin,
        0x5D => Apps,
        0x6A => NumpadMultiply,
        0x6B => NumpadAdd,
        0x6D => NumpadSubtract,
        0x6E => NumpadDecimal,
        0x6F => NumpadDivide,
        0x90 => NumLock,
        0x91 => ScrollLock,
        0xA0 => LShift,
        0xA1 => RShift,
        0xA2 => LCtrl,
        0xA3 => RCtrl,
        0xA4 => LAlt,
        0xA5 => RAlt,
        0xAD => VolumeMute,
        0xAE => VolumeDown,
        0xAF => VolumeUp,
        0xB0 => MediaNext,
        0xB1 => MediaPrev,
        0xB3 => MediaPlayPause,
        0xBA => Semicolon,
        0xBB => Equal,
        0xBC => Comma,
        0xBD => Minus,
        0xBE => Period,
        0xBF => Slash,
        0xC0 => Grave,
        0xDB => LeftBracket,
        0xDC => Backslash,
        0xDD => RightBracket,
        0xDE => Quote,
        0xE2 => IntlBackslash,
        _ => Other(vk),
    }
}

/// Carbon `kVK_*` codes. PC-only keys follow Apple's own PC-keyboard convention:
/// Print Screen = F13, Scroll Lock = F14, Pause = F15, Num Lock = keypad Clear,
/// Insert = Help.
pub(crate) fn mac_virtual_keycode(key: Key) -> Option<u16> {
    let code = match key {
        A => 0x00,
        S => 0x01,
        D => 0x02,
        F => 0x03,
        H => 0x04,
        G => 0x05,
        Z => 0x06,
        X => 0x07,
        C => 0x08,
        V => 0x09,
        IntlBackslash => 0x0A,
        B => 0x0B,
        Q => 0x0C,
        W => 0x0D,
        E => 0x0E,
        R => 0x0F,
        Y => 0x10,
        T => 0x11,
        Digit1 => 0x12,
        Digit2 => 0x13,
        Digit3 => 0x14,
        Digit4 => 0x15,
        Digit6 => 0x16,
        Digit5 => 0x17,
        Equal => 0x18,
        Digit9 => 0x19,
        Digit7 => 0x1A,
        Minus => 0x1B,
        Digit8 => 0x1C,
        Digit0 => 0x1D,
        RightBracket => 0x1E,
        O => 0x1F,
        U => 0x20,
        LeftBracket => 0x21,
        I => 0x22,
        P => 0x23,
        Enter => 0x24,
        L => 0x25,
        J => 0x26,
        Quote => 0x27,
        K => 0x28,
        Semicolon => 0x29,
        Backslash => 0x2A,
        Comma => 0x2B,
        Slash => 0x2C,
        N => 0x2D,
        M => 0x2E,
        Period => 0x2F,
        Tab => 0x30,
        Space => 0x31,
        Grave => 0x32,
        Backspace => 0x33,
        Escape => 0x35,
        RWin => 0x36,
        LWin => 0x37,
        LShift => 0x38,
        CapsLock => 0x39,
        LAlt => 0x3A,
        LCtrl => 0x3B,
        RShift => 0x3C,
        RAlt => 0x3D,
        RCtrl => 0x3E,
        F17 => 0x40,
        NumpadDecimal => 0x41,
        NumpadMultiply => 0x43,
        NumpadAdd => 0x45,
        NumLock => 0x47,
        VolumeUp => 0x48,
        VolumeDown => 0x49,
        VolumeMute => 0x4A,
        NumpadDivide => 0x4B,
        NumpadEnter => 0x4C,
        NumpadSubtract => 0x4E,
        F18 => 0x4F,
        F19 => 0x50,
        Numpad0 => 0x52,
        Numpad1 => 0x53,
        Numpad2 => 0x54,
        Numpad3 => 0x55,
        Numpad4 => 0x56,
        Numpad5 => 0x57,
        Numpad6 => 0x58,
        Numpad7 => 0x59,
        F20 => 0x5A,
        Numpad8 => 0x5B,
        Numpad9 => 0x5C,
        F5 => 0x60,
        F6 => 0x61,
        F7 => 0x62,
        F3 => 0x63,
        F8 => 0x64,
        F9 => 0x65,
        F11 => 0x67,
        F13 | PrintScreen => 0x69,
        F16 => 0x6A,
        F14 | ScrollLock => 0x6B,
        F10 => 0x6D,
        Apps => 0x6E,
        F12 => 0x6F,
        F15 | Pause => 0x71,
        Insert => 0x72,
        Home => 0x73,
        PageUp => 0x74,
        Delete => 0x75,
        F4 => 0x76,
        End => 0x77,
        F2 => 0x78,
        PageDown => 0x79,
        F1 => 0x7A,
        Left => 0x7B,
        Right => 0x7C,
        Down => 0x7D,
        Up => 0x7E,
        F21 | F22 | F23 | F24 | MediaPlayPause | MediaNext | MediaPrev | Other(_) => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_samples() {
        assert_eq!(from_windows(0x41, 0x1E, false), A);
        assert_eq!(from_windows(0x5A, 0x2C, false), Z);
        assert_eq!(from_windows(0x30, 0x0B, false), Digit0);
        assert_eq!(from_windows(0x0D, 0x1C, false), Enter);
        assert_eq!(from_windows(0x0D, 0x1C, true), NumpadEnter);
        assert_eq!(from_windows(0xA2, 0x1D, false), LCtrl);
        assert_eq!(from_windows(0xA3, 0x1D, true), RCtrl);
        assert_eq!(from_windows(0x11, 0x1D, true), RCtrl);
        assert_eq!(from_windows(0x12, 0x38, false), LAlt);
        assert_eq!(from_windows(0x10, 0x36, false), RShift);
        assert_eq!(from_windows(0x10, 0x2A, false), LShift);
        assert_eq!(from_windows(0xA0, 0x2A, false), LShift);
        assert_eq!(from_windows(0xA0, 0x2A, true), Other(0xA0));
        // Right Shift as a low-level hook reports it: VK_RSHIFT, scan 0x36, extended flag set.
        assert_eq!(from_windows(0xA1, 0x36, true), RShift);
        assert_eq!(from_windows(0x10, 0x36, true), RShift);
        assert_eq!(from_windows(0x5B, 0x5B, true), LWin);
        assert_eq!(from_windows(0x5D, 0x5D, true), Apps);
        assert_eq!(from_windows(0xBA, 0x27, false), Semicolon);
        assert_eq!(from_windows(0xC0, 0x29, false), Grave);
        assert_eq!(from_windows(0xDE, 0x28, false), Quote);
        assert_eq!(from_windows(0xE2, 0x56, false), IntlBackslash);
        assert_eq!(from_windows(0x70, 0x3B, false), F1);
        assert_eq!(from_windows(0x87, 0x00, false), F24);
        assert_eq!(from_windows(0x67, 0x47, false), Numpad7);
        // Num Lock off: numpad 7 arrives as a non-extended Home and stays Home.
        assert_eq!(from_windows(0x24, 0x47, false), Home);
        assert_eq!(from_windows(0x24, 0x47, true), Home);
        assert_eq!(from_windows(0x6F, 0x35, true), NumpadDivide);
        assert_eq!(from_windows(0x14, 0x3A, false), CapsLock);
        assert_eq!(from_windows(0xAF, 0x30, true), VolumeUp);
        assert_eq!(from_windows(0x0C, 0x4C, false), Other(0x0C));
    }

    #[test]
    fn mac_keycode_samples() {
        assert_eq!(mac_virtual_keycode(A), Some(0x00));
        assert_eq!(mac_virtual_keycode(Q), Some(0x0C));
        assert_eq!(mac_virtual_keycode(Enter), Some(0x24));
        assert_eq!(mac_virtual_keycode(Backspace), Some(0x33));
        assert_eq!(mac_virtual_keycode(Delete), Some(0x75));
        assert_eq!(mac_virtual_keycode(Left), Some(0x7B));
        assert_eq!(mac_virtual_keycode(Up), Some(0x7E));
        assert_eq!(mac_virtual_keycode(LWin), Some(0x37));
        assert_eq!(mac_virtual_keycode(NumpadEnter), Some(0x4C));
        assert_eq!(mac_virtual_keycode(F12), Some(0x6F));
        assert_eq!(mac_virtual_keycode(PrintScreen), Some(0x69));
        assert_eq!(mac_virtual_keycode(F24), None);
        assert_eq!(mac_virtual_keycode(Other(0x0C)), None);
    }

    #[test]
    fn letters_and_digits_have_distinct_mac_codes() {
        let mut seen = std::collections::BTreeSet::new();
        for key in LETTERS.iter().chain(&DIGITS).chain(&NUMPAD).chain(&FUNCTION[..20]) {
            let code = mac_virtual_keycode(*key).unwrap();
            assert!(seen.insert(code), "{key:?} reuses {code:#x}");
        }
    }
}
