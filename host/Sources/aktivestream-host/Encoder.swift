// Hardware HEVC encoding with VideoToolbox. Checkpoint 2a: encode synthetic CVPixelBuffers so
// the encode -> Annex B path (and later the client decode) is proven without needing Screen
// Recording TCC. Swapping in ScreenCaptureKit's real CVPixelBuffers is then a localized change.
import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

/// One encoded frame in Annex B (start-code-delimited NAL units). IDR frames are prefixed with
/// the VPS/SPS/PPS parameter sets, so the decoder needs no out-of-band configuration.
struct EncodedFrame {
    let data: Data
    let isKeyframe: Bool
    let ptsNanos: UInt64
}

// VideoToolbox sessions are thread-safe, so encoding from the capture queue is sound.
final class HEVCEncoder: @unchecked Sendable {
    private var session: VTCompressionSession?
    let width: Int
    let height: Int
    private let onFrame: @Sendable (EncodedFrame) -> Void

    init(width: Int, height: Int, fps: Int, bitrate: Int, onFrame: @escaping @Sendable (EncodedFrame) -> Void) throws {
        self.width = width
        self.height = height
        self.onFrame = onFrame

        var session: VTCompressionSession?
        let status = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            width: Int32(width), height: Int32(height),
            codecType: kCMVideoCodecType_HEVC,
            encoderSpecification: nil, imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: nil, refcon: nil,
            compressionSessionOut: &session)
        guard status == noErr, let session else { throw EncoderError.create(status) }
        self.session = session

        func set(_ key: CFString, _ value: CFTypeRef) {
            VTSessionSetProperty(session, key: key, value: value)
        }
        set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue)
        set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel)
        set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse) // no B-frames: low latency
        set(kVTCompressionPropertyKey_MaxKeyFrameInterval, NSNumber(value: fps * 5))
        set(kVTCompressionPropertyKey_AverageBitRate, NSNumber(value: bitrate))
        set(kVTCompressionPropertyKey_ExpectedFrameRate, NSNumber(value: fps))
        VTCompressionSessionPrepareToEncodeFrames(session)
    }

    deinit {
        if let session { VTCompressionSessionInvalidate(session) }
    }

    /// Flushes any frames still being encoded so their callbacks run.
    func finish() {
        if let session { VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid) }
    }

    /// Encodes one frame. `forceKeyframe` asks for an IDR (first frame, or a client request).
    func encode(_ pixelBuffer: CVPixelBuffer, ptsNanos: UInt64, forceKeyframe: Bool) throws {
        guard let session else { throw EncoderError.invalid }
        let pts = CMTime(value: CMTimeValue(ptsNanos), timescale: 1_000_000_000)
        var properties: CFDictionary?
        if forceKeyframe {
            properties = [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue] as CFDictionary
        }
        let onFrame = self.onFrame
        let status = VTCompressionSessionEncodeFrame(
            session, imageBuffer: pixelBuffer, presentationTimeStamp: pts, duration: .invalid,
            frameProperties: properties,
            infoFlagsOut: nil
        ) { status, _, sample in
            guard status == noErr, let sample, let frame = annexB(from: sample) else { return }
            onFrame(frame)
        }
        if status != noErr { throw EncoderError.encode(status) }
    }
}

enum EncoderError: Error, CustomStringConvertible {
    case create(OSStatus), encode(OSStatus), invalid
    var description: String {
        switch self {
        case .create(let s): return "VTCompressionSessionCreate failed: \(s)"
        case .encode(let s): return "VTCompressionSessionEncodeFrame failed: \(s)"
        case .invalid: return "encoder session invalid"
        }
    }
}

private let startCode: [UInt8] = [0, 0, 0, 1]

/// Converts a CMSampleBuffer (length-prefixed HEVC NAL units) to Annex B, prepending the
/// VPS/SPS/PPS parameter sets on keyframes.
private func annexB(from sample: CMSampleBuffer) -> EncodedFrame? {
    guard let dataBuffer = CMSampleBufferGetDataBuffer(sample) else { return nil }
    let keyframe = isKeyframe(sample)
    var out = Data()

    if keyframe, let format = CMSampleBufferGetFormatDescription(sample) {
        var count = 0
        CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
            format, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil,
            parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil)
        for i in 0..<count {
            var ptr: UnsafePointer<UInt8>?
            var size = 0
            if CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                format, parameterSetIndex: i, parameterSetPointerOut: &ptr, parameterSetSizeOut: &size,
                parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil) == noErr, let ptr {
                out.append(contentsOf: startCode)
                out.append(ptr, count: size)
            }
        }
    }

    var lengthAtOffset = 0
    var totalLength = 0
    var dataPointer: UnsafeMutablePointer<Int8>?
    guard CMBlockBufferGetDataPointer(
        dataBuffer, atOffset: 0, lengthAtOffsetOut: &lengthAtOffset, totalLengthOut: &totalLength,
        dataPointerOut: &dataPointer) == noErr, let dataPointer else { return nil }

    // The buffer is a sequence of {UInt32 big-endian length, NAL bytes}. Rewrite each with a
    // 4-byte start code.
    let bytes = UnsafeRawPointer(dataPointer).assumingMemoryBound(to: UInt8.self)
    var offset = 0
    while offset + 4 <= totalLength {
        // Big-endian u32 length prefix, read byte-wise (the buffer is not 4-byte aligned).
        let nalLength = (Int(bytes[offset]) << 24) | (Int(bytes[offset + 1]) << 16)
            | (Int(bytes[offset + 2]) << 8) | Int(bytes[offset + 3])
        offset += 4
        if nalLength <= 0 || offset + nalLength > totalLength { break }
        out.append(contentsOf: startCode)
        out.append(Data(bytes: bytes.advanced(by: offset), count: nalLength))
        offset += nalLength
    }

    let pts = CMSampleBufferGetPresentationTimeStamp(sample)
    let ptsNanos = UInt64(max(0, pts.seconds) * 1_000_000_000)
    return EncodedFrame(data: out, isKeyframe: keyframe, ptsNanos: ptsNanos)
}

private func isKeyframe(_ sample: CMSampleBuffer) -> Bool {
    guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false),
        CFArrayGetCount(attachments) > 0
    else {
        return true
    }
    let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFDictionary.self)
    let key = Unmanaged.passUnretained(kCMSampleAttachmentKey_NotSync).toOpaque()
    // Absence of NotSync (or NotSync == false) means this is a sync (key) frame.
    if let value = CFDictionaryGetValue(dict, key) {
        return !CFBooleanGetValue(unsafeBitCast(value, to: CFBoolean.self))
    }
    return true
}

/// A BGRA pixel buffer filled with a moving test pattern (checkpoint 2a, no screen capture).
func syntheticPixelBuffer(width: Int, height: Int, tick: Int) -> CVPixelBuffer? {
    var pb: CVPixelBuffer?
    let attrs = [kCVPixelBufferIOSurfacePropertiesKey as String: [:]] as CFDictionary
    guard CVPixelBufferCreate(kCFAllocatorDefault, width, height, kCVPixelFormatType_32BGRA, attrs, &pb) == kCVReturnSuccess,
        let pb
    else {
        return nil
    }
    CVPixelBufferLockBaseAddress(pb, [])
    defer { CVPixelBufferUnlockBaseAddress(pb, []) }
    guard let base = CVPixelBufferGetBaseAddress(pb) else { return nil }
    let stride = CVPixelBufferGetBytesPerRow(pb)
    let shift = tick * 3
    for y in 0..<height {
        let row = base.advanced(by: y * stride).assumingMemoryBound(to: UInt8.self)
        for x in 0..<width {
            let p = x * 4
            row[p] = UInt8((x + shift) & 0xff)      // B
            row[p + 1] = UInt8((y + shift) & 0xff)  // G
            row[p + 2] = UInt8((x + y) & 0xff)      // R
            row[p + 3] = 255
        }
    }
    return pb
}
