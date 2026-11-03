import CryptoKit
import Foundation
import Network
import PadBridgeProtocol

/// One encrypted PadBridge connection over TCP (Wi‑Fi or a usbmux tunnel).
///
/// Before `onReady` the connection runs the secure handshake (see
/// `Secure.swift`); nothing is sent or delivered until the peer is
/// authenticated and, if new, paired. All callbacks run on the queue passed
/// to `init`.
public final class PeerConnection: @unchecked Sendable {
    public let connection: NWConnection
    public var onReady: (() -> Void)?
    public var onMessage: ((Message) -> Void)?
    public var onClose: ((Error?) -> Void)?

    /// The authenticated peer, once `onReady` has fired.
    public private(set) var peerName: String?
    public private(set) var peerKey: [UInt8]?

    private enum Stage { case connecting, hello, intro, deciding, verdict, open }

    private let queue: DispatchQueue
    private let config: SecureConfig
    private var stage = Stage.connecting
    private var ephemeral = Curve25519.KeyAgreement.PrivateKey()
    private var keys: SessionKeys?
    private var sender: RecordCipher?
    private var receiver: RecordCipher?
    private var splitter = RecordSplitter()
    private var decoder = FrameDecoder()
    private var closed = false

    public init(connection: NWConnection, queue: DispatchQueue, security: SecureConfig) {
        self.connection = connection
        self.queue = queue
        config = security
    }

    public convenience init(host: String, port: UInt16, queue: DispatchQueue, security: SecureConfig) {
        let connection = NWConnection(host: NWEndpoint.Host(host), port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        self.init(connection: connection, queue: queue, security: security)
    }

    public var remoteDescription: String { "\(connection.endpoint)" }

    private var isHost: Bool {
        if case .host = config.role { return true }
        return false
    }

    public func start() {
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready: self?.beginHandshake()
            case let .failed(error): self?.close(error)
            case .cancelled: self?.close()
            default: break
            }
        }
        connection.start(queue: queue)
        receive()
    }

    /// Messages handed to the network stack but not yet sent. Only touch on the queue.
    public private(set) var pendingSends = 0

    /// Must be called on the queue passed to `init`. Dropped until the peer is authenticated.
    public func send(_ message: Message, then completion: (() -> Void)? = nil) {
        guard stage == .open, let frame = try? message.frame() else { return }
        sendRecord(frame, then: completion)
    }

    /// Sends `Bye` and closes once it has been handed to the network stack.
    public func sayGoodbye() {
        guard stage == .open else { return close() }
        send(.bye) { [weak self] in self?.close() }
    }

    public func close(_ error: Error? = nil) {
        guard !closed else { return }
        closed = true
        connection.cancel()
        onClose?(error)
        onReady = nil
        onMessage = nil
        onClose = nil
    }

    // MARK: Handshake

    private func beginHandshake() {
        stage = .hello
        sendRaw(Handshake.hello(identity: config.identity, ephemeral: ephemeral))
    }

    private func sendRaw(_ bytes: [UInt8], then completion: (() -> Void)? = nil) {
        guard !closed else { return }
        pendingSends += 1
        connection.send(content: Data(bytes), completion: .contentProcessed { [weak self] error in
            guard let self else { return }
            pendingSends -= 1
            if let error { close(error) } else { completion?() }
        })
    }

    private func sendRecord(_ plaintext: [UInt8], then completion: (() -> Void)? = nil) {
        guard !closed, var cipher = sender, let record = try? cipher.seal(plaintext) else { return }
        sender = cipher
        sendRaw(record, then: completion)
    }

    private func process() throws {
        if stage == .hello {
            guard let hello = splitter.take(SecureWire.helloLength) else { return }
            let peer = try Handshake.parseHello(hello)
            let keys = try Handshake.deriveKeys(host: isHost, identity: config.identity, ephemeral: ephemeral,
                                                peerStatic: peer.static, peerEphemeral: peer.ephemeral)
            self.keys = keys
            peerKey = peer.static
            sender = RecordCipher(key: isHost ? keys.hostToIpad : keys.ipadToHost)
            receiver = RecordCipher(key: isHost ? keys.ipadToHost : keys.hostToIpad)
            ephemeral = Curve25519.KeyAgreement.PrivateKey()  // forget the used one
            stage = .intro
            sendRecord(Handshake.introRecord(name: config.name))
        }
        while !closed, stage != .hello, let record = try splitter.nextRecord() {
            guard var cipher = receiver else { throw SecureError.decrypt }
            let plain = try cipher.open(record)
            receiver = cipher
            try handle(record: plain)
        }
    }

    private func handle(record plain: [UInt8]) throws {
        switch stage {
        case .intro:
            let name = try Handshake.parseIntro(plain)
            peerName = name
            guard let key = peerKey, let keys else { throw SecureError.malformed("state") }
            let request = PairingRequest(peerName: name, peerKey: key, code: keys.code)
            switch config.role {
            case let .device(isPaired, requestPairing, deferPairing, onPaired):
                if isPaired(key) {
                    accept()
                } else if let requestPairing {
                    stage = .deciding
                    requestPairing(request) { [weak self] approved in
                        guard let self else { return }
                        queue.async { [weak self] in
                            guard let self, !self.closed, self.stage == .deciding else { return }
                            if approved {
                                onPaired(key, name)
                                self.accept()
                            } else {
                                self.refuse(.rejected)
                            }
                        }
                    }
                } else if let deferPairing {
                    deferPairing(request)
                    refuse(.pendingApproval)
                } else {
                    refuse(.notPaired)
                }
            case let .host(isKnown, confirm):
                guard isKnown(key) || confirm(request) else { return close() }
                stage = .verdict
            }
        case .verdict:
            guard plain.count == 2, plain[0] == SecureWire.verdict else { throw SecureError.malformed("verdict") }
            guard plain[1] == Verdict.accepted.rawValue else {
                throw SecureError.refused(Verdict(rawValue: plain[1]) ?? .rejected)
            }
            open()
        case .open:
            decoder.push(plain)
            while let message = try decoder.nextMessage() { onMessage?(message) }
        case .deciding:
            // The host sends nothing until it has our verdict.
            throw SecureError.malformed("record before verdict")
        case .connecting, .hello:
            throw SecureError.malformed("record before hello")
        }
    }

    private func accept() {
        sendRecord([SecureWire.verdict, Verdict.accepted.rawValue])
        open()
    }

    private func refuse(_ verdict: Verdict) {
        sendRecord([SecureWire.verdict, verdict.rawValue]) { [weak self] in self?.close(SecureError.refused(verdict)) }
    }

    private func open() {
        stage = .open
        onReady?()
    }

    private func receive() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 1 << 16) { [weak self] data, _, isComplete, error in
            guard let self, !closed else { return }
            if let data, !data.isEmpty {
                splitter.push(data)
                do {
                    try process()
                } catch {
                    close(error)
                    return
                }
            }
            if let error {
                close(error)
            } else if isComplete {
                close()
            } else {
                receive()
            }
        }
    }
}
