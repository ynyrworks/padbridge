import Foundation
import Network
import PadBridgeProtocol

/// TCP listener on the iPad side. The Windows host always connects to us, over
/// Wi‑Fi (found via Bonjour) or through the usbmux tunnel to the same port.
///
/// Every connection is encrypted and authenticated first (`PeerConnection`):
/// hosts in `store` are let in; a new host is only accepted if
/// `onPairingRequest` is set and the user approves it.
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
    public let store: PairingStore
    public var onEvent: ((Event) -> Void)?
    /// Asks the user whether to pair with an unknown host; `respond` may be
    /// called later from any thread. When nil, unknown hosts are refused.
    public var onPairingRequest: ((PeerConnection, PairingRequest, @escaping (Bool) -> Void) -> Void)?

    private let requestedPort: UInt16
    private let advertise: Bool
    private let queue = DispatchQueue(label: "padbridge.server")
    private var listener: NWListener?
    private var peers: [PeerConnection] = []

    /// `port` 0 picks any free port. `advertise` publishes a Bonjour service.
    public init(role: Role, name: String, port: UInt16, advertise: Bool = true, store: PairingStore = .shared) {
        self.role = role
        self.name = name
        self.store = store
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

    /// Called (on the server queue) when a greeted peer needs a keyframe: it just
    /// said Hello, sent `RequestKeyframe`, or fell behind and had frames dropped.
    public var onKeyframeNeeded: (() -> Void)?

    /// A peer with this many unsent messages is considered backed up.
    public var maxPendingSends = 6

    /// Sends one encoded frame (plus, for keyframes, its preceding config
    /// messages) to every greeted peer. Peers that are backed up or waiting
    /// for a keyframe skip delta frames instead of queueing them, which keeps
    /// latency bounded on slow links.
    public func broadcastVideo(_ messages: [Message], keyframe: Bool) {
        queue.async { [self] in
            var needKeyframe = false
            for peer in peers where greeted.contains(ObjectIdentifier(peer)) {
                let id = ObjectIdentifier(peer)
                let backedUp = peer.pendingSends >= maxPendingSends
                if keyframe && peer.pendingSends < maxPendingSends * 2 {
                    awaitingKeyframe.remove(id)
                } else if keyframe || awaitingKeyframe.contains(id) || backedUp {
                    awaitingKeyframe.insert(id)
                    needKeyframe = true
                    continue
                }
                for message in messages { peer.send(message) }
            }
            if keyframe { keyframeRequested = false }
            if needKeyframe { requestKeyframe() }
        }
    }

    private var greeted = Set<ObjectIdentifier>()
    private var awaitingKeyframe = Set<ObjectIdentifier>()
    private var keyframeRequested = false

    private func requestKeyframe() {
        guard !keyframeRequested else { return }
        keyframeRequested = true
        onKeyframeNeeded?()
    }

    private func markAwaitingKeyframe(_ peer: PeerConnection) {
        awaitingKeyframe.insert(ObjectIdentifier(peer))
        keyframeRequested = false
        requestKeyframe()
    }

    private final class WeakPeer { weak var peer: PeerConnection? }

    private func accept(_ connection: NWConnection) {
        let holder = WeakPeer()
        let requestPairing = onPairingRequest.map { handler in
            { (request: PairingRequest, respond: @escaping (Bool) -> Void) in
                guard let peer = holder.peer else { return respond(false) }
                handler(peer, request, respond)
            }
        }
        let security = SecureConfig(identity: store.identity, name: name, role: .device(
            isPaired: { [store] in store.isPaired($0) },
            requestPairing: requestPairing,
            onPaired: { [store] key, name in store.add(key: key, name: name) }))
        let peer = PeerConnection(connection: connection, queue: queue, security: security)
        holder.peer = peer
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
            greeted.remove(ObjectIdentifier(peer))
            awaitingKeyframe.remove(ObjectIdentifier(peer))
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
            greeted.insert(ObjectIdentifier(peer))
            onEvent?(.hello(peer, name: name, role: role))
            markAwaitingKeyframe(peer)
        case .requestKeyframe:
            markAwaitingKeyframe(peer)
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
