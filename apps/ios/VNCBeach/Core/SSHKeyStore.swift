import Foundation
import Security

enum AccessError: LocalizedError {
    case message(String)
    var errorDescription: String? {
        switch self { case .message(let message): message }
    }
}

/// Imported identities stay in the Keychain. libssh2 receives a protected,
/// temporary copy for one attempt, removed as soon as preparation finishes.
struct SSHKeyStore {
    static let keychain = Keychain(service: "at.schlossers.vncbeach.ssh-keys")

    static func importKey(from url: URL) throws -> String {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard size > 0, size <= 1_048_576 else {
            throw AccessError.message("Die Schlüsseldatei ist leer oder größer als 1 MB.")
        }
        let text = try String(contentsOf: url, encoding: .utf8)
        guard text.contains("PRIVATE KEY-----"), text.contains("-----END ") else {
            throw AccessError.message("Bitte einen privaten SSH-Schlüssel im OpenSSH- oder PEM-Format auswählen.")
        }
        let id = UUID().uuidString
        let status = keychain.write(text, account: id, accessibility: kSecAttrAccessibleWhenUnlockedThisDeviceOnly)
        guard status == errSecSuccess else {
            throw AccessError.message("Der SSH-Schlüssel konnte nicht im Schlüsselbund gespeichert werden (\(status)).")
        }
        return id
    }

    static func removeUnused(_ ids: Set<String>, servers: [Server]) {
        let used = servers.reduce(into: Set<String>()) { $0.formUnion($1.access?.keyIDs ?? []) }
        for id in ids.subtracting(used) { keychain.delete(account: id) }
    }
}

final class PreparedAccess {
    private static var cleanedPreviousAttempts = false
    let json: String
    private let directory: URL

    init(_ settings: AccessSettings, action: ConnectionAction) throws {
        // After a crash there are no live attempts from the previous process.
        if !Self.cleanedPreviousAttempts {
            Self.cleanedPreviousAttempts = true
            let old = (try? FileManager.default.contentsOfDirectory(
                at: FileManager.default.temporaryDirectory, includingPropertiesForKeys: nil)) ?? []
            for file in old where file.lastPathComponent.hasPrefix("vnc-beach-ssh-") {
                try? FileManager.default.removeItem(at: file)
            }
        }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("vnc-beach-ssh-\(UUID().uuidString)")
        directory = dir
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
            var copy = settings
            var files: [String: String] = [:]
            func prepare(_ ssh: inout SSHSettings) throws {
                guard case .key(let id) = ssh.auth else { return }
                if let path = files[id] { ssh.auth = .key(path); return }
                guard let text = SSHKeyStore.keychain.read(account: id) else {
                    throw AccessError.message("Der SSH-Schlüssel fehlt. Bitte die Schlüsseldatei erneut importieren.")
                }
                let file = dir.appendingPathComponent(UUID().uuidString)
                try Data(text.utf8).write(to: file, options: .completeFileProtection)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
                files[id] = file.path
                ssh.auth = .key(file.path)
            }
            if action == .signIn, var ssh = copy.ssh {
                try prepare(&ssh)
                copy.ssh = ssh
            }
            if action == .wake, var wake = copy.wake {
                for i in wake.relays.indices { try prepare(&wake.relays[i].ssh) }
                copy.wake = wake
            }
            json = try copy.json()
        } catch {
            try? FileManager.default.removeItem(at: dir)
            throw error
        }
    }

    deinit { try? FileManager.default.removeItem(at: directory) }
}
