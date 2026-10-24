// Picks and applies display resolutions for the dynamic-resolution feature. A client asks for the
// pixel size of its current view; the host switches the streamed display to the nearest mode the
// display actually advertises (the dummy adapter / external monitor only offers its EDID modes), and
// the client scales whatever remains to its window. On hardware that advertises the requested size
// (a 4K display or 4K EDID dongle) the match is exact and the stream is pixel-perfect.
import CoreGraphics
import Foundation

enum DisplayModes {
    /// The current mode's pixel dimensions for `display` (falls back to the display's pixel size).
    static func currentSize(_ display: CGDirectDisplayID) -> (Int, Int) {
        if let mode = CGDisplayCopyDisplayMode(display) {
            return (mode.pixelWidth, mode.pixelHeight)
        }
        return (Int(CGDisplayPixelsWide(display)), Int(CGDisplayPixelsHigh(display)))
    }

    /// The advertised mode nearest `w`x`h`: closest in pixel count first, then in aspect ratio.
    /// `nil` when the mode list cannot be read.
    static func nearest(_ display: CGDirectDisplayID, _ w: Int, _ h: Int) -> CGDisplayMode? {
        guard let modes = CGDisplayCopyAllDisplayModes(display, nil) as? [CGDisplayMode], !modes.isEmpty else {
            return nil
        }
        let targetArea = Double(max(w * h, 1))
        let targetAspect = Double(w) / Double(max(h, 1))
        func score(_ m: CGDisplayMode) -> (Double, Double) {
            let areaDiff = abs(Double(m.pixelWidth * m.pixelHeight) - targetArea)
            let aspect = Double(m.pixelWidth) / Double(max(m.pixelHeight, 1))
            return (areaDiff, abs(aspect - targetAspect))
        }
        return modes.min { score($0) < score($1) }
    }

    /// Switches `display` to `mode`, returning its pixel size, or `nil` on failure. Not persisted
    /// across reboot; `streamVideo` restores the original mode when the session ends.
    @discardableResult
    static func set(_ display: CGDirectDisplayID, _ mode: CGDisplayMode) -> (Int, Int)? {
        CGDisplaySetDisplayMode(display, mode, nil) == .success ? (mode.pixelWidth, mode.pixelHeight) : nil
    }
}
