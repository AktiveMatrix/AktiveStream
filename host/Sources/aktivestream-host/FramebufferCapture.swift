// Screen capture via the legacy CGDisplayCreateImage API. ScreenCaptureKit only shares displays
// the window server considers "online", which excludes a clamshell-off built-in main display; but
// CGDisplayCreateImage can still grab that display's composited framebuffer (the same content Screen
// Sharing shows), so this is the capture path for a headless / lid-closed Mac. It polls the display
// at a fixed rate and delivers scaled BGRA CVPixelBuffers to the encoder, matching the pixel format
// ScreenCapture produces so the encoder path is identical.
import CoreGraphics
import CoreVideo
import Foundation

final class FramebufferCapture: @unchecked Sendable {
    private let displayID: CGDirectDisplayID
    private let width: Int
    private let height: Int
    private let fps: Int
    private let onFrame: @Sendable (CVPixelBuffer, UInt64) -> Void
    private let queue = DispatchQueue(label: "aktivestream.fbcapture")
    private let colorSpace = CGColorSpaceCreateDeviceRGB()
    private let startTime = DispatchTime.now()
    private var pool: CVPixelBufferPool?
    private var running = false

    init(displayID: CGDirectDisplayID, width: Int, height: Int, fps: Int,
         onFrame: @escaping @Sendable (CVPixelBuffer, UInt64) -> Void) {
        self.displayID = displayID
        self.width = width
        self.height = height
        self.fps = fps
        self.onFrame = onFrame
        let attrs: [String: Any] = [
            kCVPixelBufferPixelFormatTypeKey as String: Int(kCVPixelFormatType_32BGRA),
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
            kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
        ]
        CVPixelBufferPoolCreate(nil, nil, attrs as CFDictionary, &pool)
    }

    func begin() {
        running = true
        queue.async { [weak self] in self?.loop() }
    }

    func stop() {
        running = false
    }

    private func loop() {
        let interval = 1.0 / Double(fps)
        while running {
            let frameStart = DispatchTime.now()
            if let image = CGDisplayCreateImage(displayID), let pb = makePixelBuffer(image) {
                let pts = frameStart.uptimeNanoseconds &- startTime.uptimeNanoseconds
                onFrame(pb, pts)
            }
            let spent = Double(DispatchTime.now().uptimeNanoseconds - frameStart.uptimeNanoseconds) / 1e9
            let remaining = interval - spent
            if remaining > 0 { Thread.sleep(forTimeInterval: remaining) }
        }
    }

    // Draws the captured CGImage (full display resolution) into a pooled BGRA pixel buffer at the
    // encode size; the CGContext handles the scaling.
    private func makePixelBuffer(_ image: CGImage) -> CVPixelBuffer? {
        guard let pool else { return nil }
        var pb: CVPixelBuffer?
        guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &pb) == kCVReturnSuccess, let pb else { return nil }
        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        guard let ctx = CGContext(
            data: CVPixelBufferGetBaseAddress(pb),
            width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(pb),
            space: colorSpace,
            bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
        ) else { return nil }
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return pb
    }
}
