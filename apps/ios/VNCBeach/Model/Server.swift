import Foundation

/// A saved computer. The password lives in the Keychain, see `ServerStore`.
struct Server: Identifiable, Codable, Hashable {
    var id = UUID()
    var name: String
    /// What the user typed: `sebastian-pc`, `10.0.0.2:1`, `host::5901`, …
    var host: String
    var viewOnly = false

    var displayName: String {
        name.trimmingCharacters(in: .whitespaces).isEmpty ? host : name
    }
}
