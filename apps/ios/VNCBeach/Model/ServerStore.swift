import Foundation
import Observation

/// Favourites as JSON in Application Support, passwords in the Keychain.
/// With an account, `AccountStore` keeps them in step with the other viewers
/// (see `FavoriteSync`); `deleted.json` remembers removed ones for that.
@Observable
@MainActor
final class ServerStore {
    private(set) var servers: [Server] = []
    /// Counts local changes, so the sync knows when to run.
    private(set) var revision = 0
    private var deleted: [String: Int64] = [:]
    private let file: URL
    private let deletedFile: URL
    private let keychain: Keychain

    init(directory: URL? = nil, keychainService: String = "at.schlossers.vncbeach") {
        let base =
            directory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("VNC Beach", isDirectory: true)
        file = base.appendingPathComponent("servers.json")
        deletedFile = base.appendingPathComponent("deleted.json")
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
        var server = server
        server.updated = FavoriteSync.now()
        revision += 1
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
        deleted[server.id.uuidString] = FavoriteSync.now()
        revision += 1
        SSHKeyStore.removeUnused(server.access?.keyIDs ?? [], servers: servers)
        keychain.delete(account: server.id.uuidString)
        persist()
    }

    func move(from source: IndexSet, to destination: Int) {
        servers.move(fromOffsets: source, toOffset: destination)
        persist()
    }

    /// Everything the sync compares: the favourites and the tombstones.
    var favorites: [Favorite] {
        servers.map {
            Favorite(
                id: $0.id.uuidString, name: $0.name, host: $0.host, alternatives: $0.access?.alternatives ?? [],
                viewOnly: $0.viewOnly, updated: $0.updated ?? 0)
        } + deleted.map { Favorite(id: $0.key, updated: $0.value, deleted: true) }
    }

    /// Takes over favourites that are newer on the server; passwords and
    /// the other local settings stay.
    func apply(_ remote: [Favorite]) {
        guard !remote.isEmpty else { return }
        let local = favorites
        for r in remote {
            guard let id = UUID(uuidString: r.id) else { continue }
            var index = servers.firstIndex { $0.id == id }
            if index == nil, let match = local.first(where: { FavoriteSync.sameHost(r, $0) }) {
                index = servers.firstIndex { $0.id.uuidString == match.id }
                // The password follows the favourite to its synced ID.
                if let index {
                    let old = servers[index].id.uuidString
                    if let password = keychain.read(account: old) {
                        keychain.write(password, account: id.uuidString)
                        keychain.delete(account: old)
                    }
                }
            }
            if r.deleted {
                if let index {
                    let server = servers.remove(at: index)
                    keychain.delete(account: server.id.uuidString)
                    SSHKeyStore.removeUnused(server.access?.keyIDs ?? [], servers: servers)
                }
                deleted[r.id] = r.updated
                continue
            }
            if let gone = deleted[r.id], gone >= r.updated { continue }
            deleted[r.id] = nil
            var server = index.map { servers[$0] } ?? Server(name: "", host: "")
            server.id = id
            server.name = r.name
            server.host = r.host
            server.viewOnly = r.viewOnly
            var access = server.access ?? AccessSettings()
            access.alternatives = r.alternatives
            server.access = access.isEmpty ? nil : access
            server.updated = r.updated
            if let index { servers[index] = server } else { servers.append(server) }
        }
        persist()
    }

    private func load() {
        if let data = try? Data(contentsOf: deletedFile) {
            deleted = (try? JSONDecoder().decode([String: Int64].self, from: data)) ?? [:]
        }
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
            try encoder.encode(deleted).write(to: deletedFile, options: .atomic)
        } catch {
            NSLog("saving servers failed: \(error)")
        }
    }
}
