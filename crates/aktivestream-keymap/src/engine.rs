//! Profile engine state: physical keys in, host key and modifier transitions out.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Disposition, HostEvent, HostMod, HostMods, Key, KeyInput, LocalAction, MouseButton, Output, PhysMods, Profile,
    SystemShortcutMode,
};

/// An accepted physical modifier: the host modifier it maps to, and what its press told
/// Windows so its release tells Windows the same.
#[derive(Debug, Clone, Copy)]
struct HeldModifier {
    host: HostMod,
    disposition: Disposition,
}

/// A left click rewritten to another button by a mouse rule.
#[derive(Debug)]
struct RewrittenClick {
    physical: MouseButton,
    sent: MouseButton,
    /// Modifier keys whose host modifiers are released for the click.
    triggers: BTreeSet<Key>,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    fullscreen: bool,
    /// Flipped by ToggleSystemShortcuts, cleared when full-screen state changes.
    toggled: bool,
    /// Physical modifier keys held that map to host modifiers.
    modifiers: BTreeMap<Key, HeldModifier>,
    /// Physical modifier keys held that were handed to Windows and reach nothing on the host
    /// (Win keys while system shortcuts stay local).
    passed_modifiers: BTreeSet<Key>,
    /// Non-modifier keys handed to Windows; their releases go to Windows too.
    passed_keys: BTreeSet<Key>,
    /// Keys whose press ran a local shortcut.
    local_keys: BTreeSet<Key>,
    /// Keys of Windows chords handed to Windows; auto-repeat replays them again.
    chord_keys: BTreeSet<Key>,
    /// Non-modifier keys held on the host, with the host key their press sent.
    held: BTreeMap<Key, Key>,
    /// Modifiers currently down on the host.
    host_mods: BTreeSet<HostMod>,
    /// Modifiers forced by the active conversion.
    override_mods: Option<HostMods>,
    click: Option<RewrittenClick>,
}

fn output(disposition: Disposition, host: Vec<HostEvent>) -> Output {
    Output { disposition, host, local: None }
}

fn phys_flag(key: Key) -> PhysMods {
    match key {
        Key::LShift | Key::RShift => PhysMods::SHIFT,
        Key::LCtrl | Key::RCtrl => PhysMods::CTRL,
        Key::LAlt | Key::RAlt => PhysMods::ALT,
        Key::LWin | Key::RWin => PhysMods::WIN,
        _ => PhysMods::empty(),
    }
}

fn host_mod_set(mods: HostMods) -> BTreeSet<HostMod> {
    [HostMod::Shift, HostMod::Control, HostMod::Option, HostMod::Command]
        .into_iter()
        .filter(|m| mods.contains(m.flag()))
        .collect()
}

impl State {
    pub(crate) fn set_fullscreen(&mut self, fullscreen: bool) {
        if fullscreen != self.fullscreen {
            self.toggled = false;
        }
        self.fullscreen = fullscreen;
    }

    pub(crate) fn system_shortcuts_to_host(&self, profile: &Profile) -> bool {
        let by_mode = match profile.system_shortcuts {
            SystemShortcutMode::DontSend => false,
            SystemShortcutMode::OnConnect => true,
            SystemShortcutMode::InFullscreen => self.fullscreen,
        };
        by_mode != self.toggled
    }

    pub(crate) fn handle_key(&mut self, profile: &Profile, input: KeyInput) -> Output {
        let key = input.key;
        if key.is_lock() || matches!(key, Key::Other(_)) {
            return output(Disposition::PassToOs, Vec::new());
        }
        if key.is_modifier() {
            return self.modifier(profile, key, input.down);
        }
        if input.down { self.key_down(profile, key) } else { self.key_up(key) }
    }

    fn modifier(&mut self, profile: &Profile, key: Key, down: bool) -> Output {
        let mut host = Vec::new();
        if down {
            // Auto-repeat of a held modifier: nothing changes.
            if let Some(held) = self.modifiers.get(&key) {
                return output(held.disposition, host);
            }
            if self.passed_modifiers.contains(&key) {
                return output(Disposition::PassToOs, host);
            }
            let to_host = self.system_shortcuts_to_host(profile);
            let mapped = profile.modifier_map.iter().find(|(k, _)| *k == key).map(|&(_, m)| m);
            self.override_mods = None;
            let disposition = match mapped {
                Some(host_mod) if to_host || !matches!(key, Key::LWin | Key::RWin) => {
                    // Shift, Ctrl and Alt also reach Windows while system shortcuts stay
                    // local, so Alt+Tab and Ctrl+Esc still work on the PC.
                    let disposition = if to_host { Disposition::Consume } else { Disposition::PassToOs };
                    self.modifiers.insert(key, HeldModifier { host: host_mod, disposition });
                    disposition
                }
                _ => {
                    self.passed_modifiers.insert(key);
                    Disposition::PassToOs
                }
            };
            self.reconcile(&mut host);
            output(disposition, host)
        } else {
            let disposition = if let Some(held) = self.modifiers.remove(&key) {
                held.disposition
            } else if self.passed_modifiers.remove(&key) {
                Disposition::PassToOs
            } else {
                return output(Disposition::PassToOs, host);
            };
            self.override_mods = None;
            self.reconcile(&mut host);
            output(disposition, host)
        }
    }

    fn key_down(&mut self, profile: &Profile, key: Key) -> Output {
        if self.passed_keys.contains(&key) {
            return output(Disposition::PassToOs, Vec::new());
        }
        if self.local_keys.contains(&key) {
            return output(Disposition::Consume, Vec::new());
        }
        if self.chord_keys.contains(&key) {
            return Output {
                disposition: Disposition::Consume,
                host: Vec::new(),
                local: Some(LocalAction::PassChordToWindows),
            };
        }
        // Auto-repeat re-sends whatever the original press sent.
        if let Some(&sent) = self.held.get(&key) {
            return output(Disposition::Consume, vec![HostEvent::Key { key: sent, down: true }]);
        }
        let mods = self.phys_mods();
        if !self.system_shortcuts_to_host(profile) && self.is_system_shortcut(key, mods) {
            self.passed_keys.insert(key);
            return output(Disposition::PassToOs, Vec::new());
        }
        if let Some(shortcut) = profile.local_shortcuts.iter().find(|s| s.when == mods && s.key == key) {
            self.local_keys.insert(key);
            if shortcut.action == LocalAction::ToggleSystemShortcuts {
                self.toggled = !self.toggled;
            }
            return Output { disposition: Disposition::Consume, host: Vec::new(), local: Some(shortcut.action) };
        }
        // A Windows chord (Magnifier) while the Win key is going to the host: drop the Win
        // key's Command on the host and have the platform replay Win + key to Windows.
        if profile.windows_chords.iter().any(|&(when, k)| when == mods && k == key) {
            self.chord_keys.insert(key);
            let without_win: HostMods = self
                .modifiers
                .iter()
                .filter(|(k, _)| !matches!(k, Key::LWin | Key::RWin))
                .fold(HostMods::empty(), |acc, (_, held)| acc | held.host.flag());
            self.override_mods = Some(without_win);
            let mut host = Vec::new();
            self.reconcile(&mut host);
            return Output { disposition: Disposition::Consume, host, local: Some(LocalAction::PassChordToWindows) };
        }
        let sent = match profile.conversions.iter().find(|c| c.when == mods && c.key == key) {
            Some(conversion) => {
                self.override_mods = Some(conversion.send);
                conversion.send_key
            }
            None => {
                self.override_mods = None;
                key
            }
        };
        let mut host = Vec::new();
        self.reconcile(&mut host);
        self.held.insert(key, sent);
        host.push(HostEvent::Key { key: sent, down: true });
        output(Disposition::Consume, host)
    }

    fn key_up(&mut self, key: Key) -> Output {
        if self.passed_keys.remove(&key) {
            return output(Disposition::PassToOs, Vec::new());
        }
        if self.local_keys.remove(&key) || self.chord_keys.remove(&key) {
            return output(Disposition::Consume, Vec::new());
        }
        let Some(sent) = self.held.remove(&key) else {
            // A release whose press Windows saw before this window had focus.
            return output(Disposition::PassToOs, Vec::new());
        };
        // Another physical key may still hold the same host key (a conversion target).
        let host = if self.held.values().any(|&k| k == sent) {
            Vec::new()
        } else {
            vec![HostEvent::Key { key: sent, down: false }]
        };
        output(Disposition::Consume, host)
    }

    /// Alt+Tab, Alt+Shift+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc, and anything pressed
    /// while a Win key that went to Windows is held.
    fn is_system_shortcut(&self, key: Key, mods: PhysMods) -> bool {
        if self.passed_modifiers.iter().any(|k| matches!(k, Key::LWin | Key::RWin)) {
            return true;
        }
        let alt = mods == PhysMods::ALT || mods == PhysMods::ALT | PhysMods::SHIFT;
        let ctrl = mods == PhysMods::CTRL || mods == PhysMods::CTRL | PhysMods::SHIFT;
        match key {
            Key::Tab => alt,
            Key::Escape => alt || ctrl,
            _ => false,
        }
    }

    fn phys_mods(&self) -> PhysMods {
        self.modifiers.keys().fold(PhysMods::empty(), |acc, &k| acc | phys_flag(k))
    }

    fn desired(&self) -> BTreeSet<HostMod> {
        if let Some(mods) = self.override_mods {
            return host_mod_set(mods);
        }
        self.modifiers
            .iter()
            .filter(|(key, _)| self.click.as_ref().is_none_or(|click| !click.triggers.contains(*key)))
            .map(|(_, held)| held.host)
            .collect()
    }

    /// Brings host modifiers to the desired set: releases first, then presses.
    fn reconcile(&mut self, host: &mut Vec<HostEvent>) {
        let want = self.desired();
        let release: Vec<HostMod> = self.host_mods.difference(&want).copied().collect();
        for m in release {
            self.host_mods.remove(&m);
            host.push(HostEvent::Mod { m, down: false });
        }
        let press: Vec<HostMod> = want.difference(&self.host_mods).copied().collect();
        for m in press {
            self.host_mods.insert(m);
            host.push(HostEvent::Mod { m, down: true });
        }
    }

    pub(crate) fn mouse_button(
        &mut self,
        profile: &Profile,
        button: MouseButton,
        down: bool,
    ) -> (Vec<HostEvent>, MouseButton) {
        let mut host = Vec::new();
        if !down {
            return match self.click.take_if(|click| click.physical == button) {
                Some(click) => {
                    self.reconcile(&mut host);
                    (host, click.sent)
                }
                None => (host, button),
            };
        }
        if button != MouseButton::Left || self.click.is_some() {
            self.reconcile(&mut host);
            return (host, button);
        }
        let held: BTreeSet<Key> = self.modifiers.keys().chain(&self.passed_modifiers).copied().collect();
        let mods = held.iter().fold(PhysMods::empty(), |acc, &k| acc | phys_flag(k));
        let rules = profile.mouse;
        let sent = if rules.ctrl_win_click_is_middle_click && mods == PhysMods::CTRL | PhysMods::WIN {
            MouseButton::Middle
        } else if rules.ctrl_click_is_right_click && mods == PhysMods::CTRL {
            MouseButton::Right
        } else {
            self.reconcile(&mut host);
            return (host, button);
        };
        self.override_mods = None;
        self.click = Some(RewrittenClick { physical: button, sent, triggers: held });
        self.reconcile(&mut host);
        (host, sent)
    }

    pub(crate) fn release_all(&mut self) -> Vec<HostEvent> {
        let mut host = Vec::new();
        let mut released = BTreeSet::new();
        for sent in std::mem::take(&mut self.held).into_values() {
            if released.insert(sent) {
                host.push(HostEvent::Key { key: sent, down: false });
            }
        }
        for m in std::mem::take(&mut self.host_mods) {
            host.push(HostEvent::Mod { m, down: false });
        }
        self.modifiers.clear();
        self.passed_modifiers.clear();
        self.passed_keys.clear();
        self.local_keys.clear();
        self.chord_keys.clear();
        self.override_mods = None;
        self.click = None;
        host
    }
}

#[cfg(test)]
mod tests {
    use crate::{Disposition::*, Engine, HostEvent, HostMod::*, Key, KeyInput, LocalAction, MouseButton, Profile};
    use crate::{Output, SystemShortcutMode};

    fn engine() -> Engine {
        Engine::new(Profile::mac_default())
    }

    fn engine_with(mode: SystemShortcutMode) -> Engine {
        let mut profile = Profile::mac_default();
        profile.system_shortcuts = mode;
        Engine::new(profile)
    }

    fn down(e: &mut Engine, key: Key) -> Output {
        e.handle_key(KeyInput { key, down: true })
    }

    fn up(e: &mut Engine, key: Key) -> Output {
        e.handle_key(KeyInput { key, down: false })
    }

    fn md(m: crate::HostMod) -> HostEvent {
        HostEvent::Mod { m, down: true }
    }

    fn mu(m: crate::HostMod) -> HostEvent {
        HostEvent::Mod { m, down: false }
    }

    fn kd(key: Key) -> HostEvent {
        HostEvent::Key { key, down: true }
    }

    fn ku(key: Key) -> HostEvent {
        HostEvent::Key { key, down: false }
    }

    #[track_caller]
    fn host(out: Output, expected: &[HostEvent]) {
        assert_eq!(out.host, expected, "{out:?}");
        assert_eq!(out.local, None);
    }

    #[test]
    fn base_mapping() {
        let mut e = engine();
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::C), &[kd(Key::C)]);
        host(up(&mut e, Key::C), &[ku(Key::C)]);
        host(up(&mut e, Key::LCtrl), &[mu(Command)]);

        host(down(&mut e, Key::RCtrl), &[md(Control)]);
        host(down(&mut e, Key::C), &[kd(Key::C)]);
        host(up(&mut e, Key::C), &[ku(Key::C)]);
        host(up(&mut e, Key::RCtrl), &[mu(Control)]);

        for (key, m) in [(Key::LAlt, Option), (Key::RAlt, Option), (Key::LShift, Shift), (Key::RShift, Shift)] {
            host(down(&mut e, key), &[md(m)]);
            host(up(&mut e, key), &[mu(m)]);
        }
    }

    #[test]
    fn win_keys_are_command_when_system_shortcuts_go_to_host() {
        let mut e = engine();
        e.set_fullscreen(true);
        for key in [Key::LWin, Key::RWin] {
            let out = down(&mut e, key);
            assert_eq!(out.disposition, Consume);
            host(out, &[md(Command)]);
            host(up(&mut e, key), &[mu(Command)]);
        }
    }

    #[test]
    fn shared_host_modifier_releases_after_last_contributor() {
        let mut e = engine();
        e.set_fullscreen(true);
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::LWin), &[]);
        host(up(&mut e, Key::LCtrl), &[]);
        host(up(&mut e, Key::LWin), &[mu(Command)]);
    }

    #[test]
    fn modifier_dispositions() {
        let mut e = engine();
        // Windowed: system shortcuts stay local, so Shift/Ctrl/Alt also reach Windows.
        assert_eq!(down(&mut e, Key::LCtrl).disposition, PassToOs);
        e.set_fullscreen(true);
        // The release tells Windows what the press told it.
        let out = up(&mut e, Key::LCtrl);
        assert_eq!(out.disposition, PassToOs);
        host(out, &[mu(Command)]);
        assert_eq!(down(&mut e, Key::LAlt).disposition, Consume);
        assert_eq!(up(&mut e, Key::LAlt).disposition, Consume);
        assert_eq!(down(&mut e, Key::A).disposition, Consume);
    }

    #[test]
    fn conversion_ctrl_alt_tab_cycles_apps() {
        let mut e = engine();
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::LAlt), &[md(Option)]);
        host(down(&mut e, Key::Tab), &[mu(Option), kd(Key::Tab)]);
        host(up(&mut e, Key::Tab), &[ku(Key::Tab)]);
        host(down(&mut e, Key::Tab), &[kd(Key::Tab)]);
        host(up(&mut e, Key::Tab), &[ku(Key::Tab)]);
        host(up(&mut e, Key::LAlt), &[]);
        host(up(&mut e, Key::LCtrl), &[mu(Command)]);
    }

    #[test]
    fn conversion_alt_arrows() {
        let mut e = engine();
        for key in [Key::Left, Key::Right] {
            host(down(&mut e, Key::LAlt), &[md(Option)]);
            host(down(&mut e, key), &[mu(Option), md(Command), kd(key)]);
            host(up(&mut e, key), &[ku(key)]);
            host(up(&mut e, Key::LAlt), &[mu(Command)]);
        }
    }

    #[test]
    fn conversion_ctrl_tab_and_ctrl_shift_tab_keep_control() {
        let mut e = engine();
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::Tab), &[mu(Command), md(Control), kd(Key::Tab)]);
        host(up(&mut e, Key::Tab), &[ku(Key::Tab)]);
        host(down(&mut e, Key::LShift), &[mu(Control), md(Shift), md(Command)]);
        host(down(&mut e, Key::Tab), &[mu(Command), md(Control), kd(Key::Tab)]);
        host(up(&mut e, Key::Tab), &[ku(Key::Tab)]);
        host(up(&mut e, Key::LShift), &[mu(Shift), mu(Control), md(Command)]);
        host(up(&mut e, Key::LCtrl), &[mu(Command)]);
    }

    #[test]
    fn conversion_alt_f4_quits() {
        let mut e = engine();
        host(down(&mut e, Key::LAlt), &[md(Option)]);
        host(down(&mut e, Key::F4), &[mu(Option), md(Command), kd(Key::Q)]);
        host(up(&mut e, Key::F4), &[ku(Key::Q)]);
        host(up(&mut e, Key::LAlt), &[mu(Command)]);
    }

    #[test]
    fn conversion_ctrl_f4_closes() {
        let mut e = engine();
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::F4), &[kd(Key::W)]);
        host(up(&mut e, Key::F4), &[ku(Key::W)]);
        host(up(&mut e, Key::LCtrl), &[mu(Command)]);
    }

    #[test]
    fn conversion_ctrl_y_redoes() {
        let mut e = engine();
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::Y), &[md(Shift), kd(Key::Z)]);
        host(up(&mut e, Key::Y), &[ku(Key::Z)]);
        host(up(&mut e, Key::LCtrl), &[mu(Shift), mu(Command)]);

        // Either Ctrl matches.
        host(down(&mut e, Key::RCtrl), &[md(Control)]);
        host(down(&mut e, Key::Y), &[mu(Control), md(Shift), md(Command), kd(Key::Z)]);
        host(up(&mut e, Key::Y), &[ku(Key::Z)]);
        host(up(&mut e, Key::RCtrl), &[mu(Shift), mu(Command)]);
    }

    #[test]
    fn conversion_ctrl_win_f_full_screen() {
        let mut e = engine();
        e.set_fullscreen(true);
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::LWin), &[]);
        host(down(&mut e, Key::F), &[md(Control), kd(Key::F)]);
        host(up(&mut e, Key::F), &[ku(Key::F)]);
        host(up(&mut e, Key::LWin), &[mu(Control)]);
        host(up(&mut e, Key::LCtrl), &[mu(Command)]);
    }

    #[test]
    fn conversion_ctrl_arrows_switch_spaces() {
        let mut e = engine();
        for key in [Key::Left, Key::Right] {
            host(down(&mut e, Key::LCtrl), &[md(Command)]);
            host(down(&mut e, key), &[mu(Command), md(Control), kd(key)]);
            host(up(&mut e, key), &[ku(key)]);
            host(up(&mut e, Key::LCtrl), &[mu(Control)]);
        }
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::LAlt), &[md(Option)]);
        host(down(&mut e, Key::Right), &[mu(Option), mu(Command), md(Control), kd(Key::Right)]);
    }

    #[test]
    fn ctrl_alt_left_is_the_local_toggle_not_the_conversion() {
        let mut e = engine();
        down(&mut e, Key::LCtrl);
        down(&mut e, Key::LAlt);
        let out = down(&mut e, Key::Left);
        assert_eq!(out.local, Some(LocalAction::ToggleSystemShortcuts));
        assert!(out.host.is_empty());
        assert_eq!(out.disposition, Consume);
        host(up(&mut e, Key::Left), &[]);
    }

    #[test]
    fn local_shortcuts() {
        let mut e = engine();
        down(&mut e, Key::LCtrl);
        down(&mut e, Key::RAlt);
        let out = down(&mut e, Key::F11);
        assert_eq!(out.local, Some(LocalAction::ToggleFullscreen));
        assert!(out.host.is_empty());
        // Its auto-repeat and release stay local too.
        host(down(&mut e, Key::F11), &[]);
        host(up(&mut e, Key::F11), &[]);
        let out = down(&mut e, Key::W);
        assert_eq!(out.local, Some(LocalAction::Disconnect));
        assert!(out.host.is_empty());
    }

    #[test]
    fn conversion_override_ends_on_a_different_key() {
        let mut e = engine();
        down(&mut e, Key::LCtrl);
        host(down(&mut e, Key::Y), &[md(Shift), kd(Key::Z)]);
        host(up(&mut e, Key::Y), &[ku(Key::Z)]);
        host(down(&mut e, Key::X), &[mu(Shift), kd(Key::X)]);
    }

    #[test]
    fn conversion_override_ends_on_a_modifier_change() {
        let mut e = engine();
        down(&mut e, Key::LCtrl);
        host(down(&mut e, Key::Tab), &[mu(Command), md(Control), kd(Key::Tab)]);
        // Tab is still held on the host while the modifiers change.
        host(down(&mut e, Key::LAlt), &[mu(Control), md(Option), md(Command)]);
        host(up(&mut e, Key::Tab), &[ku(Key::Tab)]);
    }

    #[test]
    fn auto_repeat() {
        let mut e = engine();
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        host(down(&mut e, Key::LCtrl), &[]);
        host(down(&mut e, Key::C), &[kd(Key::C)]);
        host(down(&mut e, Key::C), &[kd(Key::C)]);
        host(up(&mut e, Key::C), &[ku(Key::C)]);
        host(down(&mut e, Key::F4), &[kd(Key::W)]);
        host(down(&mut e, Key::F4), &[kd(Key::W)]);
        host(up(&mut e, Key::F4), &[ku(Key::W)]);
        // Repeat while a conversion is active does not end it.
        host(down(&mut e, Key::Y), &[md(Shift), kd(Key::Z)]);
        host(down(&mut e, Key::LCtrl), &[]);
        host(down(&mut e, Key::Y), &[kd(Key::Z)]);
        host(up(&mut e, Key::Y), &[ku(Key::Z)]);
    }

    #[test]
    fn shared_host_key_releases_after_last_holder() {
        let mut e = engine();
        down(&mut e, Key::LCtrl);
        host(down(&mut e, Key::F4), &[kd(Key::W)]);
        host(down(&mut e, Key::W), &[kd(Key::W)]);
        host(up(&mut e, Key::F4), &[]);
        host(up(&mut e, Key::W), &[ku(Key::W)]);
    }

    #[test]
    fn windowed_system_shortcuts_stay_on_the_pc() {
        let mut e = engine();
        assert!(!e.system_shortcuts_to_host());
        let out = down(&mut e, Key::LWin);
        assert_eq!(out.disposition, PassToOs);
        host(out, &[]);
        let out = down(&mut e, Key::E);
        assert_eq!(out.disposition, PassToOs);
        host(out, &[]);
        assert_eq!(up(&mut e, Key::E).disposition, PassToOs);
        let out = up(&mut e, Key::LWin);
        assert_eq!(out.disposition, PassToOs);
        host(out, &[]);

        host(down(&mut e, Key::LAlt), &[md(Option)]);
        let out = down(&mut e, Key::Tab);
        assert_eq!(out.disposition, PassToOs);
        host(out, &[]);
        assert_eq!(up(&mut e, Key::Tab).disposition, PassToOs);
        host(down(&mut e, Key::LShift), &[md(Shift)]);
        assert_eq!(down(&mut e, Key::Tab).disposition, PassToOs);
        up(&mut e, Key::Tab);
        assert_eq!(down(&mut e, Key::Escape).disposition, PassToOs);
        up(&mut e, Key::Escape);
        up(&mut e, Key::LShift);
        up(&mut e, Key::LAlt);

        down(&mut e, Key::LCtrl);
        let out = down(&mut e, Key::Escape);
        assert_eq!(out.disposition, PassToOs);
        host(out, &[]);
        up(&mut e, Key::Escape);
        // Ctrl+Alt+Tab is a conversion, not a system shortcut.
        down(&mut e, Key::LAlt);
        let out = down(&mut e, Key::Tab);
        assert_eq!(out.disposition, Consume);
        host(out, &[mu(Option), kd(Key::Tab)]);
    }

    #[test]
    fn fullscreen_system_shortcuts_go_to_the_mac() {
        let mut e = engine();
        e.set_fullscreen(true);
        assert!(e.system_shortcuts_to_host());
        host(down(&mut e, Key::LAlt), &[md(Option)]);
        let out = down(&mut e, Key::Tab);
        assert_eq!(out.disposition, Consume);
        host(out, &[kd(Key::Tab)]);
        up(&mut e, Key::Tab);
        up(&mut e, Key::LAlt);
        host(down(&mut e, Key::LWin), &[md(Command)]);
        let out = down(&mut e, Key::E);
        assert_eq!(out.disposition, Consume);
        host(out, &[kd(Key::E)]);
    }

    #[test]
    fn magnifier_chords_stay_on_the_pc_in_full_screen() {
        let mut e = engine();
        e.set_fullscreen(true);
        host(down(&mut e, Key::LWin), &[md(Command)]);
        let out = down(&mut e, Key::Equal);
        assert_eq!((out.disposition, out.local), (Consume, Some(LocalAction::PassChordToWindows)));
        assert_eq!(out.host, vec![mu(Command)]);
        // Holding Win+Plus keeps zooming: each repeat is handed to Windows again.
        let out = down(&mut e, Key::Equal);
        assert_eq!((out.disposition, out.local), (Consume, Some(LocalAction::PassChordToWindows)));
        assert!(out.host.is_empty());
        let out = up(&mut e, Key::Equal);
        assert_eq!((out.disposition, out.local), (Consume, None));
        host(up(&mut e, Key::LWin), &[]);
        // Other modifiers held with Win still reach the Mac; only Win's Command is dropped.
        host(down(&mut e, Key::LShift), &[md(Shift)]);
        host(down(&mut e, Key::LWin), &[md(Command)]);
        let out = down(&mut e, Key::Minus);
        assert_eq!(out.local, None, "Shift+Win+Minus is not a Magnifier chord");
        up(&mut e, Key::Minus);
        up(&mut e, Key::LWin);
        up(&mut e, Key::LShift);
        host(down(&mut e, Key::LWin), &[md(Command)]);
        let out = down(&mut e, Key::Escape);
        assert_eq!(out.local, Some(LocalAction::PassChordToWindows));
        assert_eq!(out.host, vec![mu(Command)]);
    }

    #[test]
    fn magnifier_chords_go_to_windows_natively_in_a_window() {
        let mut e = engine();
        assert_eq!(down(&mut e, Key::LWin).disposition, PassToOs);
        let out = down(&mut e, Key::Equal);
        assert_eq!((out.disposition, out.local), (PassToOs, None));
        host(out, &[]);
    }

    #[test]
    fn system_shortcut_modes() {
        let mut e = engine_with(SystemShortcutMode::OnConnect);
        assert!(e.system_shortcuts_to_host());
        assert_eq!(down(&mut e, Key::LWin).disposition, Consume);

        let mut e = engine_with(SystemShortcutMode::DontSend);
        e.set_fullscreen(true);
        assert!(!e.system_shortcuts_to_host());
        assert_eq!(down(&mut e, Key::LWin).disposition, PassToOs);
    }

    #[test]
    fn toggle_flips_routing_until_fullscreen_changes() {
        let mut e = engine();
        let toggle = |e: &mut Engine| {
            down(e, Key::LCtrl);
            down(e, Key::LAlt);
            assert_eq!(down(e, Key::Left).local, Some(LocalAction::ToggleSystemShortcuts));
            up(e, Key::Left);
            up(e, Key::LAlt);
            up(e, Key::LCtrl);
        };
        toggle(&mut e);
        assert!(e.system_shortcuts_to_host());
        assert_eq!(down(&mut e, Key::LWin).disposition, Consume);
        up(&mut e, Key::LWin);

        e.set_fullscreen(true);
        assert!(e.system_shortcuts_to_host());
        toggle(&mut e);
        assert!(!e.system_shortcuts_to_host());
        e.set_fullscreen(true);
        assert!(!e.system_shortcuts_to_host(), "same state is not a change");
        e.set_fullscreen(false);
        assert!(!e.system_shortcuts_to_host());
        toggle(&mut e);
        toggle(&mut e);
        assert!(!e.system_shortcuts_to_host());
    }

    #[test]
    fn mouse_ctrl_click_is_a_plain_right_click() {
        let mut e = engine();
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
        assert_eq!(e.mouse_button(MouseButton::Left, true), (vec![mu(Command)], MouseButton::Right));
        assert_eq!(e.mouse_button(MouseButton::Left, false), (vec![md(Command)], MouseButton::Right));
        host(up(&mut e, Key::LCtrl), &[mu(Command)]);

        host(down(&mut e, Key::RCtrl), &[md(Control)]);
        assert_eq!(e.mouse_button(MouseButton::Left, true), (vec![mu(Control)], MouseButton::Right));
        assert_eq!(e.mouse_button(MouseButton::Left, false), (vec![md(Control)], MouseButton::Right));
    }

    #[test]
    fn mouse_ctrl_released_during_click_is_not_restored() {
        let mut e = engine();
        down(&mut e, Key::LCtrl);
        assert_eq!(e.mouse_button(MouseButton::Left, true), (vec![mu(Command)], MouseButton::Right));
        host(up(&mut e, Key::LCtrl), &[]);
        assert_eq!(e.mouse_button(MouseButton::Left, false), (vec![], MouseButton::Right));
    }

    #[test]
    fn mouse_ctrl_win_click_is_a_middle_click() {
        let mut e = engine();
        e.set_fullscreen(true);
        down(&mut e, Key::LCtrl);
        down(&mut e, Key::LWin);
        assert_eq!(e.mouse_button(MouseButton::Left, true), (vec![mu(Command)], MouseButton::Middle));
        assert_eq!(e.mouse_button(MouseButton::Left, false), (vec![md(Command)], MouseButton::Middle));

        // Windowed, the Win key goes to Windows but still selects the middle click.
        let mut e = engine();
        down(&mut e, Key::LCtrl);
        down(&mut e, Key::LWin);
        assert_eq!(e.mouse_button(MouseButton::Left, true), (vec![mu(Command)], MouseButton::Middle));
    }

    #[test]
    fn mouse_without_rule_is_unchanged() {
        let mut e = engine();
        assert_eq!(e.mouse_button(MouseButton::Left, true), (vec![], MouseButton::Left));
        assert_eq!(e.mouse_button(MouseButton::Left, false), (vec![], MouseButton::Left));
        down(&mut e, Key::LCtrl);
        down(&mut e, Key::LShift);
        assert_eq!(e.mouse_button(MouseButton::Left, true), (vec![], MouseButton::Left));
        assert_eq!(e.mouse_button(MouseButton::Left, false), (vec![], MouseButton::Left));
        assert_eq!(e.mouse_button(MouseButton::Right, true), (vec![], MouseButton::Right));
        assert_eq!(e.mouse_button(MouseButton::Right, false), (vec![], MouseButton::Right));
    }

    #[test]
    fn release_all_releases_everything_once() {
        let mut e = engine();
        down(&mut e, Key::LCtrl);
        down(&mut e, Key::LAlt);
        down(&mut e, Key::C);
        assert_eq!(e.release_all(), vec![ku(Key::C), mu(Option), mu(Command)]);
        assert!(e.release_all().is_empty());
        // Releases of keys pressed before are now unknown and go to Windows.
        let out = up(&mut e, Key::C);
        assert_eq!(out.disposition, PassToOs);
        host(out, &[]);
        host(up(&mut e, Key::LCtrl), &[]);
        host(down(&mut e, Key::LCtrl), &[md(Command)]);
    }

    #[test]
    fn lock_and_unknown_keys_go_to_windows() {
        let mut e = engine();
        for key in [Key::CapsLock, Key::NumLock, Key::ScrollLock, Key::Other(0x0C)] {
            for out in [down(&mut e, key), up(&mut e, key)] {
                assert_eq!(out.disposition, PassToOs, "{key:?}");
                assert!(out.host.is_empty());
            }
        }
        let out = up(&mut e, Key::Q);
        assert_eq!(out.disposition, PassToOs);
        host(out, &[]);
    }
}
