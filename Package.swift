// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "OrbitDisk",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "OrbitDisk", targets: ["OrbitDisk"]),
        .executable(name: "OrbitScan", targets: ["OrbitScan"]),
        .executable(name: "OrbitCoreChecks", targets: ["OrbitCoreChecks"]),
        .library(name: "OrbitCore", targets: ["OrbitCore"])
    ],
    targets: [
        .systemLibrary(name: "CSQLite"),
        .target(name: "OrbitCore", dependencies: ["CSQLite"]),
        .executableTarget(name: "OrbitDisk", dependencies: ["OrbitCore"]),
        .executableTarget(name: "OrbitScan", dependencies: ["OrbitCore"]),
        .executableTarget(name: "OrbitCoreChecks", dependencies: ["OrbitCore"], path: "Tests/OrbitCoreTests")
    ],
    swiftLanguageModes: [.v5]
)
