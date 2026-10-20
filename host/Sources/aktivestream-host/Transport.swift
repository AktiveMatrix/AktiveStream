// QUIC transport (Network.framework). Checkpoint 1: start a listener with the host's TLS
// identity, accept a QUIC connection, and log it, so quinn <-> Network.framework interop is
// proven before the handshake and streams are layered on.
import Foundation
import Network

let alpn = "astream/1"
let defaultPort: UInt16 = 35390

func log(_ s: String) {
    FileHandle.standardError.write((s + "\n").data(using: .utf8)!)
}

/// QUIC parameters with our ALPN. Returns nil only if QUIC is unavailable.
func quicParameters() -> NWParameters? {
    let options = NWProtocolQUIC.Options(alpn: [alpn])
    options.direction = .bidirectional
    return NWParameters(quic: options)
}

final class HostListener {
    private let identity: HostIdentity
    private let port: UInt16
    private var listener: NWListener?
    private let queue = DispatchQueue(label: "aktivestream.listener")

    init(identity: HostIdentity, port: UInt16) {
        self.identity = identity
        self.port = port
    }

    func start() throws {
        let options = NWProtocolQUIC.Options(alpn: [alpn])
        options.direction = .bidirectional
        // Present the host's self-signed identity for TLS.
        guard let secIdentity = sec_identity_create(identity.secIdentity) else {
            throw IdentityError.noIdentity
        }
        sec_protocol_options_set_local_identity(options.securityProtocolOptions, secIdentity)

        let params = NWParameters(quic: options)
        let listener = try NWListener(using: params, on: NWEndpoint.Port(rawValue: port)!)
        self.listener = listener

        let port = self.port
        let fingerprint = identity.fingerprint
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready:
                log("listening on UDP \(port), fingerprint \(fingerprint)")
            case .failed(let error):
                log("listener failed: \(error)")
                exit(1)
            default:
                break
            }
        }
        listener.newConnectionHandler = { [queue] connection in
            handleConnection(connection, on: queue)
        }
        listener.start(queue: queue)
    }
}

/// Wraps a non-Sendable value so it can cross into a Task. NWConnection's own methods are
/// thread-safe (they marshal to its queue), so this is sound for how the session uses it.
struct SendableBox<T>: @unchecked Sendable { let value: T }

/// Handles one accepted QUIC connection. Free function so the `@Sendable` connection handler
/// captures only the (Sendable) queue, not the listener.
func handleConnection(_ connection: NWConnection, on queue: DispatchQueue) {
    let box = SendableBox(value: connection)
    connection.stateUpdateHandler = { state in
        switch state {
        case .ready:
            log("connection ready from \(box.value.endpoint.debugDescription)")
            Task { await runSession(box.value) }
        case .failed(let error):
            log("connection failed: \(error)")
        case .cancelled:
            log("connection cancelled")
        default:
            break
        }
    }
    connection.start(queue: queue)
}
