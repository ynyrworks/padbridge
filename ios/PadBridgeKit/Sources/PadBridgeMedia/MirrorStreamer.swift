import CoreMedia
import CoreVideo
import Foundation
import PadBridgeNet
import PadBridgeProtocol

/// Mirror mode: encodes captured frames and streams them to every connected
/// host. Used by the broadcast extension; free of ReplayKit so it can be
/// tested on macOS.
public final class MirrorStreamer: @unchecked Sendable {
    public let server: Server
    private let encoder: VideoEncoder
    private let queue = DispatchQueue(label: "padbridge.mirror")
    private var lastFrame: (buffer: CVPixelBuffer, rotation: UInt8)?

    public init(name: String, port: UInt16 = Wire.mirrorPort, advertise: Bool = true,
                settings: VideoEncoder.Settings = VideoEncoder.Settings()) {
        server = Server(role: .mirrorSource, name: name, port: port, advertise: advertise)
        encoder = VideoEncoder(settings: settings)

        encoder.onFrame = { [server, settings] frame in
            let video = Message.videoFrame(ptsUs: frame.ptsUs, keyframe: frame.keyframe, rotation: frame.rotation, data: frame.data)
            guard frame.keyframe else {
                server.broadcastVideo([video], keyframe: false)
                return
            }
            server.broadcastVideo([
                .streamConfig(codec: settings.codec, width: UInt32(frame.width), height: UInt32(frame.height),
                              fps: UInt16(settings.fps), bitrateKbps: UInt32(settings.bitrateKbps)),
                .codecParams(codec: settings.codec, data: frame.parameterSets ?? []),
                video,
            ], keyframe: true)
        }
        // The screen may be static, so no new frame would arrive to carry the
        // keyframe: re-encode the last one.
        server.onKeyframeNeeded = { [weak self] in
            guard let self else { return }
            self.encoder.requestKeyframe()
            self.queue.async {
                guard let last = self.lastFrame else { return }
                try? self.encoder.encode(last.buffer, pts: CMClockGetTime(CMClockGetHostTimeClock()), rotation: last.rotation)
            }
        }
    }

    public func start() throws {
        try server.start()
    }

    public func stop() {
        server.stop()
        queue.sync { lastFrame = nil }
    }

    /// Encodes one captured frame. Blocks briefly while the frame is submitted.
    public func process(_ pixelBuffer: CVPixelBuffer, pts: CMTime, rotation: UInt8) throws {
        try queue.sync {
            lastFrame = (pixelBuffer, rotation)
            try encoder.encode(pixelBuffer, pts: pts, rotation: rotation)
        }
    }
}
