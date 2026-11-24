import Foundation
import Security
import SwiftUI

/// The FileMaker client license saved in Settings, for hosts that require
/// one: a Keychain item of this device only (never synced or restored to
/// another), readable once the device has been unlocked since it started.
enum SavedLicense {
  private static let item: [String: Any] = [
    kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: "dev.openmaker.go",
    kSecAttrAccount as String: "filemaker-client-license",
  ]

  static var isSaved: Bool {
    var query = item
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    return SecItemCopyMatching(query as CFDictionary, nil) == errSecSuccess
  }

  /// The saved license, or empty when none is saved.
  static func load() -> String {
    var query = item
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var data: CFTypeRef?
    guard SecItemCopyMatching(query as CFDictionary, &data) == errSecSuccess,
      let data = data as? Data
    else { return "" }
    return String(decoding: data, as: UTF8.self)
  }

  /// Save `license` in place of any saved before; a failed save keeps the
  /// one there was.
  static func save(_ license: String) throws {
    let value: [String: Any] = [
      kSecValueData as String: Data(license.utf8),
      kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
    ]
    var status = SecItemUpdate(item as CFDictionary, value as CFDictionary)
    if status == errSecItemNotFound {
      status = SecItemAdd(item.merging(value) { $1 } as CFDictionary, nil)
    }
    guard status == errSecSuccess else {
      throw CocoaError(.fileWriteUnknown, userInfo: [
        NSLocalizedDescriptionKey: "The license could not be saved in the Keychain.",
      ])
    }
  }

  static func remove() {
    SecItemDelete(item as CFDictionary)
  }
}

/// The app's settings: the FileMaker client license, as the desktop's
/// FileMaker License dialog keeps it.
struct SettingsView: View {
  @Binding var licenseSaved: Bool
  @Environment(\.dismiss) private var dismiss
  @State private var license = ""
  @State private var error = ""

  var body: some View {
    NavigationStack {
      Form {
        Section {
          Text(licenseSaved ? "A license is saved on this device." : "No license is saved.")
            .foregroundStyle(.secondary)
          // A license key, not a password: AutoFill leaves it alone.
          TextField("License key", text: $license)
            .textContentType(nil)
            .textInputAutocapitalization(.characters)
            .autocorrectionDisabled()
            .privacySensitive()
          Button("Save License", action: save)
            .disabled(license.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
          if licenseSaved {
            Button("Remove License", role: .destructive) {
              SavedLicense.remove()
              licenseSaved = SavedLicense.isSaved
            }
          }
          if !error.isEmpty { Text(error).foregroundStyle(.red) }
        } header: {
          Text("FileMaker License")
        } footer: {
          Text(
            "Optional. FileMaker Server may require a client license; OpenMaker Server does not. "
              + "It is sent to a host when its sign-in's license is left empty.")
        }
      }
      .navigationTitle("Settings")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { Button("Done") { dismiss() } }
    }
  }

  private func save() {
    do {
      try SavedLicense.save(license.trimmingCharacters(in: .whitespacesAndNewlines))
      license = ""
      error = ""
    } catch {
      self.error = error.goMessage
    }
    licenseSaved = SavedLicense.isSaved
  }
}
