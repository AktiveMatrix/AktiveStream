// One client session over a QUIC stream: message framing, the handshake, and (checkpoint 1b)
// a synthetic test frame. Framing matches crates/aktivestream-proto/src/wire.rs: u32 little-endian
// length, then that many bytes whose first byte is the message kind.
import Foundation
import Network

let protoVersion: UInt32 = 1

enum MsgKind {
    static let error: UInt8 = 0
    static let hello: UInt8 = 1
    static let helloOk: UInt8 = 2
    static let auth: UInt8 = 3
    static let authOk: UInt8 = 4
    static let testFrame: UInt8 = 20
    static let videoFrame: UInt8 = 21
    static let videoStart: UInt8 = 22
}

/// How many synthetic frames `serve` streams before closing (checkpoint 2c).
let streamFrameCount = 150

enum SessionError: Error { case closed, tooLarge }

/// Receives exactly `n` bytes (QUIC is a reliable byte stream, so min==max returns all `n`).
func receiveExact(_ connection: NWConnection, _ n: Int) async throws -> Data {
    if n == 0 { return Data() }
    return try await withCheckedThrowingContinuation { cont in
        connection.receive(minimumIncompleteLength: n, maximumLength: n) { data, _, _, error in
            if let error { cont.resume(throwing: error); return }
            guard let data, data.count == n else { cont.resume(throwing: SessionError.closed); return }
            cont.resume(returning: data)
        }
    }
}

func sendData(_ connection: NWConnection, _ data: Data) async throws {
    try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
        connection.send(content: data, completion: .contentProcessed { error in
            if let error { cont.resume(throwing: error) } else { cont.resume() }
        })
    }
}

func readMessage(_ connection: NWConnection) async throws -> (UInt8, Data) {
    let header = try await receiveExact(connection, 4)
    let len = Int(header.withUnsafeBytes { $0.load(as: UInt32.self).littleEndian })
    guard len >= 1, len <= 64 << 20 else { throw SessionError.tooLarge }
    let body = try await receiveExact(connection, len)
    return (body[body.startIndex], body.subdata(in: (body.startIndex + 1)..<body.endIndex))
}

func writeMessage(_ connection: NWConnection, kind: UInt8, body: Data) async throws {
    var out = Data()
    var len = UInt32(body.count + 1).littleEndian
    withUnsafeBytes(of: &len) { out.append(contentsOf: $0) }
    out.append(kind)
    out.append(body)
    try await sendData(connection, out)
}

/// Little-endian body builder matching wire::Writer on the Rust side.
struct Body {
    var data = Data()
    mutating func u8(_ v: UInt8) { data.append(v) }
    mutating func u16(_ v: UInt16) { var x = v.littleEndian; withUnsafeBytes(of: &x) { data.append(contentsOf: $0) } }
    mutating func u32(_ v: UInt32) { var x = v.littleEndian; withUnsafeBytes(of: &x) { data.append(contentsOf: $0) } }
    mutating func shortString(_ s: String) {
        let bytes = Array(s.utf8.prefix(255))
        u8(UInt8(bytes.count))
        data.append(contentsOf: bytes)
    }
    mutating func bytes(_ b: [UInt8]) { data.append(contentsOf: b) }
}

/// A recognisable RGBA test pattern: a colour gradient with a white diagonal, so the client's
/// saved PNG is obviously correct and orientation is visible.
func testFrameBody(width: Int, height: Int) -> Data {
    var body = Body()
    body.u32(UInt32(width))
    body.u32(UInt32(height))
    var pixels = [UInt8](repeating: 0, count: width * height * 4)
    for y in 0..<height {
        for x in 0..<width {
            let i = (y * width + x) * 4
            pixels[i] = UInt8(255 * x / max(width - 1, 1))      // R ramps left->right
            pixels[i + 1] = UInt8(255 * y / max(height - 1, 1)) // G ramps top->bottom
            pixels[i + 2] = (x / 16 + y / 16) % 2 == 0 ? 64 : 160 // B checkerboard
            pixels[i + 3] = 255
            if abs(x * height - y * width) < width { pixels[i] = 255; pixels[i + 1] = 255; pixels[i + 2] = 255 }
        }
    }
    body.bytes(pixels)
    return body.data
}

func runSession(_ connection: NWConnection) async {
    do {
        let (k, helloBody) = try await readMessage(connection)
        guard k == MsgKind.hello else { throw SessionError.closed }
        _ = helloBody
        log("session: Hello received")

        var ok = Body()
        ok.u32(protoVersion)
        ok.shortString(Host.current().localizedName ?? ProcessInfo.processInfo.hostName)
        ok.u8(1) // display count (checkpoint 1b)
        try await writeMessage(connection, kind: MsgKind.helloOk, body: ok.data)

        let (ak, authBody) = try await readMessage(connection)
        guard ak == MsgKind.auth else { throw SessionError.closed }
        // Checkpoint 1b: accept any token. Real check (macOS account + TLS channel binding) later.
        log("session: Auth received (\(authBody.count) bytes), accepting")
        try await writeMessage(connection, kind: MsgKind.authOk, body: Data())
        try await streamVideo(connection)
    } catch {
        log("session ended: \(error)")
    }
}

/// Streams HEVC over the control stream: the real screen when Screen Recording is granted,
/// otherwise synthetic frames (so the pipeline runs without the grant, e.g. over SSH).
func streamVideo(_ connection: NWConnection) async throws {
    let w = 1920, h = 1200, fps = 60
    var start = Body()
    start.u32(UInt32(w))
    start.u32(UInt32(h))
    start.u8(0) // codec 0 = HEVC
    try await writeMessage(connection, kind: MsgKind.videoStart, body: start.data)

    let queue = FrameQueue()
    let encoder = try HEVCEncoder(width: w, height: h, fps: fps, bitrate: 20_000_000) { frame in
        queue.append(frame)
    }
    func flush() async throws {
        for frame in queue.drain() {
            var body = Body()
            body.u8(frame.isKeyframe ? 1 : 0)
            body.data.append(frame.data)
            try await writeMessage(connection, kind: MsgKind.videoFrame, body: body.data)
        }
    }

    if let display = try? await ScreenCapture.mainDisplay() {
        log("session: streaming real screen capture")
        let capture = ScreenCapture(width: w, height: h) { pixelBuffer, ptsNanos in
            try? encoder.encode(pixelBuffer, ptsNanos: ptsNanos, forceKeyframe: false)
        }
        try await capture.start(display: display)
        // Send frames as they arrive until the client disconnects (a send then throws).
        do {
            while true {
                try await flush()
                try await Task.sleep(nanoseconds: 5_000_000)
            }
        } catch {
            log("session: capture stream ended (\(error))")
        }
        await capture.stop()
        encoder.finish()
    } else {
        log("session: no screen recording permission; streaming synthetic frames")
        for i in 0..<streamFrameCount {
            guard let pb = syntheticPixelBuffer(width: w, height: h, tick: i) else { continue }
            try encoder.encode(pb, ptsNanos: UInt64(i) * UInt64(1_000_000_000 / fps), forceKeyframe: i == 0)
            try await flush()
        }
        encoder.finish()
        try await flush()
        try await closeStream(connection)
        log("session: streamed \(streamFrameCount) synthetic frames")
    }
}

/// Finishes the stream's send direction so the client sees a clean end.
func closeStream(_ connection: NWConnection) async throws {
    try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
        connection.send(
            content: nil, contentContext: .finalMessage, isComplete: true,
            completion: .contentProcessed { error in
                if let error { cont.resume(throwing: error) } else { cont.resume() }
            })
    }
}

/// Thread-safe queue for encoded frames (the VideoToolbox callback runs off the session task).
final class FrameQueue: @unchecked Sendable {
    private let lock = NSLock()
    private var frames: [EncodedFrame] = []
    func append(_ frame: EncodedFrame) {
        lock.lock(); frames.append(frame); lock.unlock()
    }
    func drain() -> [EncodedFrame] {
        lock.lock(); defer { frames.removeAll(); lock.unlock() }
        return frames
    }
}
