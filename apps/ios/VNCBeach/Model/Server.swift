import Foundation

/// A saved computer. The password lives in the Keychain, see `ServerStore`.
struct Server: Identifiable, Codable, Hashable {
    var id = UUID()
    var name: String
    /// What the user typed: `sebastian-pc`, `10.0.0.2:1`, `host::5901`, …
    var host: String
    var viewOnly = false
    /// Optional so favourites saved by older app versions still decode.
    var access: AccessSettings?
    /// Device ID of an account device (see `AccountStore`); its sessions
    /// sign in with a ticket from the coordination server.
    var device: String?
    /// Unix milliseconds of the last change, for the favourites sync; nil
    /// if never synced.
    var updated: Int64?

    var displayName: String {
        name.trimmingCharacters(in: .whitespaces).isEmpty ? host : name
    }
}
