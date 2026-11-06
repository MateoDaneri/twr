import AppKit
import CoreText
import SwiftTerm
import WebKit

@MainActor
final class WorkspaceApplication: NSObject, NSApplicationDelegate, NSWindowDelegate,
  LocalProcessTerminalViewDelegate, WKNavigationDelegate, NSTextFieldDelegate {
  let environment: ApplicationEnvironment
  let service: WorkspaceService
  let profile: GhosttyProfile
  var preferences: WorkspacePreferences
  var window: NSWindow!
  let root = Canvas(), sidebar = Canvas(), toolbar = NSView(), body = Canvas()
  let tabScroll = NSScrollView(), tabStack = NSStackView()
  let sessionStack = NSStackView(), paneStack = NSStackView()
  let sessionScroll = NSScrollView(), paneScroll = NSScrollView()
  var recentMenu: NSMenu!
  var terminalPolicy: TerminalPolicy?
  let status = NSTextField(labelWithString: "Preparando workspace…")
  let workspaceTitle = NSTextField(labelWithString: "Tu terminal, en casa.")
  let previewHeader = NSView()
  let urlField = NSTextField()
  let diffView = NSTextView()
  var diffScroll: NSScrollView!
  var web: WKWebView!
  var terminal: LocalProcessTerminalView!
  var live: LiveWorkspace?
  var currentRendering: WorkspaceState?
  var timer: Timer?
  var keyMonitor: Any?
  var refreshing = false
  var terminating = false
  var previewMode = 0
  var sessionButtons: [NSButton] = []
  var tabButtons: [NSButton] = []
  var paneButtons: [NSButton] = []
  var startupSession = ""
  var fontName = "Menlo"
  var lastError = ""

  init(demo: Bool) {
    environment = ApplicationEnvironment(demo: demo)
    service = WorkspaceService(environment)
    profile = environment.profile
    preferences = WorkspacePreferences.load(from: environment.dataRoot.appendingPathComponent("preferences.json"))
    super.init()
  }
  func applicationDidFinishLaunching(_ notification: Notification) {
    NSApp.appearance = NSAppearance(named: .darkAqua)
    configureFont()
    makeMenu()
    makeWindow()
    service.onChange = { [weak self] in self?.refresh() }
    keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
      guard let self, event.window === self.window,
        self.window.firstResponder is TerminalView,
        let text = self.profile.text(for: event) else { return event }
      self.terminal.send(source: self.terminal, data: Array(text.utf8)[...])
      return nil
    }
    window.makeKeyAndOrderFront(nil)
    NSApp.activate(ignoringOtherApps: true)
    service.prepare(preferredSession: preferences.lastSessionID) { [weak self] result in
      guard let self else { return }
      switch result {
      case .success(let session):
        self.startupSession = session
        self.connect(session)
      case .failure(let error):
        self.status.stringValue = error.localizedDescription + " · Revisá Diagnóstico."
        self.lastError = error.localizedDescription
      }
    }
  }
  func configureFont() {
    let font = environment.resources.appendingPathComponent("Fonts/JetBrainsMonoNerdFont-Regular.ttf")
    if let provider = CGDataProvider(url: font as CFURL), let graphic = CGFont(provider) {
      let ct = CTFontCreateWithGraphicsFont(graphic, 13, nil, nil)
      fontName = CTFontCopyPostScriptName(ct) as String
      CTFontManagerRegisterFontsForURL(font as CFURL, .process, nil)
    }
    if let explicit = profile.values["font-family"], !explicit.isEmpty {
      fontName = profile.font.fontName
    }
  }
  var currentFont: NSFont {
    NSFont(name: fontName, size: preferences.fontSize)
      ?? .monospacedSystemFont(ofSize: preferences.fontSize, weight: .regular)
  }
  func label(_ text: String, size: CGFloat = 12, color: String = "#a8bcb8") -> NSTextField {
    let field = NSTextField(labelWithString: text)
    field.font = .systemFont(ofSize: size)
    field.textColor = NSColor(hex: color)
    field.lineBreakMode = .byTruncatingTail
    return field
  }
  func button(_ title: String, _ action: Selector) -> NSButton {
    let value = NSButton(title: title, target: self, action: action)
    value.bezelStyle = .rounded
    value.font = .systemFont(ofSize: 12, weight: .medium)
    return value
  }
  func makeMenu() {
    let main = NSMenu()
    func submenu(_ title: String) -> NSMenu {
      let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
      let menu = NSMenu(title: title)
      main.addItem(item); item.submenu = menu
      return menu
    }
    let app = submenu("Terminal Workspace")
    app.addItem(withTitle: "Acerca de Terminal Workspace", action: #selector(about), keyEquivalent: "")
    app.addItem(.separator())
    app.addItem(withTitle: "Salir de Terminal Workspace", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
    let workspace = submenu("Workspace")
    workspace.addItem(withTitle: "Abrir carpeta…", action: #selector(openFolder), keyEquivalent: "")
    let recentItem = NSMenuItem(title: "Carpetas recientes", action: nil, keyEquivalent: "")
    recentMenu = NSMenu(title: "Carpetas recientes")
    recentItem.submenu = recentMenu
    workspace.addItem(recentItem)
    rebuildRecents()
    workspace.addItem(withTitle: "Renombrar sesión…", action: #selector(renameSession), keyEquivalent: "")
    workspace.addItem(withTitle: "Dividir al lado", action: #selector(splitHorizontal), keyEquivalent: "")
    workspace.addItem(withTitle: "Dividir abajo", action: #selector(splitVertical), keyEquivalent: "")
    workspace.addItem(withTitle: "Zoom del pane", action: #selector(zoomPane), keyEquivalent: "")
    workspace.addItem(withTitle: "Equilibrar panes", action: #selector(balancePanes), keyEquivalent: "")
    workspace.addItem(withTitle: "Nueva sesión…", action: #selector(newSession), keyEquivalent: "")
    workspace.addItem(withTitle: "Nueva ventana tmux", action: #selector(newWindow), keyEquivalent: "")
    workspace.addItem(withTitle: "Renombrar ventana…", action: #selector(renameWindow), keyEquivalent: "")
    workspace.addItem(withTitle: "Cerrar pane con el menú de tmux", action: #selector(closePane), keyEquivalent: "")
    workspace.addItem(withTitle: "Cerrar ventana con el menú de tmux", action: #selector(closeTmuxWindow), keyEquivalent: "")
    let edit = submenu("Edición")
    edit.addItem(withTitle: "Copiar", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
    edit.addItem(withTitle: "Pegar", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
    edit.addItem(withTitle: "Seleccionar todo", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
    let view = submenu("Vista")
    view.addItem(withTitle: "Mostrar / ocultar sesiones", action: #selector(toggleSidebar), keyEquivalent: "")
    view.addItem(withTitle: "Aumentar fuente", action: #selector(increaseFont), keyEquivalent: "")
    view.addItem(withTitle: "Disminuir fuente", action: #selector(decreaseFont), keyEquivalent: "")
    view.addItem(withTitle: "Volver al terminal", action: #selector(focusTerminal), keyEquivalent: "")
    let help = submenu("Ayuda")
    help.addItem(withTitle: "Guía operativa", action: #selector(showGuide), keyEquivalent: "")
    help.addItem(withTitle: "Diagnóstico", action: #selector(diagnostics), keyEquivalent: "")
    NSApp.mainMenu = main
  }
  func makeWindow() {
    window = NSWindow(contentRect: NSRect(x:0,y:0,width:1320,height:840),
      styleMask:[.titled,.closable,.resizable,.miniaturizable], backing:.buffered, defer:false)
    window.title = environment.demo ? "Terminal Workspace · demo sintética" : "Terminal Workspace"
    window.titlebarAppearsTransparent = true
    window.minSize = NSSize(width:900,height:590)
    window.delegate = self
    window.center()
    if let saved = preferences.windowFrame {
      var frame = NSRectFromString(saved)
      if let screen = NSScreen.main?.visibleFrame {
        frame.size.width = min(screen.width, max(900, frame.width))
        frame.size.height = min(screen.height, max(590, frame.height))
        frame.origin.x = max(screen.minX, min(frame.minX, screen.maxX-frame.width))
        frame.origin.y = max(screen.minY, min(frame.minY, screen.maxY-frame.height))
        window.setFrame(frame, display: false)
      }
    }
    root.frame = NSRect(x:0,y:0,width:1320,height:840)
    root.autoresizingMask = [.width,.height]
    root.wantsLayer = true
    root.layer?.backgroundColor = NSColor(hex:"#101e24").cgColor
    window.contentView = root
    sidebar.wantsLayer = true
    sidebar.layer?.backgroundColor = NSColor(hex:"#14262b").cgColor
    root.addSubview(toolbar); root.addSubview(sidebar); root.addSubview(body); root.addSubview(status)
    workspaceTitle.font = .systemFont(ofSize:16,weight:.semibold)
    workspaceTitle.textColor = NSColor(hex:"#edf1ec")
    toolbar.addSubview(workspaceTitle)
    let actions: [(String, Selector)] = [
      ("Abrir carpeta",#selector(openFolder)), ("Sesión +",#selector(newSession)),
      ("−",#selector(decreaseFont)), ("+",#selector(increaseFont)),
      ("Preview",#selector(toggleWeb)), ("Diff",#selector(showDiff)), ("Guía",#selector(showGuide))]
    for (index, action) in actions.enumerated() {
      let b = button(action.0, action.1); b.tag = 100+index; toolbar.addSubview(b)
    }
    let heading = label(environment.demo ? "SESIONES · DEMO" : "SESIONES",size:10)
    heading.frame = NSRect(x:17,y:12,width:160,height:18); sidebar.addSubview(heading)
    sessionStack.orientation = .vertical; sessionStack.alignment = .leading; sessionStack.spacing = 6
    sessionScroll.drawsBackground = false
    sessionScroll.hasVerticalScroller = true; sessionScroll.autohidesScrollers = true
    sessionScroll.documentView = sessionStack
    sidebar.addSubview(sessionScroll)
    let paneTitle = label("PANES DE ESTA VENTANA",size:10)
    paneTitle.identifier = NSUserInterfaceItemIdentifier("pane-heading")
    sidebar.addSubview(paneTitle)
    paneStack.orientation = .vertical; paneStack.alignment = .leading; paneStack.spacing = 5
    paneScroll.drawsBackground = false
    paneScroll.hasVerticalScroller = true; paneScroll.autohidesScrollers = true
    paneScroll.documentView = paneStack
    sidebar.addSubview(paneScroll)
    let shortcuts = label("Tu teclado sigue en tmux.\nCtrl+h/j/k/l → Neovim\nCmd+Shift+h/j/k/l → tmux",size:11)
    shortcuts.identifier = NSUserInterfaceItemIdentifier("key-note")
    shortcuts.maximumNumberOfLines = 4
    sidebar.addSubview(shortcuts)
    let pending = button("Pendientes",#selector(pendingPopup))
    pending.identifier = NSUserInterfaceItemIdentifier("pending-button")
    sidebar.addSubview(pending)
    let agents = button("Agentes",#selector(agentPopup))
    agents.identifier = NSUserInterfaceItemIdentifier("agents-button")
    sidebar.addSubview(agents)
    tabScroll.drawsBackground = false; tabScroll.hasHorizontalScroller = true
    tabScroll.hasVerticalScroller = false; tabScroll.autohidesScrollers = true
    tabStack.orientation = .horizontal; tabStack.spacing = 7; tabStack.alignment = .centerY
    tabScroll.documentView = tabStack; body.addSubview(tabScroll)
    terminal = LocalProcessTerminalView(frame:NSRect(x:0,y:0,width:1000,height:680),
      font:currentFont, options:TerminalOptions())
    terminal.nativeBackgroundColor = profile.background
    terminal.nativeForegroundColor = profile.foreground
    terminal.caretColor = profile.accent
    terminal.minimumContrastRatio = 1
    terminal.installProfilePalette(profile)
    terminal.processDelegate = self
    terminalPolicy = TerminalPolicy(terminal: terminal, demo: environment.demo)
    terminal.terminalDelegate = terminalPolicy
    body.addSubview(terminal)
    let webConfiguration = WKWebViewConfiguration()
    webConfiguration.websiteDataStore = .nonPersistent()
    web = WKWebView(frame:.zero,configuration:webConfiguration)
    web.navigationDelegate = self
    body.addSubview(web)
    urlField.placeholderString = "https://…"
    urlField.delegate = self
    urlField.target = self; urlField.action = #selector(openURL)
    previewHeader.addSubview(urlField)
    let go = button("Ir",#selector(openURL)); go.tag = 1; previewHeader.addSubview(go)
    let close = button("×",#selector(closePreview)); close.tag = 2; previewHeader.addSubview(close)
    body.addSubview(previewHeader)
    diffView.isEditable = false; diffView.isRichText = false
    diffView.font = .monospacedSystemFont(ofSize:12,weight:.regular)
    diffView.backgroundColor = NSColor(hex:"#12282c"); diffView.textColor = NSColor(hex:"#d8e2dc")
    diffScroll = NSScrollView(); diffScroll.hasVerticalScroller = true; diffScroll.documentView = diffView
    body.addSubview(diffScroll)
    status.font = .systemFont(ofSize:11); status.textColor = NSColor(hex:"#9bb5ac")
    root.resized = { [weak self] in self?.layout() }
    body.resized = { [weak self] in self?.layoutBody() }
    layout()
  }
  func layout() {
    guard window != nil, terminal != nil else { return }
    let width = root.bounds.width, height = root.bounds.height
    let rail: CGFloat = preferences.showSidebar ? 190 : 0
    toolbar.frame = NSRect(x:0,y:0,width:width,height:58)
    workspaceTitle.frame = NSRect(x:18,y:18,width:max(160,width-690),height:23)
    let widths: [CGFloat] = [110,85,31,31,78,58,60]
    var x = width - widths.reduce(0,+) - 18 - CGFloat(widths.count-1)*5
    for (index,w) in widths.enumerated() {
      toolbar.viewWithTag(100+index)?.frame = NSRect(x:x,y:15,width:w,height:28); x += w+5
    }
    sidebar.isHidden = !preferences.showSidebar
    sidebar.frame = NSRect(x:0,y:58,width:rail,height:max(100,height-84))
    let sessionHeight = min(CGFloat(max(1,sessionButtons.count))*34, max(80,sidebar.bounds.height*0.40))
    sessionScroll.frame = NSRect(x:12,y:38,width:166,height:sessionHeight)
    sessionStack.frame = NSRect(x:0,y:0,width:164,height:CGFloat(max(1,sessionButtons.count))*34)
    let paneY = min(sidebar.bounds.height-240,sessionScroll.frame.maxY+28)
    sidebar.subviews.first(where:{$0.identifier?.rawValue=="pane-heading"})?.frame =
      NSRect(x:17,y:max(110,paneY),width:166,height:18)
    let paneTop = max(136,paneY+26)
    paneScroll.frame = NSRect(x:12,y:paneTop,width:166,height:max(45,sidebar.bounds.height-paneTop-170))
    paneStack.frame = NSRect(x:0,y:0,width:164,height:CGFloat(paneButtons.count)*31)
    sidebar.subviews.first(where:{$0.identifier?.rawValue=="pending-button"})?.frame =
      NSRect(x:12,y:sidebar.bounds.height-147,width:166,height:28)
    sidebar.subviews.first(where:{$0.identifier?.rawValue=="agents-button"})?.frame =
      NSRect(x:12,y:sidebar.bounds.height-113,width:166,height:28)
    sidebar.subviews.first(where:{$0.identifier?.rawValue=="key-note"})?.frame =
      NSRect(x:17,y:sidebar.bounds.height-73,width:162,height:62)
    body.frame = NSRect(x:rail+12,y:63,width:max(250,width-rail-24),height:max(250,height-96))
    status.frame = NSRect(x:16,y:height-25,width:width-32,height:18)
    layoutBody()
  }
  func layoutBody() {
    guard terminal != nil, web != nil else { return }
    let width = body.bounds.width, height = body.bounds.height
    tabScroll.frame = NSRect(x:0,y:0,width:width,height:42)
    let tabWidth = tabButtons.reduce(CGFloat(0)) { $0 + max(105,$1.fittingSize.width+12)+7 }
    tabStack.frame = NSRect(x:0,y:0,width:max(width,tabWidth),height:33)
    let previewWidth: CGFloat = previewMode == 0 ? 0 : min(440,max(280,width*0.37))
    let terminalWidth = max(180,width-previewWidth-(previewWidth>0 ? 12:0))
    let frame = NSRect(x:0,y:48,width:terminalWidth,height:height-48)
    if terminal.frame != frame { terminal.frame = frame }
    let px = terminalWidth+12
    previewHeader.isHidden = previewMode == 0
    previewHeader.frame = NSRect(x:px,y:48,width:previewWidth,height:36)
    urlField.frame = NSRect(x:0,y:3,width:max(90,previewWidth-88),height:25)
    previewHeader.viewWithTag(1)?.frame = NSRect(x:previewWidth-84,y:1,width:48,height:28)
    previewHeader.viewWithTag(2)?.frame = NSRect(x:previewWidth-32,y:1,width:30,height:28)
    web.isHidden = previewMode != 1
    diffScroll.isHidden = previewMode != 2
    let previewFrame = NSRect(x:px,y:88,width:previewWidth,height:max(100,height-88))
    web.frame = previewFrame; diffScroll.frame = previewFrame
    diffView.setFrameSize(NSSize(width:previewWidth,height:max(previewFrame.height,diffView.fittingSize.height)))
  }
  func connect(_ session: String) {
    guard !terminal.process.running else { return }
    root.layoutSubtreeIfNeeded()
    terminal.startProcess(executable:environment.tmux,
      args:["-S",environment.socket,"attach-session","-f","no-detach-on-destroy","-t",session],
      environment:environment.processEnvironment.map {"\($0.key)=\($0.value)"},
      currentDirectory:environment.sample)
    timer?.invalidate()
    timer = Timer.scheduledTimer(withTimeInterval:1.25,repeats:true) { [weak self] _ in
      Task { @MainActor in self?.refresh() }
    }
    DispatchQueue.main.asyncAfter(deadline:.now()+0.35) { [weak self] in
      self?.refresh(); self?.focusTerminal()
    }
  }
  func refresh() {
    guard !terminating, !refreshing, terminal.process.shellPid > 0 else { return }
    refreshing = true
    service.refresh(clientPID:terminal.process.shellPid) { [weak self] result in
      guard let self else { return }
      self.refreshing = false
      switch result {
      case .success(let value):
        let changed = self.currentRendering != value.state
        self.live = value
        self.lastError = ""
        if changed { self.render(value); self.currentRendering = value.state }
        let session = value.state.sessions.first(where:{$0.id==value.state.selection?.sessionID})?.name ?? ""
        let selected = value.state.windows.first(where:{$0.id==value.state.selection?.windowID})?.name ?? ""
        self.status.stringValue = (self.environment.demo ? "Demo sintética · " : "Conectado · ")
          + session + " / " + selected + " · tus sesiones siguen vivas al cerrar"
        self.preferences.lastSessionID = value.state.selection?.sessionID
        self.writeHealth(value)
      case .failure(let error):
        self.status.stringValue = error.localizedDescription
        self.lastError = error.localizedDescription
      }
    }
  }
  func removeArranged(_ stack: NSStackView) {
    for view in stack.arrangedSubviews { stack.removeArrangedSubview(view); view.removeFromSuperview() }
  }
  func render(_ live: LiveWorkspace) {
    removeArranged(sessionStack); removeArranged(tabStack); removeArranged(paneStack)
    sessionButtons.removeAll(); tabButtons.removeAll(); paneButtons.removeAll()
    let selected = live.state.selection
    for session in live.state.sessions {
      let display = session.name.hasPrefix("project-") && session.id==selected?.sessionID
        ? session.projectPath.map{URL(fileURLWithPath:$0).lastPathComponent} ?? session.name : session.name
      let b = button(display,#selector(selectSession(_:))); b.identifier = NSUserInterfaceItemIdentifier(session.id)
      b.contentTintColor = session.id==selected?.sessionID ? NSColor(hex:"#d3b6f4") : NSColor(hex:"#acbfb6")
      b.widthAnchor.constraint(equalToConstant:164).isActive = true
      b.toolTip = "Cambiar a la sesión " + display
      sessionStack.addArrangedSubview(b); sessionButtons.append(b)
    }
    if let session = live.state.sessions.first(where:{$0.id==selected?.sessionID}) {
      for link in session.windows.sorted(by:{$0.index<$1.index}) {
        guard let value=live.state.windows.first(where:{$0.id==link.windowID}) else { continue }
        let badge = live.busyWindows.contains(value.id) ? " ◌" : live.pendingWindows.contains(value.id) ? " •" : ""
        let b = button("\(link.index) · \(value.name)\(badge)",#selector(selectWindow(_:)))
        b.identifier = NSUserInterfaceItemIdentifier(value.id)
        b.contentTintColor = value.id==selected?.windowID ? NSColor(hex:"#d3b6f4") : NSColor(hex:"#bbc9c0")
        tabStack.addArrangedSubview(b); tabButtons.append(b)
      }
    }
    let new = button("+",#selector(newWindow)); tabStack.addArrangedSubview(new); tabButtons.append(new)
    if let current=live.state.windows.first(where:{$0.id==selected?.windowID}) {
      for (index,id) in current.paneIDs.enumerated() {
        let b = button("Pane \(index+1)" + (id==selected?.paneID ? "  ●" : ""),#selector(selectPane(_:)))
        b.identifier = NSUserInterfaceItemIdentifier(id)
        b.widthAnchor.constraint(equalToConstant:164).isActive = true
        paneStack.addArrangedSubview(b); paneButtons.append(b)
      }
    }
    workspaceTitle.stringValue = live.project.map{URL(fileURLWithPath:$0).lastPathComponent} ?? "Terminal Workspace"
    layout()
  }
  func perform(_ args:[String]) {
    service.perform(args) { [weak self] result in
      switch result {
      case .success: self?.refresh(); self?.focusTerminal()
      case .failure(let error): self?.status.stringValue=error.localizedDescription
      }
    }
  }
  @objc func selectSession(_ sender:NSButton) {
    guard let id=sender.identifier?.rawValue, WorkspaceService.validID(id,prefix:"$"), let live else{return}
    perform(["switch-client","-c",live.client,"-t",id])
  }
  @objc func selectWindow(_ sender:NSButton) {
    guard let id=sender.identifier?.rawValue, WorkspaceService.validID(id,prefix:"@") else{return}
    guard let session = live?.state.selection?.sessionID else { return }
    perform(["select-window","-t",session+":"+id])
  }
  @objc func selectPane(_ sender:NSButton) {
    guard let id=sender.identifier?.rawValue, WorkspaceService.validID(id,prefix:"%") else{return}
    perform(["select-pane","-t",id])
  }
  @objc func newWindow() {
    guard let selection=live?.state.selection else{return}
    perform(["new-window","-t",selection.sessionID+":"])
  }
  func prompt(_ title:String, value:String="", completion:@escaping(String)->Void) {
    let alert=NSAlert(); alert.messageText=title
    let field=NSTextField(string:value); field.frame=NSRect(x:0,y:0,width:320,height:26)
    alert.accessoryView=field; alert.addButton(withTitle:"Guardar"); alert.addButton(withTitle:"Cancelar")
    alert.beginSheetModal(for:window) { response in
      if response == .alertFirstButtonReturn, !field.stringValue.isEmpty {completion(field.stringValue)}
    }
  }
  @objc func newSession() {
    prompt("Nueva sesión de tmux") { [weak self] name in
      guard let self, let client=self.live?.client else{return}
      self.service.perform(["new-session","-d","-P","-F","#{session_id}","-s",name]) { [weak self] result in
        switch result {
        case .success(let id):
          self?.perform(["switch-client","-c",client,"-t",id.trimmingCharacters(in:.newlines)])
        case .failure(let error): self?.status.stringValue = error.localizedDescription
        }
      }
    }
  }
  @objc func renameWindow() {
    guard let live, let selection=live.state.selection else{return}
    let name=live.state.windows.first(where:{$0.id==selection.windowID})?.name ?? ""
    prompt("Renombrar ventana",value:name) { [weak self] value in
      self?.perform(["rename-window","-t",selection.windowID,value])
    }
  }
  func sendPrefix(_ key:String) {
    guard let client=live?.client else{return}
    service.perform(["show-options","-gv","prefix"]) { [weak self] result in
      if case .success(let prefix)=result {
        self?.perform(["send-keys","-K","-c",client,"--",prefix.trimmingCharacters(in:.newlines),key])
      }
    }
  }
  @objc func closePane(){sendPrefix("x")}
  @objc func closeTmuxWindow(){sendPrefix("&")}
  @objc func pendingPopup(){terminal.send(source:terminal,data:[0x14]);focusTerminal()}
  @objc func agentPopup(){sendPrefix("a")}
  @objc func focusTerminal(){window.makeFirstResponder(terminal)}
  @objc func toggleSidebar(){preferences.showSidebar.toggle();layout();savePreferences()}
  @objc func increaseFont(){changeFont(1)}
  @objc func decreaseFont(){changeFont(-1)}
  func changeFont(_ amount:Double) {
    preferences.fontSize=min(30,max(9,preferences.fontSize+amount))
    terminal.font=currentFont
    savePreferences();focusTerminal()
  }
  func savePreferences() {
    if window != nil { preferences.windowFrame = NSStringFromRect(window.frame) }
    try? preferences.save(to:environment.dataRoot.appendingPathComponent("preferences.json"))
  }
  func rebuildRecents() {
    recentMenu?.removeAllItems()
    for path in preferences.projects.suffix(12).reversed() {
      let item = NSMenuItem(title: URL(fileURLWithPath:path).lastPathComponent,
        action:#selector(openRecent(_:)),keyEquivalent:"")
      item.target = self; item.representedObject = path
      recentMenu?.addItem(item)
    }
  }
  @objc func openRecent(_ sender:NSMenuItem) {
    guard let path=sender.representedObject as? String else{return}
    openProjectPath(path)
  }
  func openProjectPath(_ path:String) {
    guard let client=live?.client else{return}
    service.openProject(path,client:client) { [weak self] result in
      switch result {
      case .success:
        if self?.preferences.projects.contains(path)==false {self?.preferences.projects.append(path)}
        self?.savePreferences();self?.rebuildRecents();self?.refresh();self?.focusTerminal()
      case .failure(let error):self?.status.stringValue=error.localizedDescription
      }
    }
  }
  @objc func renameSession() {
    guard let selected=live?.state.selection else{return}
    prompt("Renombrar sesión") { [weak self] value in
      self?.perform(["rename-session","-t",selected.sessionID,value])
    }
  }
  @objc func splitHorizontal(){sendPrefix("%")}
  @objc func splitVertical(){sendPrefix("\"")}
  @objc func zoomPane(){sendPrefix("z")}
  @objc func balancePanes() {
    guard let selected=live?.state.selection else{return}
    perform(["select-layout","-E","-t",selected.paneID])
  }
  @objc func openFolder() {
    let panel=NSOpenPanel();panel.canChooseDirectories=true;panel.canChooseFiles=false
    panel.allowsMultipleSelection=false;panel.prompt="Abrir workspace"
    panel.beginSheetModal(for:window) { [weak self] result in
      guard result == .OK, let self, let path=panel.url?.path, let client=self.live?.client else{return}
      _ = client
      self.openProjectPath(path)
    }
  }
  @objc func toggleWeb() {
    if previewMode==1 {closePreview();return}
    previewMode=1;layoutBody();window.makeFirstResponder(urlField)
  }
  @objc func closePreview(){previewMode=0;layoutBody();focusTerminal()}
  @objc func openURL() {
    let value=urlField.stringValue
    guard let url=URL(string:value),let scheme=url.scheme,["https","http"].contains(scheme) else {
      urlField.placeholderString="Ingresá una URL http o https";return
    }
    previewMode=1;layoutBody();web.load(URLRequest(url:url))
  }
  @objc func showGuide() {
    previewMode=1;layoutBody()
    let guide=environment.resources.appendingPathComponent("Guides/RUNBOOK.html")
    web.loadFileURL(guide,allowingReadAccessTo:guide.deletingLastPathComponent())
    urlField.stringValue=""
  }
  @objc func showDiff() {
    previewMode=2;layoutBody()
    guard let project=live?.project else {
      diffView.string="Elegí una carpeta con Abrir carpeta.\n\nEl diff se consulta únicamente dentro del workspace elegido."
      return
    }
    diffView.string="Consultando Git…"
    let env=environment.processEnvironment
    service.queue.async { [weak self] in
      var values=env;values["GIT_OPTIONAL_LOCKS"]="0";values["GIT_TERMINAL_PROMPT"]="0"
      let diff=CommandRunner.capture("/usr/bin/git",
        ["-C",project,"--no-pager","diff","--no-ext-diff","--no-textconv","HEAD","--"],
        environment:values,timeout:5)
      DispatchQueue.main.async {
        self?.diffView.string=diff.isEmpty ? "Sin cambios rastreados respecto a HEAD." : diff
      }
    }
  }
  func webView(_ webView:WKWebView,decidePolicyFor navigationAction:WKNavigationAction,
    decisionHandler:@escaping(WKNavigationActionPolicy)->Void) {
    guard let url=navigationAction.request.url else{decisionHandler(.cancel);return}
    if ["http","https","about"].contains(url.scheme ?? ""){decisionHandler(.allow);return}
    let guides=environment.resources.appendingPathComponent("Guides").standardizedFileURL.path+"/"
    decisionHandler(url.isFileURL && url.standardizedFileURL.path.hasPrefix(guides) ? .allow : .cancel)
  }
  @objc func diagnostics() {
    let alert=NSAlert();alert.messageText="Diagnóstico del workspace"
    alert.informativeText="Socket privado: "+environment.socket+"\n\n"
      + (lastError.isEmpty ? "Conectado. No se inspeccionaron configs desde el diagnóstico." : lastError)
      + "\n\nLa app mantiene las sesiones al salir. No guarda transcripciones."
    alert.beginSheetModal(for:window)
  }
  @objc func about() {
    let alert=NSAlert();alert.messageText="Terminal Workspace"
    alert.informativeText="App personal para Neovim y tmux.\n\nSwiftTerm + AppKit. Servidor tmux privado.\n"
      + (environment.demo ? "Perfil sintético de pruebas." : "Tu configuración se carga localmente.")
    alert.beginSheetModal(for:window)
  }
  func writeHealth(_ value:LiveWorkspace) {
    let info:[String:Any]=["clientPID":terminal.process.shellPid,"connected":true,
      "session":value.state.selection?.sessionID ?? "", "window":value.state.selection?.windowID ?? "",
      "pane":value.state.selection?.paneID ?? "", "sessions":value.state.sessions.count,
      "windows":value.state.windows.count,"panes":value.state.panes.count,"demo":environment.demo,
      "terminalColumns":terminal.terminalDimensions.cols,"terminalRows":terminal.terminalDimensions.rows]
    if let data=try? JSONSerialization.data(withJSONObject:info,options:.sortedKeys) {
      try? data.write(to:environment.dataRoot.appendingPathComponent("health.json"),options:.atomic)
    }
  }
  func sizeChanged(source:LocalProcessTerminalView,newCols:Int,newRows:Int) {
    // LocalProcessTerminalView already updates its PTY. tmux handles SIGWINCH.
    // No secondary client, resize-window command or Neovim RPC owns this grid.
  }
  func setTerminalTitle(source:LocalProcessTerminalView,title:String){}
  func hostCurrentDirectoryUpdate(source:TerminalView,directory:String?){}
  func processTerminated(source:TerminalView,exitCode:Int32?) {
    guard !terminating else{return}
    status.stringValue="Cliente desconectado · las sesiones siguen en tmux"
  }
  func processFailedToStart(source:TerminalView,error:LocalProcessError) {
    status.stringValue="No se pudo iniciar el terminal. Abrí Diagnóstico."
  }
  func windowDidResize(_ notification:Notification){root.needsLayout=true;root.layoutSubtreeIfNeeded()}
  func windowWillClose(_ notification:Notification){NSApp.terminate(nil)}
  func applicationWillTerminate(_ notification:Notification) {
    terminating=true;timer?.invalidate()
    if let keyMonitor{NSEvent.removeMonitor(keyMonitor)}
    savePreferences();service.stop()
    // Only this PTY client exits with the GUI; no kill-session or kill-server.
    terminal?.terminate()
  }
}
