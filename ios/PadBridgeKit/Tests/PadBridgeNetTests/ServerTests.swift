import XCTest
import PadBridgeProtocol
@testable import PadBridgeNet

final class ServerTests: XCTestCase {
    func testHelloAndPingOverLoopback() throws {
        let server = Server(role: .displaySink, name: "Test iPad", port: 0, advertise: false)
        let ready = expectation(description: "listener ready")
        let gotClientHello = expectation(description: "server saw client hello")
        var port: UInt16 = 0
        server.onEvent = { event in
            switch event {
            case let .ready(p):
                port = p
                ready.fulfill()
            case .hello(_, name: "Test PC", role: .host):
                gotClientHello.fulfill()
            default:
                break
            }
        }
        try server.start()
        wait(for: [ready], timeout: 5)
        XCTAssertNotEqual(port, 0)

        let gotServerHello = expectation(description: "client saw server hello")
        let gotPong = expectation(description: "client got pong")
        let client = PeerConnection(host: "127.0.0.1", port: port, queue: DispatchQueue(label: "test.client"))
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

        client.close()
        server.stop()
    }

    func testTxtRecordEncoding() {
        let data = Server.txtRecord(["role": "1", "proto": "1"])
        XCTAssertEqual(Array(data), [7] + Array("proto=1".utf8) + [6] + Array("role=1".utf8))
    }
}
