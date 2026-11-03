import CryptoKit
import Foundation

/// Encrypted, mutually authenticated channel — Swift side of
/// `crates/host/src/secure.rs`. The byte layout and derivations must match
/// the Rust implementation exactly; `SecureTests` checks shared test vectors.
///
///     hello (both, plain):  "PBS1" · static public key (32) · ephemeral public key (32)
///     transcript h  = SHA-256("padbridge-secure-v1" · S_host · E_host · S_ipad · E_ipad)
///     secret        = DH(e_host, E_ipad) · DH(s_host, E_ipad) · DH(e_host, S_ipad)
///     keys          = HKDF-SHA256(salt h, secret, "padbridge-secure-v1 keys") → host→iPad · iPad→host
///     code          = SHA-256(h · "padbridge-secure-v1 code")[0..4] as u32 BE mod 10^6
///     record        = length u32 LE · ChaCha20-Poly1305(key, nonce = 0u32 · counter u64 LE, plaintext)
public enum SecureWire {
    public static let magic: [UInt8] = Array("PBS1".utf8)
    static let label: [UInt8] = Array("padbridge-secure-v1".utf8)
    public static let helloLength = 68
    static let tagLength = 16
    static let maxRecord = 16 * 1024 * 1024 + 64
    static let intro: UInt8 = 0x01
    static let verdict: UInt8 = 0x02
}

public enum SecureError: Error, Equatable {
    case badMagic
    case weakKey
    case decrypt
    case malformed(String)
    case refused(Verdict)
}

/// The iPad's answer to a host after the intro.
public enum Verdict: UInt8, Sendable {
    case accepted = 0
    case rejected = 1
    /// This endpoint can't pair (the broadcast extension).
    case notPaired = 2
}

/// A host asking to pair, shown to the user.
public struct PairingRequest: Equatable, Sendable {
    public let peerName: String
    public let peerKey: [UInt8]
    /// Six digits; must match what the other screen shows.
    public let code: String
}

/// A long-term X25519 key pair.
public struct DeviceIdentity: @unchecked Sendable {
    public let privateKey: Curve25519.KeyAgreement.PrivateKey

    public init() { privateKey = Curve25519.KeyAgreement.PrivateKey() }
    public init(rawRepresentation: some ContiguousBytes) throws {
        privateKey = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: rawRepresentation)
    }

    public var publicKey: [UInt8] { Array(privateKey.publicKey.rawRepresentation) }
}

public struct SessionKeys: Equatable, Sendable {
    public let hostToIpad: [UInt8]
    public let ipadToHost: [UInt8]
    public let transcript: [UInt8]

    public var code: String { Handshake.code(transcript: transcript) }
}

public enum Handshake {
    public static func hello(identity: DeviceIdentity, ephemeral: Curve25519.KeyAgreement.PrivateKey) -> [UInt8] {
        SecureWire.magic + identity.publicKey + Array(ephemeral.publicKey.rawRepresentation)
    }

    /// Splits a peer's hello into (static key, ephemeral key).
    public static func parseHello(_ bytes: [UInt8]) throws -> (static: [UInt8], ephemeral: [UInt8]) {
        guard bytes.count == SecureWire.helloLength else { throw SecureError.malformed("hello length") }
        guard Array(bytes[0..<4]) == SecureWire.magic else { throw SecureError.badMagic }
        return (Array(bytes[4..<36]), Array(bytes[36..<68]))
    }

    static func dh(_ secret: Curve25519.KeyAgreement.PrivateKey, _ peer: [UInt8]) throws -> [UInt8] {
        let key = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: peer)
        let shared = try secret.sharedSecretFromKeyAgreement(with: key).withUnsafeBytes { Array($0) }
        guard shared.contains(where: { $0 != 0 }) else { throw SecureError.weakKey }
        return shared
    }

    /// `host` is the side that connected.
    public static func deriveKeys(host: Bool, identity: DeviceIdentity,
                                  ephemeral: Curve25519.KeyAgreement.PrivateKey,
                                  peerStatic: [UInt8], peerEphemeral: [UInt8]) throws -> SessionKeys {
        let myStatic = identity.publicKey
        let myEphemeral = Array(ephemeral.publicKey.rawRepresentation)
        let (hs, he, ipadStatic, ie) = host
            ? (myStatic, myEphemeral, peerStatic, peerEphemeral)
            : (peerStatic, peerEphemeral, myStatic, myEphemeral)
        // ee · (host static, iPad ephemeral) · (host ephemeral, iPad static)
        let secret: [UInt8]
        if host {
            secret = try dh(ephemeral, ie) + dh(identity.privateKey, ie) + dh(ephemeral, ipadStatic)
        } else {
            secret = try dh(ephemeral, he) + dh(ephemeral, hs) + dh(identity.privateKey, he)
        }
        let transcript = Array(SHA256.hash(data: SecureWire.label + hs + he + ipadStatic + ie))
        let okm = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: secret), salt: transcript,
                                         info: Array("padbridge-secure-v1 keys".utf8), outputByteCount: 64)
        let bytes = okm.withUnsafeBytes { Array($0) }
        return SessionKeys(hostToIpad: Array(bytes[0..<32]), ipadToHost: Array(bytes[32..<64]), transcript: transcript)
    }

    public static func code(transcript: [UInt8]) -> String {
        let digest = Array(SHA256.hash(data: transcript + Array("padbridge-secure-v1 code".utf8)))
        let n = (UInt32(digest[0]) << 24 | UInt32(digest[1]) << 16 | UInt32(digest[2]) << 8 | UInt32(digest[3])) % 1_000_000
        return String(format: "%06u", n)
    }

    static func introRecord(name: String) -> [UInt8] {
        let utf8 = Array(name.utf8.prefix(1024))
        return [SecureWire.intro, UInt8(utf8.count & 0xFF), UInt8(utf8.count >> 8)] + utf8
    }

    static func parseIntro(_ record: [UInt8]) throws -> String {
        guard record.count >= 3, record[0] == SecureWire.intro else { throw SecureError.malformed("intro") }
        let length = Int(record[1]) | Int(record[2]) << 8
        guard record.count == 3 + length, let name = String(bytes: record[3...], encoding: .utf8) else {
            throw SecureError.malformed("intro name")
        }
        return name
    }
}

/// One direction of the record layer.
public struct RecordCipher {
    private let key: SymmetricKey
    public private(set) var counter: UInt64

    public init(key: [UInt8], counter: UInt64 = 0) {
        self.key = SymmetricKey(data: key)
        self.counter = counter
    }

    private func nonce() throws -> ChaChaPoly.Nonce {
        var bytes = [UInt8](repeating: 0, count: 4)
        withUnsafeBytes(of: counter.littleEndian) { bytes.append(contentsOf: $0) }
        return try ChaChaPoly.Nonce(data: bytes)
    }

    /// Encrypts one record, length prefix included.
    public mutating func seal(_ plaintext: [UInt8]) throws -> [UInt8] {
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: nonce())
        counter += 1
        let sealed = Array(box.ciphertext) + Array(box.tag)
        var out: [UInt8] = []
        withUnsafeBytes(of: UInt32(sealed.count).littleEndian) { out.append(contentsOf: $0) }
        return out + sealed
    }

    /// Decrypts one record body (without its length prefix).
    public mutating func open(_ sealed: [UInt8]) throws -> [UInt8] {
        guard sealed.count >= SecureWire.tagLength else { throw SecureError.decrypt }
        let box = try ChaChaPoly.SealedBox(nonce: nonce(), ciphertext: sealed.dropLast(SecureWire.tagLength),
                                           tag: sealed.suffix(SecureWire.tagLength))
        guard let plain = try? ChaChaPoly.open(box, using: key) else { throw SecureError.decrypt }
        counter += 1
        return Array(plain)
    }
}

/// Splits a byte stream into record bodies.
struct RecordSplitter {
    private var buffer: [UInt8] = []

    mutating func push(_ bytes: some Sequence<UInt8>) { buffer.append(contentsOf: bytes) }

    /// Takes `n` raw bytes (the plain hello) if available.
    mutating func take(_ n: Int) -> [UInt8]? {
        guard buffer.count >= n else { return nil }
        defer { buffer.removeFirst(n) }
        return Array(buffer.prefix(n))
    }

    mutating func nextRecord() throws -> [UInt8]? {
        guard buffer.count >= 4 else { return nil }
        let length = Int(UInt32(buffer[0]) | UInt32(buffer[1]) << 8 | UInt32(buffer[2]) << 16 | UInt32(buffer[3]) << 24)
        guard length >= SecureWire.tagLength, length <= SecureWire.maxRecord else { throw SecureError.malformed("record length") }
        guard buffer.count >= 4 + length else { return nil }
        defer { buffer.removeFirst(4 + length) }
        return Array(buffer[4..<4 + length])
    }
}

/// Who we are in a handshake, and how we decide about the peer.
public enum SecureRole {
    /// iPad side. `requestPairing` asks the user about an unknown host and
    /// calls back with their answer; nil means this endpoint can't pair.
    case device(isPaired: ([UInt8]) -> Bool,
                requestPairing: ((PairingRequest, @escaping (Bool) -> Void) -> Void)?,
                onPaired: ([UInt8], String) -> Void)
    /// Host side (tests): `confirm` decides about an iPad it doesn't know.
    case host(isKnown: ([UInt8]) -> Bool, confirm: (PairingRequest) -> Bool)
}

public struct SecureConfig {
    public let identity: DeviceIdentity
    public let name: String
    public let role: SecureRole

    public init(identity: DeviceIdentity, name: String, role: SecureRole) {
        self.identity = identity
        self.name = name
        self.role = role
    }
}

public func hexString(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
