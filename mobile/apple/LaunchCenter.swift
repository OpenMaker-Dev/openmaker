import SwiftUI
import UniformTypeIdentifiers

extension Error {
  var goMessage: String {
    switch self as? GoError {
    case .Credentials(let message), .SignInRequired(let message), .Failed(_, let message):
      return message
    case .Untrusted(_, _, let message, _, _, _, _, _, _):
      return message
    case nil:
      return localizedDescription
    }
  }

  /// A host whose certificate could not be verified, if that is the error.
  var untrustedHost: UntrustedHost? {
    guard
      case .Untrusted(
        let host, let port, let reason, let subject, let issuer, let validFrom, let validUntil,
        let fingerprint, _) = self as? GoError
    else { return nil }
    return UntrustedHost(
      host: host, port: port, reason: reason, subject: subject, issuer: issuer,
      valid: validFrom.isEmpty && validUntil.isEmpty ? "" : "\(validFrom) to \(validUntil)",
      fingerprint: fingerprint)
  }
}

/// A host whose certificate could not be verified, as FileMaker Go asks
/// about one ([`GoError.Untrusted`]).
struct UntrustedHost: Identifiable {
  let id = UUID()
  var host: String
  var port: UInt16
  var reason: String
  var subject: String
  var issuer: String
  var valid: String
  var fingerprint: String
  /// What met it, done again once its certificate is permitted.
  var retry: (() -> Void)?
}

/// The question about a host whose certificate could not be verified: why,
/// what the certificate says, and Always Permit, Connect (this time) or
/// Don't Connect, as FileMaker Go asks.
private struct TrustAlert: ViewModifier {
  @Binding var asking: UntrustedHost?
  let permit: (UntrustedHost, Bool) -> Void

  func body(content: Content) -> some View {
    content.alert(
      "Connect to \(asking?.host ?? "")?",
      isPresented: Binding(get: { asking != nil }, set: { if !$0 { asking = nil } }),
      presenting: asking
    ) { host in
      Button("Always Permit") { permit(host, true) }
      Button("Connect") { permit(host, false) }
      Button("Don't Connect", role: .cancel) {}
    } message: { host in
      Text(
        [
          host.reason,
          "Connect only if you trust this host and its certificate's fingerprint is the one its administrator gave you.",
          host.subject.isEmpty ? "" : "Certificate: \(host.subject)",
          host.issuer.isEmpty ? "" : "Issued by: \(host.issuer)",
          host.valid.isEmpty ? "" : "Valid: \(host.valid)",
          "SHA-256 fingerprint: \(host.fingerprint)",
        ].filter { !$0.isEmpty }.joined(separator: "\n\n"))
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

/// A host of the Hosts list: its address and port, and the name it is
/// listed by, empty for its location. Two hosts are the same host when the
/// engine says so (`hostKey`: ASCII case and an international name's
/// punycode form, as DNS compares names).
struct SavedHost: Codable, Hashable, Identifiable {
  var address: String
  var port: UInt16 = 5003
  var name = ""
  var id: String { hostKey(address: address, port: port) }
  /// As the engine takes it.
  var listed: ListedHost { ListedHost(address: address, port: port, name: name) }

  init(address: String, port: UInt16 = 5003, name: String = "") {
    self.address = address
    self.port = port
    self.name = name
  }

  init(_ listed: ListedHost) {
    self.init(address: listed.address, port: listed.port, name: listed.name)
  }
  /// The address, with the port when it is not FileMaker's (an IPv6
  /// address then in brackets).
  var location: String {
    if port == 5003 { return address }
    return address.contains(":") ? "[\(address)]:\(port)" : "\(address):\(port)"
  }
  var label: String { name.isEmpty ? location : name }

  func isHost(_ other: SavedHost) -> Bool { id == other.id }
}

/// A host's file list, as its page shows it.
private enum Listing {
  /// Being asked for. Each request is its own: a reply to an older one is
  /// dropped.
  case pending(UUID)
  case listed([FileEntry])
  case failed(String)
}

/// Add Host, or Edit Host for the host it replaces: the fields as typed.
private struct HostEdit: Identifiable {
  let id = UUID()
  var replaces: SavedHost?
  var address = ""
  var port = "5003"
  var name = ""
  var certificate = ""
  /// The certificate authorities Edit Host showed when it opened.
  var loaded = ""
  /// The fingerprint of the certificate permitted for the host edited;
  /// empty for none.
  var permitted = ""
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
    case .hosts: return "Add a host to see the files it shares."
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
  /// The Hosts list; none until it is first saved.
  @AppStorage("hosts") private var hostsData: Data?
  /// The one host the Hosts tab remembered before it kept a list: the
  /// host form's fields as last typed.
  @AppStorage("lastHost") private var lastHost: String?
  @AppStorage("lastPort") private var lastPort: String?
  @AppStorage("trustedHostCertificates") private var certificates = Data()
  /// The fingerprints of certificates permitted (Always Permit), by host
  /// and port.
  @AppStorage("permittedHostCertificates") private var permitsData = Data()
  /// Certificates permitted with Connect, for as long as the app runs, by
  /// host (lowercase) and port.
  @State private var connectedOnce: [String: String] = [:]
  /// The host whose certificate a listing could not verify, asked about.
  @State private var askingTrust: UntrustedHost?
  /// The host whose files the Hosts tab shows, if any.
  @State private var hostPath: [SavedHost] = []
  /// Each host's file list, by its address (as typed) and port, while the
  /// app runs.
  @State private var listings: [String: Listing] = [:]
  @State private var hostEdit: HostEdit?
  @State private var namingFile: SavedHost?
  @State private var typedFileName = ""
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
  /// The Hosts list, each host once (`distinctHosts`).
  private var hosts: [SavedHost] {
    let stored = hostsData.flatMap { try? JSONDecoder().decode([SavedHost].self, from: $0) } ?? []
    return distinctHosts(hosts: stored.map(\.listed)).map(SavedHost.init)
  }
  /// Private certificate authorities by host and port.
  private var authorities: [String: String] {
    (try? JSONDecoder().decode([String: String].self, from: certificates)) ?? [:]
  }

  var body: some View {
    TabView(selection: $tab) {
      ForEach(SectionTab.allCases, id: \.self) { section in
        Group {
          if section == .hosts {
            hostsTab
          } else {
            NavigationStack {
              List {
                let entries = files(in: section)
                ForEach(entries) { file in row(file, in: section) }
                if entries.isEmpty {
                  Label(section.empty, systemImage: section.icon).foregroundStyle(.secondary)
                }
                if section == .local { Button("Import a File…") { importing = true } }
              }
              .navigationTitle(section.rawValue)
              .toolbar { launchCenterToolbar }
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
      keepFormerHosts()
      reload()
    }
    .onChange(of: phase) { if $0 == .active { reload() } }
    .onOpenURL { url in importFile(url) }
    .sheet(item: $hostEdit) { edit in
      HostEditor(edit: edit, save: { try commit($0) }, forgetPermitted: forgetPermitted)
    }
    .modifier(
      TrustAlert(asking: $askingTrust) { host, always in
        // The host the alert shows, and what met it.
        permit(host, always: always)
        host.retry?()
      })
    .alert("Open by Name", isPresented: present($namingFile), presenting: namingFile) { host in
      TextField("File name", text: $typedFileName)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
      Button("Cancel", role: .cancel) {}
      Button("Sign In") { openByName(on: host) }
    }
    .fileImporter(isPresented: $importing, allowedContentTypes: [.fileMakerFile]) { result in
      do { importFile(try result.get()) } catch { self.error = error.goMessage }
    }
    .sheet(item: $signingIn) { file in
      SignInView(
        file: file, licenseSaved: licenseSaved,
        open: { account, password, license in
          try await open(file, account: account, password: password, license: license)
        },
        permit: { host, always in permit(host, always: always) })
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

  @ToolbarContentBuilder private var launchCenterToolbar: some ToolbarContent {
    ToolbarItem(placement: .principal) { Text("OpenMaker Go").font(.headline) }
    ToolbarItem(placement: .primaryAction) {
      Button {
        showingSettings = true
      } label: {
        Label("Settings", systemImage: "gear")
      }
    }
  }

  /// The Hosts list, as FileMaker Go's: a host opens to the files it
  /// lists, and swiping one (or a long press) edits or removes it.
  private var hostsTab: some View {
    NavigationStack(path: $hostPath) {
      List {
        ForEach(hosts) { host in
          NavigationLink(value: host) {
            Label {
              VStack(alignment: .leading) {
                Text(host.label)
                if !host.name.isEmpty {
                  Text(host.location).font(.caption).foregroundStyle(.secondary)
                }
              }
            } icon: {
              Image(systemName: SectionTab.hosts.icon)
            }
          }
          .swipeActions {
            Button("Remove", role: .destructive) { removeSaved(host) }
            Button("Edit") { editHost(host) }
          }
          .contextMenu {
            Button("Edit Host…") { editHost(host) }
            Button("Remove Host", role: .destructive) { removeSaved(host) }
          }
        }
        if hosts.isEmpty {
          Label(SectionTab.hosts.empty, systemImage: SectionTab.hosts.icon)
            .foregroundStyle(.secondary)
          Button("Add Host…") { hostEdit = HostEdit() }
        }
      }
      .navigationTitle(SectionTab.hosts.rawValue)
      .toolbar {
        launchCenterToolbar
        ToolbarItem(placement: .primaryAction) {
          Button {
            hostEdit = HostEdit()
          } label: {
            Label("Add Host", systemImage: "plus")
          }
        }
      }
      .navigationDestination(for: SavedHost.self) { host in
        HostFiles(
          host: host, listing: listings[key(host)],
          list: { await listHost(host) },
          edit: { editHost(host) },
          openByName: {
            typedFileName = ""
            namingFile = host
          }
        ) { file in
          row(file, in: .hosts)
        }
      }
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
            // A host's page lists only its own files.
            if section != .hosts {
              Text(file.subtitle).font(.caption).foregroundStyle(.secondary)
            }
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
    case .hosts: return []
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

  private func key(_ host: SavedHost) -> String { authority(host.address, host.port) }

  private func storeAuthorities(_ saved: [String: String]) {
    certificates = (try? JSONEncoder().encode(saved)) ?? Data()
  }

  /// The certificate authorities Edit Host shows for one host and port
  /// only, under any spelling of its address (`trustedCertificate`).
  private func trusted(_ address: String, _ port: UInt16) -> String? {
    let pem = trustedCertificate(certificates: authorities, address: address, port: port)
    return pem.isEmpty ? nil : pem
  }

  private func storeHosts(_ list: [SavedHost]) {
    hostsData = (try? JSONEncoder().encode(list)) ?? Data()
    lastHost = nil
    lastPort = nil
  }

  /// Before the Hosts tab kept a list it had one host form, whose address
  /// and port were kept as typed, and it kept the certificate authorities
  /// it was told to trust: a host the form still describes is the list's
  /// first, followed by those hosts (`formerHosts`).
  private func keepFormerHosts() {
    guard hostsData == nil else { return }
    let list = formerHosts(
      lastAddress: lastHost ?? "", lastPort: lastPort ?? "5003", certificates: authorities)
    storeHosts(list.map(SavedHost.init))
  }

  /// Edit Host, with the certificate authority trusted for the host and
  /// the certificate permitted for it.
  private func editHost(_ host: SavedHost) {
    let certificate = trusted(host.address, host.port) ?? ""
    hostEdit = HostEdit(
      replaces: host, address: host.address, port: String(host.port), name: host.name,
      certificate: certificate, loaded: certificate,
      permitted: permitted(host.address, host.port))
  }

  private var permits: [String: String] {
    (try? JSONDecoder().decode([String: String].self, from: permitsData)) ?? [:]
  }

  /// The fingerprint of the certificate permitted for a host and port
  /// (Always Permit), under any spelling of its address (`hostKey`);
  /// empty for none.
  private func permitted(_ address: String, _ port: UInt16) -> String {
    let all = permits
    if let exact = all[authority(address, port)] { return exact }
    return pinKeys(hostKey(address: address, port: port), in: all).sorted().first.flatMap { all[$0] } ?? ""
  }

  /// The keys of `all` that name the host `key` identifies.
  private func pinKeys(_ key: String, in all: [String: String]) -> [String] {
    all.keys.filter { stored in
      guard let colon = stored.lastIndex(of: ":"), colon > stored.startIndex,
        let port = UInt16(stored[stored.index(after: colon)...])
      else { return false }
      return hostKey(address: String(stored[..<colon]), port: port) == key
    }
  }

  /// Forget the certificates permitted for a host and port, under any
  /// spelling of its address.
  private func unpermit(_ address: String, _ port: UInt16, in all: inout [String: String]) {
    for stored in pinKeys(hostKey(address: address, port: port), in: all) { all[stored] = nil }
  }

  private func storePermits(_ all: [String: String]) {
    permitsData = (try? JSONEncoder().encode(all)) ?? Data()
  }

  /// Connect (this time), or with `always` Always Permit: the certificate
  /// asked about is accepted for its host.
  private func permit(_ host: UntrustedHost, always: Bool) {
    let once = hostKey(address: host.host, port: host.port)
    if always {
      var all = permits
      unpermit(host.host, host.port, in: &all)
      all[authority(host.host, host.port)] = host.fingerprint
      storePermits(all)
      connectedOnce[once] = nil
    } else {
      connectedOnce[once] = host.fingerprint
    }
  }

  /// Forget the certificate permitted for the host Edit Host edits.
  private func forgetPermitted(_ host: SavedHost) {
    var all = permits
    unpermit(host.address, host.port, in: &all)
    storePermits(all)
    connectedOnce[host.id] = nil
    listings[key(host)] = nil
  }

  /// A host as the engine connects to it: with every certificate
  /// authority saved for it (`connectCertificate`, which throws when they
  /// cannot all be used), and the certificate permitted for it, the one
  /// permitted with Connect in this run first (a newer certificate than
  /// one saved with Always Permit).
  private func endpoint(_ address: String, _ port: UInt16) throws -> Host {
    let once = connectedOnce[hostKey(address: address, port: port)]
    return Host(
      address: address, port: port,
      certificate: try connectCertificate(certificates: authorities, address: address, port: port),
      permitted: once ?? permitted(address, port))
  }

  /// Save Add Host or Edit Host's `edit` by the engine's rules
  /// (`saveHost`), or throw what to fix, or that the host was changed in
  /// another window since the editor opened. A host that leaves the list
  /// (moved, or merged into another) loses the certificate permitted for
  /// it. A host whose page is shown stays shown, at its new address.
  private func commit(_ edit: HostEdit) throws {
    let before = hosts
    let result = try saveHost(
      saved: SavedHosts(hosts: before.map(\.listed), certificates: authorities),
      change: HostChange(
        replaces: edit.replaces?.listed, loaded: edit.loaded, address: edit.address,
        // As typed: the engine checks its bounds before anything else.
        port: edit.port, name: edit.name, certificate: edit.certificate))
    let host = SavedHost(result.host)
    let listed = result.saved.hosts.map(SavedHost.init)
    var all = permits
    for old in before + (edit.replaces.map { [$0] } ?? []) where !listed.contains(where: { $0.isHost(old) }) {
      unpermit(old.address, old.port, in: &all)
      connectedOnce[old.id] = nil
    }
    storeHosts(listed)
    storeAuthorities(result.saved.certificates)
    storePermits(all)
    hostEdit = nil
    // Its files are listed again: the address, port or certificate
    // authority they were listed with may have changed.
    for old in before.filter({ $0.isHost(host) }) + (edit.replaces.map { [$0] } ?? []) {
      listings[key(old)] = nil
    }
    listings[key(host)] = nil
    if let shown = hostPath.last, edit.replaces?.isHost(shown) == true || shown.isHost(host) {
      Task { await listHost(host) }
      hostPath = [host]
    }
  }

  /// Remove `host` from the list, with the certificate authorities trusted
  /// for it (`removeHost`) and the certificate permitted for it.
  private func removeSaved(_ host: SavedHost) {
    var all = permits
    for old in hosts.filter({ $0.isHost(host) }) + [host] {
      unpermit(old.address, old.port, in: &all)
      connectedOnce[old.id] = nil
      listings[key(old)] = nil
    }
    let result = removeHost(
      saved: SavedHosts(hosts: hosts.map(\.listed), certificates: authorities), host: host.listed)
    storeHosts(result.hosts.map(SavedHost.init))
    storeAuthorities(result.certificates)
    storePermits(all)
    if hostPath.contains(where: { $0.isHost(host) }) { hostPath = [] }
  }

  /// Ask `host` for the files it lists. The answer shows on its page; one
  /// that comes after the host was changed or removed, or asked again, is
  /// dropped.
  private func listHost(_ host: SavedHost) async {
    let id = key(host)
    let asked = UUID()
    listings[id] = .pending(asked)
    let listing: Listing
    var untrusted: UntrustedHost?
    do {
      let server = try endpoint(host.address, host.port)
      let names = try await Task.detached { try hostedFiles(host: server) }.value
      listing = .listed(names.map { FileEntry(name: $0, address: host.address, port: host.port) })
    } catch {
      listing = .failed(error.goMessage)
      untrusted = error.untrustedHost
    }
    guard case .pending(asked)? = listings[id] else { return }
    listings[id] = listing
    // Asked about, and listed again if permitted.
    if var untrusted {
      untrusted.retry = { Task { await listHost(host) } }
      askingTrust = untrusted
    }
  }

  /// Sign in to a file `host` does not list, by its name (name.fmp12).
  private func openByName(on host: SavedHost) {
    let name = typedFileName.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !name.isEmpty else { return }
    signingIn = FileEntry(name: hostedFileName(name: name), address: host.address, port: host.port)
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
    let source: Source =
      try file.local
      ? .local(path: file.url.path)
      : .hosted(host: endpoint(file.address, file.port), file: file.name)
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

/// A host's files, as FileMaker Go's host pages show them: with Search
/// Files, and pulled down to list them again.
private struct HostFiles<Row: View>: View {
  let host: SavedHost
  let listing: Listing?
  let list: () async -> Void
  let edit: () -> Void
  let openByName: () -> Void
  @ViewBuilder let row: (FileEntry) -> Row
  @State private var query = ""

  var body: some View {
    List {
      switch listing {
      case .listed(let files)?:
        let typed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let shown = typed.isEmpty ? files : files.filter { $0.name.localizedCaseInsensitiveContains(typed) }
        Section {
          ForEach(shown) { file in row(file) }
          if shown.isEmpty {
            Text(files.isEmpty ? "This host lists no files." : "No files match “\(typed)”.")
              .foregroundStyle(.secondary)
          }
        } header: {
          if !host.name.isEmpty { Text(host.location) }
        }
      case .failed(let reason)?:
        Section {
          VStack(alignment: .leading, spacing: 4) {
            Text("The files could not be listed").font(.headline)
            Text(reason).foregroundStyle(.secondary)
          }
          Button("Try Again") { Task { await list() } }
          Button("Edit Host…", action: edit)
        }
      case .pending?, nil:
        HStack {
          Spacer()
          ProgressView()
          Spacer()
        }
        .listRowBackground(Color.clear)
      }
    }
    // Shown at once, not only when the list is pulled down.
    .searchable(
      text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search Files")
    .refreshable { await list() }
    .navigationTitle(host.label)
    .toolbar {
      ToolbarItem(placement: .primaryAction) { Button("Open by Name…", action: openByName) }
    }
    .task { if listing == nil { await list() } }
  }
}

/// Add Host and Edit Host, after FileMaker Go's Add Host (a host name or IP
/// address and an optional name), with the port and a private certificate
/// authority besides.
private struct HostEditor: View {
  @State private var edit: HostEdit
  let save: (HostEdit) throws -> Void
  let forgetPermitted: (SavedHost) -> Void
  @Environment(\.dismiss) private var dismiss
  @State private var showsCertificate: Bool
  @State private var error = ""

  init(
    edit: HostEdit, save: @escaping (HostEdit) throws -> Void,
    forgetPermitted: @escaping (SavedHost) -> Void
  ) {
    _edit = State(initialValue: edit)
    _showsCertificate = State(initialValue: !edit.certificate.isEmpty)
    self.save = save
    self.forgetPermitted = forgetPermitted
  }

  var body: some View {
    NavigationStack {
      Form {
        // First, where it is seen without scrolling.
        if !error.isEmpty {
          Text(error).foregroundStyle(.red)
        }
        // Labeled, as a filled field shows no placeholder.
        Section {
          Group {
            LabeledContent("Address") {
              TextField("Host name or IP address", text: $edit.address)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .keyboardType(.URL)
            }
            LabeledContent("Port") {
              TextField("5003", text: $edit.port).keyboardType(.numberPad)
            }
            LabeledContent("Name") {
              TextField("Optional", text: $edit.name)
                .onSubmit(submit)
            }
          }
          .multilineTextAlignment(.trailing)
        } footer: {
          Text("The Hosts list shows this name instead of the address.")
        }
        Section {
          DisclosureGroup("Private certificate authority", isExpanded: $showsCertificate) {
            TextField("CA certificate (PEM)", text: $edit.certificate, axis: .vertical)
              .textInputAutocapitalization(.never)
              .autocorrectionDisabled()
              .font(.caption.monospaced())
          }
        } footer: {
          Text("Trusted for this host and port only.")
        }
        if let host = edit.replaces, !edit.permitted.isEmpty {
          Section {
            Button("Forget Permitted Certificate", role: .destructive) {
              forgetPermitted(host)
              edit.permitted = ""
            }
          } footer: {
            Text(
              "Connects although its certificate cannot be verified: you permitted the one with SHA-256 fingerprint \(edit.permitted)."
            )
          }
        }
      }
      .navigationTitle(edit.replaces == nil ? "Add Host" : "Edit Host")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
        ToolbarItem(placement: .confirmationAction) { Button("Save", action: submit) }
      }
    }
  }

  private func submit() {
    do {
      try save(edit)
    } catch {
      self.error = error.goMessage
    }
  }
}

private struct SignInView: View {
  let file: FileEntry
  let licenseSaved: Bool
  let open: (String, String, String) async throws -> Void
  /// Connect or Always Permit the certificate of a host the open met.
  let permit: (UntrustedHost, Bool) -> Void
  /// The host whose certificate the open could not verify, asked about.
  @State private var asking: UntrustedHost?
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
    .modifier(
      TrustAlert(asking: $asking) { host, always in
        permit(host, always)
        signIn()
      })
  }

  private func signIn() {
    busy = true
    error = ""
    Task {
      defer { busy = false }
      do {
        try await open(account, password, license)
      } catch let refused where refused.untrustedHost != nil {
        // Asked about here, over the sign-in; signed in again if permitted.
        error = refused.goMessage
        asking = refused.untrustedHost
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
