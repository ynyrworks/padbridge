import Foundation
import Network
import PadBridgeProtocol

/// One framed PadBridge connection over TCP (Wi‑Fi or a usbmux tunnel).
///
/// All callbacks run on the queue passed to `init`.
public final class PeerConnection: @unchecked Sendable {
    public let connection: NWConnection
    public var onReady: (() -> Void)?
    public var onMessage: ((Message) -> Void)?
    public var onClose: ((Error?) -> Void)?

    private let queue: DispatchQueue
    private var decoder = FrameDecoder()
    private var closed = false

    public init(connection: NWConnection, queue: DispatchQueue) {
        self.connection = connection
        self.queue = queue
    }

    public convenience init(host: String, port: UInt16, queue: DispatchQueue) {
        let connection = NWConnection(host: NWEndpoint.Host(host), port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        self.init(connection: connection, queue: queue)
    }

    public var remoteDescription: String { "\(connection.endpoint)" }

    public func start() {
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready: self?.onReady?()
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

    /// Must be called on the queue passed to `init`.
    public func send(_ message: Message, then completion: (() -> Void)? = nil) {
        guard !closed, let frame = try? message.frame() else { return }
        pendingSends += 1
        connection.send(content: Data(frame), completion: .contentProcessed { [weak self] error in
            guard let self else { return }
            pendingSends -= 1
            if let error { close(error) } else { completion?() }
        })
    }

    /// Sends `Bye` and closes once it has been handed to the network stack.
    public func sayGoodbye() {
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

    private func receive() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 1 << 16) { [weak self] data, _, isComplete, error in
            guard let self, !closed else { return }
            if let data, !data.isEmpty {
                decoder.push(data)
                do {
                    while let message = try decoder.nextMessage() { onMessage?(message) }
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
