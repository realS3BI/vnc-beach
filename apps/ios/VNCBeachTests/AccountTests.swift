import Foundation
import Testing

@testable import VNCBeach

struct AccountTests {
    private static let devicesJSON = """
        [
          {"id":"d1","name":"Büro-PC","platform":"windows","org":{"id":"o1","name":"Schlosser"},
           "addresses":["100.64.0.2","fd7a::2","192.168.1.20"],"port":5900,"online":true,
           "last_seen":1700000000,"version":"0.4.0"},
          {"id":"d2","name":"MacBook","platform":"macos","org":null,
           "addresses":[],"port":5900,"online":false,"last_seen":null,"version":"0.4.0"}
        ]
        """

    @Test func addressesNormalizeToHTTPS() throws {
        #expect(try AccountAPI.normalize(" beach.example.com/ ") == "https://beach.example.com")
        #expect(try AccountAPI.normalize("http://127.0.0.1:18080") == "http://127.0.0.1:18080")
        #expect(throws: AccountError.self) { try AccountAPI.normalize("") }
        #expect(throws: AccountError.self) { try AccountAPI.normalize("ftp://x") }
    }

    @Test func loopbackLoginAnswersTheBrowser() async throws {
        let login = try LoopbackLogin()
        #expect(login.waitForCode(milliseconds: 10) == nil)
        let url = try #require(URL(string: "http://127.0.0.1:\(login.port)/callback?code=abc%2Bd"))
        async let page = URLSession.shared.data(from: url)
        var code: String?
        for _ in 0..<20 where code == nil {
            code = await Task.detached { login.waitForCode(milliseconds: 250) }.value
        }
        #expect(code == "abc+d")
        let (data, _) = try await page
        #expect(String(decoding: data, as: UTF8.self).contains("Angemeldet"))
    }

    @Test func devicesDecodeAndDescribeThemselves() throws {
        let devices = try JSONDecoder().decode([AccountDevice].self, from: Data(Self.devicesJSON.utf8))
        #expect(devices[0].endpoints == ["100.64.0.2:5900", "[fd7a::2]:5900", "192.168.1.20:5900"])
        #expect(devices[0].status() == "online · 100.64.0.2")
        #expect(devices[1].status() == "noch nie online")
        var seen = devices[0]
        seen.online = false
        #expect(seen.status(now: Date(timeIntervalSince1970: 1_700_000_000 + 7200)) == "offline · vor 2 Std.")
    }

    @Test @MainActor func cachedDevicesBecomeTicketServers() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: dir) }
        let service = "at.schlossers.vncbeach.tests.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: service))
        defer { defaults.removePersistentDomain(forName: service) }
        defaults.set("https://beach.example.com", forKey: "account.url")
        Keychain(service: service).write("session-token", account: "session")
        defer { Keychain(service: service).delete(account: "session") }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let snapshot = """
            {"url":"https://beach.example.com","orgs":[],"devices":\(Self.devicesJSON)}
            """
        try Data(snapshot.utf8).write(to: dir.appendingPathComponent("account.json"))

        let account = AccountStore(directory: dir, keychainService: service, defaults: defaults)
        #expect(account.signedIn)
        #expect(account.groups.map(\.title) == ["Persönlich", "Schlosser"])

        let pc = account.server(for: account.devices[0])
        #expect(pc.host == "100.64.0.2:5900")
        #expect(pc.access?.alternatives == ["[fd7a::2]:5900", "192.168.1.20:5900"])
        #expect(account.server(id: pc.id) == pc)
        let target = try #require(account.ticketTarget(for: pc))
        let fields = try JSONDecoder().decode([String: String].self, from: Data(target.utf8))
        #expect(fields == ["url": "https://beach.example.com", "token": "session-token", "device": "d1"])
        #expect(account.ticketTarget(for: Server(name: "", host: "x")) == nil)
        #expect(account.server(for: account.devices[1]).host.isEmpty)
    }
}

struct FavoriteSyncTests {
    private func fav(_ id: String, _ host: String, _ updated: Int64) -> Favorite {
        Favorite(id: id, name: host, host: host, updated: updated)
    }

    @Test func newerSideWins() {
        let local = [fav("a", "mac", 20), fav("b", "pc", 10), fav("c", "nas", 5)]
        let remote = [fav("a", "mac", 10), fav("b", "pc.local", 30), fav("d", "ipad", 7)]
        let merged = FavoriteSync.merge(local: local, remote: remote)
        #expect(merged.apply == [fav("b", "pc.local", 30), fav("d", "ipad", 7)])
        #expect(merged.push == [fav("a", "mac", 20), fav("c", "nas", 5)])
    }

    @Test func tombstonesAndFirstSync() {
        var gone = fav("a", "mac", 30)
        gone.deleted = true
        #expect(FavoriteSync.merge(local: [fav("a", "mac", 20)], remote: [gone]).apply == [gone])
        #expect(FavoriteSync.merge(local: [], remote: [gone]) == FavoriteSync.Merged())
        let merged = FavoriteSync.merge(
            local: [fav("x", "Sebastian-PC", 0), fav("n", "nas", 0)], remote: [fav("y", "sebastian-pc", 9)], now: 99)
        #expect(merged.push == [fav("n", "nas", 99)])
        #expect(merged.apply == [fav("y", "sebastian-pc", 9), fav("n", "nas", 99)])
    }

    @Test func wireFormatMatchesTheServer() throws {
        let json = #"{"id":"A","host":"mac","view_only":true,"updated":5}"#
        let f = try JSONDecoder().decode(Favorite.self, from: Data(json.utf8))
        #expect(f.viewOnly && !f.deleted && f.name.isEmpty)
        let back = String(decoding: try JSONEncoder().encode(f), as: UTF8.self)
        #expect(back.contains(#""view_only":true"#))
    }

    @Test @MainActor func storeAppliesRemoteChangesAndKeepsPasswords() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: dir) }
        let service = "at.schlossers.vncbeach.tests.\(UUID().uuidString)"
        // A favourite saved before the sync existed (no `updated`).
        let legacy = Server(name: "PC", host: "sebastian-pc")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try JSONEncoder().encode([legacy]).write(to: dir.appendingPathComponent("servers.json"))
        Keychain(service: service).write("geheim", account: legacy.id.uuidString)
        let store = ServerStore(directory: dir, keychainService: service)

        // Another viewer synced the same computer first: matched by address,
        // and the password moves to the synced ID.
        let remoteID = UUID()
        store.apply([Favorite(id: remoteID.uuidString, name: "Büro-PC", host: "SEBASTIAN-PC", updated: 50)])
        #expect(store.servers.map(\.id) == [remoteID])
        #expect(store.servers[0].name == "Büro-PC")
        #expect(store.password(for: store.servers[0]) == "geheim")

        // A deletion elsewhere removes it here and leaves a tombstone.
        store.apply([Favorite(id: remoteID.uuidString, updated: 60, deleted: true)])
        #expect(store.servers.isEmpty)
        #expect(store.favorites == [Favorite(id: remoteID.uuidString, updated: 60, deleted: true)])
        // An older copy doesn't bring it back.
        store.apply([Favorite(id: remoteID.uuidString, name: "Alt", host: "x", updated: 55)])
        #expect(store.servers.isEmpty)
    }
}
