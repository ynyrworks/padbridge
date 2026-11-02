// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "PadBridgeKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "PadBridgeProtocol", targets: ["PadBridgeProtocol"]),
        .library(name: "PadBridgeNet", targets: ["PadBridgeNet"]),
    ],
    targets: [
        .target(name: "PadBridgeProtocol"),
        .target(name: "PadBridgeNet", dependencies: ["PadBridgeProtocol"]),
        .testTarget(name: "PadBridgeProtocolTests", dependencies: ["PadBridgeProtocol"]),
        .testTarget(name: "PadBridgeNetTests", dependencies: ["PadBridgeNet"]),
    ]
)
