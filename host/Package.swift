// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "aktivestream-host",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "aktivestream-host",
            path: "Sources/aktivestream-host"
        )
    ]
)
