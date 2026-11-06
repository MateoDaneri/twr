import AppKit
import CryptoKit
import Foundation
import Darwin

// Own the short-lived command's descriptors and wait status. Foundation Process
// is reserved for the long-lived control client, never used for UI queries.
enum CommandRunner {
  static func capture(_ executable: String, _ arguments: [String], environment: [String: String],
                      timeout: TimeInterval = 8) -> String {
    var descriptors: [Int32] = [0, 0]
    guard pipe(&descriptors) == 0 else { return "ERROR: pipe" }
    defer { close(descriptors[0]); close(descriptors[1]) }
    var actions: posix_spawn_file_actions_t?
    var attributes: posix_spawnattr_t?
    posix_spawn_file_actions_init(&actions)
    posix_spawnattr_init(&attributes)
    defer { posix_spawn_file_actions_destroy(&actions); posix_spawnattr_destroy(&attributes) }
    posix_spawn_file_actions_addopen(&actions, STDIN_FILENO, "/dev/null", O_RDONLY, 0)
    posix_spawn_file_actions_adddup2(&actions, descriptors[1], STDOUT_FILENO)
    posix_spawn_file_actions_adddup2(&actions, descriptors[1], STDERR_FILENO)
    posix_spawn_file_actions_addclose(&actions, descriptors[0])
    posix_spawn_file_actions_addclose(&actions, descriptors[1])
    var mask = sigset_t()
    sigemptyset(&mask)
    posix_spawnattr_setsigmask(&attributes, &mask)
    var defaults = sigset_t()
    sigfillset(&defaults)
    sigdelset(&defaults, SIGKILL)
    sigdelset(&defaults, SIGSTOP)
    posix_spawnattr_setsigdefault(&attributes, &defaults)
    posix_spawnattr_setflags(&attributes,
      Int16(POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_SETSIGDEF))
    let argv = ([executable] + arguments).map { strdup($0) } + [nil]
    let envp = environment.map { strdup("\($0.key)=\($0.value)") } + [nil]
    defer { argv.forEach { free($0) }; envp.forEach { free($0) } }
    var pid: pid_t = 0
    let launch = argv.withUnsafeBufferPointer { argv in
      envp.withUnsafeBufferPointer { envp in
        posix_spawn(&pid, executable, &actions, &attributes,
                    UnsafeMutablePointer(mutating: argv.baseAddress!),
                    UnsafeMutablePointer(mutating: envp.baseAddress!))
      }
    }
    guard launch == 0 else { return "ERROR: spawn \(launch)" }
    _ = fcntl(descriptors[0], F_SETFL, O_NONBLOCK)
    var output = Data()
    var buffer = [UInt8](repeating: 0, count: 8192)
    func drain() {
      // Bound each pass so an infinite producer cannot starve the timeout.
      for _ in 0..<32 {
        let count = read(descriptors[0], &buffer, buffer.count)
        guard count > 0 else { return }
        if output.count < 2_097_152 { output.append(contentsOf: buffer.prefix(count)) }
      }
    }
    let deadline = ProcessInfo.processInfo.systemUptime + timeout
    var status: Int32 = 0
    while true {
      drain()
      let result = waitpid(pid, &status, WNOHANG)
      if result == pid { drain(); break }
      if result == -1 && errno != EINTR { return "ERROR: waitpid \(errno)" }
      if ProcessInfo.processInfo.systemUptime >= deadline {
        // Only this command client is stopped. The tmux server and panes live on.
        kill(pid, SIGKILL)
        while waitpid(pid, &status, 0) == -1 && errno == EINTR {}
        return "ERROR: command timed out"
      }
      usleep(5_000)
    }
    let text = String(decoding: output, as: UTF8.self)
    return status == 0 ? text : "ERROR: command status \(status): \(text)"
  }
}

struct PrototypePaths {
  let root: String
  var runtime: String { root + "/runtime" }
  var socketDirectory: String {
    let digest = SHA256.hash(data: Data(root.utf8)).prefix(6).map { String(format: "%02x", $0) }
      .joined()
    return "/private/tmp/terminal-workspace-\(getuid())-\(digest)"
  }
  var socket: String { socketDirectory + "/tmux.sock" }
  var nvimSocket: String { socketDirectory + "/nvim.sock" }
  var sample: String { root + "/sample" }
  let tmux = "/opt/homebrew/bin/tmux"
  let nvim = "/opt/homebrew/bin/nvim"
  var config: String { NSHomeDirectory() + "/.config/tmux/tmux.conf" }
}

struct GhosttyProfile {
  var values: [String: String] = [:]
  var bindings: [String: String] = [:]
  var palette: [Int: String] = [:]
  var files: [String] = []
  init(path: String) { load(path, depth: 0) }
  mutating func load(_ path: String, depth: Int) {
    guard depth < 12 else { return }
    let resolved = (path as NSString).expandingTildeInPath
    guard !files.contains(resolved),
      let text = try? String(contentsOfFile: resolved, encoding: .utf8)
    else { return }
    files.append(resolved)
    for raw in text.components(separatedBy: .newlines) {
      let line = raw.trimmingCharacters(in: .whitespaces)
      guard !line.hasPrefix("#"), let equal = line.firstIndex(of: "=") else { continue }
      let key = String(line[..<equal]).trimmingCharacters(in: .whitespaces)
      let value = String(line[line.index(after: equal)...]).trimmingCharacters(in: .whitespaces)
      if key == "config-file" {
        let pathValue = value.trimmingCharacters(in: CharacterSet(charactersIn: "\""))
        let included =
          pathValue.hasPrefix("/") || pathValue.hasPrefix("~")
          ? pathValue : (resolved as NSString).deletingLastPathComponent + "/" + pathValue
        load(included, depth: depth + 1)
      } else if key == "palette", let sep = value.firstIndex(of: "="),
        let index = Int(value[..<sep])
      {
        palette[index] = String(value[value.index(after: sep)...])
      } else if key == "keybind", let sep = value.firstIndex(of: "="),
        value[value.index(after: sep)...].hasPrefix("text:")
      {
        bindings[String(value[..<sep])] = Self.unescape(
          String(value[value.index(sep, offsetBy: 6)...]))
      } else {
        values[key] = value
      }
    }
  }
  static func unescape(_ text: String) -> String {
    let bytes = Array(text.utf8)
    var result: [UInt8] = []
    var i = 0
    while i < bytes.count {
      if bytes[i] == 92, i + 3 < bytes.count, bytes[i + 1] == 120,
        let byte = UInt8(String(bytes: bytes[(i + 2)...(i + 3)], encoding: .utf8) ?? "", radix: 16)
      {
        result.append(byte)
        i += 4
      } else if bytes[i] == 92, i + 1 < bytes.count, bytes[i + 1] == 110 {
        result.append(10)
        i += 2
      } else {
        result.append(bytes[i])
        i += 1
      }
    }
    return String(decoding: result, as: UTF8.self)
  }
  func text(for event: NSEvent) -> String? {
    let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
    let aliases: [String] = [
      flags.contains(.command) ? "super" : "", flags.contains(.control) ? "ctrl" : "",
      flags.contains(.shift) ? "shift" : "", flags.contains(.option) ? "alt" : "",
    ].filter { !$0.isEmpty }
    let digitNames = ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine"]
    let char = (event.charactersIgnoringModifiers ?? "").lowercased()
    let key =
      Int(char).flatMap { $0 >= 1 && $0 <= 9 ? "physical:" + digitNames[$0 - 1] : nil } ?? char
    let namedKeys: [UInt16: String] = [123: "left", 124: "right", 125: "down", 126: "up",
                                      36: "enter", 48: "tab", 53: "escape"]
    var candidates = Set([key, char])
    if let named = namedKeys[event.keyCode] {
      candidates.insert(named)
      candidates.insert("arrow_" + named)
    }
    for (binding, value) in bindings {
      let parts = binding.split(separator: "+").map(String.init)
      guard let last = parts.last, candidates.contains(last) else { continue }
      let modifiers = parts.dropLast().map { $0 == "control" ? "ctrl" : $0 }
      if Set(modifiers) == Set(aliases) { return value }
    }
    return nil
  }
  var background: NSColor { NSColor(hex: values["background"] ?? "#021c23") }
  var foreground: NSColor { NSColor(hex: values["foreground"] ?? "#ffffff") }
  var accent: NSColor { NSColor(hex: values["cursor-color"] ?? "#ff79bd") }
  var font: NSFont {
    let name = (values["font-family"] ?? "Menlo").trimmingCharacters(
      in: CharacterSet(charactersIn: "\""))
    let size = Double(values["font-size"] ?? "13") ?? 13
    return NSFont(name: name, size: size) ?? .monospacedSystemFont(ofSize: size, weight: .regular)
  }
}

extension NSColor {
  convenience init(hex: String) {
    let n = UInt64(hex.trimmingCharacters(in: CharacterSet(charactersIn: "#")), radix: 16) ?? 0
    self.init(
      srgbRed: CGFloat((n >> 16) & 255) / 255, green: CGFloat((n >> 8) & 255) / 255,
      blue: CGFloat(n & 255) / 255, alpha: 1)
  }
}

struct PaneInfo: Equatable {
  var id: String
  var window: String
  var windowName: String
  var command: String
  var x: Int
  var y: Int
  var width: Int
  var height: Int
  var active: Bool
  var cursorX: Int
  var cursorY: Int
  var zoomed: Bool
}

enum ControlDecoder {
  static func output(_ data: Data) -> (String, [UInt8])? {
    let bytes = Array(data)
    let prefix = Array("%output ".utf8)
    guard bytes.starts(with: prefix), let end = bytes.dropFirst(prefix.count).firstIndex(of: 32)
    else { return nil }
    let pane = String(decoding: bytes[prefix.count..<end], as: UTF8.self)
    return (pane, unescape(Array(bytes[(end + 1)...])))
  }
  static func unescape(_ bytes: [UInt8]) -> [UInt8] {
    var out: [UInt8] = []
    var i = 0
    while i < bytes.count {
      if bytes[i] == 92, i + 3 < bytes.count, bytes[i + 1] <= 51,
        bytes[(i + 1)...(i + 3)].allSatisfy({ $0 >= 48 && $0 <= 55 })
      {
        out.append((bytes[i + 1] - 48) * 64 + (bytes[i + 2] - 48) * 8 + bytes[i + 3] - 48)
        i += 4
      } else {
        out.append(bytes[i])
        i += 1
      }
    }
    return out
  }
  static func quote(_ text: String) -> String {
    "\""
      + text.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(
        of: "\"", with: "\\\""
      ).replacingOccurrences(of: "\n", with: "\\n").replacingOccurrences(of: "\r", with: "\\r")
      + "\""
  }
}

final class TmuxBackend {
  let paths: PrototypePaths
  var onOutput: ((String, [UInt8]) -> Void)?
  var onEvent: (() -> Void)?
  var process: Process?
  var input: FileHandle?
  var pending = Data()
  var client = ""
  var editorPaneID = ""
  init(_ paths: PrototypePaths) { self.paths = paths }
  @discardableResult func run(_ args: [String]) -> String {
    var environment = ProcessInfo.processInfo.environment
    environment["PATH"] =
      "/opt/homebrew/bin:" + NSHomeDirectory() + "/.local/bin:"
      + (environment["PATH"] ?? "/usr/bin:/bin:/usr/sbin:/sbin")
    environment.removeValue(forKey: "TMUX")
    environment.removeValue(forKey: "NVIM")
    return CommandRunner.capture(paths.tmux, ["-S", paths.socket] + args, environment: environment)
  }
  func prepare() throws {
    try FileManager.default.createDirectory(
      atPath: paths.runtime, withIntermediateDirectories: true,
      attributes: [.posixPermissions: 0o700])
    try Self.prepareSocketDirectory(paths.socketDirectory)
    if run(["has-session", "-t", "prototype"]).isEmpty {
      rememberEditorPane()
      return
    }
    let result = run([
      "-f", paths.config, "new-session", "-d", "-s", "prototype", "-x", "165", "-y", "44", "-c",
      paths.sample, paths.nvim, "--listen", paths.nvimSocket, paths.sample + "/parser.lua",
    ])
    guard result.isEmpty else {
      throw NSError(domain: "tmux", code: 1, userInfo: [NSLocalizedDescriptionKey: result])
    }
    _ = run(["split-window", "-h", "-p", "34", "-t", "prototype:1", "-c", paths.sample])
    _ = run(["split-window", "-v", "-p", "50", "-t", "prototype:1.2", "-c", paths.sample])
    _ = run(["select-pane", "-t", "prototype:1.1"])
    rememberEditorPane()
  }
  private func rememberEditorPane() {
    let saved = run(["show-options", "-qv", "-t", "prototype", "@workspace_editor_pane"])
      .trimmingCharacters(in: .whitespacesAndNewlines)
    if saved.hasPrefix("%") { editorPaneID = saved; return }
    editorPaneID = run(["display-message", "-p", "-t", "prototype:1.1", "#{pane_id}"])
      .trimmingCharacters(in: .whitespacesAndNewlines)
    if editorPaneID.hasPrefix("%") {
      _ = run(["set-option", "-t", "prototype", "@workspace_editor_pane", editorPaneID])
    }
  }
  static func prepareSocketDirectory(_ path: String) throws {
    if !FileManager.default.fileExists(atPath: path) {
      try FileManager.default.createDirectory(
        atPath: path, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
    }
    let attributes = try FileManager.default.attributesOfItem(atPath: path)
    guard attributes[.type] as? FileAttributeType == .typeDirectory,
      (attributes[.ownerAccountID] as? NSNumber)?.uint32Value == getuid(),
      (attributes[.posixPermissions] as? NSNumber)?.intValue == 0o700
    else {
      throw NSError(
        domain: "socket", code: 1,
        userInfo: [
          NSLocalizedDescriptionKey:
            "El directorio de sockets debe ser propio, privado y no ser un enlace simbólico."
        ])
    }
  }
  func attach() throws {
    let p = Process()
    let incoming = Pipe()
    let outgoing = Pipe()
    let error = Pipe()
    p.executableURL = URL(fileURLWithPath: paths.tmux)
    p.arguments = ["-S", paths.socket, "-C", "attach-session", "-t", "prototype"]
    p.standardInput = incoming
    p.standardOutput = outgoing
    p.standardError = error
    outgoing.fileHandleForReading.readabilityHandler = { [weak self] handle in
      let data = handle.availableData
      guard !data.isEmpty else { return }
      DispatchQueue.main.async { self?.receive(data) }
    }
    // Error text is bounded to the UI; never persist terminal transcripts.
    error.fileHandleForReading.readabilityHandler = { handle in _ = handle.availableData }
    try p.run()
    process = p
    input = incoming.fileHandleForWriting
    command("display-message -p \"__WORKSPACE_CLIENT__#{client_name}\"")
  }
  func receive(_ data: Data) {
    pending.append(data)
    while let end = pending.firstIndex(of: 10) {
      let line = Data(pending[..<end])
      pending.removeSubrange(...end)
      if let (pane, bytes) = ControlDecoder.output(line) {
        onOutput?(pane, bytes)
      } else {
        let s = String(decoding: line, as: UTF8.self)
        if s.hasPrefix("__WORKSPACE_CLIENT__") {
          client = String(s.dropFirst("__WORKSPACE_CLIENT__".count))
        }
        if s.hasPrefix("%") && !s.hasPrefix("%begin") && !s.hasPrefix("%end") { onEvent?() }
      }
    }
  }
  func command(_ value: String) { try? input?.write(contentsOf: Data((value + "\n").utf8)) }
  func keys(_ keys: [String]) {
    guard !client.isEmpty else { return }
    command(
      "send-keys -K -c \(ControlDecoder.quote(client)) -- "
        + keys.map(ControlDecoder.quote).joined(separator: " "))
  }
  func literal(_ value: String) {
    guard !client.isEmpty else { return }
    command("send-keys -K -l -c \(ControlDecoder.quote(client)) -- \(ControlDecoder.quote(value))")
  }
  func bytes(_ bytes: [UInt8], to pane: String) {
    guard !bytes.isEmpty else { return }
    for offset in stride(from: 0, to: bytes.count, by: 2048) {
      let chunk = bytes[offset..<min(offset + 2048, bytes.count)]
      command(
        "send-keys -H -t \(ControlDecoder.quote(pane)) -- "
          + chunk.map { String(format: "%02x", $0) }.joined(separator: " "))
    }
  }
  func panes() -> [PaneInfo] {
    let fmt =
      "#{pane_id}\t#{window_id}\t#{window_name}\t#{pane_current_command}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t#{pane_active}\t#{cursor_x}\t#{cursor_y}\t#{window_zoomed_flag}"
    return run(["list-panes", "-t", "prototype:", "-F", fmt]).split(separator: "\n").compactMap {
      line in
      let f = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
      guard f.count == 12, f[0].hasPrefix("%") else { return nil }
      return PaneInfo(
        id: f[0], window: f[1], windowName: f[2], command: f[3], x: Int(f[4]) ?? 0,
        y: Int(f[5]) ?? 0, width: Int(f[6]) ?? 80, height: Int(f[7]) ?? 24, active: f[8] == "1",
        cursorX: Int(f[9]) ?? 0, cursorY: Int(f[10]) ?? 0, zoomed: f[11] == "1")
    }
  }
  func stop() {
    command("detach-client")
    try? input?.close()
    input = nil
  }
}
