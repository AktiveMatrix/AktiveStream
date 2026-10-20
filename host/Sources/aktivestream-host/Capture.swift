// Screen capture with ScreenCaptureKit. Delivers the display's frames as CVPixelBuffers to the
// encoder. Requires Screen Recording TCC; until that is granted (e.g. when run over SSH), the
// attempt to list shareable content fails and the caller falls back to synthetic frames. Just
// making the attempt registers the (signed) agent in System Settings > Screen Recording.
import CoreMedia
import CoreVideo
import Foundation
import ScreenCaptureKit

enum CaptureError: Error, CustomStringConvertible {
    case noDisplay
    var description: String { "no display available to capture" }
}

final class ScreenCapture: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    let width: Int
    let height: Int
    private let onFrame: @Sendable (CVPixelBuffer, UInt64) -> Void
    private let sampleQueue = DispatchQueue(label: "aktivestream.capture")
    private var stream: SCStream?

    init(width: Int, height: Int, onFrame: @escaping @Sendable (CVPixelBuffer, UInt64) -> Void) {
        self.width = width
        self.height = height
        self.onFrame = onFrame
    }

    /// The main display, if Screen Recording is permitted. Throws (and thereby registers the
    /// agent for the permission) when it is not.
    static func mainDisplay() async throws -> SCDisplay {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        guard let display = content.displays.first else { throw CaptureError.noDisplay }
        return display
    }

    func start(display: SCDisplay) async throws {
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.width = width
        config.height = height
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        config.queueDepth = 5
        config.showsCursor = true
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: sampleQueue)
        try await stream.startCapture()
        self.stream = stream
    }

    func stop() async {
        try? await stream?.stopCapture()
        stream = nil
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, CMSampleBufferGetNumSamples(sampleBuffer) > 0,
            let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer)
        else {
            return
        }
        // Skip frames the compositor marked incomplete (no change / occluded).
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false)
            as? [[SCStreamFrameInfo: Any]], let first = attachments.first,
            let statusRaw = first[.status] as? Int, let status = SCFrameStatus(rawValue: statusRaw),
            status != .complete {
            return
        }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        let ns = UInt64(max(0, pts.seconds) * 1_000_000_000)
        onFrame(pixelBuffer, ns)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        log("capture stopped: \(error)")
    }
}
