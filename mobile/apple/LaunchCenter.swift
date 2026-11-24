import SwiftUI
import UniformTypeIdentifiers

extension Error {
  var goMessage: String {
    switch self as? GoError {
    case .Credentials(let message), .SignInRequired(let message), .Failed(_, let message):
      return message
    case nil:
      return localizedDescription
    }
  }
}

extension UTType {
  /// FileMaker Pro's own identifier for .fmp12 files, which Info.plist
  /// imports.
  static let fileMakerFile = UTType(importedAs: "com.filemaker.doc.fmp12")
}

/// A file the Launch Center lists: one in the app's documents, by name (its
/// sandbox path can change across updates), or one a host publishes.
struct FileEntry: Codable, Hashable, Identifiable {
  var name: String
  var address = ""
  var port: UInt16 = 5003
  var id: Self { self }
  var local: Bool { address.isEmpty }
  var subtitle: String { local ? "On this device" : "\(address):\(port)" }

  static var directory: URL {
    FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
  }
  var url: URL { Self.directory.appendingPathComponent(name) }
}

@main
struct GoApp: App {
  var body: some Scene { WindowGroup { LaunchCenter() } }
}

private enum SectionTab: String, CaseIterable {
  case local = "My Apps"
  case favorites = "Favorites"
  case recent = "Recent"
  case hosts = "Hosts"

  var icon: String {
    switch self {
    case .local: return "square.grid.2x2"
    case .favorites: return "star"
    case .recent: return "clock"
    case .hosts: return "server.rack"
    }
  }

  var empty: String {
    switch self {
    case .local: return "Import a FileMaker app to open it here."
    case .favorites: return "Star a file to add it here."
    case .recent: return "Opened files appear here."
    case .hosts: return "Connect to a host to see the files it shares."
    }
  }
}

struct LaunchCenter: View {
  @State private var tab = SectionTab.local
  @Environment(\.scenePhase) private var phase
  @Environment(\.displayScale) private var displayScale
  @State private var local: [FileEntry] = []
  @AppStorage("favorites") private var favoritesData = Data()
  @AppStorage("recent") private var recentData = Data()
  @AppStorage("lastHost") private var address = ""
  @AppStorage("lastPort") private var port = "5003"
  @AppStorage("trustedHostCertificates") private var certificates = Data()
  @State private var certificate = ""
  @State private var namedFile = ""
  @State private var hosted: [FileEntry] = []
  @State private var signingIn: FileEntry?
  @State private var opened: DocumentModel?
  /// Why the last document stopped, shown once its screen has gone.
  @State private var stopped: String?
  @State private var importing = false
  @State private var deleting: FileEntry?
  @State private var busy = false
  @State private var error: String?
  @State private var showingSettings = false
  @State private var licenseSaved = SavedLicense.isSaved

  private var favorites: [FileEntry] { decode(favoritesData) }
  private var recent: [FileEntry] { decode(recentData) }
  /// Private certificate authorities by host and port.
  private var authorities: [String: String] {
    (try? JSONDecoder().decode([String: String].self, from: certificates)) ?? [:]
  }

  var body: some View {
    TabView(selection: $tab) {
      ForEach(SectionTab.allCases, id: \.self) { section in
        NavigationStack {
          List {
            if section == .hosts { hostForm }
            let entries = files(in: section)
            ForEach(entries) { file in row(file, in: section) }
            if entries.isEmpty {
              Label(section.empty, systemImage: section.icon).foregroundStyle(.secondary)
            }
            if section == .local { Button("Import a File…") { importing = true } }
          }
          .navigationTitle(section.rawValue)
          .toolbar {
            ToolbarItem(placement: .principal) { Text("OpenMaker Go").font(.headline) }
            ToolbarItem(placement: .primaryAction) {
              Button {
                showingSettings = true
              } label: {
                Label("Settings", systemImage: "gear")
              }
            }
          }
        }
        .tabItem { Label(section.rawValue, systemImage: section.icon) }
        .tag(section)
      }
    }
    .overlay { if busy { ProgressView().padding().background(.regularMaterial, in: Capsule()) } }
    .disabled(busy)
    .onAppear {
      reload()
      restoreCertificate()
    }
    .onChange(of: phase) { if $0 == .active { reload() } }
    .onChange(of: address) { _ in hostChanged() }
    .onChange(of: port) { _ in hostChanged() }
    .onOpenURL { url in importFile(url) }
    .fileImporter(isPresented: $importing, allowedContentTypes: [.fileMakerFile]) { result in
      do { importFile(try result.get()) } catch { self.error = error.goMessage }
    }
    .sheet(item: $signingIn) { file in
      SignInView(file: file, licenseSaved: licenseSaved) { account, password, license in
        try await open(file, account: account, password: password, license: license)
      }
    }
    .sheet(isPresented: $showingSettings) { SettingsView(licenseSaved: $licenseSaved) }
    .fullScreenCover(item: $opened, onDismiss: { error = stopped; stopped = nil }) { model in
      DocumentView(model: model) { failure in
        stopped = failure
        opened = nil
      }
      .interactiveDismissDisabled()
    }
    .confirmationDialog(
      "Delete \(deleting?.name ?? "")?", isPresented: present($deleting), titleVisibility: .visible
    ) {
      Button("Delete", role: .destructive) { if let file = deleting { delete(file) } }
    } message: {
      Text("The file is removed from this device.")
    }
    .alert("OpenMaker Go", isPresented: present($error)) {
      Button("OK") { error = nil }
    } message: {
      Text(error ?? "")
    }
  }

  private var hostForm: some View {
    Section("Connect to a host") {
      TextField("Host name or IP address", text: $address)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .keyboardType(.URL)
      TextField("Port", text: $port).keyboardType(.numberPad)
      TextField("File name (optional)", text: $namedFile)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
      DisclosureGroup("Private certificate authority") {
        TextField("CA certificate (PEM)", text: $certificate, axis: .vertical)
          .textInputAutocapitalization(.never)
          .autocorrectionDisabled()
          .font(.caption.monospaced())
      }
      Button(namedFile.isEmpty ? "Browse Host" : "Open File", action: browseHost)
        .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
    }
  }

  private func row(_ file: FileEntry, in section: SectionTab) -> some View {
    HStack {
      Button {
        choose(file)
      } label: {
        Label {
          VStack(alignment: .leading) {
            Text(file.name).foregroundStyle(.primary)
            Text(file.subtitle).font(.caption).foregroundStyle(.secondary)
          }
        } icon: {
          Image(systemName: file.local ? "doc" : "network")
        }
      }
      Spacer()
      Button {
        toggleFavorite(file)
      } label: {
        Image(systemName: favorites.contains(file) ? "star.fill" : "star")
      }
      .accessibilityLabel(favorites.contains(file) ? "Remove Favorite" : "Add Favorite")
    }
    .buttonStyle(.borderless)
    .swipeActions {
      switch section {
      case .local:
        Button("Delete", role: .destructive) { deleting = file }
      case .recent:
        Button("Remove") { recentData = encode(recent.filter { $0 != file }) }
      case .favorites:
        Button("Remove") { toggleFavorite(file) }
      case .hosts:
        EmptyView()
      }
    }
  }

  private func files(in section: SectionTab) -> [FileEntry] {
    switch section {
    case .local: return local
    case .favorites: return favorites
    case .recent: return recent
    case .hosts: return hosted
    }
  }

  private func decode(_ data: Data) -> [FileEntry] {
    (try? JSONDecoder().decode([FileEntry].self, from: data)) ?? []
  }

  private func encode(_ entries: [FileEntry]) -> Data {
    (try? JSONEncoder().encode(entries)) ?? Data()
  }

  private func toggleFavorite(_ file: FileEntry) {
    favoritesData = encode(
      favorites.contains(file) ? favorites.filter { $0 != file } : favorites + [file])
  }

  private func reload() {
    do {
      local = try FileManager.default.contentsOfDirectory(
        at: FileEntry.directory, includingPropertiesForKeys: [.isRegularFileKey]
      )
      .filter {
        $0.pathExtension.lowercased() == "fmp12"
          && (try? $0.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true
      }
      .map { FileEntry(name: $0.lastPathComponent) }
      .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    } catch {
      self.error = error.goMessage
    }
  }

  /// Copy a file into the app's documents. The copy is made under a
  /// temporary name and then linked in under its own, so the list never
  /// shows half a file, and a file already there of that name is kept.
  private func importFile(_ url: URL) {
    // A file of My Apps itself (Files shows the app's documents) opens as a
    // tap on it does: it is not imported over itself.
    let folder = FileEntry.directory.standardizedFileURL.resolvingSymlinksInPath().path
    if url.isFileURL,
      url.standardizedFileURL.resolvingSymlinksInPath().deletingLastPathComponent().path == folder
    {
      reload()
      tab = .local
      // One file is open at a time: the open one keeps its window (and a
      // save it may be waiting on) until it is closed.
      if let open = opened {
        error = "Close \(open.entry.name) to open \(url.lastPathComponent)."
      } else {
        choose(FileEntry(name: url.lastPathComponent))
      }
      return
    }
    busy = true
    Task {
      defer { busy = false }
      do {
        try await Task.detached {
          let scoped = url.startAccessingSecurityScopedResource()
          defer { if scoped { url.stopAccessingSecurityScopedResource() } }
          let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
          guard url.pathExtension.lowercased() == "fmp12", values.isRegularFile == true,
            values.isSymbolicLink != true
          else {
            throw CocoaError(.fileReadUnsupportedScheme, userInfo: [
              NSLocalizedDescriptionKey: "Choose a FileMaker file (.fmp12).",
            ])
          }
          let manager = FileManager.default
          let temporary = FileEntry.directory.appendingPathComponent(".import-" + UUID().uuidString)
          defer { try? manager.removeItem(at: temporary) }
          try manager.copyItem(at: url, to: temporary)
          do {
            try manager.linkItem(at: temporary, to: FileEntry(name: url.lastPathComponent).url)
          } catch CocoaError.fileWriteFileExists {
            throw CocoaError(.fileWriteFileExists, userInfo: [
              NSLocalizedDescriptionKey: "My Apps already has a file named \(url.lastPathComponent).",
            ])
          }
        }.value
        reload()
        tab = .local
      } catch {
        self.error = error.goMessage
      }
    }
  }

  private func delete(_ file: FileEntry) {
    do {
      try FileManager.default.removeItem(at: file.url)
      recentData = encode(recent.filter { $0 != file })
      favoritesData = encode(favorites.filter { $0 != file })
      reload()
    } catch {
      self.error = error.goMessage
    }
  }

  private func authority(_ address: String, _ port: UInt16) -> String { "\(address):\(port)" }

  private func hostChanged() {
    hosted = []
    restoreCertificate()
  }

  /// The certificate authority saved for the host and port typed, never
  /// another host's.
  private func restoreCertificate() {
    // Saved under the trimmed address, as browseHost saves it.
    let name = address.trimmingCharacters(in: .whitespacesAndNewlines)
    certificate = UInt16(port).flatMap { authorities[authority(name, $0)] } ?? ""
  }

  private func saveCertificate(_ name: String, _ number: UInt16) {
    var saved = authorities
    saved[authority(name, number)] = certificate.isEmpty ? nil : certificate
    certificates = encode(saved)
  }

  private func encode(_ authorities: [String: String]) -> Data {
    (try? JSONEncoder().encode(authorities)) ?? Data()
  }

  private func browseHost() {
    let name = address.trimmingCharacters(in: .whitespacesAndNewlines)
    guard let number = UInt16(port), number > 0 else {
      error = "Enter a port from 1 to 65535."
      return
    }
    // A cleared certificate authority is removed now: a browse that fails
    // without it must not leave the host trusted through it.
    if certificate.isEmpty { saveCertificate(name, number) }
    let fileName = namedFile.trimmingCharacters(in: .whitespacesAndNewlines)
    if !fileName.isEmpty {
      saveCertificate(name, number)
      signingIn = FileEntry(name: fileName, address: name, port: number)
      return
    }
    let host = Host(address: name, port: number, certificate: certificate)
    busy = true
    Task {
      defer { busy = false }
      do {
        let names = try await Task.detached { try hostedFiles(host: host) }.value
        saveCertificate(name, number)
        hosted = names.map { FileEntry(name: $0, address: name, port: number) }
        if hosted.isEmpty { error = "This host shares no files." }
      } catch {
        self.error = error.goMessage
      }
    }
  }

  /// A local file opens as FileMaker Go opens one, signed in by the file's
  /// own settings, and asks for an account only when the file needs one; a
  /// hosted file asks first.
  private func choose(_ file: FileEntry) {
    guard file.local else {
      signingIn = file
      return
    }
    busy = true
    Task {
      defer { busy = false }
      do {
        try await open(file, account: "", password: "", license: "")
      } catch GoError.SignInRequired(_), GoError.Credentials(_) {
        signingIn = file
      } catch {
        self.error = error.goMessage
      }
    }
  }

  private func open(_ file: FileEntry, account: String, password: String, license: String)
    async throws
  {
    let host = Host(
      address: file.address, port: file.port,
      certificate: authorities[authority(file.address, file.port)] ?? "")
    let source: Source =
      file.local ? .local(path: file.url.path) : .hosted(host: host, file: file.name)
    let typed = license.trimmingCharacters(in: .whitespacesAndNewlines)
    let local = file.local
    // The canvas sends its own size once it is shown.
    let viewport = Viewport(
      width: 768, height: 1024, scale: Float(displayScale), maxTextureSide: 8192)
    let signal = FrameSignal()
    let document = try await Task.detached {
      // A hosted sign-in with no license typed sends the one saved in
      // Settings.
      let key = typed.isEmpty && !local ? SavedLicense.load() : typed
      let signIn = SignIn(account: account, password: password, license: key)
      return try Document.open(source: source, signIn: signIn, viewport: viewport, observer: signal)
    }.value
    signingIn = nil
    recentData = encode(Array(([file] + recent.filter { $0 != file }).prefix(30)))
    // A file opened meanwhile keeps its window: this one is let go of,
    // which closes it.
    guard opened == nil else { return }
    opened = DocumentModel(entry: file, document: document, signal: signal)
  }

  private func present<T>(_ value: Binding<T?>) -> Binding<Bool> {
    Binding(get: { value.wrappedValue != nil }, set: { if !$0 { value.wrappedValue = nil } })
  }
}

private struct SignInView: View {
  let file: FileEntry
  let licenseSaved: Bool
  let open: (String, String, String) async throws -> Void
  @Environment(\.dismiss) private var dismiss
  @State private var account = ""
  @State private var password = ""
  @State private var license = ""
  @State private var busy = false
  @State private var error = ""
  @FocusState private var passwordFocused: Bool

  var body: some View {
    NavigationStack {
      Form {
        Section(file.subtitle) {
          TextField("Account Name", text: $account)
            .textContentType(.username)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
          SecureField("Password", text: $password)
            .textContentType(.password)
            .focused($passwordFocused)
        }
        if !file.local {
          Section {
            // A license key, not a password: AutoFill leaves it alone.
            TextField(
              licenseSaved
                ? "Client license (leave empty for the saved one)"
                : "Client license (if the host needs one)", text: $license)
              .textContentType(nil)
              .textInputAutocapitalization(.characters)
              .autocorrectionDisabled()
              .privacySensitive()
          }
        }
        if !error.isEmpty {
          Text(error).foregroundStyle(.red)
        }
        Button("Sign In") { signIn() }
          .disabled(busy)
        if busy { ProgressView() }
      }
      .disabled(busy)
      .navigationTitle(file.name)
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { Button("Cancel") { dismiss() }.disabled(busy) }
    }
    .interactiveDismissDisabled(busy)
  }

  private func signIn() {
    busy = true
    error = ""
    Task {
      defer { busy = false }
      do {
        try await open(account, password, license)
      } catch GoError.Credentials(let message) {
        // The account was refused: try the password again.
        error = message
        password = ""
        passwordFocused = true
      } catch {
        self.error = error.goMessage
      }
    }
  }
}
