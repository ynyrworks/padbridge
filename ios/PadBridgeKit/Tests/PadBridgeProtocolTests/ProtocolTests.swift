import XCTest
@testable import PadBridgeProtocol

final class ProtocolTests: XCTestCase {
    let samples: [Message] = [
        .hello(version: Wire.protocolVersion, role: .displaySink, name: "Ynyr 的 iPad"),
        .displayInfo(widthPx: 2732, heightPx: 2048, scaleMilli: 2000, maxFps: 120,
                     codecs: Codec.h264.maskBit | Codec.hevc.maskBit),
        .streamConfig(codec: .hevc, width: 2732, height: 2048, fps: 60, bitrateKbps: 20_000),
        .codecParams(codec: .h264, data: [0, 0, 0, 2, 0x67, 0x42]),
        .videoFrame(ptsUs: 1_234_567, keyframe: true, rotation: 3, data: [0, 0, 0, 1, 0x65]),
        .videoFrame(ptsUs: 1_250_000, keyframe: false, rotation: 1, data: []),
        .requestKeyframe,
        .pointer(id: 7, kind: .pen, phase: .move, x: 0.25, y: 0.75, pressure: 0.5, altitude: 1.2, azimuth: -0.3),
        .ping(nonce: 42, tUs: 99),
        .pong(nonce: 42, tUs: 99),
        .log(level: 2, text: "hello"),
        .bye,
    ]

    func testEveryMessageRoundtrips() throws {
        for message in samples {
            var decoder = FrameDecoder()
            decoder.push(try message.frame())
            XCTAssertEqual(try decoder.nextMessage(), message)
            XCTAssertNil(try decoder.nextMessage())
        }
    }

    func testStreamSplitIntoSingleBytes() throws {
        var stream: [UInt8] = []
        for message in samples { try message.encode(into: &stream) }
        var decoder = FrameDecoder()
        var got: [Message] = []
        for byte in stream {
            decoder.push([byte])
            while let message = try decoder.nextMessage() { got.append(message) }
        }
        XCTAssertEqual(got, samples)
    }

    /// Same golden bytes as crates/protocol/tests/roundtrip.rs.
    func testGoldenPingAndHello() throws {
        XCTAssertEqual(try Message.ping(nonce: 1, tUs: 2).frame(),
                       [17, 0, 0, 0, 0x30, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0])
        XCTAssertEqual(try Message.hello(version: 1, role: .host, name: "PC").frame(),
                       [12, 0, 0, 0, 0x01, 0x50, 0x44, 0x42, 0x52, 1, 0, 0, 2, 0, 0x50, 0x43])
    }

    func testUnknownKindIsSkipped() throws {
        var stream: [UInt8] = [3, 0, 0, 0, 0x55, 0xAA, 0xBB]
        try Message.bye.encode(into: &stream)
        var decoder = FrameDecoder()
        decoder.push(stream)
        XCTAssertEqual(try decoder.nextMessage(), .bye)
    }

    func testMalformedInputIsRejected() {
        var decoder = FrameDecoder()
        decoder.push([0xFF, 0xFF, 0xFF, 0x7F, 0x01])
        XCTAssertThrowsError(try decoder.nextMessage())

        XCTAssertThrowsError(try Message.decode(kind: 0x01, body: (Array("XXXX".utf8) + [1, 0, 0, 0, 0])[...])) {
            XCTAssertEqual($0 as? ProtocolError, .badMagic)
        }
        XCTAssertThrowsError(try Message.decode(kind: 0x30, body: [UInt8](repeating: 0, count: 15)[...])) {
            XCTAssertEqual($0 as? ProtocolError, .truncated)
        }
        XCTAssertThrowsError(try Message.decode(kind: 0x12, body: [0][...])) {
            XCTAssertEqual($0 as? ProtocolError, .trailingBytes)
        }
        XCTAssertThrowsError(try Message.decode(kind: 0x03, body: ([9] + [UInt8](repeating: 0, count: 14))[...])) {
            XCTAssertEqual($0 as? ProtocolError, .badEnum(field: "Codec", value: 9))
        }
    }
}
