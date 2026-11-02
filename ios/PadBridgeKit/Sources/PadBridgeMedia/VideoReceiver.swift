import AVFoundation
import CoreMedia
import Foundation
import PadBridgeProtocol

public enum VideoReceiverError: Error {
    case coreMedia(OSStatus)
    case missingParameterSets
}

/// Display mode, iPad side: turns `CodecParams` / `VideoFrame` messages into
/// sample buffers for an `AVSampleBufferDisplayLayer`, which decodes them in
/// hardware and shows them immediately.
///
/// `handle` may be called from any single queue.
public final class VideoReceiver: @unchecked Sendable {
    public let layer = AVSampleBufferDisplayLayer()
    /// Called when the receiver cannot continue without a new keyframe.
    public var onNeedKeyframe: (() -> Void)?

    private var format: CMVideoFormatDescription?
    private var waitingForKeyframe = true

    public init() {
        layer.videoGravity = .resizeAspect
        layer.backgroundColor = CGColor(gray: 0, alpha: 1)
    }

    public func handle(_ message: Message) {
        switch message {
        case let .codecParams(codec, data):
            do {
                format = try Self.makeFormat(codec: codec, parameterSets: data)
            } catch {
                format = nil
                requestKeyframe()
            }
        case let .videoFrame(_, keyframe, _, data):
            if waitingForKeyframe && !keyframe { return }
            let renderer = layer.sampleBufferRenderer
            if renderer.status == .failed {
                renderer.flush()
                requestKeyframe()
                return
            }
            guard let format, let sample = try? Self.makeSampleBuffer(data: data, format: format) else {
                requestKeyframe()
                return
            }
            waitingForKeyframe = false
            renderer.enqueue(sample)
        default:
            break
        }
    }

    /// Clears the screen and waits for a fresh keyframe (e.g. after a disconnect).
    public func reset() {
        layer.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
        format = nil
        waitingForKeyframe = true
    }

    private func requestKeyframe() {
        waitingForKeyframe = true
        onNeedKeyframe?()
    }

    /// Builds a format description from parameter sets in the `CodecParams`
    /// layout (each NAL prefixed by a 4-byte big-endian length).
    public static func makeFormat(codec: Codec, parameterSets data: [UInt8]) throws -> CMVideoFormatDescription {
        var sets: [[UInt8]] = []
        var i = 0
        while i + 4 <= data.count {
            let length = data[i..<i + 4].reduce(0) { $0 << 8 | Int($1) }
            guard i + 4 + length <= data.count else { break }
            sets.append(Array(data[i + 4..<i + 4 + length]))
            i += 4 + length
        }
        guard !sets.isEmpty else { throw VideoReceiverError.missingParameterSets }

        var format: CMVideoFormatDescription?
        let status: OSStatus = sets.withUnsafeBufferPointers { pointers in
            let sizes = sets.map(\.count)
            switch codec {
            case .h264:
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: nil, parameterSetCount: sets.count, parameterSetPointers: pointers,
                    parameterSetSizes: sizes, nalUnitHeaderLength: 4, formatDescriptionOut: &format)
            case .hevc:
                return CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: nil, parameterSetCount: sets.count, parameterSetPointers: pointers,
                    parameterSetSizes: sizes, nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &format)
            }
        }
        guard status == noErr, let format else { throw VideoReceiverError.coreMedia(status) }
        return format
    }

    /// Wraps AVCC frame data in a sample buffer marked for immediate display.
    public static func makeSampleBuffer(data: [UInt8], format: CMVideoFormatDescription) throws -> CMSampleBuffer {
        guard !data.isEmpty else { throw VideoReceiverError.coreMedia(kCMBlockBufferEmptyBBufErr) }
        var block: CMBlockBuffer?
        var status = CMBlockBufferCreateWithMemoryBlock(
            allocator: nil, memoryBlock: nil, blockLength: data.count, blockAllocator: nil,
            customBlockSource: nil, offsetToData: 0, dataLength: data.count,
            flags: kCMBlockBufferAssureMemoryNowFlag, blockBufferOut: &block)
        guard status == noErr, let block else { throw VideoReceiverError.coreMedia(status) }
        status = data.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: data.count)
        }
        guard status == noErr else { throw VideoReceiverError.coreMedia(status) }

        var sample: CMSampleBuffer?
        var size = data.count
        status = CMSampleBufferCreateReady(
            allocator: nil, dataBuffer: block, formatDescription: format, sampleCount: 1,
            sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1,
            sampleSizeArray: &size, sampleBufferOut: &sample)
        guard status == noErr, let sample else { throw VideoReceiverError.coreMedia(status) }

        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(dict,
                                 Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }
}

private extension Array where Element == [UInt8] {
    /// Calls `body` with stable pointers to every element's bytes.
    func withUnsafeBufferPointers<R>(_ body: ([UnsafePointer<UInt8>]) -> R) -> R {
        func recurse(_ index: Int, _ acc: [UnsafePointer<UInt8>]) -> R {
            if index == count { return body(acc) }
            return self[index].withUnsafeBufferPointer { recurse(index + 1, acc + [$0.baseAddress!]) }
        }
        return recurse(0, [])
    }
}
