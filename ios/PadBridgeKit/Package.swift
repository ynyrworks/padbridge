// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "PadBridgeKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "PadBridgeProtocol", targets: ["PadBridgeProtocol"]),
        .library(name: "PadBridgeNet", targets: ["PadBridgeNet"]),
        .library(name: "PadBridgeMedia", targets: ["PadBridgeMedia"]),
    ],
    targets: [
        .target(name: "PadBridgeProtocol"),
        .target(name: "PadBridgeNet", dependencies: ["PadBridgeProtocol"]),
        .target(name: "PadBridgeMedia", dependencies: ["PadBridgeNet", "PadBridgeProtocol"]),
        .testTarget(name: "PadBridgeProtocolTests", dependencies: ["PadBridgeProtocol"]),
        .testTarget(name: "PadBridgeNetTests", dependencies: ["PadBridgeNet"]),
        .testTarget(name: "PadBridgeMediaTests", dependencies: ["PadBridgeMedia"]),
    ]
)
