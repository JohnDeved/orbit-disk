// swift-tools-version: 6.3
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
    dependencies: [
        .package(url: "https://github.com/sparkle-project/Sparkle", exact: "2.10.1")
    ],
    targets: [
        .systemLibrary(name: "CSQLite"),
        .target(name: "OrbitCore", dependencies: ["CSQLite"]),
        .executableTarget(
            name: "OrbitDisk",
            dependencies: ["OrbitCore", .product(name: "Sparkle", package: "Sparkle")],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-rpath", "-Xlinker", "@executable_path/../Frameworks"])]
        ),
        .executableTarget(name: "OrbitScan", dependencies: ["OrbitCore"]),
        .executableTarget(name: "OrbitCoreChecks", dependencies: ["OrbitCore"], path: "Tests/OrbitCoreTests")
    ],
    swiftLanguageModes: [.v5]
)
