// AktiveStream host agent (Phase 2): captures the Mac screen, hardware-encodes it, and streams
// it to the AktiveStream client over QUIC, injecting input and syncing the clipboard in the
// user's GUI session. See docs/phase2-protocol.md. This file is the entry point; the pieces
// land in sibling files as the agent grows.
import Foundation
import Network
import ScreenCaptureKit
import VideoToolbox

let version = "0.1.0"

// Confirms the frameworks the agent depends on are available and prints what it found.
func preflight() {
    log("aktivestream-host \(version) preflight")
    var listOut: CFArray?
    VTCopyVideoEncoderList(nil, &listOut)
    if let list = listOut as? [[String: Any]] {
        let hevc = list.contains { ($0[kVTVideoEncoderList_CodecType as String] as? Int) == Int(kCMVideoCodecType_HEVC) }
        log("video encoders: \(list.count), HEVC: \(hevc)")
    }
    log("QUIC parameters built: \(quicParameters() != nil)")
    do {
        let id = try loadOrCreateIdentity()
        log("identity fingerprint: \(id.fingerprint)")
    } catch {
        log("identity error: \(error)")
    }
}

func serve(port: UInt16) {
    do {
        let identity = try loadOrCreateIdentity()
        let listener = HostListener(identity: identity, port: port)
        // Probe Screen Recording on startup so the signed agent registers in
        // System Settings > Screen Recording for the user to grant it.
        Task {
            do {
                _ = try await ScreenCapture.mainDisplay()
                log("screen recording: granted")
            } catch {
                log("screen recording: not granted yet - enable 'AktiveStream Host' in System Settings > Privacy & Security > Screen Recording (\(error))")
            }
        }
        // Register for (and report) Accessibility, needed to inject keyboard/mouse input.
        let axGranted = accessibilityGranted(prompt: false)
        log("accessibility: \(axGranted ? "granted" : "not granted yet - enable 'AktiveStream Host' in System Settings > Privacy & Security > Accessibility")")
        try listener.start()
        dispatchMain()
    } catch {
        log("serve failed: \(error)")
        exit(1)
    }
}

// Thread-safe collector for the encode test (the VideoToolbox callback runs off-thread).
final class EncTestCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    private var totalBytes = 0
    private var idr = 0
    private var submitted: [UInt64: DispatchTime] = [:]
    private var latencies: [Double] = []

    func submit(_ pts: UInt64) {
        lock.lock(); submitted[pts] = .now(); lock.unlock()
    }
    func record(_ frame: EncodedFrame) {
        let now = DispatchTime.now()
        lock.lock()
        count += 1
        totalBytes += frame.data.count
        if frame.isKeyframe { idr += 1 }
        if let t = submitted[frame.ptsNanos] {
            latencies.append(Double(now.uptimeNanoseconds - t.uptimeNanoseconds) / 1e6)
        }
        lock.unlock()
    }
    func report(seconds: Double, w: Int, h: Int) {
        lock.lock(); defer { lock.unlock() }
        let avg = latencies.isEmpty ? 0 : latencies.reduce(0, +) / Double(latencies.count)
        let mbps = Double(totalBytes) * 8 / 1e6 / max(seconds, 1e-9)
        log(String(format: "HEVC %dx%d: %d frames in %.2fs = %.1f fps, %d IDR, %.2f Mbit/s, encode latency avg %.1f ms max %.1f ms",
                   w, h, count, seconds, Double(count) / max(seconds, 1e-9), idr, mbps, avg, latencies.max() ?? 0))
    }
}

// Encodes synthetic frames to HEVC and reports throughput/latency: the number that justifies
// Phase 2 (vs. Apple's ~11 Mpixel/s lossless VNC). No capture, no TCC.
func encodeTest(frames: Int) {
    let w = 1920, h = 1200, fps = 60
    let stats = EncTestCollector()
    do {
        let encoder = try HEVCEncoder(width: w, height: h, fps: fps, bitrate: 20_000_000) { frame in
            stats.record(frame)
        }
        let wall = DispatchTime.now()
        for i in 0..<frames {
            guard let pb = syntheticPixelBuffer(width: w, height: h, tick: i) else { continue }
            let pts = UInt64(i) * UInt64(1_000_000_000 / fps)
            stats.submit(pts)
            try encoder.encode(pb, ptsNanos: pts, forceKeyframe: i == 0)
        }
        encoder.finish()
        stats.report(seconds: Double(DispatchTime.now().uptimeNanoseconds - wall.uptimeNanoseconds) / 1e9, w: w, h: h)
    } catch {
        log("encode test failed: \(error)")
        exit(1)
    }
}

// Length-prefixed frame collector: u32 little-endian length + Annex B, so the client splits
// access units without parsing NAL types. Thread-safe (VideoToolbox callback runs off-thread).
final class DumpCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var data = Data()
    private var count = 0
    func append(_ frame: EncodedFrame) {
        lock.lock()
        var len = UInt32(frame.data.count).littleEndian
        withUnsafeBytes(of: &len) { data.append(contentsOf: $0) }
        data.append(frame.data)
        count += 1
        lock.unlock()
    }
    func snapshot() -> (Data, Int) {
        lock.lock(); defer { lock.unlock() }
        return (data, count)
    }
}

func hevcDump(frames: Int, width: Int, height: Int, to path: String) {
    let collector = DumpCollector()
    do {
        let encoder = try HEVCEncoder(width: width, height: height, fps: 60, bitrate: 10_000_000) { frame in
            collector.append(frame)
        }
        for i in 0..<frames {
            guard let pb = syntheticPixelBuffer(width: width, height: height, tick: i) else { continue }
            try encoder.encode(pb, ptsNanos: UInt64(i) * 16_666_666, forceKeyframe: i == 0)
        }
        encoder.finish()
        let (data, count) = collector.snapshot()
        try data.write(to: URL(fileURLWithPath: path))
        log("wrote \(count) HEVC frames (\(data.count) bytes) to \(path)")
    } catch {
        log("hevcdump failed: \(error)")
        exit(1)
    }
}

let args = CommandLine.arguments
switch args.dropFirst().first {
case "serve":
    let port = args.count > 2 ? (UInt16(args[2]) ?? defaultPort) : defaultPort
    serve(port: port)
case "enctest":
    encodeTest(frames: args.count > 2 ? (Int(args[2]) ?? 120) : 120)
case "hevcdump":
    // hevcdump <frames> <width> <height> <outfile>: write Annex B HEVC of synthetic frames,
    // for developing the client decoder against real data.
    let frames = args.count > 2 ? (Int(args[2]) ?? 30) : 30
    let w = args.count > 3 ? (Int(args[3]) ?? 320) : 320
    let h = args.count > 4 ? (Int(args[4]) ?? 240) : 240
    let outPath = args.count > 5 ? args[5] : "/tmp/synthetic.h265"
    hevcDump(frames: frames, width: w, height: h, to: outPath)
default:
    preflight()
}
