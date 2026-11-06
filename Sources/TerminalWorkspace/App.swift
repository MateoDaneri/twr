import AppKit
import SwiftTerm
import WebKit

@main struct Main {
  static func main() {
    if CommandLine.arguments.contains("--self-test") {
      SelfChecks.run()
      return
    }
    if let index = CommandLine.arguments.firstIndex(of: "--workspace-snapshot"),
      index + 1 < CommandLine.arguments.count,
      let pid = Int32(CommandLine.arguments[index + 1]) {
      do {
        let service = WorkspaceService(ApplicationEnvironment(demo: true))
        let live = try service.snapshot(clientPID: pid)
        let data = try JSONEncoder().encode(live.state)
        print(String(decoding: data, as: UTF8.self))
      } catch { fputs("Workspace snapshot failed: \(error)\n", stderr); exit(1) }
      return
    }
    let app = NSApplication.shared
    app.setActivationPolicy(.regular)
    let demo = CommandLine.arguments.contains("--demo")
      || Bundle.main.object(forInfoDictionaryKey: "WorkspaceDemoMode") as? Bool == true
    let controller: NSObject & NSApplicationDelegate
    if CommandLine.arguments.contains("--legacy-prototype") {
      controller = WorkspaceController()
    } else {
      controller = WorkspaceApplication(demo: demo)
    }
    app.delegate = controller
    withExtendedLifetime(controller) { app.run() }
  }
}

final class Canvas: NSView {
  var resized: (() -> Void)?
  override var isFlipped: Bool { true }
  override func layout() {
    super.layout()
    resized?()
  }
}

final class PaneBox: NSView {
  let terminal: TerminalView
  let heading = NSTextField(labelWithString: "")
  let button = NSButton(title: "↗", target: nil, action: nil)
  let id: String
  init(id: String, profile: GhosttyProfile) {
    self.id = id
    terminal = TerminalView(frame: .zero, font: profile.font)
    super.init(frame: .zero)
    wantsLayer = true
    layer?.cornerRadius = 8
    layer?.borderWidth = 1
    layer?.backgroundColor = NSColor(hex: "#192b34").cgColor
    terminal.nativeBackgroundColor = profile.background
    terminal.nativeForegroundColor = profile.foreground
    terminal.caretColor = profile.accent
    terminal.selectedTextBackgroundColor = NSColor(hex: "#285565")
    terminal.minimumContrastRatio = 1
    terminal.installProfilePalette(profile)
    heading.font = .systemFont(ofSize: 12, weight: .medium)
    heading.textColor = .white
    addSubview(terminal)
    addSubview(heading)
    addSubview(button)
  }
  required init?(coder: NSCoder) { fatalError() }
  override func layout() {
    super.layout()
    heading.frame = NSRect(
      x: 10, y: bounds.height - 27, width: max(10, bounds.width - 50), height: 20)
    button.frame = NSRect(x: bounds.width - 34, y: bounds.height - 29, width: 27, height: 23)
    terminal.frame = NSRect(
      x: 2, y: 2, width: max(1, bounds.width - 4), height: max(1, bounds.height - 34))
  }
}

final class WorkspaceController: NSObject, NSApplicationDelegate, TerminalViewDelegate,
  LocalProcessTerminalViewDelegate, NSWindowDelegate
{
  let paths: PrototypePaths
  let profile: GhosttyProfile
  let backend: TmuxBackend
  var window: NSWindow!
  let root = Canvas(), content = Canvas(), sidebar = NSView(), toolbar = NSView(),
    status = NSTextField(labelWithString: "")
  var compatible: LocalProcessTerminalView!
  var boxes: [String: PaneBox] = [:]
  var paneInfos: [PaneInfo] = []
  var active = ""
  var nativeMode = false
  var previewVisible = false
  var zoomed: String?
  var modeControl: NSSegmentedControl!
  var preview: WKWebView!
  var sideButtons: [NSButton] = []
  var timer: Timer?
  var keyMonitor: Any?
  var mouseMonitor: Any?
  var lastGeometry = ""
  var lastEditorGeometry = ""
  var updateQueued = false
  var terminating = false
  let editorRPC = DispatchQueue(label: "local.terminalworkspace.editor-rpc")
  init(root: String? = nil) {
    let executable = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL
    let fallback = executable.deletingLastPathComponent().deletingLastPathComponent()
      .deletingLastPathComponent().path
    let selected =
      root ?? ProcessInfo.processInfo.environment["TERMINAL_WORKSPACE_ROOT"] ?? Bundle.main.object(
        forInfoDictionaryKey: "PrototypeSourceRoot") as? String ?? fallback
    paths = PrototypePaths(root: selected)
    profile = GhosttyProfile(path: NSHomeDirectory() + "/.config/ghostty/config")
    backend = TmuxBackend(paths)
    super.init()
  }
  func traceStartup(_ stage:String) {
    let value="pid=\(getpid()) stage=\(stage)\n"
    try? Data(value.utf8).write(to:URL(fileURLWithPath:paths.socketDirectory+"/startup.txt"),options:.atomic)
  }
  func applicationDidFinishLaunching(_ notification: Notification) {
    traceStartup("prepare")
    do { try backend.prepare() } catch {
      let a = NSAlert()
      a.messageText = "No se pudo iniciar la sesión aislada"
      a.informativeText = error.localizedDescription
      a.runModal()
      NSApp.terminate(nil)
      return
    }
    traceStartup("build-window")
    buildMenu()
    buildWindow()
    traceStartup("window-built")
    backend.onOutput = { [weak self] pane, bytes in
      self?.boxes[pane]?.terminal.feed(byteArray: bytes[...])
    }
    backend.onEvent = { [weak self] in self?.queueRefresh() }
    do { try backend.attach() } catch { status.stringValue = error.localizedDescription }
    traceStartup("initial-refresh")
    refresh()
    traceStartup("refreshed")
    timer = Timer.scheduledTimer(withTimeInterval: 0.65, repeats: true) { [weak self] _ in
      Task { @MainActor in self?.refresh() }
    }
    keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
      guard let self, event.window === self.window else { return event }
      return self.handle(event) ? nil : event
    }
    mouseMonitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
      guard let self, self.nativeMode, event.window === self.window else { return event }
      for (id, box) in self.boxes where !box.isHidden {
        if box.bounds.contains(box.convert(event.locationInWindow, from: nil)) {
          self.select(id)
          break
        }
      }
      return event
    }
    traceStartup("present-window")
    window.makeKeyAndOrderFront(nil)
    NSApp.activate(ignoringOtherApps: true)
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { [weak self] in
      self?.focusActive()
      self?.redrawNvim()
    }
  }
  func buildMenu() {
    let main = NSMenu()
    let appItem = NSMenuItem()
    main.addItem(appItem)
    let appMenu = NSMenu()
    appMenu.addItem(
      withTitle: "Salir de Terminal Workspace", action: #selector(NSApplication.terminate(_:)),
      keyEquivalent: "q")
    appItem.submenu = appMenu
    let editItem = NSMenuItem()
    main.addItem(editItem)
    let edit = NSMenu(title: "Edición")
    edit.addItem(withTitle: "Copiar", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
    edit.addItem(withTitle: "Pegar", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
    editItem.submenu = edit
    NSApp.mainMenu = main
  }
  func buildWindow() {
    window = NSWindow(
      contentRect: NSRect(x: 0, y: 0, width: 1320, height: 860),
      styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false
    )
    window.title = "Terminal Workspace · prototipo real"
    window.delegate = self
    window.minSize = NSSize(width: 900, height: 600)
    window.center()
    window.contentView = root
    root.wantsLayer = true
    root.layer?.backgroundColor = NSColor(hex: "#111d24").cgColor
    sidebar.wantsLayer = true
    sidebar.layer?.backgroundColor = NSColor(hex: "#14232b").cgColor
    root.addSubview(toolbar)
    root.addSubview(sidebar)
    root.addSubview(content)
    root.addSubview(status)
    let title = NSTextField(labelWithString: "Terminal Workspace")
    title.font = .systemFont(ofSize: 14, weight: .semibold)
    title.textColor = .white
    title.frame = NSRect(x: 16, y: 13, width: 205, height: 22)
    toolbar.addSubview(title)
    modeControl = NSSegmentedControl(
      labels: ["Paneles · exp.", "tmux completo"], trackingMode: .selectOne, target: self,
      action: #selector(changeMode))
    modeControl.selectedSegment = nativeMode ? 0 : 1
    modeControl.frame = NSRect(x: 230, y: 10, width: 230, height: 28)
    toolbar.addSubview(modeControl)
    let previewButton = NSButton(
      title: "Guía / Preview", target: self, action: #selector(togglePreview))
    previewButton.frame = NSRect(x: 480, y: 10, width: 130, height: 28)
    toolbar.addSubview(previewButton)
    let closeNote = NSTextField(labelWithString: "Sesión aislada · configs reales")
    closeNote.font = .systemFont(ofSize: 11)
    closeNote.textColor = NSColor(hex: "#afc0c9")
    closeNote.frame = NSRect(x: 630, y: 14, width: 260, height: 19)
    toolbar.addSubview(closeNote)
    compatible = LocalProcessTerminalView(
      frame: NSRect(x: 0, y: 0, width: 1000, height: 700), font: profile.font,
      options: TerminalOptions())
    compatible.nativeBackgroundColor = profile.background
    compatible.nativeForegroundColor = profile.foreground
    compatible.caretColor = profile.accent
    compatible.processDelegate = self
    content.addSubview(compatible)
    compatible.installProfilePalette(profile)
    let webConfig = WKWebViewConfiguration()
    webConfig.defaultWebpagePreferences.allowsContentJavaScript = false
    preview = WKWebView(frame: .zero, configuration: webConfig)
    content.addSubview(preview)
    preview.isHidden = true
    let guide = URL(fileURLWithPath: paths.root + "/docs/RUNBOOK.html")
    preview.loadFileURL(guide, allowingReadAccessTo: guide.deletingLastPathComponent())
    status.font = .systemFont(ofSize: 11)
    status.textColor = NSColor(hex: "#afc0c9")
    status.lineBreakMode = .byTruncatingMiddle
    root.resized = { [weak self] in self?.layoutRoot() }
    content.resized = { [weak self] in self?.layoutPanes() }
    layoutRoot()
    var env = ProcessInfo.processInfo.environment
    env["TERM"] = "xterm-256color"
    env["PATH"] =
      "/opt/homebrew/bin:" + NSHomeDirectory() + "/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    env.removeValue(forKey: "TMUX")
    compatible.startProcess(
      executable: paths.tmux, args: ["-S", paths.socket, "attach-session", "-t", "prototype"],
      environment: env.map { "\($0.key)=\($0.value)" }, currentDirectory: paths.sample)
  }
  func layoutRoot() {
    let w = root.bounds.width
    let h = root.bounds.height
    toolbar.frame = NSRect(x: 0, y: 0, width: w, height: 48)
    sidebar.frame = NSRect(x: 0, y: 48, width: 178, height: h - 76)
    content.frame = NSRect(x: 190, y: 58, width: max(100, w - 202), height: max(100, h - 98))
    status.frame = NSRect(x: 12, y: h - 28, width: w - 24, height: 22)
    layoutPanes()
  }
  func layoutPanes() {
    guard compatible != nil else { return }
    let area = content.bounds
    let previewWidth: CGFloat = previewVisible ? min(420, area.width * 0.35) : 0
    let work = NSRect(
      x: 0, y: 0, width: area.width - previewWidth - (previewVisible ? 10 : 0), height: area.height)
    compatible.frame = work
    compatible.isHidden = nativeMode
    preview.isHidden = !previewVisible
    preview.frame = NSRect(x: work.maxX + 10, y: 0, width: previewWidth, height: area.height)
    let all = paneInfos
    let cols = all.map { $0.x + $0.width }.max() ?? 165
    let rows = all.map { $0.y + $0.height }.max() ?? 44
    let effectiveZoom = zoomed ?? all.first(where: { $0.active && $0.zoomed })?.id
    for p in all {
      guard let box = boxes[p.id] else { continue }
      box.isHidden = !nativeMode || (effectiveZoom != nil && effectiveZoom != p.id)
      if effectiveZoom == p.id {
        box.frame = work
      } else {
        let x = CGFloat(p.x) / CGFloat(cols) * work.width
        let y = CGFloat(p.y) / CGFloat(rows) * work.height
        let width = CGFloat(p.width) / CGFloat(cols) * work.width
        let height = CGFloat(p.height) / CGFloat(rows) * work.height
        box.frame = NSRect(x: x, y: y, width: max(30, width - 5), height: max(50, height - 5))
      }
      // Hydration must see the final grid, not TerminalView's initial 80x25.
      box.layoutSubtreeIfNeeded()
      box.layer?.borderColor = (p.id == active ? profile.accent : NSColor(hex: "#304751")).cgColor
    }
    if nativeMode, work.width > 300, work.height > 200 {
      let font = profile.font
      let cellWidth = max(5, "M".size(withAttributes: [.font: font]).width)
      let cellHeight = ceil(font.ascender - font.descender + font.leading)
      let stackCount = max(1, all.filter { $0.x == (all.map { $0.x }.max() ?? 0) }.count)
      let c = max(60, Int((work.width - 24) / cellWidth))
      let r = max(15, Int((work.height - CGFloat(stackCount * 38)) / max(15, cellHeight)))
      let geo = "\(c)x\(r)"
      if geo != lastGeometry {
        lastGeometry = geo
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.12) { [weak self] in
          guard let self, self.nativeMode else { return }
          _ = self.backend.run([
            "resize-window", "-t", "prototype:", "-x", String(c), "-y", String(r),
          ])
          self.queueRefresh()
        }
      }
    }
  }
  func queueRefresh() {
    guard !updateQueued else { return }
    updateQueued = true
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.08) { [weak self] in
      self?.updateQueued = false
      self?.refresh()
    }
  }
  func refresh() {
    guard !terminating else { return }
    let panes = backend.panes()
    guard !panes.isEmpty else {
      status.stringValue =
        "La sesión no tiene panes activos. Abrí de nuevo la app para recuperar la sesión."
      return
    }
    let rehydrate = panes.filter { pane in
      guard let old = paneInfos.first(where: { $0.id == pane.id }) else { return true }
      return old.width != pane.width || old.height != pane.height
    }
    let ids = Set(panes.map(\.id))
    for id in Array(boxes.keys) where !ids.contains(id) {
      boxes[id]?.removeFromSuperview()
      boxes.removeValue(forKey: id)
    }
    for p in panes where boxes[p.id] == nil {
      let box = PaneBox(id: p.id, profile: profile)
      box.terminal.terminalDelegate = self
      box.button.target = self
      box.button.action = #selector(maximizePane(_:))
      box.button.identifier = NSUserInterfaceItemIdentifier(p.id)
      boxes[p.id] = box
      content.addSubview(box)
    }
    let changed = paneInfos != panes
    let previous = active
    paneInfos = panes
    if let editor = panes.first(where: { $0.id == backend.editorPaneID }) {
      let geometry = "\(editor.width)x\(editor.height)"
      if geometry != lastEditorGeometry {
        lastEditorGeometry = geometry
        redrawNvim()
      }
    }
    active = panes.first(where: { $0.active })?.id ?? panes[0].id
    for p in panes { boxes[p.id]?.heading.stringValue = "\(p.command)  ·  tmux \(p.id)" }
    if changed {
      layoutPanes()
      rebuildSidebar()
    }
    if nativeMode { for pane in rehydrate { hydrate(pane) } }
    if previous != active && nativeMode { focusActive() }
    status.stringValue =
      "\(nativeMode ? "Paneles gráficos" : "tmux completo")  ·  \(panes.count) panes  ·  foco \(active)  ·  Ctrl+A y Ctrl+T abren tmux completo para menús/popups"
    writeHealth()
  }
  func hydrate(_ p: PaneInfo) {
    traceStartup("hydrate-"+p.id)
    let text = backend.run(["capture-pane", "-e", "-p", "-t", p.id])
    traceStartup("feed-"+p.id)
    boxes[p.id]?.terminal.feed(
      text: "\u{1b}[0m\u{1b}[2J\u{1b}[H" + text.replacingOccurrences(of: "\n", with: "\r\n")
        + "\u{1b}[\(p.cursorY+1);\(p.cursorX+1)H")
  }
  func rebuildSidebar() {
    sideButtons.forEach { $0.removeFromSuperview() }
    sideButtons = []
    let head = NSTextField(labelWithString: "PROTOTIPO")
    head.font = .systemFont(ofSize: 10, weight: .semibold)
    head.textColor = NSColor(hex: "#afc0c9")
    head.frame = NSRect(x: 14, y: sidebar.bounds.height - 38, width: 145, height: 22)
    sidebar.addSubview(head)
    // Keep labels in the same collection so rebuilding doesn't accumulate them.
    for old in sidebar.subviews where old is NSTextField && old !== head {
      old.removeFromSuperview()
    }
    for (i, p) in paneInfos.enumerated() {
      let b = NSButton(
        title: "\(p.id)   \(p.command)", target: self, action: #selector(sideSelect(_:)))
      b.identifier = NSUserInterfaceItemIdentifier(p.id)
      b.bezelStyle = .rounded
      b.contentTintColor = p.id == active ? profile.accent : .white
      b.frame = NSRect(
        x: 10, y: sidebar.bounds.height - 80 - CGFloat(i * 40), width: 158, height: 31)
      sidebar.addSubview(b)
      sideButtons.append(b)
    }
    let note = NSTextField(
      wrappingLabelWithString:
        "tmux.conf y nvim reales\n\nMotor SwiftTerm\nNo usa GhosttyKit\n\nPaneles: prototipo\ntmux completo: compatibilidad"
    )
    note.font = .systemFont(ofSize: 11)
    note.textColor = NSColor(hex: "#afc0c9")
    note.frame = NSRect(x: 14, y: 24, width: 150, height: 160)
    sidebar.addSubview(note)
  }
  @objc func sideSelect(_ sender: NSButton) {
    if let id = sender.identifier?.rawValue {
      select(id)
      focusActive()
    }
  }
  @objc func maximizePane(_ sender: NSButton) {
    if let id = sender.identifier?.rawValue {
      zoomed = zoomed == id ? nil : id
      select(id)
      layoutPanes()
      focusActive()
    }
  }
  func select(_ id: String) {
    guard active != id else { return }
    _ = backend.run(["select-pane", "-t", id])
    active = id
    queueRefresh()
  }
  func focusActive() {
    if nativeMode, let terminal = boxes[active]?.terminal {
      window.makeFirstResponder(terminal)
    } else {
      window.makeFirstResponder(compatible)
    }
  }
  @objc func changeMode() { setMode(modeControl.selectedSegment == 0) }
  func setMode(_ native: Bool) {
    nativeMode = native
    modeControl.selectedSegment = native ? 0 : 1
    zoomed = nil
    lastGeometry = ""
    layoutPanes()
    if !native {
      let d = compatible.terminalDimensions
      _ = backend.run([
        "resize-window", "-t", "prototype:", "-x", String(d.cols), "-y", String(d.rows),
      ])
    }
    refresh()
    focusActive()
    if native {
      for p in paneInfos { hydrate(p) }
      redrawNvim()
    }
  }
  @objc func togglePreview() {
    previewVisible.toggle()
    layoutPanes()
    focusActive()
  }
  func redrawNvim() {
    guard let editor = paneInfos.first(where: { $0.id == backend.editorPaneID }) else { return }
    let executable = paths.nvim, socket = paths.nvimSocket
    // This RPC socket belongs only to the sample editor launched by the app.
    // Its TUI can retain old dimensions across reconnects, even after tmux
    // resized the PTY. Match the grid before requesting the next full redraw.
    let expression = "execute('set columns=\(max(12, editor.width)) lines=\(max(2, editor.height)) | redraw!')"
    editorRPC.async {
      _ = CommandRunner.capture(executable, ["--server", socket, "--remote-expr", expression],
                                environment: ProcessInfo.processInfo.environment, timeout: 2)
    }
  }
  func fallback(_ text: String) {
    setMode(false)
    DispatchQueue.main.asyncAfter(deadline: .now() + 0.12) { [weak self] in
      guard let self else { return }
      self.compatible.send(source: self.compatible, data: Array(text.utf8)[...])
    }
  }
  func handle(_ event: NSEvent) -> Bool {
    let terminalFocused = window.firstResponder is TerminalView
    guard terminalFocused else { return false }
    if let text = profile.text(for: event) {
      if !nativeMode {
        compatible.send(source: compatible, data: Array(text.utf8)[...])
        return true
      }
      if text == "\u{1}x" || text == "\u{1}&" {
        fallback(text)
        return true
      }
      let userKeys = [
        "\u{1b}[1;10A": "User0", "\u{1b}[1;10B": "User1", "\u{1b}[1;10C": "User2",
        "\u{1b}[1;10D": "User3",
      ]
      if let key = userKeys[text] {
        backend.keys([key])
      } else {
        backend.keys(
          text.unicodeScalars.map {
            $0.value < 32
              ? ($0.value == 13 ? "Enter" : "C-" + String(UnicodeScalar($0.value + 96)!))
              : String($0)
          })
      }
      queueRefresh()
      return true
    }
    guard nativeMode else { return false }
    if event.modifierFlags.contains(.command) { return false }
    let base = (event.charactersIgnoringModifiers ?? "").lowercased()
    if event.modifierFlags.contains(.control), ["a", "t"].contains(base) {
      fallback(base == "a" ? "\u{1}" : "\u{14}")
      return true
    }
    let functional: [UInt16: String] = [
      36: "Enter", 76: "Enter", 48: "Tab", 53: "Escape", 51: "BSpace", 117: "DC", 123: "Left",
      124: "Right", 125: "Down", 126: "Up", 115: "Home", 119: "End", 116: "PPage", 121: "NPage",
      122: "F1", 120: "F2", 99: "F3", 118: "F4", 96: "F5", 97: "F6", 98: "F7", 100: "F8", 101: "F9",
      109: "F10", 103: "F11", 111: "F12",
    ]
    let mods = event.modifierFlags
    let special = functional[event.keyCode]
    if special != nil || mods.contains(.control) || mods.contains(.option) {
      var key = special ?? base
      if mods.contains(.shift), special != nil || mods.contains(.control) { key = "S-" + key }
      if mods.contains(.option) { key = "M-" + key }
      if mods.contains(.control) { key = "C-" + key }
      backend.keys([key])
    } else if let text = event.characters {
      backend.literal(text)
    }
    queueRefresh()
    return true
  }
  func send(source: TerminalView, data: ArraySlice<UInt8>) {
    if let pane = boxes.first(where: { $0.value.terminal === source })?.key {
      backend.bytes(Array(data), to: pane)
    }
  }
  func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {}
  func setTerminalTitle(source: TerminalView, title: String) {}
  func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
  func scrolled(source: TerminalView, position: Double) {}
  func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}
  func programStatusChanged(source: TerminalView, records: [TerminalProgramStatus]) {}
  func kittyClipboardCapabilities(source: TerminalView) -> KittyClipboardCapabilities { [] }
  func kittyClipboardAvailableMimeTypes(source: TerminalView, location: KittyClipboardLocation)
    -> [String]?
  { nil }
  func kittyClipboardRead(source: TerminalView, location: KittyClipboardLocation, mimeType: String)
    -> KittyClipboardReadResult?
  { nil }
  func kittyClipboardWrite(
    source: TerminalView, location: KittyClipboardLocation, content: KittyClipboardWriteContent
  ) -> KittyClipboardWriteResult { .unsupported }
  func kittyClipboardRequestPermission(
    source: TerminalView, request: KittyClipboardPermissionRequest
  ) -> KittyClipboardPermissionResult { .deny }
  func clipboardCopy(source: TerminalView, content: Data) {
    if let s = String(data: content, encoding: .utf8) {
      NSPasteboard.general.clearContents()
      NSPasteboard.general.setString(s, forType: .string)
    }
  }
  func clipboardRead(source: TerminalView) -> Data? { nil }
  func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {
    if let url = URL(string: link), ["https", "http"].contains(url.scheme ?? "") {
      NSWorkspace.shared.open(url)
    }
  }
  func sizeChanged(source: LocalProcessTerminalView, newCols: Int, newRows: Int) {
    if !nativeMode {
      _ = backend.run([
        "resize-window", "-t", "prototype:", "-x", String(newCols), "-y", String(newRows),
      ])
    }
  }
  func setTerminalTitle(source: LocalProcessTerminalView, title: String) {}
  func processTerminated(source: TerminalView, exitCode: Int32?) {
    if !terminating {
      status.stringValue =
        "Cliente tmux desconectado. La sesión permanece en el socket del prototipo."
    }
  }
  func writeHealth() {
    let payload: [String: Any] = [
      "mode": nativeMode ? "panes" : "compatible", "socket": paths.socket, "activePane": active,
      "controlClient": backend.client, "ghosttyConfigFiles": profile.files,
      "importedTextKeybindings": profile.bindings.count, "tmuxConfig": paths.config,
      "panes": paneInfos.map {
        [
          "id": $0.id, "command": $0.command, "width": $0.width, "height": $0.height,
          "active": $0.active,
        ] as [String: Any]
      },
    ]
    if let d = try? JSONSerialization.data(
      withJSONObject: payload, options: [.prettyPrinted, .sortedKeys])
    {
      try? d.write(to: URL(fileURLWithPath: paths.runtime + "/health.json"), options: .atomic)
    }
  }
  func windowWillClose(_ notification: Notification) { NSApp.terminate(nil) }
  func applicationWillTerminate(_ notification: Notification) {
    terminating = true
    timer?.invalidate()
    if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
    if let mouseMonitor { NSEvent.removeMonitor(mouseMonitor) }
    backend.stop()
  }
}

extension TerminalView {
  func installProfilePalette(_ profile: GhosttyProfile) {
    let colors = (0..<16).compactMap { i -> Color? in
      guard let hex = profile.palette[i],
        let n = UInt64(hex.trimmingCharacters(in: CharacterSet(charactersIn: "#")), radix: 16)
      else { return nil }
      return Color(
        red8: UInt16((n >> 16) & 255), green8: UInt16((n >> 8) & 255), blue8: UInt16(n & 255))
    }
    if colors.count == 16 { installColors(colors) }
  }
}
