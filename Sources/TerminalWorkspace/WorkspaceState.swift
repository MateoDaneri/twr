import Foundation

// Window identities belong to the server. A window can be linked into more
// than one session, with a different index in each session.
struct WindowLink: Codable, Equatable {
  var windowID: String
  var index: Int
}

struct SessionState: Codable, Equatable {
  var id: String
  var name: String
  var windows: [WindowLink]
  var projectPath: String?
}

struct WindowState: Codable, Equatable {
  var id: String
  var name: String
  var paneIDs: [String]
}

struct PaneState: Codable, Equatable {
  var id: String
  var columns: Int
  var rows: Int
}

struct ClientSelection: Codable, Equatable {
  var sessionID: String
  var windowID: String
  var paneID: String
}

enum WorkspaceStateError: Error, Equatable {
  case invalidID(String)
  case duplicateID(String)
  case duplicateWindowIndex(String)
  case missingReference(String)
  case invalidGeometry(String)
  case invalidPaneOwnership(String)
  case invalidSelection
}

struct WorkspaceState: Codable, Equatable {
  var serverID: String
  var sessions: [SessionState]
  var windows: [WindowState]
  var panes: [PaneState]
  var selection: ClientSelection?

  func validate() throws {
    func identities(_ values: [String], prefix: Character) throws -> Set<String> {
      var seen = Set<String>()
      for value in values {
        guard value.first == prefix, value.count > 1,
          value.dropFirst().allSatisfy({ $0.isASCII && $0.isNumber })
        else { throw WorkspaceStateError.invalidID(value) }
        guard seen.insert(value).inserted else { throw WorkspaceStateError.duplicateID(value) }
      }
      return seen
    }
    let sessionIDs = try identities(sessions.map(\.id), prefix: "$")
    let windowIDs = try identities(windows.map(\.id), prefix: "@")
    let paneIDs = try identities(panes.map(\.id), prefix: "%")
    for session in sessions {
      var indices = Set<Int>()
      var links = Set<String>()
      for link in session.windows {
        guard indices.insert(link.index).inserted, links.insert(link.windowID).inserted else {
          throw WorkspaceStateError.duplicateWindowIndex(session.id)
        }
        guard windowIDs.contains(link.windowID) else {
          throw WorkspaceStateError.missingReference(link.windowID)
        }
      }
    }
    var owners = Set<String>()
    for window in windows {
      for pane in window.paneIDs {
        guard paneIDs.contains(pane) else { throw WorkspaceStateError.missingReference(pane) }
        guard owners.insert(pane).inserted else {
          throw WorkspaceStateError.invalidPaneOwnership(pane)
        }
      }
    }
    for pane in panes {
      guard pane.columns > 0, pane.rows > 0 else {
        throw WorkspaceStateError.invalidGeometry(pane.id)
      }
      guard owners.contains(pane.id) else {
        throw WorkspaceStateError.invalidPaneOwnership(pane.id)
      }
    }
    if let selection {
      guard sessionIDs.contains(selection.sessionID),
        let session = sessions.first(where: { $0.id == selection.sessionID }),
        session.windows.contains(where: { $0.windowID == selection.windowID }),
        let window = windows.first(where: { $0.id == selection.windowID }),
        window.paneIDs.contains(selection.paneID)
      else { throw WorkspaceStateError.invalidSelection }
    }
  }
}
