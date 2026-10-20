//! The built-in PC-to-Mac keyboard profile: base modifier mapping, key conversions,
//! client shortcuts and mouse rules. See docs/keyboard-profile.md.

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
        // Conversion order.
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
            // Shadowed by the Toggle Windows Shortcuts client shortcut.
            conversion(CTRL | ALT, Left, CONTROL, Left),
            conversion(CTRL | ALT, Right, CONTROL, Right),
        ],
        local_shortcuts: vec![
            shortcut(CTRL | ALT, F11, LocalAction::ToggleFullscreen),
            shortcut(CTRL | ALT, Left, LocalAction::ToggleSystemShortcuts),
            shortcut(CTRL | ALT, W, LocalAction::Disconnect),
        ],
        system_shortcuts: SystemShortcutMode::InFullscreen,
        mouse: MouseRules { ctrl_click_is_right_click: true, ctrl_win_click_is_middle_click: true },
        // Windows Magnifier: zoom in, zoom out, quit.
        windows_chords: vec![(WIN, Equal), (WIN, NumpadAdd), (WIN, Minus), (WIN, NumpadSubtract), (WIN, Escape)],
    }
}
