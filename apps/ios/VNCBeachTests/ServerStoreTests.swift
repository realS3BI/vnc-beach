import Foundation
import Testing

@testable import VNCBeach

struct ServerStoreTests {
    @Test @MainActor func favouritesRoundTrip() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: dir) }
        let service = "at.schlossers.vncbeach.tests.\(UUID().uuidString)"
        let store = ServerStore(directory: dir, keychainService: service)
        let server = Server(name: "PC", host: "sebastian-pc", viewOnly: true)
        store.save(server, password: "geheim")

        let reloaded = ServerStore(directory: dir, keychainService: service)
        #expect(reloaded.servers == [server])
        #expect(reloaded.password(for: server) == "geheim")

        reloaded.remove(server)
        #expect(reloaded.servers.isEmpty)
        #expect(ServerStore(directory: dir, keychainService: service).servers.isEmpty)
    }

    @Test @MainActor func urlsOpenFavouritesOrCreateThem() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: dir) }
        let service = "at.schlossers.vncbeach.tests.\(UUID().uuidString)"
        let store = ServerStore(directory: dir, keychainService: service)
        let pc = Server(name: "Büro-PC", host: "sebastian-pc")
        store.save(pc, password: "x")
        #expect(store.server(for: URL(string: "vncbeach://sebastian-pc")!) == pc)
        #expect(store.server(for: URL(string: "vncbeach://b%C3%BCro-pc")!) == pc)
        #expect(store.server(for: URL(string: "https://sebastian-pc")!) == nil)
        let fresh = try #require(store.server(for: URL(string: "vncbeach://mac.local:1?password=abc")!))
        #expect(fresh.host == "mac.local:1")
        #expect(store.password(for: fresh) == "abc")
        #expect(store.servers.count == 2)
        store.remove(pc)
        store.remove(fresh)
    }

    @Test func displayNameFallsBackToHost() {
        #expect(Server(name: " ", host: "pc").displayName == "pc")
        #expect(Server(name: "Büro", host: "pc").displayName == "Büro")
    }
}
