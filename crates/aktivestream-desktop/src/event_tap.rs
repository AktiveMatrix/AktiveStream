//! macOS event tap: intercepts system keyboard shortcuts (Cmd+Tab and friends) that the window
//! server grabs before winit ever sees them, and queues them for the client to forward to the
//! remote Mac. Needs Accessibility permission; without it the tap can't be created and the client
//! simply runs without system-shortcut forwarding.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aktivestream_keymap::Key;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_graphics::event::{
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType, EventField,
};
use winit::event_loop::EventLoopProxy;

type SystemKeys = Arc<Mutex<VecDeque<(Key, bool)>>>;

/// macOS virtual keycode for Tab (kVK_Tab).
const KVK_TAB: i64 = 0x30;

/// Spawns the event tap on its own CFRunLoop thread. Returns immediately.
pub fn spawn(proxy: EventLoopProxy<()>, keys: SystemKeys) {
    let _ = std::thread::Builder::new().name("event-tap".into()).spawn(move || run(proxy, keys));
}

fn run(proxy: EventLoopProxy<()>, keys: SystemKeys) {
    let callback = move |_proxy, event_type, event: &core_graphics::event::CGEvent| {
        let keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
        let flags = event.get_flags();
        // Cmd+Tab (optionally with Shift): the app switcher. The user's Cmd already reaches the
        // remote via winit, so forwarding just the Tab yields Cmd+Tab there; consume it locally.
        if keycode == KVK_TAB && flags.contains(CGEventFlags::CGEventFlagCommand) {
            let down = matches!(event_type, CGEventType::KeyDown);
            keys.lock().unwrap().push_back((Key::Tab, down));
            let _ = proxy.send_event(());
            None
        } else {
            Some(event.clone())
        }
    };

    let tap = match CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![CGEventType::KeyDown, CGEventType::KeyUp],
        callback,
    ) {
        Ok(tap) => tap,
        Err(()) => {
            eprintln!(
                "event tap: Cmd+Tab not captured — grant Accessibility to this binary in \
                 System Settings > Privacy & Security > Accessibility, then relaunch"
            );
            return;
        }
    };
    let Ok(source) = tap.mach_port.create_runloop_source(0) else {
        eprintln!("event tap: run-loop source failed");
        return;
    };
    let run_loop = CFRunLoop::get_current();
    unsafe {
        run_loop.add_source(&source, kCFRunLoopCommonModes);
    }
    tap.enable();
    eprintln!("event tap: forwarding Cmd+Tab to the remote Mac");
    CFRunLoop::run_current();
}
