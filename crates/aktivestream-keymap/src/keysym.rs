//! RFB keysyms for macOS Screen Sharing.

use std::collections::HashMap;

use crate::{HostEvent, HostMod, HostMods, Key};

const XK_SHIFT_L: u32 = 0xffe1;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_META_L: u32 = 0xffe7;
const XK_SUPER_L: u32 = 0xffeb;

/// Recorded for a held key whose press sent nothing (a dead key), so its release sends
/// nothing either. Keysym 0 is X11's NoSymbol.
const NOTHING: u32 = 0;

#[derive(Debug, Default)]
pub(crate) struct Translator {
    mods: HostMods,
    caps: bool,
    /// Keysym each held key's press sent, so its release matches.
    held: HashMap<Key, u32>,
    /// A dead key (Option+E, Option+U, ...) waiting for the next key.
    pending: Option<Accent>,
}

impl Translator {
    pub(crate) fn set_caps_lock(&mut self, on: bool) {
        self.caps = on;
    }

    pub(crate) fn translate(&mut self, event: HostEvent) -> Vec<(u32, bool)> {
        match event {
            HostEvent::Mod { m, down } => {
                self.mods.set(m.flag(), down);
                vec![(modifier_keysym(m), down)]
            }
            HostEvent::Key { key, down: true } => {
                if let Some(&keysym) = self.held.get(&key) {
                    return if keysym == NOTHING { Vec::new() } else { vec![(keysym, true)] };
                }
                self.press(key)
            }
            HostEvent::Key { key, down: false } => match self.held.remove(&key) {
                Some(NOTHING) => Vec::new(),
                Some(keysym) => vec![(keysym, false)],
                None => self.keysym(key).map(|keysym| vec![(keysym, false)]).unwrap_or_default(),
            },
        }
    }

    /// Apple's server types each keysym's own character and ignores Option when choosing
    /// it (measured: Option+8 typed "8"), so the Option layer of the Mac's US layout, and
    /// its dead-key accents, are applied here and sent as the resulting characters.
    fn press(&mut self, key: Key) -> Vec<(u32, bool)> {
        let mut out = Vec::new();
        let typing = !self.mods.intersects(HostMods::COMMAND | HostMods::CONTROL);
        let printable = letter(key).is_some() || us_symbol(key).is_some();
        let shift = self.mods.contains(HostMods::SHIFT);
        let mut keysym = None;
        if typing && printable && self.mods.contains(HostMods::OPTION) {
            match option_layer(key, shift) {
                Some(OptionOut::Dead(accent)) => {
                    if let Some(previous) = self.pending.replace(accent) {
                        tap(&mut out, previous.spacing());
                    }
                    self.held.insert(key, NOTHING);
                    return out;
                }
                Some(OptionOut::Char(c)) => {
                    if let Some(previous) = self.pending.take() {
                        tap(&mut out, previous.spacing());
                    }
                    keysym = Some(char_keysym(c));
                }
                None => {}
            }
        } else if let Some(accent) = self.pending.take()
            && typing
        {
            let base = self.keysym(key).filter(|_| printable).and_then(char::from_u32);
            match base {
                Some(' ') => keysym = Some(char_keysym(accent.spacing())),
                Some(base) if accent.compose(base).is_some() => keysym = accent.compose(base).map(char_keysym),
                // Anything else shows the accent itself, then the key as usual.
                _ => tap(&mut out, accent.spacing()),
            }
        }
        if let Some(keysym) = keysym.or_else(|| self.keysym(key)) {
            self.held.insert(key, keysym);
            out.push((keysym, true));
        }
        out
    }

    fn keysym(&self, key: Key) -> Option<u32> {
        let shift = self.mods.contains(HostMods::SHIFT);
        if let Some(lower) = letter(key) {
            let upper = shift != self.caps;
            return Some(u32::from(if upper { lower.to_ascii_uppercase() } else { lower }));
        }
        if let Some((plain, shifted)) = us_symbol(key) {
            return Some(u32::from(if shift { shifted } else { plain }));
        }
        special(key)
    }
}

fn tap(out: &mut Vec<(u32, bool)>, c: char) {
    let keysym = char_keysym(c);
    out.push((keysym, true));
    out.push((keysym, false));
}

/// X11 keysym for a character: Latin-1 characters are their own keysym, everything else is
/// 0x01000000 + the Unicode code point.
fn char_keysym(c: char) -> u32 {
    let cp = u32::from(c);
    if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) { cp } else { 0x0100_0000 | cp }
}

/// The five dead keys of the Mac US layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Accent {
    Grave,
    Acute,
    Circumflex,
    Diaeresis,
    Tilde,
}

impl Accent {
    /// What the dead key types on its own (followed by Space or an unaccentable key).
    fn spacing(self) -> char {
        match self {
            Accent::Grave => '`',
            Accent::Acute => '\u{b4}',
            Accent::Circumflex => '\u{2c6}',
            Accent::Diaeresis => '\u{a8}',
            Accent::Tilde => '\u{2dc}',
        }
    }

    fn compose(self, base: char) -> Option<char> {
        let table: &[(char, char)] = match self {
            Accent::Grave => &[
                ('a', 'à'),
                ('e', 'è'),
                ('i', 'ì'),
                ('o', 'ò'),
                ('u', 'ù'),
                ('A', 'À'),
                ('E', 'È'),
                ('I', 'Ì'),
                ('O', 'Ò'),
                ('U', 'Ù'),
            ],
            Accent::Acute => &[
                ('a', 'á'),
                ('e', 'é'),
                ('i', 'í'),
                ('o', 'ó'),
                ('u', 'ú'),
                ('y', 'ý'),
                ('A', 'Á'),
                ('E', 'É'),
                ('I', 'Í'),
                ('O', 'Ó'),
                ('U', 'Ú'),
                ('Y', 'Ý'),
            ],
            Accent::Circumflex => &[
                ('a', 'â'),
                ('e', 'ê'),
                ('i', 'î'),
                ('o', 'ô'),
                ('u', 'û'),
                ('A', 'Â'),
                ('E', 'Ê'),
                ('I', 'Î'),
                ('O', 'Ô'),
                ('U', 'Û'),
            ],
            Accent::Diaeresis => &[
                ('a', 'ä'),
                ('e', 'ë'),
                ('i', 'ï'),
                ('o', 'ö'),
                ('u', 'ü'),
                ('y', 'ÿ'),
                ('A', 'Ä'),
                ('E', 'Ë'),
                ('I', 'Ï'),
                ('O', 'Ö'),
                ('U', 'Ü'),
                ('Y', 'Ÿ'),
            ],
            Accent::Tilde => &[('a', 'ã'), ('n', 'ñ'), ('o', 'õ'), ('A', 'Ã'), ('N', 'Ñ'), ('O', 'Õ')],
        };
        table.iter().find(|&&(b, _)| b == base).map(|&(_, c)| c)
    }
}

enum OptionOut {
    Char(char),
    Dead(Accent),
}

/// Option and Shift+Option characters of the Mac "U.S." keyboard layout.
fn option_layer(key: Key, shift: bool) -> Option<OptionOut> {
    use Key::*;
    use OptionOut::{Char as Ch, Dead as Dk};
    let (plain, shifted) = match key {
        Grave => (Dk(Accent::Grave), Ch('`')),
        Digit1 => (Ch('¡'), Ch('⁄')),
        Digit2 => (Ch('™'), Ch('€')),
        Digit3 => (Ch('£'), Ch('‹')),
        Digit4 => (Ch('¢'), Ch('›')),
        Digit5 => (Ch('∞'), Ch('ﬁ')),
        Digit6 => (Ch('§'), Ch('ﬂ')),
        Digit7 => (Ch('¶'), Ch('‡')),
        Digit8 => (Ch('•'), Ch('°')),
        Digit9 => (Ch('ª'), Ch('·')),
        Digit0 => (Ch('º'), Ch('‚')),
        Minus => (Ch('–'), Ch('—')),
        Equal => (Ch('≠'), Ch('±')),
        Q => (Ch('œ'), Ch('Œ')),
        W => (Ch('∑'), Ch('„')),
        E => (Dk(Accent::Acute), Ch('´')),
        R => (Ch('®'), Ch('‰')),
        T => (Ch('†'), Ch('ˇ')),
        Y => (Ch('¥'), Ch('Á')),
        U => (Dk(Accent::Diaeresis), Ch('¨')),
        I => (Dk(Accent::Circumflex), Ch('ˆ')),
        O => (Ch('ø'), Ch('Ø')),
        P => (Ch('π'), Ch('∏')),
        LeftBracket => (Ch('“'), Ch('”')),
        RightBracket => (Ch('‘'), Ch('’')),
        Backslash | IntlBackslash => (Ch('«'), Ch('»')),
        A => (Ch('å'), Ch('Å')),
        S => (Ch('ß'), Ch('Í')),
        D => (Ch('∂'), Ch('Î')),
        F => (Ch('ƒ'), Ch('Ï')),
        G => (Ch('©'), Ch('˝')),
        H => (Ch('˙'), Ch('Ó')),
        J => (Ch('∆'), Ch('Ô')),
        K => (Ch('˚'), Ch('\u{f8ff}')),
        L => (Ch('¬'), Ch('Ò')),
        Semicolon => (Ch('…'), Ch('Ú')),
        Quote => (Ch('æ'), Ch('Æ')),
        Z => (Ch('Ω'), Ch('¸')),
        X => (Ch('≈'), Ch('˛')),
        C => (Ch('ç'), Ch('Ç')),
        V => (Ch('√'), Ch('◊')),
        B => (Ch('∫'), Ch('ı')),
        N => (Dk(Accent::Tilde), Ch('˜')),
        M => (Ch('µ'), Ch('Â')),
        Comma => (Ch('≤'), Ch('¯')),
        Period => (Ch('≥'), Ch('˘')),
        Slash => (Ch('÷'), Ch('¿')),
        Space => (Ch('\u{a0}'), Ch('\u{a0}')),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
}

/// Measured on macOS 26.6.2 Screen Sharing: these four are the keysyms that produce
/// Shift, Control, Option and Command. Control_R produces nothing and is never sent.
fn modifier_keysym(m: HostMod) -> u32 {
    match m {
        HostMod::Shift => XK_SHIFT_L,
        HostMod::Control => XK_CONTROL_L,
        HostMod::Option => XK_META_L,
        HostMod::Command => XK_SUPER_L,
    }
}

fn letter(key: Key) -> Option<u8> {
    use Key::*;
    let c = match key {
        A => b'a',
        B => b'b',
        C => b'c',
        D => b'd',
        E => b'e',
        F => b'f',
        G => b'g',
        H => b'h',
        I => b'i',
        J => b'j',
        K => b'k',
        L => b'l',
        M => b'm',
        N => b'n',
        O => b'o',
        P => b'p',
        Q => b'q',
        R => b'r',
        S => b's',
        T => b't',
        U => b'u',
        V => b'v',
        W => b'w',
        X => b'x',
        Y => b'y',
        Z => b'z',
        _ => return None,
    };
    Some(c)
}

/// Unshifted and shifted characters of the non-letter printable keys on a US layout.
fn us_symbol(key: Key) -> Option<(u8, u8)> {
    use Key::*;
    let pair = match key {
        Digit1 => (b'1', b'!'),
        Digit2 => (b'2', b'@'),
        Digit3 => (b'3', b'#'),
        Digit4 => (b'4', b'$'),
        Digit5 => (b'5', b'%'),
        Digit6 => (b'6', b'^'),
        Digit7 => (b'7', b'&'),
        Digit8 => (b'8', b'*'),
        Digit9 => (b'9', b'('),
        Digit0 => (b'0', b')'),
        Grave => (b'`', b'~'),
        Minus => (b'-', b'_'),
        Equal => (b'=', b'+'),
        LeftBracket => (b'[', b'{'),
        RightBracket => (b']', b'}'),
        // The ISO key types the same as Backslash on a US Windows layout.
        Backslash | IntlBackslash => (b'\\', b'|'),
        Semicolon => (b';', b':'),
        Quote => (b'\'', b'"'),
        Comma => (b',', b'<'),
        Period => (b'.', b'>'),
        Slash => (b'/', b'?'),
        Space => (b' ', b' '),
        _ => return None,
    };
    Some(pair)
}

/// X11 keysyms (keysymdef.h, XF86keysym.h) for keys that do not type a character.
fn special(key: Key) -> Option<u32> {
    use Key::*;
    let keysym = match key {
        Backspace => 0xff08,
        Tab => 0xff09,
        Enter => 0xff0d,
        Pause => 0xff13,
        Escape => 0xff1b,
        Home => 0xff50,
        Left => 0xff51,
        Up => 0xff52,
        Right => 0xff53,
        Down => 0xff54,
        PageUp => 0xff55,
        PageDown => 0xff56,
        End => 0xff57,
        PrintScreen => 0xff61,
        Insert => 0xff63,
        Apps => 0xff67,
        NumpadEnter => 0xff8d,
        NumpadMultiply => 0xffaa,
        NumpadAdd => 0xffab,
        NumpadSubtract => 0xffad,
        NumpadDecimal => 0xffae,
        NumpadDivide => 0xffaf,
        Numpad0 => 0xffb0,
        Numpad1 => 0xffb1,
        Numpad2 => 0xffb2,
        Numpad3 => 0xffb3,
        Numpad4 => 0xffb4,
        Numpad5 => 0xffb5,
        Numpad6 => 0xffb6,
        Numpad7 => 0xffb7,
        Numpad8 => 0xffb8,
        Numpad9 => 0xffb9,
        F1 => 0xffbe,
        F2 => 0xffbf,
        F3 => 0xffc0,
        F4 => 0xffc1,
        F5 => 0xffc2,
        F6 => 0xffc3,
        F7 => 0xffc4,
        F8 => 0xffc5,
        F9 => 0xffc6,
        F10 => 0xffc7,
        F11 => 0xffc8,
        F12 => 0xffc9,
        F13 => 0xffca,
        F14 => 0xffcb,
        F15 => 0xffcc,
        F16 => 0xffcd,
        F17 => 0xffce,
        F18 => 0xffcf,
        F19 => 0xffd0,
        F20 => 0xffd1,
        F21 => 0xffd2,
        F22 => 0xffd3,
        F23 => 0xffd4,
        F24 => 0xffd5,
        Delete => 0xffff,
        VolumeDown => 0x1008_ff11,
        VolumeMute => 0x1008_ff12,
        VolumeUp => 0x1008_ff13,
        MediaPlayPause => 0x1008_ff14,
        MediaPrev => 0x1008_ff16,
        MediaNext => 0x1008_ff17,
        _ => return None,
    };
    Some(keysym)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(t: &mut Translator, key: Key) -> Vec<(u32, bool)> {
        t.translate(HostEvent::Key { key, down: true })
    }

    fn release(t: &mut Translator, key: Key) -> Vec<(u32, bool)> {
        t.translate(HostEvent::Key { key, down: false })
    }

    fn modifier(t: &mut Translator, m: HostMod, down: bool) -> Vec<(u32, bool)> {
        t.translate(HostEvent::Mod { m, down })
    }

    fn type_key(t: &mut Translator, key: Key) -> Vec<(u32, bool)> {
        let mut out = press(t, key);
        out.extend(release(t, key));
        out
    }

    fn typed(c: char) -> Vec<(u32, bool)> {
        let k = char_keysym(c);
        vec![(k, true), (k, false)]
    }

    #[test]
    fn option_layer_characters() {
        let mut t = Translator::default();
        modifier(&mut t, HostMod::Option, true);
        assert_eq!(type_key(&mut t, Key::Digit8), typed('•'));
        assert_eq!(type_key(&mut t, Key::Digit2), typed('™'));
        assert_eq!(char_keysym('™'), 0x0100_2122);
        assert_eq!(type_key(&mut t, Key::G), typed('©'));
        assert_eq!(char_keysym('©'), 0xa9, "Latin-1 characters are their own keysym");
        modifier(&mut t, HostMod::Shift, true);
        assert_eq!(type_key(&mut t, Key::Digit8), typed('°'));
        assert_eq!(type_key(&mut t, Key::Minus), typed('—'));
    }

    #[test]
    fn dead_keys_compose_with_the_next_letter() {
        let mut t = Translator::default();
        modifier(&mut t, HostMod::Option, true);
        assert_eq!(type_key(&mut t, Key::E), [], "Option+E is a dead key");
        modifier(&mut t, HostMod::Option, false);
        assert_eq!(type_key(&mut t, Key::E), typed('é'));
        modifier(&mut t, HostMod::Option, true);
        type_key(&mut t, Key::N);
        modifier(&mut t, HostMod::Option, false);
        modifier(&mut t, HostMod::Shift, true);
        assert_eq!(type_key(&mut t, Key::N), typed('Ñ'));
        modifier(&mut t, HostMod::Shift, false);
        modifier(&mut t, HostMod::Option, true);
        type_key(&mut t, Key::U);
        modifier(&mut t, HostMod::Option, false);
        assert_eq!(type_key(&mut t, Key::Space), typed('¨'), "dead key + Space types the accent");
        modifier(&mut t, HostMod::Option, true);
        type_key(&mut t, Key::I);
        modifier(&mut t, HostMod::Option, false);
        let mut expected = typed('ˆ');
        expected.extend([(0x78, true), (0x78, false)]);
        assert_eq!(type_key(&mut t, Key::X), expected, "unaccentable key: accent, then the key");
    }

    #[test]
    fn option_with_command_stays_a_shortcut() {
        let mut t = Translator::default();
        modifier(&mut t, HostMod::Option, true);
        modifier(&mut t, HostMod::Command, true);
        // Option+Cmd+8 toggles macOS Zoom: it must arrive as the 8 key, not a bullet.
        assert_eq!(type_key(&mut t, Key::Digit8), [(0x38, true), (0x38, false)]);
        assert_eq!(type_key(&mut t, Key::E), [(0x65, true), (0x65, false)]);
    }

    #[test]
    fn modifiers_use_measured_keysyms() {
        let mut t = Translator::default();
        assert_eq!(modifier(&mut t, HostMod::Command, true), [(0xffeb, true)]);
        assert_eq!(modifier(&mut t, HostMod::Option, true), [(0xffe7, true)]);
        assert_eq!(modifier(&mut t, HostMod::Control, true), [(0xffe3, true)]);
        assert_eq!(modifier(&mut t, HostMod::Shift, true), [(0xffe1, true)]);
        assert_eq!(modifier(&mut t, HostMod::Control, false), [(0xffe3, false)]);
    }

    #[test]
    fn letters_follow_shift_and_caps_lock() {
        let mut t = Translator::default();
        assert_eq!(press(&mut t, Key::A), [(0x61, true)]);
        assert_eq!(release(&mut t, Key::A), [(0x61, false)]);
        modifier(&mut t, HostMod::Shift, true);
        assert_eq!(press(&mut t, Key::A), [(0x41, true)]);
        release(&mut t, Key::A);
        t.set_caps_lock(true);
        assert_eq!(press(&mut t, Key::A), [(0x61, true)]);
        release(&mut t, Key::A);
        modifier(&mut t, HostMod::Shift, false);
        assert_eq!(press(&mut t, Key::A), [(0x41, true)]);
        release(&mut t, Key::A);
        // Caps Lock does not shift digits.
        assert_eq!(press(&mut t, Key::Digit1), [(u32::from(b'1'), true)]);
    }

    #[test]
    fn release_repeats_the_press_keysym() {
        let mut t = Translator::default();
        modifier(&mut t, HostMod::Shift, true);
        assert_eq!(press(&mut t, Key::Digit1), [(u32::from(b'!'), true)]);
        modifier(&mut t, HostMod::Shift, false);
        assert_eq!(release(&mut t, Key::Digit1), [(u32::from(b'!'), false)]);
        // A held key's auto-repeat re-sends its original keysym too.
        assert_eq!(press(&mut t, Key::Z), [(u32::from(b'z'), true)]);
        modifier(&mut t, HostMod::Shift, true);
        assert_eq!(press(&mut t, Key::Z), [(u32::from(b'z'), true)]);
        assert_eq!(release(&mut t, Key::Z), [(u32::from(b'z'), false)]);
    }

    #[test]
    fn us_symbols() {
        let mut t = Translator::default();
        for (key, plain, shifted) in [
            (Key::Grave, b'`', b'~'),
            (Key::Minus, b'-', b'_'),
            (Key::Equal, b'=', b'+'),
            (Key::LeftBracket, b'[', b'{'),
            (Key::RightBracket, b']', b'}'),
            (Key::Backslash, b'\\', b'|'),
            (Key::Semicolon, b';', b':'),
            (Key::Quote, b'\'', b'"'),
            (Key::Comma, b',', b'<'),
            (Key::Period, b'.', b'>'),
            (Key::Slash, b'/', b'?'),
            (Key::Digit0, b'0', b')'),
            (Key::Space, b' ', b' '),
        ] {
            assert_eq!(press(&mut t, key), [(u32::from(plain), true)], "{key:?}");
            release(&mut t, key);
            modifier(&mut t, HostMod::Shift, true);
            assert_eq!(press(&mut t, key), [(u32::from(shifted), true)], "{key:?} shifted");
            release(&mut t, key);
            modifier(&mut t, HostMod::Shift, false);
        }
    }

    #[test]
    fn special_keys() {
        let mut t = Translator::default();
        for (key, keysym) in [
            (Key::Enter, 0xff0d),
            (Key::NumpadEnter, 0xff8d),
            (Key::Backspace, 0xff08),
            (Key::Delete, 0xffff),
            (Key::Escape, 0xff1b),
            (Key::Tab, 0xff09),
            (Key::Insert, 0xff63),
            (Key::PageUp, 0xff55),
            (Key::PageDown, 0xff56),
            (Key::F1, 0xffbe),
            (Key::F12, 0xffc9),
            (Key::F24, 0xffd5),
            (Key::Numpad7, 0xffb7),
            (Key::NumpadDecimal, 0xffae),
            (Key::Apps, 0xff67),
            (Key::PrintScreen, 0xff61),
            (Key::VolumeUp, 0x1008_ff13),
            (Key::MediaPlayPause, 0x1008_ff14),
        ] {
            assert_eq!(press(&mut t, key), [(keysym, true)], "{key:?}");
            assert_eq!(release(&mut t, key), [(keysym, false)], "{key:?}");
        }
    }

    #[test]
    fn keys_without_keysyms_send_nothing() {
        let mut t = Translator::default();
        for key in [Key::CapsLock, Key::NumLock, Key::ScrollLock, Key::LShift, Key::RCtrl, Key::Other(0x0C)] {
            assert!(press(&mut t, key).is_empty(), "{key:?}");
            assert!(release(&mut t, key).is_empty(), "{key:?}");
        }
    }

    #[test]
    fn unmatched_release_still_releases() {
        let mut t = Translator::default();
        assert_eq!(release(&mut t, Key::B), [(u32::from(b'b'), false)]);
    }
}
