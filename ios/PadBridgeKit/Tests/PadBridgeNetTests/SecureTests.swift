import CryptoKit
import XCTest
@testable import PadBridgeNet

/// Same fixed keys and expected bytes as `secure::tests::test_vectors` in
/// crates/host/src/secure.rs: the two implementations must agree exactly.
final class SecureTests: XCTestCase {
    private func key(_ byte: UInt8) throws -> Curve25519.KeyAgreement.PrivateKey {
        try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: [UInt8](repeating: byte, count: 32))
    }

    private func bytes(_ hex: String) -> [UInt8] {
        var out: [UInt8] = []
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            out.append(UInt8(hex[index..<next], radix: 16)!)
            index = next
        }
        return out
    }

    func testVectorsMatchRust() throws {
        let host = DeviceIdentity(privateKey: try key(1))
        let hostEphemeral = try key(2)
        let ipad = DeviceIdentity(privateKey: try key(3))
        let ipadEphemeral = try key(4)

        XCTAssertEqual(hexString(host.publicKey), "a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209")
        XCTAssertEqual(hexString(Array(hostEphemeral.publicKey.rawRepresentation)),
                       "ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59")
        XCTAssertEqual(hexString(ipad.publicKey), "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22")
        XCTAssertEqual(hexString(Array(ipadEphemeral.publicKey.rawRepresentation)),
                       "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b")

        let hostSide = try Handshake.deriveKeys(host: true, identity: host, ephemeral: hostEphemeral,
                                                peerStatic: ipad.publicKey,
                                                peerEphemeral: Array(ipadEphemeral.publicKey.rawRepresentation))
        let ipadSide = try Handshake.deriveKeys(host: false, identity: ipad, ephemeral: ipadEphemeral,
                                                peerStatic: host.publicKey,
                                                peerEphemeral: Array(hostEphemeral.publicKey.rawRepresentation))
        XCTAssertEqual(hostSide, ipadSide)
        XCTAssertEqual(hexString(hostSide.transcript), "3da1af07a054eb739bfaa9980de5c55f2789df5a5c163ebd8a35887ff35a06a5")
        XCTAssertEqual(hexString(hostSide.hostToIpad), "e426ab3631da972e309f56211fbc829f7b22d8e80f61249c401c81163794f7da")
        XCTAssertEqual(hexString(hostSide.ipadToHost), "561d707dae8bab0d15e86f575311cc119e916324c8d4dd8f814c4427dc8c3f94")
        XCTAssertEqual(hostSide.code, "785065")

        var toIpad = RecordCipher(key: hostSide.hostToIpad)
        XCTAssertEqual(hexString(try toIpad.seal(Handshake.introRecord(name: "PC"))),
                       "150000001a1784824de9225753fbe3822c86c3cb06cab6c838")
        var toHost = RecordCipher(key: hostSide.ipadToHost, counter: 5)
        XCTAssertEqual(hexString(try toHost.seal(Array("PadBridge".utf8))),
                       "190000003aa9d7fd74e52fe5ae8c57c0cb53e5375e4f0d42a04414e28b")

        // And the other direction decrypts it.
        var open = RecordCipher(key: hostSide.ipadToHost, counter: 5)
        let sealed = bytes("190000003aa9d7fd74e52fe5ae8c57c0cb53e5375e4f0d42a04414e28b")
        XCTAssertEqual(try open.open(Array(sealed[4...])), Array("PadBridge".utf8))
    }

    func testTamperedRecordFails() throws {
        var sealer = RecordCipher(key: [UInt8](repeating: 7, count: 32))
        var record = try sealer.seal([1, 2, 3])
        record[6] ^= 1
        var opener = RecordCipher(key: [UInt8](repeating: 7, count: 32))
        XCTAssertThrowsError(try opener.open(Array(record[4...])))
    }

    func testIntroRoundTrip() throws {
        XCTAssertEqual(try Handshake.parseIntro(Handshake.introRecord(name: "Ynyr 的電腦")), "Ynyr 的電腦")
        XCTAssertThrowsError(try Handshake.parseIntro([1, 5, 0, 65]))
    }

    func testStorePersistsIdentityAndHosts() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("pairing-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: url) }
        let store = PairingStore(url: url, isShared: true)
        let key = store.identity.publicKey
        store.add(key: [9, 9, 9], name: "PC")
        let reopened = PairingStore(url: url, isShared: true)
        XCTAssertEqual(reopened.identity.publicKey, key)
        XCTAssertTrue(reopened.isPaired([9, 9, 9]))
        XCTAssertEqual(reopened.hosts.map(\.name), ["PC"])
        // A change by another process (the app) is seen without reopening.
        reopened.remove(key: hexString([9, 9, 9]))
        XCTAssertFalse(store.isPaired([9, 9, 9]))
    }
}

extension DeviceIdentity {
    init(privateKey: Curve25519.KeyAgreement.PrivateKey) {
        try! self.init(rawRepresentation: privateKey.rawRepresentation)
    }
}
