// swift-tools-version:6.2
import PackageDescription
let package = Package(name: "TerminalWorkspace", platforms: [.macOS(.v14)], products: [.executable(name: "TerminalWorkspace", targets: ["TerminalWorkspace"])], dependencies: [.package(path: "Vendor/SwiftTerm")], targets: [.executableTarget(name: "TerminalWorkspace", dependencies: [.product(name: "SwiftTerm", package: "SwiftTerm")])], swiftLanguageModes: [.v5])
