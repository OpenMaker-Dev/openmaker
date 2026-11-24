import SwiftUI
import UIKit

/// An open file and what its chrome shows.
@MainActor
final class DocumentModel: ObservableObject, Identifiable {
  let id = UUID()
  let entry: FileEntry
  let document: Document
  let signal: FrameSignal
  /// The engine sends a texture once and then only its changes: the
  /// painter that holds them lasts as long as the document, so a canvas
  /// SwiftUI makes again draws with them.
  let painter: MetalPainter
  @Published private(set) var state: DocumentState?
  /// FileMaker's menus by title: Layouts, Records or Requests, View, Scripts.
  @Published private(set) var menus: [String: [MenuNode]] = [:]
  /// The screen reader's elements as last sent: the engine sends them only
  /// when they change, so a canvas SwiftUI makes again starts with these.
  private(set) var elements: [Element] = []

  init(entry: FileEntry, document: Document, signal: FrameSignal) {
    guard let painter = MetalPainter() else {
      fatalError("OpenMaker Go needs Metal")
    }
    self.entry = entry
    self.document = document
    self.signal = signal
    self.painter = painter
  }

  /// The canvas that shows the document, while SwiftUI shows one.
  weak var canvas: DocumentCanvas?

  func send(_ input: Input) { document.send(input: input) }
  /// A command of the chrome or a menu choice takes what is typed as
  /// committed, never marked text the input method still composes.
  func run(_ command: Command) { settle(.run(command: command)) }
  func choose(_ row: String) { settle(.menu(id: row)) }

  private func settle(_ input: Input) {
    if let canvas { canvas.settle(then: input) } else { send(input) }
  }

  /// What a frame tells the chrome.
  func take(_ frame: Frame) {
    if frame.state != state { state = frame.state }
    if let elements = frame.elements { self.elements = elements }
    if let menus = frame.menus {
      self.menus = Dictionary(
        menus.map { ($0.title, MenuNode.tree($0.rows)) }, uniquingKeysWith: { first, _ in first })
    }
    if let copied = frame.copied { UIPasteboard.general.string = copied }
    for url in frame.openUrls.compactMap(URL.init(string:)) { UIApplication.shared.open(url) }
  }
}

/// A row of a FileMaker menu with the rows of its submenu.
struct MenuNode: Identifiable {
  let id: Int
  let row: MenuRow
  let children: [MenuNode]

  /// The rows as a tree: those after a submenu at a greater depth are its.
  static func tree(_ rows: [MenuRow]) -> [MenuNode] {
    var at = 0
    return level(rows, depth: 0, at: &at)
  }

  private static func level(_ rows: [MenuRow], depth: UInt32, at: inout Int) -> [MenuNode] {
    var nodes = [MenuNode]()
    while at < rows.count, rows[at].depth >= depth {
      let row = rows[at]
      let index = at
      at += 1
      let children = row.kind == .submenu ? level(rows, depth: depth + 1, at: &at) : []
      nodes.append(MenuNode(id: index, row: row, children: children))
    }
    return nodes
  }
}

/// FileMaker's own menu rows as a native menu's.
struct MenuRows: View {
  let nodes: [MenuNode]
  let choose: (String) -> Void

  var body: some View {
    ForEach(nodes) { node in
      switch node.row.kind {
      case .separator:
        Divider()
      case .submenu:
        SwiftUI.Menu(node.row.label) { MenuRows(nodes: node.children, choose: choose) }
          .disabled(!node.row.enabled)
      case .command:
        Button {
          choose(node.row.id)
        } label: {
          if node.row.checked {
            Label(node.row.label, systemImage: "checkmark")
          } else {
            Text(node.row.label)
          }
        }
        .disabled(!node.row.enabled)
      }
    }
  }
}

struct DocumentView: View {
  @ObservedObject var model: DocumentModel
  /// The file closed, or the document stopped on an error (its message).
  let close: (String?) -> Void
  @Environment(\.scenePhase) private var phase
  @Environment(\.horizontalSizeClass) private var sizeClass
  @State private var quickFind: String?
  @State private var goTo: String?

  private var state: DocumentState? { model.state }
  private var finding: Bool { state?.mode == .find }

  var body: some View {
    NavigationStack {
      CanvasView(model: model)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
    }
    .onChange(of: phase) { phase in
      // Leaving the foreground saves the record's changes (Input.active).
      if phase == .background { saveInBackground() }
      if phase == .active { model.send(.active(active: true)) }
    }
    .onChange(of: state?.closed == true) { closed in if closed { close(state?.failure) } }
    .alert(
      "Quick Find", isPresented: present($quickFind),
      actions: {
        TextField("Search", text: text($quickFind))
        Button("Search") { model.run(.quickFind(words: quickFind ?? "")) }
        Button("Cancel", role: .cancel) {}
      })
    .alert(
      "Go to Record", isPresented: present($goTo),
      actions: {
        TextField("Record number", text: text($goTo)).keyboardType(.numberPad)
        Button("Go") {
          if let number = UInt64(goTo ?? "") { model.run(.goToRecord(number: number)) }
        }
        Button("Cancel", role: .cancel) {}
      })
  }

  /// The system's toolbars place the controls for an iPhone or an iPad;
  /// View and Scripts go into its overflow menu where space is short.
  @ToolbarContentBuilder
  private var toolbar: some ToolbarContent {
    ToolbarItemGroup(placement: .topBarLeading) {
      Button {
        // Close names the file shown: one a script brought forward since
        // is not closed.
        if let file = state?.file { model.run(.close(file: file)) }
      } label: {
        Label("Files", systemImage: "chevron.left").labelStyle(.titleAndIcon)
      }
      .disabled(state?.canClose != true)
      if finding {
        Button("Cancel") { model.run(.cancelFind) }.disabled(state?.canCancelFind != true)
      }
    }
    ToolbarItem(placement: .principal) {
      HStack(spacing: 6) {
        if state?.busy == true { ProgressView().controlSize(.small) }
        Text(state?.title ?? model.entry.name).font(.headline).lineLimit(1)
      }
    }
    ToolbarItemGroup(placement: .topBarTrailing) {
      if finding {
        menu("Requests", systemImage: "list.bullet.rectangle")
        Button("Search") { model.run(.performFind) }
          .bold()
          .disabled(state?.canPerformFind != true)
      } else {
        SwiftUI.Menu {
          Button("Quick Find…") { quickFind = "" }.disabled(state?.canQuickFind != true)
          Button("New Find") { model.run(.find) }.disabled(state?.canFind != true)
        } label: {
          Label("Find", systemImage: "magnifyingglass")
        }
        menu("Records", systemImage: "ellipsis.circle")
      }
    }
    ToolbarItemGroup(placement: .secondaryAction) {
      menu("View", systemImage: "rectangle.split.3x1")
      if model.menus["Scripts"]?.contains(where: { $0.row.kind == .command }) == true {
        menu("Scripts", systemImage: "scroll")
      }
    }
    ToolbarItem(placement: .bottomBar) {
      SwiftUI.Menu {
        MenuRows(nodes: model.menus["Layouts"] ?? [], choose: model.choose)
      } label: {
        // The layout's name shows, as in FileMaker Go: a toolbar draws a
        // Label as its icon alone.
        HStack(spacing: 6) {
          Image(systemName: "rectangle.grid.1x2")
          Text(layoutTitle).lineLimit(1).frame(maxWidth: sizeClass == .compact ? 120 : 280)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Layout \(layoutTitle)")
      }
    }
    ToolbarItem(placement: .bottomBar) { Spacer() }
    // One item, so that the bar keeps the arrows and the count together.
    ToolbarItem(placement: .bottomBar) { navigation }
  }

  @ViewBuilder
  private var navigation: some View {
    if let state, state.mode != .other {
      HStack(spacing: 14) {
        if finding {
          Toggle(isOn: Binding(get: { state.omitting }, set: { _ in model.run(.toggleOmit) })) {
            Text("Omit")
          }
          .toggleStyle(.button)
          .disabled(!state.canToggleOmit)
        }
        Button {
          model.run(.previous)
        } label: {
          Image(systemName: "chevron.left")
        }
        .accessibilityLabel(finding ? "Previous Request" : "Previous Record")
        .disabled(!state.canPrevious)
        Button {
          goTo = ""
        } label: {
          Text(position(state)).monospacedDigit().fixedSize()
        }
        .disabled(!state.canGoToRecord)
        .accessibilityLabel(finding ? "Find request" : "Go to record")
        Button {
          model.run(.next)
        } label: {
          Image(systemName: "chevron.right")
        }
        .accessibilityLabel(finding ? "Next Request" : "Next Record")
        .disabled(!state.canNext)
      }
      .padding(.horizontal, 4)
    }
  }

  private var layoutTitle: String {
    guard let layout = state?.layout, !layout.isEmpty else { return "Layout" }
    return layout
  }

  private func menu(_ title: String, systemImage: String) -> some View {
    SwiftUI.Menu {
      MenuRows(nodes: model.menus[title] ?? [], choose: model.choose)
    } label: {
      Label(title, systemImage: systemImage)
    }
    .disabled(model.menus[title]?.isEmpty != false)
  }

  private func position(_ state: DocumentState) -> String {
    if finding {
      return "\(state.omitting ? "Omit" : "Find") \(state.position + 1)/\(state.count)"
    }
    if state.count == 0 { return "No records" }
    // An iPhone's bar has room for the short form only.
    return sizeClass == .compact
      ? "\(state.position + 1)/\(state.count)" : "\(state.position + 1) of \(state.count)"
  }

  /// Keep the app running until the record's changes are saved, or iOS's
  /// background time is nearly up.
  private func saveInBackground() {
    let task = BackgroundTask(name: "Save the record")
    let document = model.document
    model.send(.active(active: false))
    Task.detached {
      let deadline = Date().addingTimeInterval(25)
      // From the moment it is sent until the save is over, saved or kept by
      // the window (a trigger refused it, a dialog asks, or the connection
      // is lost).
      while document.isSaving(), Date() < deadline {
        try? await Task.sleep(nanoseconds: 100_000_000)
      }
      await task.end()
    }
  }

  private func present(_ value: Binding<String?>) -> Binding<Bool> {
    Binding(get: { value.wrappedValue != nil }, set: { if !$0 { value.wrappedValue = nil } })
  }

  private func text(_ value: Binding<String?>) -> Binding<String> {
    Binding(get: { value.wrappedValue ?? "" }, set: { value.wrappedValue = $0 })
  }
}

/// Time iOS gives the app after it leaves the foreground, ended once.
@MainActor
private final class BackgroundTask {
  private var id = UIBackgroundTaskIdentifier.invalid

  init(name: String) {
    id = UIApplication.shared.beginBackgroundTask(withName: name) { [weak self] in self?.end() }
  }

  func end() {
    guard id != .invalid else { return }
    UIApplication.shared.endBackgroundTask(id)
    id = .invalid
  }
}

/// The canvas inside SwiftUI: it draws, and its frames reach the model.
private struct CanvasView: UIViewRepresentable {
  @ObservedObject var model: DocumentModel

  func makeUIView(context: Context) -> DocumentCanvas {
    let canvas = DocumentCanvas(document: model.document, painter: model.painter)
    canvas.present(model.elements)
    canvas.onFrame = { [weak model] frame in model?.take(frame) }
    model.canvas = canvas
    model.signal.canvas = canvas
    let voiceOver = { [weak model] in
      model?.send(
        .accessibility(
          enabled: UIAccessibility.isVoiceOverRunning || UIAccessibility.isSwitchControlRunning))
    }
    voiceOver()
    context.coordinator.observers = [
      UIAccessibility.voiceOverStatusDidChangeNotification,
      UIAccessibility.switchControlStatusDidChangeNotification,
    ].map { NotificationCenter.default.addObserver(forName: $0, object: nil, queue: .main) { _ in voiceOver() } }
    return canvas
  }

  func updateUIView(_ canvas: DocumentCanvas, context: Context) {
    guard let state = model.state else { return }
    canvas.keyboard(shown: state.keyboard, caret: state.caret, finding: state.mode == .find)
  }

  func makeCoordinator() -> Coordinator { Coordinator() }

  final class Coordinator {
    var observers: [NSObjectProtocol] = []
    deinit { observers.forEach(NotificationCenter.default.removeObserver) }
  }
}
