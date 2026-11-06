// swift-tools-version:6.2
// macOS-only packaging of the upstream sources recorded in UPSTREAM.json.
import PackageDescription
let package = Package(
    name: "SwiftTerm",
    platforms: [.macOS(.v14)],
    products: [.library(name: "SwiftTerm", targets: ["SwiftTerm"])],
    targets: [
        .target(name: "SwiftTerm", plugins: [.plugin(name: "SwiftTermBuildInfoPlugin")]),
        .executableTarget(name: "SwiftTermBuildInfoGenerator"),
        .plugin(name: "SwiftTermBuildInfoPlugin", capability: .buildTool(), dependencies: ["SwiftTermBuildInfoGenerator"])
    ],
    swiftLanguageModes: [.v6]
)
