// PadBridge wire protocol — Swift mirror of crates/protocol (Rust).
//
// Frame: [len: u32 LE][kind: u8][body], len = 1 + body length.
// Byte layouts are documented in docs/DESIGN.md; golden bytes are shared
// with the Rust tests.

import Foundation

public enum Wire {
    public static let protocolVersion: UInt16 = 2
    public static let magic: [UInt8] = Array("PDBR".utf8)
    public static let maxFrameLength = 16 * 1024 * 1024
    public static let displayPort: UInt16 = 47800
    public static let mirrorPort: UInt16 = 47801
    public static let bonjourType = "_padbridge._tcp"
}

enum Kind {
    static let hello: UInt8 = 0x01
    static let displayInfo: UInt8 = 0x02
    static let streamConfig: UInt8 = 0x03
    static let codecParams: UInt8 = 0x10
    static let videoFrame: UInt8 = 0x11
    static let requestKeyframe: UInt8 = 0x12
    static let pointer: UInt8 = 0x20
    static let cursor: UInt8 = 0x21
    static let ping: UInt8 = 0x30
    static let pong: UInt8 = 0x31
    static let log: UInt8 = 0x40
    static let bye: UInt8 = 0x7F
}

public enum Role: UInt8, Sendable { case host = 0, displaySink = 1, mirrorSource = 2 }

public enum Codec: UInt8, Sendable {
    case h264 = 1, hevc = 2

    /// Bit used for this codec in `Message.displayInfo(codecs:)`.
    public var maskBit: UInt8 { 1 << (rawValue - 1) }
}

public enum PointerKind: UInt8, Sendable { case touch = 0, pen = 1, mouse = 2 }

public enum Phase: UInt8, Sendable { case down = 0, move = 1, up = 2, cancel = 3, hover = 4 }

public enum Message: Equatable, Sendable {
    case hello(version: UInt16, role: Role, name: String)
    /// `scaleMilli` is points-to-pixels scale × 1000; `codecs` is a `Codec.maskBit` bitmask.
    case displayInfo(widthPx: UInt32, heightPx: UInt32, scaleMilli: UInt16, maxFps: UInt16, codecs: UInt8)
    case streamConfig(codec: Codec, width: UInt32, height: UInt32, fps: UInt16, bitrateKbps: UInt32)
    /// Parameter sets, each prefixed by a big-endian u32 length.
    case codecParams(codec: Codec, data: [UInt8])
    /// AVCC NAL units (big-endian u32 length prefixes).
    /// `rotation`: clockwise quarter turns (0...3) the receiver applies to show the frame upright.
    case videoFrame(ptsUs: UInt64, keyframe: Bool, rotation: UInt8, data: [UInt8])
    case requestKeyframe
    /// `x`/`y` are normalized to the video frame (0...1); angles are radians.
    case pointer(id: UInt32, kind: PointerKind, phase: Phase, x: Float, y: Float,
                 pressure: Float, altitude: Float, azimuth: Float)
    /// Host cursor in normalized video coordinates; the receiver draws it.
    case cursor(x: Float, y: Float, visible: Bool)
    case ping(nonce: UInt64, tUs: UInt64)
    case pong(nonce: UInt64, tUs: UInt64)
    case log(level: UInt8, text: String)
    case bye
}

public enum ProtocolError: Error, Equatable {
    case truncated
    case trailingBytes
    case frameTooLarge(Int)
    case unknownKind(UInt8)
    case badMagic
    case badUTF8
    case stringTooLong(Int)
    case badEnum(field: String, value: UInt8)
}

extension Message {
    var kind: UInt8 {
        switch self {
        case .hello: Kind.hello
        case .displayInfo: Kind.displayInfo
        case .streamConfig: Kind.streamConfig
        case .codecParams: Kind.codecParams
        case .videoFrame: Kind.videoFrame
        case .requestKeyframe: Kind.requestKeyframe
        case .pointer: Kind.pointer
        case .cursor: Kind.cursor
        case .ping: Kind.ping
        case .pong: Kind.pong
        case .log: Kind.log
        case .bye: Kind.bye
        }
    }

    /// Appends this message as one complete frame to `out`.
    public func encode(into out: inout [UInt8]) throws {
        let start = out.count
        out.append(contentsOf: [0, 0, 0, 0])
        out.append(kind)
        // Move the buffer into the writer so appends don't trigger a copy-on-write.
        var w = Writer(bytes: out)
        out = []
        do {
            try writeBody(to: &w)
        } catch {
            out = Array(w.bytes.prefix(start))
            throw error
        }
        out = w.bytes

        let length = out.count - start - 4
        guard length <= Wire.maxFrameLength else {
            out.removeSubrange(start...)
            throw ProtocolError.frameTooLarge(length)
        }
        withUnsafeBytes(of: UInt32(length).littleEndian) { out.replaceSubrange(start..<start + 4, with: $0) }
    }

    private func writeBody(to w: inout Writer) throws {
        switch self {
        case let .hello(version, role, name):
            w.append(Wire.magic)
            w.u16(version)
            w.u8(role.rawValue)
            try w.string(name)
        case let .displayInfo(widthPx, heightPx, scaleMilli, maxFps, codecs):
            w.u32(widthPx)
            w.u32(heightPx)
            w.u16(scaleMilli)
            w.u16(maxFps)
            w.u8(codecs)
        case let .streamConfig(codec, width, height, fps, bitrateKbps):
            w.u8(codec.rawValue)
            w.u32(width)
            w.u32(height)
            w.u16(fps)
            w.u32(bitrateKbps)
        case let .codecParams(codec, data):
            w.u8(codec.rawValue)
            w.append(data)
        case let .videoFrame(ptsUs, keyframe, rotation, data):
            w.u64(ptsUs)
            w.u8((keyframe ? 1 : 0) | (rotation & 3) << 1)
            w.append(data)
        case let .pointer(id, kind, phase, x, y, pressure, altitude, azimuth):
            w.u32(id)
            w.u8(kind.rawValue)
            w.u8(phase.rawValue)
            for v in [x, y, pressure, altitude, azimuth] { w.u32(v.bitPattern) }
        case let .cursor(x, y, visible):
            w.u32(x.bitPattern)
            w.u32(y.bitPattern)
            w.u8(visible ? 1 : 0)
        case let .ping(nonce, tUs), let .pong(nonce, tUs):
            w.u64(nonce)
            w.u64(tUs)
        case let .log(level, text):
            w.u8(level)
            try w.string(text)
        case .requestKeyframe, .bye:
            break
        }
    }

    public func frame() throws -> [UInt8] {
        var out: [UInt8] = []
        try encode(into: &out)
        return out
    }

    /// Decodes a frame body (everything after the kind byte).
    public static func decode(kind: UInt8, body: ArraySlice<UInt8>) throws -> Message {
        var r = Reader(bytes: body)
        let message: Message
        switch kind {
        case Kind.hello:
            guard Array(try r.take(4)) == Wire.magic else { throw ProtocolError.badMagic }
            message = .hello(version: try r.u16(), role: try r.enumValue(), name: try r.string())
        case Kind.displayInfo:
            message = .displayInfo(widthPx: try r.u32(), heightPx: try r.u32(), scaleMilli: try r.u16(),
                                   maxFps: try r.u16(), codecs: try r.u8())
        case Kind.streamConfig:
            message = .streamConfig(codec: try r.enumValue(), width: try r.u32(), height: try r.u32(),
                                    fps: try r.u16(), bitrateKbps: try r.u32())
        case Kind.codecParams:
            message = .codecParams(codec: try r.enumValue(), data: r.rest())
        case Kind.videoFrame:
            let ptsUs = try r.u64()
            let flags = try r.u8()
            message = .videoFrame(ptsUs: ptsUs, keyframe: flags & 1 != 0, rotation: (flags >> 1) & 3, data: r.rest())
        case Kind.requestKeyframe:
            message = .requestKeyframe
        case Kind.pointer:
            message = .pointer(id: try r.u32(), kind: try r.enumValue(), phase: try r.enumValue(),
                               x: try r.f32(), y: try r.f32(), pressure: try r.f32(),
                               altitude: try r.f32(), azimuth: try r.f32())
        case Kind.cursor:
            message = .cursor(x: try r.f32(), y: try r.f32(), visible: try r.u8() != 0)
        case Kind.ping:
            message = .ping(nonce: try r.u64(), tUs: try r.u64())
        case Kind.pong:
            message = .pong(nonce: try r.u64(), tUs: try r.u64())
        case Kind.log:
            message = .log(level: try r.u8(), text: try r.string())
        case Kind.bye:
            message = .bye
        default:
            throw ProtocolError.unknownKind(kind)
        }
        guard r.isAtEnd else { throw ProtocolError.trailingBytes }
        return message
    }
}

/// Incremental decoder for a byte stream (TCP or usbmux tunnel).
public struct FrameDecoder {
    private var buffer: [UInt8] = []

    public init() {}

    public mutating func push<S: Sequence>(_ data: S) where S.Element == UInt8 {
        buffer.append(contentsOf: data)
    }

    /// Returns the next complete message, or nil if more bytes are needed.
    ///
    /// Frames with an unknown kind are skipped. Any other error leaves the
    /// stream unsynchronized; the caller should drop the connection.
    public mutating func nextMessage() throws -> Message? {
        while buffer.count >= 4 {
            let length = Int(UInt32(buffer[0]) | UInt32(buffer[1]) << 8 | UInt32(buffer[2]) << 16 | UInt32(buffer[3]) << 24)
            guard length > 0, length <= Wire.maxFrameLength else { throw ProtocolError.frameTooLarge(length) }
            guard buffer.count >= 4 + length else { return nil }
            let kind = buffer[4]
            let body = buffer[5..<4 + length]
            buffer.removeFirst(4 + length)
            do {
                return try Message.decode(kind: kind, body: body)
            } catch ProtocolError.unknownKind {
                continue
            }
        }
        return nil
    }
}

private struct Writer {
    var bytes: [UInt8]

    mutating func append<C: Collection>(_ data: C) where C.Element == UInt8 { bytes.append(contentsOf: data) }
    mutating func u8(_ v: UInt8) { bytes.append(v) }
    mutating func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { bytes.append(contentsOf: $0) } }
    mutating func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { bytes.append(contentsOf: $0) } }
    mutating func u64(_ v: UInt64) { withUnsafeBytes(of: v.littleEndian) { bytes.append(contentsOf: $0) } }

    mutating func string(_ s: String) throws {
        let utf8 = Array(s.utf8)
        guard utf8.count <= Int(UInt16.max) else { throw ProtocolError.stringTooLong(utf8.count) }
        u16(UInt16(utf8.count))
        append(utf8)
    }
}

private struct Reader {
    let bytes: ArraySlice<UInt8>
    var position: Int

    init(bytes: ArraySlice<UInt8>) {
        self.bytes = bytes
        position = bytes.startIndex
    }

    var isAtEnd: Bool { position == bytes.endIndex }

    mutating func take(_ n: Int) throws -> ArraySlice<UInt8> {
        guard bytes.endIndex - position >= n else { throw ProtocolError.truncated }
        defer { position += n }
        return bytes[position..<position + n]
    }

    mutating func littleEndian<T: FixedWidthInteger>(_: T.Type) throws -> T {
        try take(MemoryLayout<T>.size).reversed().reduce(T(0)) { $0 << 8 | T($1) }
    }

    mutating func u8() throws -> UInt8 { try take(1).first! }
    mutating func u16() throws -> UInt16 { try littleEndian(UInt16.self) }
    mutating func u32() throws -> UInt32 { try littleEndian(UInt32.self) }
    mutating func u64() throws -> UInt64 { try littleEndian(UInt64.self) }
    mutating func f32() throws -> Float { Float(bitPattern: try u32()) }

    mutating func enumValue<E: RawRepresentable>() throws -> E where E.RawValue == UInt8 {
        let raw = try u8()
        guard let value = E(rawValue: raw) else {
            throw ProtocolError.badEnum(field: String(describing: E.self), value: raw)
        }
        return value
    }

    mutating func string() throws -> String {
        let length = Int(try u16())
        guard let s = String(bytes: try take(length), encoding: .utf8) else { throw ProtocolError.badUTF8 }
        return s
    }

    mutating func rest() -> [UInt8] {
        defer { position = bytes.endIndex }
        return Array(bytes[position...])
    }
}
