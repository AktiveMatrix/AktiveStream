//! The built-in PC-to-Mac keyboard profile: the macOS layout and key conversions a Windows
//! remote-desktop client uses, plus the Mac-native identity profile for the macOS client.

use crate::{
    Conversion, HostMod, HostMods, Key, LocalAction, LocalShortcut, MouseRules, PhysMods, Profile, SystemShortcutMode,
};

fn conversion(when: PhysMods, key: Key, send: HostMods, send_key: Key) -> Conversion {
    Conversion { when, key, send, send_key }
}

fn shortcut(when: PhysMods, key: Key, action: LocalAction) -> LocalShortcut {
    LocalShortcut { when, key, action }
}

pub(crate) fn mac_default() -> Profile {
    use Key::*;
    const CTRL: PhysMods = PhysMods::CTRL;
    const ALT: PhysMods = PhysMods::ALT;
    const SHIFT: PhysMods = PhysMods::SHIFT;
    const WIN: PhysMods = PhysMods::WIN;
    const CMD: HostMods = HostMods::COMMAND;
    const CONTROL: HostMods = HostMods::CONTROL;

    Profile {
        name: "macOS".to_string(),
        modifier_map: vec![
            (LCtrl, HostMod::Command),
            (RCtrl, HostMod::Control),
            (LAlt, HostMod::Option),
            (RAlt, HostMod::Option),
            (LWin, HostMod::Command),
            (RWin, HostMod::Command),
            (LShift, HostMod::Shift),
            (RShift, HostMod::Shift),
        ],
        // Same order as Jump's stored list.
        conversions: vec![
            conversion(CTRL | ALT, Tab, CMD, Tab),
            conversion(ALT, Left, CMD, Left),
            conversion(ALT, Right, CMD, Right),
            conversion(CTRL, Tab, CONTROL, Tab),
            conversion(CTRL | SHIFT, Tab, CONTROL | HostMods::SHIFT, Tab),
            conversion(ALT, F4, CMD, Q),
            conversion(CTRL, F4, CMD, W),
            conversion(CTRL, Y, CMD | HostMods::SHIFT, Z),
            conversion(CTRL | WIN, F, CONTROL | CMD, F),
            conversion(CTRL, Left, CONTROL, Left),
            conversion(CTRL, Right, CONTROL, Right),
            // Shadowed by the Toggle Windows Shortcuts shortcut, as in Jump.
            conversion(CTRL | ALT, Left, CONTROL, Left),
            conversion(CTRL | ALT, Right, CONTROL, Right),
        ],
        local_shortcuts: vec![
            shortcut(CTRL | ALT, F11, LocalAction::ToggleFullscreen),
            shortcut(CTRL | ALT, Left, LocalAction::ToggleSystemShortcuts),
            shortcut(CTRL | ALT, W, LocalAction::Disconnect),
            shortcut(CTRL | ALT, S, LocalAction::ToggleStats),
        ],
        system_shortcuts: SystemShortcutMode::InFullscreen,
        mouse: MouseRules { ctrl_click_is_right_click: true, ctrl_win_click_is_middle_click: true },
        // Windows Magnifier: zoom in, zoom out, quit.
        windows_chords: vec![(WIN, Equal), (WIN, NumpadAdd), (WIN, Minus), (WIN, NumpadSubtract), (WIN, Escape)],
    }
}

/// Identity profile for a Mac keyboard driving a remote Mac: Command, Option and Control pass
/// straight through (the Command key arrives in the Super/Win position), with no PC-style
/// conversions. Used by the macOS client.
pub(crate) fn mac_native() -> Profile {
    use Key::*;
    const CTRL: PhysMods = PhysMods::CTRL;
    const ALT: PhysMods = PhysMods::ALT;

    Profile {
        name: "macOS (native)".to_string(),
        modifier_map: vec![
            (LWin, HostMod::Command),
            (RWin, HostMod::Command),
            (LAlt, HostMod::Option),
            (RAlt, HostMod::Option),
            (LCtrl, HostMod::Control),
            (RCtrl, HostMod::Control),
            (LShift, HostMod::Shift),
            (RShift, HostMod::Shift),
        ],
        // A Mac keyboard already matches the Mac, so nothing is rewritten.
        conversions: vec![],
        // Client-side chords use Control+Option so they do not collide with the remote Mac's
        // Command shortcuts; the top-edge toolbar offers the same actions by pointer.
        local_shortcuts: vec![
            shortcut(CTRL | ALT, F11, LocalAction::ToggleFullscreen),
            shortcut(CTRL | ALT, W, LocalAction::Disconnect),
            shortcut(CTRL | ALT, S, LocalAction::ToggleStats),
        ],
        system_shortcuts: SystemShortcutMode::InFullscreen,
        // Ctrl-click is the Mac's own secondary-click convention.
        mouse: MouseRules { ctrl_click_is_right_click: true, ctrl_win_click_is_middle_click: false },
        windows_chords: vec![],
    }
}

/// A PC keyboard driving a generic PC or Linux VNC server (wayvnc, TigerVNC, x11vnc, ...).
/// Ctrl is Ctrl, Alt is Alt and the Win key is Super, so nothing is rewritten and the remote
/// desktop handles its own shortcuts. Pairs with [`crate::RfbTranslator::generic`].
pub(crate) fn pc_generic() -> Profile {
    use Key::*;
    const CTRL: PhysMods = PhysMods::CTRL;
    const ALT: PhysMods = PhysMods::ALT;
    const WIN: PhysMods = PhysMods::WIN;

    Profile {
        name: "PC / Linux".to_string(),
        modifier_map: vec![
            (LCtrl, HostMod::Control),
            (RCtrl, HostMod::Control),
            (LAlt, HostMod::Option),
            (RAlt, HostMod::Option),
            (LWin, HostMod::Command),
            (RWin, HostMod::Command),
            (LShift, HostMod::Shift),
            (RShift, HostMod::Shift),
        ],
        // A PC keyboard already matches a PC/Linux target; the remote desktop owns its shortcuts.
        conversions: vec![],
        // Only the client's own escape hatches, as on the Mac-native profile; the toolbar offers
        // the same actions. Ctrl+Alt+Left is left for the remote (Linux workspace switching).
        local_shortcuts: vec![
            shortcut(CTRL | ALT, F11, LocalAction::ToggleFullscreen),
            shortcut(CTRL | ALT, W, LocalAction::Disconnect),
            shortcut(CTRL | ALT, S, LocalAction::ToggleStats),
        ],
        system_shortcuts: SystemShortcutMode::InFullscreen,
        // A PC/Linux target has a real right button; Ctrl+click is just Ctrl+click.
        mouse: MouseRules { ctrl_click_is_right_click: false, ctrl_win_click_is_middle_click: false },
        // Keep the local Windows Magnifier chords on the PC so magnification still works.
        windows_chords: vec![(WIN, Equal), (WIN, NumpadAdd), (WIN, Minus), (WIN, NumpadSubtract), (WIN, Escape)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_native_is_identity() {
        let p = mac_native();
        assert!(p.conversions.is_empty(), "a Mac keyboard needs no PC-style rewrites");
        assert!(p.windows_chords.is_empty(), "no Windows Magnifier chords on a Mac");
        let modifier = |k: Key| p.modifier_map.iter().find(|(key, _)| *key == k).map(|(_, m)| *m);
        assert_eq!(modifier(Key::LWin), Some(HostMod::Command), "Command passes through");
        assert_eq!(modifier(Key::LAlt), Some(HostMod::Option), "Option passes through");
        assert_eq!(modifier(Key::LCtrl), Some(HostMod::Control), "Control passes through");
        assert_eq!(modifier(Key::RWin), Some(HostMod::Command));
    }

    #[test]
    fn pc_generic_maps_ctrl_to_control_not_command() {
        let p = pc_generic();
        assert!(p.conversions.is_empty(), "a PC keyboard needs no rewrites for a PC/Linux target");
        let modifier = |k: Key| p.modifier_map.iter().find(|(key, _)| *key == k).map(|(_, m)| *m);
        // The key Jump's macOS profile turns into Command stays Control here.
        assert_eq!(modifier(Key::LCtrl), Some(HostMod::Control), "Left Ctrl is Control, not Command");
        assert_eq!(modifier(Key::LAlt), Some(HostMod::Option), "Alt is Option (Alt_L on the wire)");
        assert_eq!(modifier(Key::LWin), Some(HostMod::Command), "Win is Command (Super on the wire)");
        assert!(!p.mouse.ctrl_click_is_right_click, "Ctrl+click is not a right click on a PC/Linux target");
        assert!(!p.windows_chords.is_empty(), "local Magnifier chords are kept");
    }
}
