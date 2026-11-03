import XCTest
import PadBridgeProtocol
@testable import PadBridgeNet

final class ServerTests: XCTestCase {
    private func temporaryStore() -> PairingStore {
        PairingStore(url: FileManager.default.temporaryDirectory.appendingPathComponent("pairing-\(UUID()).json"),
                     isShared: true)
    }

    private func startServer(_ server: Server) throws -> UInt16 {
        let ready = expectation(description: "listener ready")
        var port: UInt16 = 0
        let previous = server.onEvent
        server.onEvent = { event in
            if case let .ready(p) = event {
                port = p
                ready.fulfill()
            }
            previous?(event)
        }
        try server.start()
        wait(for: [ready], timeout: 5)
        XCTAssertNotEqual(port, 0)
        return port
    }

    private func hostConfig(_ identity: DeviceIdentity = DeviceIdentity(), confirm: @escaping (PairingRequest) -> Bool = { _ in true })
        -> SecureConfig {
        SecureConfig(identity: identity, name: "Test PC", role: .host(isKnown: { _ in false }, confirm: confirm))
    }

    func testPairingThenHelloAndPing() throws {
        let store = temporaryStore()
        let server = Server(role: .displaySink, name: "Test iPad", port: 0, advertise: false, store: store)
        let gotClientHello = expectation(description: "server saw client hello")
        var serverCode = ""
        server.onPairingRequest = { _, request, respond in
            serverCode = request.code
            respond(true)
        }
        server.onEvent = { event in
            if case .hello(_, name: "Test PC", role: .host) = event { gotClientHello.fulfill() }
        }
        let port = try startServer(server)

        let host = DeviceIdentity()
        var clientCode = ""
        let gotServerHello = expectation(description: "client saw server hello")
        let gotPong = expectation(description: "client got pong")
        let client = PeerConnection(host: "127.0.0.1", port: port, queue: DispatchQueue(label: "test.client"),
                                    security: hostConfig(host) { request in
                                        clientCode = request.code
                                        return true
                                    })
        client.onReady = {
            client.send(.hello(version: Wire.protocolVersion, role: .host, name: "Test PC"))
            client.send(.ping(nonce: 7, tUs: 123))
        }
        client.onMessage = { message in
            switch message {
            case .hello(Wire.protocolVersion, .displaySink, "Test iPad"): gotServerHello.fulfill()
            case .pong(7, 123): gotPong.fulfill()
            default: break
            }
        }
        client.start()
        wait(for: [gotServerHello, gotClientHello, gotPong], timeout: 5)
        XCTAssertEqual(clientCode.count, 6)
        XCTAssertEqual(clientCode, serverCode, "both screens must show the same code")
        XCTAssertTrue(store.isPaired(host.publicKey))
        XCTAssertEqual(client.peerName, "Test iPad")

        client.close()
        server.stop()
    }

    func testUnpairedHostIsRefusedWithoutPairingHandler() throws {
        let server = Server(role: .mirrorSource, name: "Mirror", port: 0, advertise: false, store: temporaryStore())
        let port = try startServer(server)
        let closed = expectation(description: "client closed")
        let client = PeerConnection(host: "127.0.0.1", port: port, queue: DispatchQueue(label: "test.client"),
                                    security: hostConfig())
        client.onReady = { XCTFail("an unpaired host must not get in") }
        client.onClose = { error in
            XCTAssertEqual(error as? SecureError, .refused(.notPaired))
            closed.fulfill()
        }
        client.start()
        wait(for: [closed], timeout: 5)
        server.stop()
    }

    func testDeviceIDMatchesRust() {
        // Same as `secure::device_id` in crates/host/src/secure.rs.
        XCTAssertEqual(Server.deviceID(Array(1...32)), "0102030405060708")
    }

    func testTxtRecordEncoding() {
        let data = Server.txtRecord(["role": "1", "proto": "1"])
        XCTAssertEqual(Array(data), [7] + Array("proto=1".utf8) + [6] + Array("role=1".utf8))
    }
}
