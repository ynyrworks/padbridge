import CoreMedia
import CoreVideo
import VideoToolbox
import XCTest
import PadBridgeNet
import PadBridgeProtocol
@testable import PadBridgeMedia

/// Splits AVCC data (4-byte big-endian length prefixes) into NAL units.
func nalUnits(_ data: [UInt8]) -> [[UInt8]]? {
    var units: [[UInt8]] = []
    var i = 0
    while i < data.count {
        guard i + 4 <= data.count else { return nil }
        let length = data[i..<i + 4].reduce(0) { $0 << 8 | Int($1) }
        guard i + 4 + length <= data.count else { return nil }
        units.append(Array(data[i + 4..<i + 4 + length]))
        i += 4 + length
    }
    return units
}

/// A 4:2:0 full-range buffer like ReplayKit delivers, filled with a moving gradient.
func makePixelBuffer(width: Int, height: Int, seed: Int) -> CVPixelBuffer {
    var buffer: CVPixelBuffer?
    let attributes = [kCVPixelBufferIOSurfacePropertiesKey: [:] as [CFString: Any]] as CFDictionary
    CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, attributes, &buffer)
    let pb = buffer!
    CVPixelBufferLockBaseAddress(pb, [])
    for plane in 0..<CVPixelBufferGetPlaneCount(pb) {
        let base = CVPixelBufferGetBaseAddressOfPlane(pb, plane)!.assumingMemoryBound(to: UInt8.self)
        let rowBytes = CVPixelBufferGetBytesPerRowOfPlane(pb, plane)
        for y in 0..<CVPixelBufferGetHeightOfPlane(pb, plane) {
            for x in 0..<rowBytes { base[y * rowBytes + x] = UInt8(truncatingIfNeeded: x + y + seed * 8) }
        }
    }
    CVPixelBufferUnlockBaseAddress(pb, [])
    return pb
}

final class MediaTests: XCTestCase {
    func testTargetSize() {
        XCTAssertTrue(VideoEncoder.targetSize(width: 2732, height: 2048, maxLongEdge: 1920) == (1920, 1438))
        XCTAssertTrue(VideoEncoder.targetSize(width: 2048, height: 2732, maxLongEdge: 1920) == (1438, 1920))
        XCTAssertTrue(VideoEncoder.targetSize(width: 1280, height: 720, maxLongEdge: 1920) == (1280, 720))
    }

    func testEncodesH264WithParameterSets() throws {
        var settings = VideoEncoder.Settings()
        settings.maxLongEdge = 640
        let encoder = VideoEncoder(settings: settings)
        var frames: [EncodedFrame] = []
        let lock = NSLock()
        encoder.onFrame = { frame in lock.withLock { frames.append(frame) } }

        // Larger than maxLongEdge, so this also exercises scaling.
        for i in 0..<5 {
            try encoder.encode(makePixelBuffer(width: 1280, height: 960, seed: i),
                               pts: CMTime(value: CMTimeValue(i), timescale: 60), rotation: 1)
        }
        encoder.flush()

        let got = lock.withLock { frames }
        XCTAssertFalse(got.isEmpty)
        let first = try XCTUnwrap(got.first)
        XCTAssertTrue(first.keyframe)
        XCTAssertEqual(first.width, 640)
        XCTAssertEqual(first.height, 480)
        XCTAssertEqual(first.rotation, 1)
        let parameterSets = try XCTUnwrap(nalUnits(try XCTUnwrap(first.parameterSets)))
        XCTAssertEqual(parameterSets.map { $0[0] & 0x1F }, [7, 8], "expected SPS then PPS")
        for frame in got {
            let units = try XCTUnwrap(nalUnits(frame.data), "frame data is not valid AVCC")
            XCTAssertFalse(units.isEmpty)
        }
        XCTAssertEqual(got.filter(\.keyframe).count, 1)
    }

    func testRequestedKeyframe() throws {
        let encoder = VideoEncoder()
        var keyframes = 0
        let lock = NSLock()
        encoder.onFrame = { frame in lock.withLock { if frame.keyframe { keyframes += 1 } } }
        for i in 0..<4 {
            if i == 2 { encoder.requestKeyframe() }
            try encoder.encode(makePixelBuffer(width: 320, height: 240, seed: i), pts: CMTime(value: CMTimeValue(i), timescale: 60))
        }
        encoder.flush()
        XCTAssertEqual(lock.withLock { keyframes }, 2)
    }

    /// Encoder output, sent through the protocol and rebuilt by `VideoReceiver`,
    /// must decode with VideoToolbox (the same decoder the display layer uses).
    func testReceiverBuildsDecodableSampleBuffers() throws {
        let encoder = VideoEncoder()
        var frames: [EncodedFrame] = []
        let lock = NSLock()
        encoder.onFrame = { frame in lock.withLock { frames.append(frame) } }
        for i in 0..<3 {
            try encoder.encode(makePixelBuffer(width: 640, height: 480, seed: i), pts: CMTime(value: CMTimeValue(i), timescale: 60))
        }
        encoder.flush()
        let encoded = lock.withLock { frames }
        let first = try XCTUnwrap(encoded.first)

        let format = try VideoReceiver.makeFormat(codec: .h264, parameterSets: try XCTUnwrap(first.parameterSets))
        let dimensions = CMVideoFormatDescriptionGetDimensions(format)
        XCTAssertEqual(dimensions.width, 640)
        XCTAssertEqual(dimensions.height, 480)

        var session: VTDecompressionSession?
        XCTAssertEqual(VTDecompressionSessionCreate(allocator: nil, formatDescription: format, decoderSpecification: nil,
                                                    imageBufferAttributes: nil, outputCallback: nil,
                                                    decompressionSessionOut: &session), noErr)
        var decoded = 0
        for frame in encoded {
            let sample = try VideoReceiver.makeSampleBuffer(data: frame.data, format: format)
            let status = VTDecompressionSessionDecodeFrame(try XCTUnwrap(session), sampleBuffer: sample, flags: [],
                                                           infoFlagsOut: nil) { status, _, image, _, _ in
                if status == noErr, let image, CVPixelBufferGetWidth(image) == 640 {
                    lock.withLock { decoded += 1 }
                }
            }
            XCTAssertEqual(status, noErr)
        }
        VTDecompressionSessionWaitForAsynchronousFrames(try XCTUnwrap(session))
        XCTAssertEqual(lock.withLock { decoded }, encoded.count)

        XCTAssertThrowsError(try VideoReceiver.makeFormat(codec: .h264, parameterSets: []))
    }

    /// Frames recorded from the Windows encoder (OpenH264, see
    /// crates/host/examples/display_fixture.rs) must decode on Apple hardware.
    func testDecodesWindowsEncodedStream() throws {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "windows_h264", withExtension: "bin", subdirectory: "Fixtures"))
        var decoder = FrameDecoder()
        decoder.push([UInt8](try Data(contentsOf: url)))
        var format: CMVideoFormatDescription?
        var samples: [CMSampleBuffer] = []
        while let message = try decoder.nextMessage() {
            switch message {
            case let .codecParams(codec, data):
                format = try VideoReceiver.makeFormat(codec: codec, parameterSets: data)
            case let .videoFrame(_, _, _, data):
                samples.append(try VideoReceiver.makeSampleBuffer(data: data, format: try XCTUnwrap(format)))
            default:
                break
            }
        }
        XCTAssertEqual(samples.count, 3)

        var session: VTDecompressionSession?
        XCTAssertEqual(VTDecompressionSessionCreate(allocator: nil, formatDescription: try XCTUnwrap(format),
                                                    decoderSpecification: nil, imageBufferAttributes: nil,
                                                    outputCallback: nil, decompressionSessionOut: &session), noErr)
        var decoded = 0
        let lock = NSLock()
        for sample in samples {
            VTDecompressionSessionDecodeFrame(try XCTUnwrap(session), sampleBuffer: sample, flags: [], infoFlagsOut: nil) {
                status, _, image, _, _ in
                if status == noErr, let image, CVPixelBufferGetWidth(image) == 320, CVPixelBufferGetHeight(image) == 240 {
                    lock.withLock { decoded += 1 }
                }
            }
        }
        VTDecompressionSessionWaitForAsynchronousFrames(try XCTUnwrap(session))
        XCTAssertEqual(lock.withLock { decoded }, 3)
    }

    func testReceiverWaitsForKeyframe() {
        let receiver = VideoReceiver()
        var requests = 0
        receiver.onNeedKeyframe = { requests += 1 }
        // A delta frame before any keyframe is dropped silently.
        receiver.handle(.videoFrame(ptsUs: 0, keyframe: false, rotation: 0, data: [0, 0, 0, 1, 0x41]))
        XCTAssertEqual(requests, 0)
        // A keyframe without parameter sets cannot be shown: ask for a new one.
        receiver.handle(.videoFrame(ptsUs: 0, keyframe: true, rotation: 0, data: [0, 0, 0, 1, 0x65]))
        XCTAssertEqual(requests, 1)
    }

    /// A host connecting to a static screen still gets a keyframe: the
    /// streamer re-encodes the last captured frame.
    func testStreamerSendsKeyframeToNewHost() throws {
        let streamer = MirrorStreamer(name: "Test Mirror", port: 0, advertise: false)
        let ready = expectation(description: "listener ready")
        var port: UInt16 = 0
        streamer.server.onEvent = { event in
            if case let .ready(p) = event {
                port = p
                ready.fulfill()
            }
        }
        try streamer.start()
        wait(for: [ready], timeout: 5)
        try streamer.process(makePixelBuffer(width: 320, height: 240, seed: 0), pts: CMTime(value: 0, timescale: 60), rotation: 0)

        let gotConfig = expectation(description: "stream config")
        let gotParams = expectation(description: "codec params")
        let gotKeyframe = expectation(description: "keyframe")
        for e in [gotConfig, gotParams, gotKeyframe] { e.assertForOverFulfill = false }
        let client = PeerConnection(host: "127.0.0.1", port: port, queue: DispatchQueue(label: "test.client"))
        client.onReady = { client.send(.hello(version: Wire.protocolVersion, role: .host, name: "Test PC")) }
        client.onMessage = { message in
            switch message {
            case .streamConfig(.h264, 320, 240, _, _): gotConfig.fulfill()
            case let .codecParams(.h264, data) where !data.isEmpty: gotParams.fulfill()
            case .videoFrame(_, true, 0, _): gotKeyframe.fulfill()
            default: break
            }
        }
        client.start()
        wait(for: [gotConfig, gotParams, gotKeyframe], timeout: 10, enforceOrder: true)

        client.close()
        streamer.stop()
    }
}
