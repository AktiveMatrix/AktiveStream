// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "aktivestream-host",
    platforms: [.macOS(.v14)],
    targets: [
        // Objective-C shim around the private CGVirtualDisplay API (headless/clamshell capture).
        .target(
            name: "CVirtualDisplay",
            cSettings: [.unsafeFlags(["-fobjc-arc"])]
        ),
        .executableTarget(
            name: "aktivestream-host",
            dependencies: ["CVirtualDisplay"],
            path: "Sources/aktivestream-host"
        )
    ]
)
