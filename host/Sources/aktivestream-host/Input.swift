// Injects client input into the Mac with CGEvent. Requires Accessibility permission; the
// events are posted to the HID event tap so they behave like real input. Message bodies are
// little-endian and match crates/aktivestream-proto (Input::encode).
import ApplicationServices
import CoreGraphics
import Foundation
import Network

/// Little-endian reader over a message body.
struct LeReader {
    let data: Data
    var pos: Int
    init(_ data: Data) {
        self.data = data
        self.pos = data.startIndex
    }
    mutating func bytes(_ n: Int) -> Data? {
        guard pos + n <= data.endIndex else { return nil }
        let slice = data.subdata(in: pos..<(pos + n))
        pos += n
        return slice
    }
    mutating func u8() -> UInt8? { bytes(1)?.first }
    mutating func u16() -> UInt16? { bytes(2).map { $0.withUnsafeBytes { $0.loadUnaligned(as: UInt16.self).littleEndian } } }
    mutating func u32() -> UInt32? { bytes(4).map { $0.withUnsafeBytes { $0.loadUnaligned(as: UInt32.self).littleEndian } } }
    mutating func f32() -> Float? { u32().map { Float(bitPattern: $0) } }
}

final class InputInjector: @unchecked Sendable {
    private var lastButtons: UInt8 = 0

    func handle(kind: UInt8, body: Data) {
        switch kind {
        case MsgKind.inputKey: injectKey(body)
        case MsgKind.inputPointer: injectPointer(body)
        case MsgKind.inputScroll: injectScroll(body)
        default: break
        }
    }

    private func injectKey(_ body: Data) {
        var r = LeReader(body)
        guard let keycode = r.u16(), let down = r.u8(), let flags = r.u32(),
            let event = CGEvent(keyboardEventSource: nil, virtualKey: keycode, keyDown: down != 0)
        else {
            return
        }
        event.flags = CGEventFlags(rawValue: UInt64(flags))
        event.post(tap: .cghidEventTap)
    }

    private func injectPointer(_ body: Data) {
        var r = LeReader(body)
        guard let buttons = r.u8(), let x = r.f32(), let y = r.f32() else { return }
        let bounds = CGDisplayBounds(CGMainDisplayID())
        let p = CGPoint(x: bounds.origin.x + CGFloat(x) * bounds.width,
                        y: bounds.origin.y + CGFloat(y) * bounds.height)

        func post(_ type: CGEventType, _ button: CGMouseButton) {
            CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: p, mouseButton: button)?
                .post(tap: .cghidEventTap)
        }
        let changed = buttons ^ lastButtons
        if changed & 1 != 0 { post(buttons & 1 != 0 ? .leftMouseDown : .leftMouseUp, .left) }
        if changed & 2 != 0 { post(buttons & 2 != 0 ? .rightMouseDown : .rightMouseUp, .right) }
        if changed & 4 != 0 { post(buttons & 4 != 0 ? .otherMouseDown : .otherMouseUp, .center) }
        if changed == 0 {
            // Pure move, or a drag if a button is held.
            if buttons & 1 != 0 {
                post(.leftMouseDragged, .left)
            } else if buttons & 2 != 0 {
                post(.rightMouseDragged, .right)
            } else if buttons & 4 != 0 {
                post(.otherMouseDragged, .center)
            } else {
                post(.mouseMoved, .left)
            }
        }
        lastButtons = buttons
    }

    private func injectScroll(_ body: Data) {
        var r = LeReader(body)
        guard let dx = r.f32(), let dy = r.f32() else { return }
        CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2,
                wheel1: Int32(dy.rounded()), wheel2: Int32(dx.rounded()), wheel3: 0)?
            .post(tap: .cghidEventTap)
    }
}

/// Reads input messages from the connection and injects them, until the stream ends. Resolution
/// requests are handed to `control` for the video task to act on.
func readInput(_ connection: NWConnection, _ control: StreamControl) async {
    let injector = InputInjector()
    while true {
        guard let (kind, body) = try? await readMessage(connection) else { break }
        switch kind {
        case MsgKind.setResolution:
            var r = LeReader(body)
            if let w = r.u32(), let h = r.u32() {
                control.request(Int(w), Int(h))
            }
        default: injector.handle(kind: kind, body: body)
        }
    }
}

/// True if the agent may post input (Accessibility). Calling it registers the signed agent in
/// System Settings > Privacy & Security > Accessibility for the user to grant.
func accessibilityGranted(prompt: Bool) -> Bool {
    // The key is kAXTrustedCheckOptionPrompt; referenced by its literal value to avoid the
    // Swift 6 non-Sendable global.
    AXIsProcessTrustedWithOptions(["AXTrustedCheckOptionPrompt": prompt] as CFDictionary)
}
