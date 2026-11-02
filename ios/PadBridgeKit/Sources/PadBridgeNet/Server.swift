import Foundation
import Network
import PadBridgeProtocol

/// TCP listener on the iPad side. The Windows host always connects to us, over
/// Wi‑Fi (found via Bonjour) or through the usbmux tunnel to the same port.
///
/// The server answers `Hello`, `Ping` and `Bye` itself and forwards every other
/// message through `onEvent`, which runs on the server's private queue.
public final class Server: @unchecked Sendable {
    public enum Event {
        case ready(port: UInt16)
        case connected(PeerConnection)
        case hello(PeerConnection, name: String, role: Role)
        case message(PeerConnection, Message)
        case disconnected(PeerConnection, Error?)
        case failed(Error)
    }

    public let role: Role
    public let name: String
    public var onEvent: ((Event) -> Void)?

    private let requestedPort: UInt16
    private let advertise: Bool
    private let queue = DispatchQueue(label: "padbridge.server")
    private var listener: NWListener?
    private var peers: [PeerConnection] = []

    /// `port` 0 picks any free port. `advertise` publishes a Bonjour service.
    public init(role: Role, name: String, port: UInt16, advertise: Bool = true) {
        self.role = role
        self.name = name
        requestedPort = port
        self.advertise = advertise
    }

    public func start() throws {
        let parameters = NWParameters.tcp
        parameters.allowLocalEndpointReuse = true
        let port = requestedPort == 0 ? NWEndpoint.Port.any : NWEndpoint.Port(rawValue: requestedPort)!
        let listener = try NWListener(using: parameters, on: port)
        if advertise {
            listener.service = NWListener.Service(
                name: name, type: Wire.bonjourType, domain: nil,
                txtRecord: Self.txtRecord(["proto": "\(Wire.protocolVersion)", "role": "\(role.rawValue)"]))
        }
        listener.stateUpdateHandler = { [weak self, weak listener] state in
            switch state {
            case .ready: self?.onEvent?(.ready(port: listener?.port?.rawValue ?? 0))
            case let .failed(error): self?.onEvent?(.failed(error))
            default: break
            }
        }
        listener.newConnectionHandler = { [weak self] connection in self?.accept(connection) }
        listener.start(queue: queue)
        self.listener = listener
    }

    public func stop() {
        queue.async { [self] in
            listener?.cancel()
            listener = nil
            for peer in peers { peer.sayGoodbye() }
        }
    }

    /// Sends `message` to every connected peer.
    public func broadcast(_ message: Message) {
        queue.async { [self] in
            for peer in peers { peer.send(message) }
        }
    }

    private func accept(_ connection: NWConnection) {
        let peer = PeerConnection(connection: connection, queue: queue)
        peers.append(peer)
        peer.onReady = { [weak self, weak peer] in
            guard let self, let peer else { return }
            peer.send(.hello(version: Wire.protocolVersion, role: role, name: name))
            onEvent?(.connected(peer))
        }
        peer.onMessage = { [weak self, weak peer] message in
            guard let self, let peer else { return }
            handle(message, from: peer)
        }
        peer.onClose = { [weak self, weak peer] error in
            guard let self, let peer else { return }
            peers.removeAll { $0 === peer }
            onEvent?(.disconnected(peer, error))
        }
        peer.start()
    }

    private func handle(_ message: Message, from peer: PeerConnection) {
        switch message {
        case let .hello(version, role, name):
            guard version == Wire.protocolVersion else {
                peer.send(.log(level: 3, text: "protocol version \(version) unsupported, expected \(Wire.protocolVersion)"))
                peer.sayGoodbye()
                return
            }
            onEvent?(.hello(peer, name: name, role: role))
        case let .ping(nonce, tUs):
            peer.send(.pong(nonce: nonce, tUs: tUs))
        case .bye:
            peer.close()
        default:
            onEvent?(.message(peer, message))
        }
    }

    /// DNS-SD TXT record: a sequence of length-prefixed `key=value` strings.
    static func txtRecord(_ entries: [String: String]) -> Data {
        var data = Data()
        for (key, value) in entries.sorted(by: { $0.key < $1.key }) {
            let entry = Array("\(key)=\(value)".utf8.prefix(255))
            data.append(UInt8(entry.count))
            data.append(contentsOf: entry)
        }
        return data
    }
}
