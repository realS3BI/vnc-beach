import BeachMobile
import Foundation

/// Mirrors beach-access's persisted capabilities. Passwords never enter this model.
struct AccessSettings: Codable, Hashable {
    var alternatives: [String] = []
    var ssh: SSHSettings?
    var wake: WakeSettings?

    var isEmpty: Bool { alternatives.isEmpty && ssh == nil && wake == nil }

    var keyIDs: Set<String> {
        Set(([ssh].compactMap { $0 } + (wake?.relays.map(\.ssh) ?? [])).compactMap {
            if case .key(let id) = $0.auth { return id }
            return nil
        })
    }

    func json() throws -> String {
        String(decoding: try JSONEncoder().encode(self), as: UTF8.self)
    }

    func validate() -> String? {
        if let wake, wake.relays.isEmpty {
            return "Zum Aufwecken auf iPhone und iPad mindestens ein Zugangsgerät im LAN des Computers hinzufügen."
        }
        do {
            var error = [CChar](repeating: 0, count: 2048)
            if beach_access_validate(try json(), &error, error.count) { return nil }
            return error.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
        } catch {
            return "Die Verbindungseinstellungen konnten nicht gelesen werden."
        }
    }
}

struct SSHSettings: Codable, Hashable {
    var address = ""
    var user = ""
    var auth = SSHAuthentication.password
    var keyName: String?
}

enum SSHAuthentication: Codable, Hashable {
    case password
    /// Keychain account, replaced with an ephemeral file path for the Rust call.
    case key(String)

    private enum CodingKeys: String, CodingKey { case method, path }
    private enum Method: String, Codable { case password, key }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(Method.self, forKey: .method) {
        case .password: self = .password
        case .key: self = .key(try values.decode(String.self, forKey: .path))
        }
    }

    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .password: try values.encode(Method.password, forKey: .method)
        case .key(let id):
            try values.encode(Method.key, forKey: .method)
            try values.encode(id, forKey: .path)
        }
    }
}

struct WakeSettings: Codable, Hashable {
    var mac = ""
    var destination = "255.255.255.255:9"
    var relays: [WakeRelay] = []
}

struct WakeRelay: Identifiable, Codable, Hashable {
    var id = UUID()
    var ssh = SSHSettings()
    var shell = RelayShell.posix
}

enum RelayShell: String, Codable, CaseIterable {
    case posix
    case powerShell = "power_shell"
    var label: String {
        switch self {
        case .posix: "macOS / Linux / NAS"
        case .powerShell: "Windows (PowerShell)"
        }
    }
}

enum ConnectionAction: Int32 {
    case connect = 0, signIn = 1, wake = 2
}

struct AccessPrompt: Identifiable, Equatable {
    let id: UInt64
    let question: Question

    enum Question: Equatable {
        case trust(address: String, fingerprint: String)
        case secret(title: String, label: String, echo: Bool)
    }
}

enum PreparationEvent: Decodable {
    case progress(String), prompt(AccessPrompt), connected, failed(String)
    private enum CodingKeys: String, CodingKey {
        case kind, id, text, address, fingerprint, title, label, echo, message
    }
    private enum Kind: String, Decodable { case progress, trust, secret, connected, failed }

    init(from decoder: Decoder) throws {
        let v = try decoder.container(keyedBy: CodingKeys.self)
        switch try v.decode(Kind.self, forKey: .kind) {
        case .progress: self = .progress(try v.decode(String.self, forKey: .text))
        case .connected: self = .connected
        case .failed: self = .failed(try v.decode(String.self, forKey: .message))
        case .trust:
            self = .prompt(AccessPrompt(id: try v.decode(UInt64.self, forKey: .id), question: .trust(
                address: try v.decode(String.self, forKey: .address),
                fingerprint: try v.decode(String.self, forKey: .fingerprint))))
        case .secret:
            self = .prompt(AccessPrompt(id: try v.decode(UInt64.self, forKey: .id), question: .secret(
                title: try v.decode(String.self, forKey: .title),
                label: try v.decode(String.self, forKey: .label),
                echo: try v.decode(Bool.self, forKey: .echo))))
        }
    }
}
