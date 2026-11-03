import Foundation
import PadBridgeMedia
import PadBridgeNet
import PadBridgeProtocol
import UIKit

/// Runs the Display-mode server (port 47800) and publishes its state to the UI.
@MainActor
final class DisplayService: ObservableObject {
    struct LogLine: Identifiable {
        let id = UUID()
        let time = Date()
        let text: String
    }

    @Published private(set) var status = "尚未啟動"
    @Published private(set) var peers: [String] = []
    @Published private(set) var log: [LogLine] = []
    /// Set while a host is streaming its screen to us; the UI then shows it full screen.
    @Published private(set) var streamInfo: String?
    /// Size of the video being shown, for mapping touches onto the Windows screen.
    @Published private(set) var streamSize: CGSize = .zero

    /// A computer asking to pair, waiting for the user's answer.
    struct PendingPairing: Identifiable {
        let id = UUID()
        let request: PairingRequest
        let peer: PeerConnection
        let respond: (Bool) -> Void
    }

    @Published var pendingPairing: PendingPairing?
    @Published private(set) var pairedHosts: [PairingStore.Host] = []
    /// False when the App Group isn't available: the broadcast extension then
    /// can't see pairings, so Mirror refuses every computer.
    let pairingShared = PairingStore.shared.isShared

    let receiver = VideoReceiver()
    let cursorLayer = CursorLayer()
    private var server: Server?

    func refreshPairedHosts() {
        pairedHosts = PairingStore.shared.hosts.sorted { $0.pairedAt > $1.pairedAt }
    }

    func unpair(_ host: PairingStore.Host) {
        PairingStore.shared.remove(key: host.key)
        append("已取消與「\(host.name)」的配對")
        refreshPairedHosts()
    }

    func answerPairing(_ approved: Bool) {
        guard let pending = pendingPairing else { return }
        pendingPairing = nil
        pending.respond(approved)
        if approved {
            append("已與「\(pending.request.peerName)」配對")
            refreshPairedHosts()
        }
    }

    func start() {
        guard server == nil else { return }
        refreshPairedHosts()
        let server = Server(role: .displaySink, name: UIDevice.current.name, port: Wire.displayPort)
        server.onPairingRequest = { [weak self] peer, request, respond in
            Task { @MainActor in
                guard let self else { return respond(false) }
                // One at a time: a second request while one is shown is refused.
                guard self.pendingPairing == nil else { return respond(false) }
                self.pendingPairing = PendingPairing(request: request, peer: peer, respond: respond)
            }
        }
        let info = Self.displayInfo()
        let receiver = receiver
        let cursorLayer = cursorLayer
        receiver.onNeedKeyframe = { [weak server] in server?.broadcast(.requestKeyframe) }
        server.onEvent = { [weak self] event in
            // Runs on the server queue: reply and feed video here, hop to the main actor for UI.
            switch event {
            case let .connected(peer):
                peer.send(info)
            case let .message(_, message):
                switch message {
                case .codecParams, .videoFrame:
                    receiver.handle(message)
                    return
                case let .cursor(x, y, visible):
                    // Frequent: update the layer directly instead of going through SwiftUI.
                    DispatchQueue.main.async { cursorLayer.update(x: x, y: y, visible: visible) }
                    return
                case .streamConfig:
                    receiver.handle(message)
                default:
                    break
                }
            case let .disconnected(peer, _):
                receiver.reset()
                // The computer gave up (or was declined there): drop its pairing prompt.
                Task { @MainActor in
                    if let pending = self?.pendingPairing, pending.peer === peer {
                        self?.pendingPairing = nil
                        pending.respond(false)
                    }
                }
                DispatchQueue.main.async { cursorLayer.update(x: 0, y: 0, visible: false) }
            default:
                break
            }
            Task { @MainActor in self?.handle(event) }
        }
        do {
            try server.start()
            self.server = server
            status = "啟動中…"
        } catch {
            status = "無法啟動：\(error.localizedDescription)"
        }
    }

    private func handle(_ event: Server.Event) {
        switch event {
        case let .ready(port):
            status = "等待連線（埠 \(port)，Wi‑Fi 與 USB）"
        case let .connected(peer):
            append("連線：\(peer.remoteDescription)")
        case let .hello(peer, name, role):
            peers.append(name)
            append("Hello：\(name)（\(role)）@ \(peer.remoteDescription)")
        case let .message(_, message):
            switch message {
            case let .log(level, text):
                append("[host L\(level)] \(text)")
            case let .streamConfig(codec, width, height, fps, _):
                if streamInfo == nil { append("開始顯示：\(codec) \(width)×\(height) @\(fps)") }
                streamInfo = "\(width)×\(height)"
                streamSize = CGSize(width: Int(width), height: Int(height))
            default:
                append("收到：\(message)")
            }
        case let .disconnected(peer, error):
            peers.removeAll()  // A single host at a time for now.
            streamInfo = nil
            append("中斷：\(peer.remoteDescription) \(error.map { "\($0)" } ?? "")")
        case let .failed(error):
            status = "監聽失敗：\(error)"
        }
    }

    /// Sends touch / Pencil / trackpad input to the connected host.
    func sendPointer(_ message: Message) {
        server?.broadcast(message)
    }

    private func append(_ text: String) {
        log.insert(LogLine(text: text), at: 0)
        if log.count > 200 { log.removeLast() }
    }

    private static func displayInfo() -> Message {
        let screen = UIScreen.main
        let size = screen.nativeBounds.size
        // nativeBounds is portrait-oriented; report landscape since that's how a monitor is used.
        return .displayInfo(
            widthPx: UInt32(max(size.width, size.height)),
            heightPx: UInt32(min(size.width, size.height)),
            scaleMilli: UInt16(screen.nativeScale * 1000),
            maxFps: UInt16(screen.maximumFramesPerSecond),
            codecs: Codec.h264.maskBit | Codec.hevc.maskBit)
    }
}
