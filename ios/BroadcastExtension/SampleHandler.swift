import ImageIO
import PadBridgeMedia
import PadBridgeProtocol
import ReplayKit
import UIKit

/// Mirror-mode broadcast upload extension. Listens on port 47801 while the
/// broadcast runs; the Windows host connects to it directly. No App Group is
/// used, so it works with free Apple ID provisioning.
///
/// Extensions are limited to roughly 50 MB of memory, so frames go straight
/// into the hardware encoder and are capped at 1920 px on the long edge.
final class SampleHandler: RPBroadcastSampleHandler {
    private var streamer: MirrorStreamer?

    override func broadcastStarted(withSetupInfo setupInfo: [String: NSObject]?) {
        let streamer = MirrorStreamer(name: "\(UIDevice.current.name) Mirror")
        streamer.server.onEvent = { event in
            if case let .connected(peer) = event {
                peer.send(.log(level: 1, text: "mirror source ready"))
            }
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
