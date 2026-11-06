import AppKit
import SwiftTerm

// Keep LocalProcessTerminalView's PTY callbacks, while making protocol reads
// of the system clipboard an explicit decision. A keyboard paste remains a
// normal user action handled by TerminalView itself.
@MainActor
final class TerminalPolicy: TerminalViewDelegate {
  weak var terminal: LocalProcessTerminalView?
  let demo: Bool
  init(terminal: LocalProcessTerminalView, demo: Bool) {
    self.terminal = terminal
    self.demo = demo
  }
  func send(source: TerminalView, data: ArraySlice<UInt8>) {
    terminal?.send(source: source, data: data)
  }
  func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {
    terminal?.sizeChanged(source: source, newCols: newCols, newRows: newRows)
  }
  func setTerminalTitle(source: TerminalView, title: String) {
    terminal?.setTerminalTitle(source: source, title: title)
  }
  func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {
    terminal?.hostCurrentDirectoryUpdate(source: source, directory: directory)
  }
  func scrolled(source: TerminalView, position: Double) {
    terminal?.scrolled(source: source, position: position)
  }
  func rangeChanged(source: TerminalView, startY: Int, endY: Int) {
    terminal?.rangeChanged(source: source, startY: startY, endY: endY)
  }
  func clipboardCopy(source: TerminalView, content: Data) {
    terminal?.clipboardCopy(source: source, content: content)
  }
  func clipboardRead(source: TerminalView) -> Data? {
    guard !demo else { return nil }
    let alert = NSAlert()
    alert.messageText = "¿Permitir que este terminal lea el portapapeles?"
    alert.informativeText = "Un programa dentro de tmux solicitó el contenido. Permitirlo una vez lo entrega a ese programa."
    alert.addButton(withTitle: "No permitir")
    alert.addButton(withTitle: "Permitir una vez")
    guard alert.runModal() == .alertSecondButtonReturn else { return nil }
    return terminal?.clipboardRead(source: source)
  }
  func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {
    guard let url = URL(string: link), ["https", "http"].contains(url.scheme ?? "") else { return }
    NSWorkspace.shared.open(url)
  }
  func programStatusChanged(source: TerminalView, records: [TerminalProgramStatus]) {
    terminal?.programStatusChanged(source: source, records: records)
  }
  func kittyClipboardCapabilities(source: TerminalView) -> KittyClipboardCapabilities { [] }
  func kittyClipboardAvailableMimeTypes(source: TerminalView, location: KittyClipboardLocation) -> [String]? { nil }
  func kittyClipboardRead(source: TerminalView, location: KittyClipboardLocation, mimeType: String) -> KittyClipboardReadResult? { nil }
  func kittyClipboardWrite(source: TerminalView, location: KittyClipboardLocation, content: KittyClipboardWriteContent) -> KittyClipboardWriteResult { .unsupported }
  func kittyClipboardRequestPermission(source: TerminalView, request: KittyClipboardPermissionRequest) -> KittyClipboardPermissionResult { .deny }
}
