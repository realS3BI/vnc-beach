import Foundation
import Observation

/// Favourites as JSON in Application Support, passwords in the Keychain.
@Observable
@MainActor
final class ServerStore {
    private(set) var servers: [Server] = []
    private let file: URL
    private let keychain: Keychain

    init(directory: URL? = nil, keychainService: String = "at.schlossers.vncbeach") {
        let base =
            directory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("VNC Beach", isDirectory: true)
        file = base.appendingPathComponent("servers.json")
        keychain = Keychain(service: keychainService)
        load()
    }

    func server(id: Server.ID) -> Server? {
        servers.first { $0.id == id }
    }

    /// `vncbeach://<favourite name or host>[?password=…]`, e.g. from the
    /// Shortcuts app. Unknown hosts are saved as a new favourite.
    func server(for url: URL) -> Server? {
        guard url.scheme == "vncbeach" else { return nil }
        let port = url.port.map { ":\($0)" } ?? ""
        let target = ((url.host ?? "") + port + url.path).removingPercentEncoding ?? ""
        guard !target.isEmpty else { return nil }
        if let known = servers.first(where: {
            $0.name.caseInsensitiveCompare(target) == .orderedSame
                || $0.host.caseInsensitiveCompare(target) == .orderedSame
        }) {
            return known
        }
        let password = URLComponents(url: url, resolvingAgainstBaseURL: false)?
            .queryItems?.first { $0.name == "password" }?.value ?? ""
        let server = Server(name: "", host: target)
        save(server, password: password)
        return server
    }

    func password(for server: Server) -> String {
        keychain.read(account: server.id.uuidString) ?? ""
    }

    func save(_ server: Server, password: String) {
        if let i = servers.firstIndex(where: { $0.id == server.id }) {
            servers[i] = server
        } else {
            servers.append(server)
        }
        keychain.write(password, account: server.id.uuidString)
        persist()
    }

    func remove(_ server: Server) {
        servers.removeAll { $0.id == server.id }
        SSHKeyStore.removeUnused(server.access?.keyIDs ?? [], servers: servers)
        keychain.delete(account: server.id.uuidString)
        persist()
    }

    func move(from source: IndexSet, to destination: Int) {
        servers.move(fromOffsets: source, toOffset: destination)
        persist()
    }

    private func load() {
        guard let data = try? Data(contentsOf: file) else { return }
        servers = (try? JSONDecoder().decode([Server].self, from: data)) ?? []
    }

    private func persist() {
        do {
            try FileManager.default.createDirectory(
                at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            try encoder.encode(servers).write(to: file, options: .atomic)
        } catch {
            NSLog("saving servers failed: \(error)")
        }
    }
}
