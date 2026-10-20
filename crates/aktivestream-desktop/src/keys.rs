//! Maps winit physical key codes (W3C-style, position-based) to the platform-neutral
//! `aktivestream_keymap::Key`. Unmapped keys become `Key::Other(0)`, which the engine passes
//! through with no host effect.

use aktivestream_keymap::Key;
use winit::keyboard::KeyCode;

pub fn to_key(code: KeyCode) -> Key {
    use KeyCode as C;
    match code {
        C::KeyA => Key::A,
        C::KeyB => Key::B,
        C::KeyC => Key::C,
        C::KeyD => Key::D,
        C::KeyE => Key::E,
        C::KeyF => Key::F,
        C::KeyG => Key::G,
        C::KeyH => Key::H,
        C::KeyI => Key::I,
        C::KeyJ => Key::J,
        C::KeyK => Key::K,
        C::KeyL => Key::L,
        C::KeyM => Key::M,
        C::KeyN => Key::N,
        C::KeyO => Key::O,
        C::KeyP => Key::P,
        C::KeyQ => Key::Q,
        C::KeyR => Key::R,
        C::KeyS => Key::S,
        C::KeyT => Key::T,
        C::KeyU => Key::U,
        C::KeyV => Key::V,
        C::KeyW => Key::W,
        C::KeyX => Key::X,
        C::KeyY => Key::Y,
        C::KeyZ => Key::Z,
        C::Digit0 => Key::Digit0,
        C::Digit1 => Key::Digit1,
        C::Digit2 => Key::Digit2,
        C::Digit3 => Key::Digit3,
        C::Digit4 => Key::Digit4,
        C::Digit5 => Key::Digit5,
        C::Digit6 => Key::Digit6,
        C::Digit7 => Key::Digit7,
        C::Digit8 => Key::Digit8,
        C::Digit9 => Key::Digit9,
        C::Backquote => Key::Grave,
        C::Minus => Key::Minus,
        C::Equal => Key::Equal,
        C::BracketLeft => Key::LeftBracket,
        C::BracketRight => Key::RightBracket,
        C::Backslash => Key::Backslash,
        C::Semicolon => Key::Semicolon,
        C::Quote => Key::Quote,
        C::Comma => Key::Comma,
        C::Period => Key::Period,
        C::Slash => Key::Slash,
        C::IntlBackslash => Key::IntlBackslash,
        C::Space => Key::Space,
        C::Enter => Key::Enter,
        C::Tab => Key::Tab,
        C::Backspace => Key::Backspace,
        C::Escape => Key::Escape,
        C::Insert => Key::Insert,
        C::Delete => Key::Delete,
        C::Home => Key::Home,
        C::End => Key::End,
        C::PageUp => Key::PageUp,
        C::PageDown => Key::PageDown,
        C::ArrowLeft => Key::Left,
        C::ArrowRight => Key::Right,
        C::ArrowUp => Key::Up,
        C::ArrowDown => Key::Down,
        C::F1 => Key::F1,
        C::F2 => Key::F2,
        C::F3 => Key::F3,
        C::F4 => Key::F4,
        C::F5 => Key::F5,
        C::F6 => Key::F6,
        C::F7 => Key::F7,
        C::F8 => Key::F8,
        C::F9 => Key::F9,
        C::F10 => Key::F10,
        C::F11 => Key::F11,
        C::F12 => Key::F12,
        C::F13 => Key::F13,
        C::F14 => Key::F14,
        C::F15 => Key::F15,
        C::F16 => Key::F16,
        C::F17 => Key::F17,
        C::F18 => Key::F18,
        C::F19 => Key::F19,
        C::F20 => Key::F20,
        C::F21 => Key::F21,
        C::F22 => Key::F22,
        C::F23 => Key::F23,
        C::F24 => Key::F24,
        C::PrintScreen => Key::PrintScreen,
        C::ScrollLock => Key::ScrollLock,
        C::Pause => Key::Pause,
        C::CapsLock => Key::CapsLock,
        C::NumLock => Key::NumLock,
        C::Numpad0 => Key::Numpad0,
        C::Numpad1 => Key::Numpad1,
        C::Numpad2 => Key::Numpad2,
        C::Numpad3 => Key::Numpad3,
        C::Numpad4 => Key::Numpad4,
        C::Numpad5 => Key::Numpad5,
        C::Numpad6 => Key::Numpad6,
        C::Numpad7 => Key::Numpad7,
        C::Numpad8 => Key::Numpad8,
        C::Numpad9 => Key::Numpad9,
        C::NumpadDecimal => Key::NumpadDecimal,
        C::NumpadAdd => Key::NumpadAdd,
        C::NumpadSubtract => Key::NumpadSubtract,
        C::NumpadMultiply => Key::NumpadMultiply,
        C::NumpadDivide => Key::NumpadDivide,
        C::NumpadEnter => Key::NumpadEnter,
        C::ShiftLeft => Key::LShift,
        C::ShiftRight => Key::RShift,
        C::ControlLeft => Key::LCtrl,
        C::ControlRight => Key::RCtrl,
        C::AltLeft => Key::LAlt,
        C::AltRight => Key::RAlt,
        C::SuperLeft => Key::LWin,
        C::SuperRight => Key::RWin,
        C::ContextMenu => Key::Apps,
        C::AudioVolumeMute => Key::VolumeMute,
        C::AudioVolumeDown => Key::VolumeDown,
        C::AudioVolumeUp => Key::VolumeUp,
        C::MediaPlayPause => Key::MediaPlayPause,
        C::MediaTrackNext => Key::MediaNext,
        C::MediaTrackPrevious => Key::MediaPrev,
        _ => Key::Other(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_letters_digits_and_sided_modifiers() {
        assert_eq!(to_key(KeyCode::KeyA), Key::A);
        assert_eq!(to_key(KeyCode::KeyZ), Key::Z);
        assert_eq!(to_key(KeyCode::Digit0), Key::Digit0);
        assert_eq!(to_key(KeyCode::ControlLeft), Key::LCtrl);
        assert_eq!(to_key(KeyCode::ControlRight), Key::RCtrl);
        assert_eq!(to_key(KeyCode::AltLeft), Key::LAlt);
        assert_eq!(to_key(KeyCode::SuperLeft), Key::LWin);
        assert_eq!(to_key(KeyCode::ArrowUp), Key::Up);
        assert_eq!(to_key(KeyCode::Backquote), Key::Grave);
        assert_eq!(to_key(KeyCode::NumpadEnter), Key::NumpadEnter);
    }

    #[test]
    fn unmapped_key_is_other() {
        // A key past the mapped range falls through to Other, which the engine ignores.
        assert_eq!(to_key(KeyCode::F35), Key::Other(0));
    }
}
