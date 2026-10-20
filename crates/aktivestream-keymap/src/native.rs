//! Native Mac input translation (Phase 2): turns [`HostEvent`]s into Carbon virtual keycodes
//! plus CGEventFlags, for the host agent to inject with `CGEvent`. The counterpart of
//! [`crate::RfbTranslator`], which produces RFB key symbols instead.

use std::collections::HashMap;

use crate::{HostEvent, HostMod, Key};

/// CGEventFlags modifier bits.
pub mod cg {
    pub const SHIFT: u32 = 0x0002_0000;
    pub const CONTROL: u32 = 0x0004_0000;
    pub const OPTION: u32 = 0x0008_0000;
    pub const COMMAND: u32 = 0x0010_0000;
}

/// One native keyboard event: a Carbon keycode, a direction, and the modifier flags in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeKey {
    pub keycode: u16,
    pub down: bool,
    pub flags: u32,
}

fn cg_flag(m: HostMod) -> u32 {
    match m {
        HostMod::Shift => cg::SHIFT,
        HostMod::Control => cg::CONTROL,
        HostMod::Option => cg::OPTION,
        HostMod::Command => cg::COMMAND,
    }
}

/// Produces native keyboard events from host events, tracking the current modifier flags so
/// every event carries the full modifier state (which is how CGEvent injection expects it).
#[derive(Debug, Default)]
pub struct NativeInput {
    flags: u32,
    /// Keycode each held key's press used, so its release matches even if flags changed.
    held: HashMap<Key, u16>,
}

impl NativeInput {
    pub fn new() -> Self {
        Self::default()
    }

    /// The modifier flags currently held (CGEventFlags bits), e.g. to stamp on a pointer event.
    pub fn flags(&self) -> u32 {
        self.flags
    }

    pub fn translate(&mut self, event: HostEvent) -> Option<NativeKey> {
        match event {
            HostEvent::Mod { m, down } => {
                let bit = cg_flag(m);
                if down {
                    self.flags |= bit;
                } else {
                    self.flags &= !bit;
                }
                Some(NativeKey { keycode: m.mac_virtual_keycode(), down, flags: self.flags })
            }
            HostEvent::Key { key, down: true } => {
                if let Some(&keycode) = self.held.get(&key) {
                    return Some(NativeKey { keycode, down: true, flags: self.flags });
                }
                let keycode = key.mac_virtual_keycode()?;
                self.held.insert(key, keycode);
                Some(NativeKey { keycode, down: true, flags: self.flags })
            }
            HostEvent::Key { key, down: false } => {
                let keycode = self.held.remove(&key).or_else(|| key.mac_virtual_keycode())?;
                Some(NativeKey { keycode, down: false, flags: self.flags })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_set_and_clear_flags() {
        let mut n = NativeInput::new();
        let e = n.translate(HostEvent::Mod { m: HostMod::Command, down: true }).unwrap();
        assert_eq!((e.keycode, e.down, e.flags), (0x37, true, cg::COMMAND));
        // A key pressed while Command is held carries the Command flag.
        let c = n.translate(HostEvent::Key { key: Key::C, down: true }).unwrap();
        assert_eq!(c.flags, cg::COMMAND);
        assert_eq!(c.keycode, Key::C.mac_virtual_keycode().unwrap());
        let up = n.translate(HostEvent::Mod { m: HostMod::Command, down: false }).unwrap();
        assert_eq!(up.flags, 0);
    }

    #[test]
    fn key_release_uses_the_press_keycode() {
        let mut n = NativeInput::new();
        let down = n.translate(HostEvent::Key { key: Key::A, down: true }).unwrap();
        let up = n.translate(HostEvent::Key { key: Key::A, down: false }).unwrap();
        assert_eq!(down.keycode, up.keycode);
        assert!(down.down && !up.down);
    }
}
