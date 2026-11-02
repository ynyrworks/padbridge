import PadBridgeNet
import PadBridgeProtocol
import ReplayKit
import UIKit

/// Mirror-mode broadcast upload extension. Listens on port 47801 while the
/// broadcast runs; the Windows host connects to it directly. No App Group is
/// used, so it works with free Apple ID provisioning.
///
/// M2: handshake only. Encoding samples with VideoToolbox comes in M3.
final class SampleHandler: RPBroadcastSampleHandler {
    private var server: Server?
    private var frameCount: UInt64 = 0

    override func broadcastStarted(withSetupInfo setupInfo: [String: NSObject]?) {
        let server = Server(role: .mirrorSource, name: "\(UIDevice.current.name) Mirror", port: Wire.mirrorPort)
        server.onEvent = { event in
            if case let .connected(peer) = event {
                peer.send(.log(level: 1, text: "mirror source ready; video arrives in M3"))
            }
        }
        do {
            try server.start()
            self.server = server
        } catch {
            finishBroadcastWithError(error)
        }
    }

    override func processSampleBuffer(_ sampleBuffer: CMSampleBuffer, with sampleBufferType: RPSampleBufferType) {
        guard sampleBufferType == .video else { return }
        frameCount += 1
    }

    override func broadcastFinished() {
        server?.stop()
        server = nil
    }
}
