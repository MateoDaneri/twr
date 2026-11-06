import Foundation

enum SelfChecks {
  static func run() {
    var failures: [String] = []
    func check(_ value: Bool, _ name: String) { if !value { failures.append(name) } }
    check(GhosttyProfile.unescape("\\x01c") == "\u{1}c", "Ghostty text C-a c")
    check(GhosttyProfile.unescape("\\x1b[1;10D") == "\u{1b}[1;10D", "Ghostty navigation encoding")
    let sample = Data("%output %7 a\\015\\012\\134\\303\\261".utf8)
    let decoded = ControlDecoder.output(sample)
    check(
      decoded?.0 == "%7" && decoded?.1 == [97, 13, 10, 92, 195, 177],
      "Control mode preserves byte stream")
    check(
      ControlDecoder.output(Data("%layout-change @1".utf8)) == nil, "Events do not become output")
    check(ControlDecoder.quote("a\"\\\n") == "\"a\\\"\\\\\\n\"", "Control command quoting")
    let bundled = Bundle.main.resourceURL?.appendingPathComponent("Fixtures").path ?? ""
    let project = ProcessInfo.processInfo.environment["TERMINAL_WORKSPACE_ROOT"]
      ?? FileManager.default.currentDirectoryPath
    let fixtures = FileManager.default.fileExists(atPath: bundled + "/ghostty/config")
      ? bundled : project + "/Resources/Fixtures"
    let profile = GhosttyProfile(path: fixtures + "/ghostty/config")
    check(profile.bindings["super+shift+h"] == "\u{1b}[1;10D", "Reads fixture navigation binding")
    check(profile.bindings["super+d"] == "\u{1}%", "Reads fixture split binding")
    check(
      profile.bindings["super+shift+d"] == "\u{1}\"", "Preserves literal quote in horizontal split")
    check(profile.palette.count >= 16, "Reads all ANSI palette entries")
    check(
      PrototypePaths(root: String(repeating: "a", count: 500)).socket.utf8.count < 104,
      "Short Unix socket path")
    let temporary = "/private/tmp/terminal-workspace-check-" + UUID().uuidString
    do {
      try TmuxBackend.prepareSocketDirectory(temporary)
      check(true, "Create private socket directory")
    } catch { check(false, "Create private socket directory") }
    do {
      try TmuxBackend.prepareSocketDirectory(temporary)
      check(true, "Reuse private socket directory")
    } catch { check(false, "Reuse private socket directory") }
    var rejected = false
    do {
      try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: temporary)
      do { try TmuxBackend.prepareSocketDirectory(temporary) } catch { rejected = true }
    } catch {}
    check(rejected, "Reject socket directory with unsafe permissions")
    try? FileManager.default.removeItem(atPath: temporary)
    check(CommandRunner.capture("/usr/bin/printf", ["hello"], environment: [:]) == "hello", "Capture short command output")
    check(CommandRunner.capture("/usr/bin/false", [], environment: [:]).hasPrefix("ERROR:"), "Report unsuccessful command")
    let start = ProcessInfo.processInfo.systemUptime
    check(CommandRunner.capture("/bin/sleep", ["3"], environment: [:], timeout: 0.1) == "ERROR: command timed out"
          && ProcessInfo.processInfo.systemUptime - start < 1, "Bound command timeout")
    do {
      let bytes = try Data(contentsOf: URL(fileURLWithPath: fixtures + "/workspace.json"))
      let state = try JSONDecoder().decode(WorkspaceState.self, from: bytes)
      try state.validate()
      check(true, "Accept window linked into two sessions")
      check(state.sessions[0].name == "código", "Preserve Unicode display names")
      check(try JSONDecoder().decode(WorkspaceState.self, from: JSONEncoder().encode(state)) == state,
            "Round trip complete workspace snapshot")
      func rejects(_ candidate: WorkspaceState, _ error: WorkspaceStateError) -> Bool {
        do { try candidate.validate(); return false }
        catch let actual as WorkspaceStateError { return actual == error }
        catch { return false }
      }
      var changed = state
      changed.panes.append(state.panes[0])
      check(rejects(changed, .duplicateID("%0")), "Reject duplicate pane identity")
      changed = state
      changed.windows[0].paneIDs.append("%99")
      check(rejects(changed, .missingReference("%99")), "Reject dangling pane reference")
      changed = state
      changed.windows[1].paneIDs.append("%0")
      check(rejects(changed, .invalidPaneOwnership("%0")), "Reject pane in two windows")
      changed = state
      changed.selection?.paneID = "%3"
      check(rejects(changed, .invalidSelection), "Reject selection outside active window")
    } catch { failures.append("Workspace fixture: \(error)") }
    let result: [String: Any] = [
      "passed": failures.isEmpty, "checks": 23, "failures": failures,
      "configIncludes": profile.files.count, "textBindings": profile.bindings.count,
      "configurationSource": "synthetic fixtures",
    ]
    print(
      String(
        data: try! JSONSerialization.data(
          withJSONObject: result, options: [.prettyPrinted, .sortedKeys]), encoding: .utf8)!)
    if !failures.isEmpty { exit(1) }
  }
}
