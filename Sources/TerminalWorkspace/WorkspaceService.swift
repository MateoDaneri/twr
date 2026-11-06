import Foundation
import CryptoKit

struct LiveWorkspace {
  var state: WorkspaceState
  var client: String
  var project: String?
  var busyWindows: Set<String>
  var pendingWindows: Set<String>
}

enum WorkspaceServiceError: LocalizedError {
  case command(String)
  case disconnected
  case invalidTarget
  var errorDescription: String? {
    switch self {
    case .command(let command): return "No respondió tmux al ejecutar \(command)."
    case .disconnected: return "El cliente está desconectado. La sesión sigue en tmux."
    case .invalidTarget: return "El destino cambió. Actualizá el workspace."
    }
  }
}

final class WorkspaceService {
  let environment: ApplicationEnvironment
  let queue = DispatchQueue(label: "local.terminalworkspace.commands", qos: .userInitiated)
  var onChange: (() -> Void)?
  private var control: Process?
  private var controlInput: FileHandle?
  private var names: [String: String] = [:]
  private var eventBuffer = Data()

  init(_ environment: ApplicationEnvironment) { self.environment = environment }

  func run(_ arguments: [String], timeout: TimeInterval = 3) throws -> String {
    let result = CommandRunner.capture(environment.tmux, ["-S", environment.socket] + arguments,
      environment: environment.processEnvironment, timeout: timeout)
    if result.hasPrefix("ERROR:") { throw WorkspaceServiceError.command(arguments.first ?? "") }
    return result
  }
  static func validID(_ text: String, prefix: Character) -> Bool {
    text.first == prefix && text.count > 1 && text.dropFirst().allSatisfy { $0.isASCII && $0.isNumber }
  }
  private func name(_ id: String, format: String) throws -> String {
    if let value = names[id] { return value }
    var value = try run(["display-message", "-p", "-t", id, format])
    if value.hasSuffix("\n") { value.removeLast() }
    // Display text only; it is never an argument used to address a tmux object.
    value = value.replacingOccurrences(of: "\n", with: " ↵ ")
      .replacingOccurrences(of: "\t", with: " ⇥ ")
      .unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }
      .map(String.init).joined()
    names[id] = value
    return value
  }

  func prepare(preferredSession: String? = nil, completion: @escaping (Result<String, Error>) -> Void) {
    queue.async {
      do {
        try self.environment.prepareStorage()
        guard FileManager.default.isExecutableFile(atPath: self.environment.tmux),
          FileManager.default.isExecutableFile(atPath: self.environment.nvim)
        else { throw WorkspaceServiceError.command("encontrar tmux y Neovim") }
        let ids = ((try? self.run(["list-sessions", "-F", "#{session_id}"])) ?? "")
          .split(separator: "\n").map(String.init).filter { Self.validID($0, prefix: "$") }
        let session: String
        if let existing = preferredSession.flatMap({ ids.contains($0) ? $0 : nil }) ?? ids.first {
          session = existing
        } else {
          var command = ["-f", self.environment.configuration, "new-session", "-d", "-P",
            "-F", "#{session_id}", "-s", "workspace", "-x", "140", "-y", "42", "-c", self.environment.sample,
            self.environment.nvim]
          if self.environment.demo {
            command += ["-u", self.environment.resources.appendingPathComponent("Fixtures/nvim/init.lua").path,
                        "-i", "NONE", "--noplugin", "-n"]
          }
          command += [self.environment.sample + "/parser.lua"]
          session = try self.run(command, timeout: 10).trimmingCharacters(in: .whitespacesAndNewlines)
          guard Self.validID(session, prefix: "$") else {
            throw WorkspaceServiceError.command("crear sesión")
          }
          _ = try self.run(["split-window", "-h", "-p", "32", "-t", session + ":", "-c", self.environment.sample])
          _ = try self.run(["select-pane", "-t", session + ":.1"])
        }
        self.startMetadata(session)
        DispatchQueue.main.async { completion(.success(session)) }
      } catch { DispatchQueue.main.async { completion(.failure(error)) } }
    }
  }

  private func startMetadata(_ session: String) {
    guard control == nil else { return }
    let process = Process(), input = Pipe(), output = Pipe(), errors = Pipe()
    process.executableURL = URL(fileURLWithPath: environment.tmux)
    process.arguments = ["-S", environment.socket, "-C", "attach-session",
                         "-f", "ignore-size,no-output", "-t", session]
    process.environment = environment.processEnvironment
    process.standardInput = input
    process.standardOutput = output
    process.standardError = errors
    output.fileHandleForReading.readabilityHandler = { [weak self] handle in
      let bytes = handle.availableData
      guard !bytes.isEmpty else { handle.readabilityHandler = nil; return }
      self?.queue.async { [weak self] in self?.receive(bytes) }
    }
    errors.fileHandleForReading.readabilityHandler = { handle in _ = handle.availableData }
    do {
      try process.run()
      control = process
      controlInput = input.fileHandleForWriting
    } catch {}
  }
  private func receive(_ data: Data) {
    eventBuffer.append(data)
    guard eventBuffer.count < 1_048_576 else { eventBuffer.removeAll(); return }
    var changed = false
    while let end = eventBuffer.firstIndex(of: 10) {
      let line = String(decoding: eventBuffer[..<end], as: UTF8.self)
      eventBuffer.removeSubrange(...end)
      if ["%window-", "%sessions-changed", "%session-", "%layout-change", "%pane-mode-changed"]
        .contains(where: { line.hasPrefix($0) }) { changed = true }
    }
    if changed {
      names.removeAll()
      DispatchQueue.main.async { self.onChange?() }
    }
  }

  func refresh(clientPID: Int32, completion: @escaping (Result<LiveWorkspace, Error>) -> Void) {
    queue.async {
      do {
        let live = try self.snapshot(clientPID: clientPID)
        DispatchQueue.main.async { completion(.success(live)) }
      } catch { DispatchQueue.main.async { completion(.failure(error)) } }
    }
  }
  func snapshot(clientPID: Int32) throws -> LiveWorkspace {
    let sessionIDs = try run(["list-sessions", "-F", "#{session_id}"])
      .split(separator: "\n").map(String.init).filter { Self.validID($0, prefix: "$") }
    var sessionMap: [String: SessionState] = [:]
    for id in sessionIDs {
      let project = try run(["show-options", "-qv", "-t", id, "@workspace_root"])
        .trimmingCharacters(in: .newlines)
      sessionMap[id] = SessionState(id: id, name: try name(id, format: "#{session_name}"), windows: [],
                                   projectPath: project.isEmpty ? nil : project)
    }
    var windowMap: [String: WindowState] = [:]
    var busy = Set<String>(), pending = Set<String>()
    let windowRows = try run(["list-windows", "-a", "-F",
      "#{session_id}\t#{window_id}\t#{window_index}\t#{@claude_busy}\t#{@claude_pending}"])
    for line in windowRows.split(separator: "\n") {
      let fields = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
      guard fields.count == 5, Self.validID(fields[0], prefix: "$"),
        Self.validID(fields[1], prefix: "@"), let index = Int(fields[2])
      else { continue }
      sessionMap[fields[0]]?.windows.append(WindowLink(windowID: fields[1], index: index))
      if windowMap[fields[1]] == nil {
        windowMap[fields[1]] = WindowState(id: fields[1],
          name: try name(fields[1], format: "#{window_name}"), paneIDs: [])
      }
      if fields[3] == "1" { busy.insert(fields[1]) }
      if fields[4] == "1" { pending.insert(fields[1]) }
    }
    var panes: [PaneState] = []
    var seenPanes = Set<String>()
    let paneRows = try run(["list-panes", "-a", "-F",
      "#{window_id}\t#{pane_id}\t#{pane_width}\t#{pane_height}"])
    for line in paneRows.split(separator: "\n") {
      let fields = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
      guard fields.count == 4, Self.validID(fields[0], prefix: "@"),
        Self.validID(fields[1], prefix: "%"), let columns = Int(fields[2]), let rows = Int(fields[3]),
        windowMap[fields[0]] != nil, seenPanes.insert(fields[1]).inserted
      else { continue }
      windowMap[fields[0]]?.paneIDs.append(fields[1])
      panes.append(PaneState(id: fields[1], columns: columns, rows: rows))
    }
    let clientRows = try run(["list-clients", "-F", "#{client_pid}\t#{client_name}\t#{client_control_mode}"])
    var client = ""
    for line in clientRows.split(separator: "\n") {
      let fields = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
      if fields.count == 3, fields[0] == String(clientPID), fields[2] == "0" { client = fields[1] }
    }
    guard !client.isEmpty else { throw WorkspaceServiceError.disconnected }
    let selected = try run(["display-message", "-p", "-c", client,
                            "#{session_id}\t#{window_id}\t#{pane_id}"])
      .trimmingCharacters(in: .newlines).split(separator: "\t").map(String.init)
    guard selected.count == 3 else { throw WorkspaceServiceError.disconnected }
    let selection = ClientSelection(sessionID: selected[0], windowID: selected[1], paneID: selected[2])
    let state = WorkspaceState(serverID: environment.demo ? "demo" : "personal",
      sessions: sessionMap.values.sorted { $0.id < $1.id },
      windows: windowMap.values.sorted { $0.id < $1.id }, panes: panes, selection: selection)
    try state.validate()
    let project = state.sessions.first(where: { $0.id == selection.sessionID })?.projectPath
    return LiveWorkspace(state: state, client: client, project: project,
                         busyWindows: busy, pendingWindows: pending)
  }

  func perform(_ arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
    queue.async {
      do {
        let output = try self.run(arguments, timeout: 10)
        self.names.removeAll()
        DispatchQueue.main.async { completion(.success(output)) }
      } catch { DispatchQueue.main.async { completion(.failure(error)) } }
    }
  }
  func openProject(_ path: String, client: String,
                   completion: @escaping (Result<String, Error>) -> Void) {
    queue.async {
      do {
        let digest = SHA256.hash(data: Data(path.utf8)).prefix(5).map { String(format:"%02x",$0) }.joined()
        let optionName = "project-" + digest
        let exists = (try? self.run(["has-session", "-t", "=" + optionName])) != nil
        var session = optionName
        if !exists {
          let created = try self.run(["new-session", "-d", "-P", "-F", "#{session_id}",
                                      "-s", optionName, "-c", path], timeout: 10)
            .trimmingCharacters(in: .newlines)
          guard Self.validID(created, prefix: "$") else { throw WorkspaceServiceError.invalidTarget }
          session = created
          _ = try self.run(["rename-session", "-t", session,
                            optionName])
          _ = try self.run(["set-option", "-t", session, "@workspace_root", path])
        }
        _ = try self.run(["switch-client", "-c", client, "-t", session])
        self.names.removeAll()
        DispatchQueue.main.async { completion(.success(path)) }
      } catch { DispatchQueue.main.async { completion(.failure(error)) } }
    }
  }
  func stop() {
    try? controlInput?.write(contentsOf: Data("detach-client\n".utf8))
    try? controlInput?.close()
    controlInput = nil
    control = nil
  }
}
