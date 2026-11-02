import CoreMedia
import CoreVideo
import Foundation
import PadBridgeProtocol
import VideoToolbox

public struct EncodedFrame: Sendable {
    public let ptsUs: UInt64
    public let keyframe: Bool
    public let rotation: UInt8
    public let width: Int
    public let height: Int
    /// AVCC NAL units (4-byte big-endian lengths).
    public let data: [UInt8]
    /// Parameter sets in the `CodecParams` layout; present on keyframes.
    public let parameterSets: [UInt8]?
}

public enum VideoEncoderError: Error {
    case videoToolbox(OSStatus)
    case pixelBufferPool(CVReturn)
}

/// Low-latency hardware video encoder (VideoToolbox). Frames larger than
/// `maxLongEdge` are scaled down first. Not thread-safe: call `encode` from one
/// thread at a time. `onFrame` runs on a VideoToolbox thread.
public final class VideoEncoder: @unchecked Sendable {
    public struct Settings: Sendable {
        public var codec: Codec = .h264
        public var maxLongEdge = 1920
        public var fps = 60
        public var bitrateKbps = 12_000

        public init() {}
    }

    public let settings: Settings
    public var onFrame: ((EncodedFrame) -> Void)?

    private var session: VTCompressionSession?
    private var sessionSize = (width: 0, height: 0)
    private var transfer: VTPixelTransferSession?
    private var pool: CVPixelBufferPool?
    private var poolKey = (width: 0, height: 0, format: OSType(0))
    private let lock = NSLock()
    private var forceKeyframe = true

    public init(settings: Settings = Settings()) {
        self.settings = settings
    }

    deinit {
        invalidateSession()
        if let transfer { VTPixelTransferSessionInvalidate(transfer) }
    }

    /// Makes the next encoded frame a keyframe. Safe to call from any thread.
    public func requestKeyframe() {
        lock.withLock { forceKeyframe = true }
    }

    public func encode(_ pixelBuffer: CVPixelBuffer, pts: CMTime, rotation: UInt8 = 0) throws {
        let source = (width: CVPixelBufferGetWidth(pixelBuffer), height: CVPixelBufferGetHeight(pixelBuffer))
        let target = Self.targetSize(width: source.width, height: source.height, maxLongEdge: settings.maxLongEdge)
        if session == nil || sessionSize != target {
            try makeSession(width: target.width, height: target.height)
        }
        let input = source == target ? pixelBuffer : try scale(pixelBuffer, to: target)

        let force = lock.withLock {
            defer { forceKeyframe = false }
            return forceKeyframe
        }
        let properties = force ? [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue] as CFDictionary : nil
        let size = target
        let status = VTCompressionSessionEncodeFrame(
            session!, imageBuffer: input, presentationTimeStamp: pts, duration: .invalid,
            frameProperties: properties, infoFlagsOut: nil
        ) { [weak self] status, _, sample in
            guard status == noErr, let sample, let self else { return }
            if let frame = Self.makeFrame(sample, codec: settings.codec, rotation: rotation, size: size) {
                onFrame?(frame)
            }
        }
        guard status == noErr else { throw VideoEncoderError.videoToolbox(status) }
    }

    /// Blocks until every submitted frame has been emitted through `onFrame`.
    public func flush() {
        guard let session else { return }
        VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
    }

    /// Scales to fit `maxLongEdge`, keeping aspect ratio and even dimensions.
    static func targetSize(width: Int, height: Int, maxLongEdge: Int) -> (width: Int, height: Int) {
        let scale = min(1, Double(maxLongEdge) / Double(max(width, height)))
        let even = { (v: Double) in max(2, Int(v.rounded()) & ~1) }
        return (even(Double(width) * scale), even(Double(height) * scale))
    }

    private func makeSession(width: Int, height: Int) throws {
        invalidateSession()
        var specification: CFDictionary?
        #if os(macOS)
        specification = [kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: true] as CFDictionary
        #endif
        var newSession: VTCompressionSession?
        var status = VTCompressionSessionCreate(
            allocator: nil, width: Int32(width), height: Int32(height),
            codecType: settings.codec == .hevc ? kCMVideoCodecType_HEVC : kCMVideoCodecType_H264,
            encoderSpecification: specification, imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: nil, refcon: nil, compressionSessionOut: &newSession)
        guard status == noErr, let newSession else { throw VideoEncoderError.videoToolbox(status) }

        // Constrained Baseline: the Windows viewer currently decodes with OpenH264,
        // which only guarantees that profile.
        let profile = settings.codec == .hevc
            ? kVTProfileLevel_HEVC_Main_AutoLevel : kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel
        let properties: [CFString: Any] = [
            kVTCompressionPropertyKey_RealTime: true,
            kVTCompressionPropertyKey_AllowFrameReordering: false,
            kVTCompressionPropertyKey_ProfileLevel: profile,
            kVTCompressionPropertyKey_AverageBitRate: settings.bitrateKbps * 1000,
            kVTCompressionPropertyKey_ExpectedFrameRate: settings.fps,
            kVTCompressionPropertyKey_MaxKeyFrameInterval: settings.fps * 10,
            kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: 10,
        ]
        for (key, value) in properties {
            status = VTSessionSetProperty(newSession, key: key, value: value as CFTypeRef)
            guard status == noErr else { throw VideoEncoderError.videoToolbox(status) }
        }
        // Optional: not every encoder supports it.
        VTSessionSetProperty(newSession, key: kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, value: kCFBooleanTrue)
        VTCompressionSessionPrepareToEncodeFrames(newSession)

        session = newSession
        sessionSize = (width, height)
        requestKeyframe()
    }

    private func invalidateSession() {
        guard let session else { return }
        VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
        VTCompressionSessionInvalidate(session)
        self.session = nil
    }

    private func scale(_ source: CVPixelBuffer, to size: (width: Int, height: Int)) throws -> CVPixelBuffer {
        if transfer == nil {
            var newTransfer: VTPixelTransferSession?
            let status = VTPixelTransferSessionCreate(allocator: nil, pixelTransferSessionOut: &newTransfer)
            guard status == noErr else { throw VideoEncoderError.videoToolbox(status) }
            transfer = newTransfer
        }
        let format = CVPixelBufferGetPixelFormatType(source)
        if pool == nil || poolKey != (size.width, size.height, format) {
            let attributes: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: format,
                kCVPixelBufferWidthKey: size.width,
                kCVPixelBufferHeightKey: size.height,
                kCVPixelBufferIOSurfacePropertiesKey: [:] as [CFString: Any],
            ]
            var newPool: CVPixelBufferPool?
            let result = CVPixelBufferPoolCreate(nil, nil, attributes as CFDictionary, &newPool)
            guard result == kCVReturnSuccess else { throw VideoEncoderError.pixelBufferPool(result) }
            pool = newPool
            poolKey = (size.width, size.height, format)
        }
        var output: CVPixelBuffer?
        let result = CVPixelBufferPoolCreatePixelBuffer(nil, pool!, &output)
        guard result == kCVReturnSuccess, let output else { throw VideoEncoderError.pixelBufferPool(result) }
        let status = VTPixelTransferSessionTransferImage(transfer!, from: source, to: output)
        guard status == noErr else { throw VideoEncoderError.videoToolbox(status) }
        return output
    }

    private static func makeFrame(_ sample: CMSampleBuffer, codec: Codec, rotation: UInt8,
                                  size: (width: Int, height: Int)) -> EncodedFrame? {
        guard CMSampleBufferDataIsReady(sample), let block = CMSampleBufferGetDataBuffer(sample) else { return nil }
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]]
        let notSync = attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool ?? false

        var data = [UInt8](repeating: 0, count: CMBlockBufferGetDataLength(block))
        guard CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: data.count, destination: &data) == noErr else {
            return nil
        }
        let parameterSets = notSync ? nil : CMSampleBufferGetFormatDescription(sample).map { parameterSetBytes($0, codec: codec) }
        let pts = CMSampleBufferGetPresentationTimeStamp(sample)
        let ptsUs = pts.isNumeric ? UInt64(max(0, CMTimeGetSeconds(pts) * 1_000_000)) : 0
        return EncodedFrame(ptsUs: ptsUs, keyframe: !notSync, rotation: rotation,
                            width: size.width, height: size.height, data: data, parameterSets: parameterSets)
    }

    /// VPS/SPS/PPS, each prefixed with a 4-byte big-endian length.
    static func parameterSetBytes(_ format: CMFormatDescription, codec: Codec) -> [UInt8] {
        typealias Getter = (CMFormatDescription, Int, UnsafeMutablePointer<UnsafePointer<UInt8>?>?,
                            UnsafeMutablePointer<Int>?, UnsafeMutablePointer<Int>?, UnsafeMutablePointer<Int32>?) -> OSStatus
        let get: Getter = codec == .hevc
            ? { CMVideoFormatDescriptionGetHEVCParameterSetAtIndex($0, parameterSetIndex: $1, parameterSetPointerOut: $2,
                                                                   parameterSetSizeOut: $3, parameterSetCountOut: $4,
                                                                   nalUnitHeaderLengthOut: $5) }
            : { CMVideoFormatDescriptionGetH264ParameterSetAtIndex($0, parameterSetIndex: $1, parameterSetPointerOut: $2,
                                                                   parameterSetSizeOut: $3, parameterSetCountOut: $4,
                                                                   nalUnitHeaderLengthOut: $5) }
        var count = 0
        guard get(format, 0, nil, nil, &count, nil) == noErr else { return [] }
        var out: [UInt8] = []
        for index in 0..<count {
            var pointer: UnsafePointer<UInt8>?
            var size = 0
            guard get(format, index, &pointer, &size, nil, nil) == noErr, let pointer else { continue }
            withUnsafeBytes(of: UInt32(size).bigEndian) { out.append(contentsOf: $0) }
            out.append(contentsOf: UnsafeBufferPointer(start: pointer, count: size))
        }
        return out
    }
}
