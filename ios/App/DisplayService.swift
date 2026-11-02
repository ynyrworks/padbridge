import Foundation
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

    private var server: Server?

    func start() {
        guard server == nil else { return }
        let server = Server(role: .displaySink, name: UIDevice.current.name, port: Wire.displayPort)
        let info = Self.displayInfo()
        server.onEvent = { [weak self] event in
            // Runs on the server queue; reply there, then hop to the main actor for UI.
            if case let .connected(peer) = event { peer.send(info) }
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
            if case let .log(level, text) = message {
                append("[host L\(level)] \(text)")
            } else {
                append("收到：\(message)")
            }
        case let .disconnected(peer, error):
            peers.removeAll()  // M2 supports a single host; refined once streaming lands.
            append("中斷：\(peer.remoteDescription) \(error.map { "\($0)" } ?? "")")
        case let .failed(error):
            status = "監聽失敗：\(error)"
        }
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
