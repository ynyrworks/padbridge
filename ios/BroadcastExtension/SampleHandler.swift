import ImageIO
import PadBridgeMedia
import PadBridgeNet
import PadBridgeProtocol
import ReplayKit
import UIKit

/// Mirror-mode broadcast upload extension. Listens on port 47801 while the
/// broadcast runs; the Windows host connects to it directly.
///
/// Computers paired in the app are let in when an App Group shares the
/// pairings (not available with free Apple ID signing); otherwise a new
/// computer is approved by restarting the broadcast (`BroadcastApproval`).
///
/// Extensions are limited to roughly 50 MB of memory, so frames go straight
/// into the hardware encoder and are capped at 1920 px on the long edge.
final class SampleHandler: RPBroadcastSampleHandler {
    private var streamer: MirrorStreamer?

    override func broadcastStarted(withSetupInfo setupInfo: [String: NSObject]?) {
        // Starting again soon after an unknown computer asked counts as approving it.
        let approval = BroadcastApproval()
        approval.approvePending(into: PairingStore.shared)
        let streamer = MirrorStreamer(name: "\(UIDevice.current.name) Mirror")
        streamer.server.onEvent = { event in
            if case let .connected(peer) = event {
                peer.send(.log(level: 1, text: "mirror source ready"))
            }
        }
        // No UI here: show the code by ending the broadcast; the computer keeps retrying.
        streamer.server.onUnknownHost = { [weak self] request in
            approval.recordPending(request)
            let error = NSError(domain: "PadBridge", code: 1,
                                userInfo: [NSLocalizedDescriptionKey: BroadcastApproval.message(for: request)])
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { self?.finishBroadcastWithError(error) }
        }
        do {
            try streamer.start()
            self.streamer = streamer
        } catch {
            finishBroadcastWithError(error)
        }
    }

    override func processSampleBuffer(_ sampleBuffer: CMSampleBuffer, with sampleBufferType: RPSampleBufferType) {
        guard sampleBufferType == .video, let streamer,
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        try? streamer.process(pixelBuffer, pts: pts, rotation: Self.rotation(of: sampleBuffer))
    }

    override func broadcastFinished() {
        streamer?.stop()
        streamer = nil
    }

    /// Clockwise quarter turns needed to show the frame upright, from the
    /// orientation ReplayKit attaches to each buffer. ReplayKit's value is the
    /// opposite of the EXIF meaning (verified on an iPad: `.left` needs a
    /// clockwise turn), hence `.left` → 1 and `.right` → 3.
    static func rotation(of sampleBuffer: CMSampleBuffer) -> UInt8 {
        let value = CMGetAttachment(sampleBuffer, key: RPVideoSampleOrientationKey as CFString, attachmentModeOut: nil)
        guard let raw = (value as? NSNumber)?.uint32Value,
              let orientation = CGImagePropertyOrientation(rawValue: raw) else { return 0 }
        switch orientation {
        case .left, .leftMirrored: return 1
        case .down, .downMirrored: return 2
        case .right, .rightMirrored: return 3
        default: return 0
        }
    }
}
