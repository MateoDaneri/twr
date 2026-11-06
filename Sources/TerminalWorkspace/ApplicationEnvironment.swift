import AppKit
import CryptoKit
import Foundation

struct ApplicationEnvironment {
  let demo: Bool
  let resources: URL
  let dataRoot: URL
  let socketDirectory: String
  let tmux = "/opt/homebrew/bin/tmux"
  let nvim = "/opt/homebrew/bin/nvim"

  init(demo: Bool) {
    self.demo = demo
    let source = ProcessInfo.processInfo.environment["TERMINAL_WORKSPACE_ROOT"]
      ?? FileManager.default.currentDirectoryPath
    let bundled = Bundle.main.resourceURL ?? URL(fileURLWithPath: source + "/Resources")
    resources = FileManager.default.fileExists(atPath: bundled.appendingPathComponent("Fixtures").path)
      ? bundled : URL(fileURLWithPath: source + "/Resources")
    if let path = ProcessInfo.processInfo.environment["TERMINAL_WORKSPACE_DATA_ROOT"] {
      dataRoot = URL(fileURLWithPath: path)
    } else if demo {
      let demoRoot = Bundle.main.object(forInfoDictionaryKey: "WorkspaceDemoDataRoot") as? String
        ?? source + "/runtime/workspace-demo"
      dataRoot = URL(fileURLWithPath: demoRoot)
    } else {
      dataRoot = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("Terminal Workspace", isDirectory: true)
    }
    let flavor = demo ? "demo" : "app"
    socketDirectory = "/private/tmp/terminal-workspace-\(flavor)-\(getuid())"
  }
  var socket: String { socketDirectory + "/tmux.sock" }
  var configuration: String {
    demo ? resources.appendingPathComponent("Fixtures/tmux.conf").path
      : NSHomeDirectory() + "/.config/tmux/tmux.conf"
  }
  var profile: GhosttyProfile {
    GhosttyProfile(path: demo ? resources.appendingPathComponent("Fixtures/ghostty/config").path
      : NSHomeDirectory() + "/.config/ghostty/config")
  }
  var sample: String { resources.appendingPathComponent("Content/sample").path }
  var processEnvironment: [String: String] {
    var values = ProcessInfo.processInfo.environment
    values["PATH"] = "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
      + (demo ? "" : ":" + NSHomeDirectory() + "/.local/bin")
    values["TERM"] = "xterm-256color"
    values.removeValue(forKey: "TMUX")
    values.removeValue(forKey: "NVIM")
    values.removeValue(forKey: "NVIM_LISTEN_ADDRESS")
    if demo {
      values["HOME"] = dataRoot.appendingPathComponent("home").path
      values["XDG_CONFIG_HOME"] = dataRoot.appendingPathComponent("home/config").path
      values["XDG_STATE_HOME"] = dataRoot.appendingPathComponent("home/state").path
      values["XDG_CACHE_HOME"] = dataRoot.appendingPathComponent("home/cache").path
    }
    return values
  }
  func prepareStorage() throws {
    try FileManager.default.createDirectory(at: dataRoot, withIntermediateDirectories: true,
                                            attributes: [.posixPermissions: 0o700])
    try TmuxBackend.prepareSocketDirectory(dataRoot.path)
    try TmuxBackend.prepareSocketDirectory(socketDirectory)
    if demo {
      try FileManager.default.createDirectory(at: dataRoot.appendingPathComponent("home"),
        withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
    }
  }
}

struct WorkspacePreferences: Codable {
  var fontSize: Double = 13
  var showSidebar: Bool = true
  var lastSessionID: String?
  var projects: [String] = []
  var windowFrame: String?

  static func load(from url: URL) -> WorkspacePreferences {
    guard let data = try? Data(contentsOf: url),
      let result = try? JSONDecoder().decode(WorkspacePreferences.self, from: data),
      (9...30).contains(result.fontSize)
    else { return WorkspacePreferences() }
    return result
  }
  func save(to url: URL) throws {
    let data = try JSONEncoder().encode(self)
    try data.write(to: url, options: .atomic)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
  }
}
