import Foundation
import Network
import Testing

@testable import VNCBeach

@MainActor
struct AccessTests {
    @Test func existingFavouritesDecodeWithoutCapabilities() throws {
        let id = UUID()
        let data = Data("[{\"id\":\"\(id)\",\"name\":\"Alt\",\"host\":\"mac.local\",\"viewOnly\":false}]".utf8)
        let servers = try JSONDecoder().decode([Server].self, from: data)
        #expect(servers.first?.id == id)
        #expect(servers.first?.access == nil)
    }

    @Test func capabilitiesRoundTripAndUseSharedValidation() throws {
        let settings = AccessSettings(alternatives: ["192.168.1.9::5900", "[fd00::9]:5900"],
            ssh: SSHSettings(address: "mac.local:22", user: "remote", auth: .key("keychain-id")),
            wake: WakeSettings(mac: "00:11:22:33:44:55", relays: [
                WakeRelay(ssh: SSHSettings(address: "pi", user: "remote")),
                WakeRelay(ssh: SSHSettings(address: "nas", user: "remote"))]))
        let server = Server(name: "Mac", host: "mac.local", access: settings)
        #expect(try JSONDecoder().decode(Server.self, from: JSONEncoder().encode(server)) == server)
        #expect(settings.validate() == nil)
        #expect(settings.keyIDs == ["keychain-id"])
        var invalid = settings
        invalid.wake?.mac = "00:11:22"
        #expect(invalid.validate()?.contains("MAC-Adresse") == true)
        invalid = settings
        invalid.ssh?.user = ""
        #expect(invalid.validate()?.contains("Benutzername") == true)
    }

    @Test func promptDecoderPreservesIdentityAndKeyboardChallenges() throws {
        let trust = try JSONDecoder().decode(PreparationEvent.self,
            from: Data(#"{"kind":"trust","id":1,"address":"pi:22","fingerprint":"SHA256:fixture"}"#.utf8))
        guard case .prompt(let prompt) = trust else { Issue.record("Missing trust prompt"); return }
        #expect(prompt == AccessPrompt(id: 1, question: .trust(address: "pi:22", fingerprint: "SHA256:fixture")))
        let challenge = try JSONDecoder().decode(PreparationEvent.self,
            from: Data(#"{"kind":"secret","id":2,"title":"SSH","label":"Code:","echo":false}"#.utf8))
        guard case .prompt(let second) = challenge else { Issue.record("Missing challenge"); return }
        #expect(second == AccessPrompt(id: 2, question: .secret(title: "SSH", label: "Code:", echo: false)))
    }

    @Test func importedKeyIsMaterializedOnlyForTheSelectedAction() throws {
        let id = UUID().uuidString
        let secret = "-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----"
        SSHKeyStore.keychain.write(secret, account: id)
        defer { SSHKeyStore.keychain.delete(account: id) }
        let settings = AccessSettings(ssh: SSHSettings(address: "mac", user: "remote", auth: .key(id)))
        var prepared: PreparedAccess? = try PreparedAccess(settings, action: .signIn)
        let decoded = try JSONDecoder().decode(AccessSettings.self, from: Data(try #require(prepared).json.utf8))
        guard case .key(let path) = decoded.ssh?.auth else { Issue.record("Missing key file"); return }
        #expect(try String(contentsOfFile: path, encoding: .utf8) == secret)
        #expect(try FileManager.default.attributesOfItem(atPath: path)[.posixPermissions] as? Int == 0o600)
        prepared = nil
        #expect(!FileManager.default.fileExists(atPath: path))
        SSHKeyStore.keychain.delete(account: id)
        // An ordinary VNC connection never needs the optional SSH identity.
        #expect(throws: Never.self) { _ = try PreparedAccess(settings, action: .connect) }
        #expect(throws: AccessError.self) { _ = try PreparedAccess(settings, action: .signIn) }
    }

    @Test func fallbackConnectsUsingStandardRFB() async throws {
        let fixture = try await RFBFixture.start()
        defer { fixture.stop() }
        let session = RemoteSession()
        defer { session.close() }
        await session.connect(address: "127.0.0.1::1", password: "", depth: .full,
            access: AccessSettings(alternatives: [fixture.address]))
        #expect(session.state == .connected)
        #expect(session.name == "iOS RFB fixture")
        #expect(session.size == CGSize(width: 2, height: 2))
    }

    @Test func cancelledAttemptCannotCloseItsReplacement() async throws {
        let stalled = try await RFBFixture.start(stall: true)
        let working = try await RFBFixture.start()
        defer { stalled.stop(); working.stop() }
        let session = RemoteSession()
        defer { session.close() }
        let first = Task { await session.connect(address: stalled.address, password: "", depth: .full) }
        try await Task.sleep(for: .milliseconds(100))
        #expect(session.state == .connecting)
        session.close(byUser: true)
        await session.connect(address: working.address, password: "", depth: .full)
        await first.value
        try await Task.sleep(for: .milliseconds(100))
        #expect(session.state == .connected)
        #expect(!session.closedByUser)
    }

    /// tools/test-access-auth.py --ios supplies disposable SSH servers.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["VNC_BEACH_TEST_INTERACTIVE_ADDRESS"] != nil))
    func sshTrustAndMultipleChallengesReachVNC() async throws {
        let address = try #require(ProcessInfo.processInfo.environment["VNC_BEACH_TEST_INTERACTIVE_ADDRESS"])
        let fixture = try await RFBFixture.start()
        defer { fixture.stop() }
        let session = RemoteSession()
        defer { session.close() }
        let operation = Task {
            await session.connect(address: fixture.address, password: "", depth: .full,
                access: AccessSettings(ssh: SSHSettings(address: address, user: "fixture")), action: .signIn)
        }
        var challenges: [String] = []
        let until = Date().addingTimeInterval(15)
        while session.state == .idle || session.state == .connecting {
            guard Date() < until else {
                session.close()
                Issue.record("SSH fixture timed out")
                break
            }
            if let prompt = session.prompt {
                switch prompt.question {
                case .trust(let host, let fingerprint):
                    #expect(host == address)
                    #expect(fingerprint.hasPrefix("SHA256:"))
                    session.answer(prompt)
                case .secret(_, let label, let echo):
                    #expect(!echo)
                    challenges.append(label)
                    session.answer(prompt, value: label.contains("Code") ? "123456" : "fixture-password")
                }
            }
            try await Task.sleep(for: .milliseconds(20))
        }
        await operation.value
        #expect(challenges.count == 2)
        #expect(session.state == .connected)
        #expect(session.name == "iOS RFB fixture")
    }
}

/// Loopback RFB 3.8 server. No screen capture or OS input is involved.
nonisolated private final class RFBFixture: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "vnc-beach.rfb-fixture")
    private let lock = NSLock()
    private var connections: [NWConnection] = []
    let stall: Bool

    var address: String { "127.0.0.1::\(listener.port!.rawValue)" }

    private init(stall: Bool) throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
        self.stall = stall
    }

    static func start(stall: Bool = false) async throws -> RFBFixture {
        let fixture = try RFBFixture(stall: stall)
        return try await withCheckedThrowingContinuation { continuation in
            fixture.listener.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    fixture.listener.stateUpdateHandler = nil
                    continuation.resume(returning: fixture)
                case .failed(let error):
                    fixture.listener.stateUpdateHandler = nil
                    continuation.resume(throwing: error)
                default: break
                }
            }
            fixture.listener.newConnectionHandler = { connection in
                fixture.lock.withLock { fixture.connections.append(connection) }
                connection.start(queue: fixture.queue)
                if !fixture.stall { fixture.handshake(connection) }
            }
            fixture.listener.start(queue: fixture.queue)
        }
    }

    func stop() {
        listener.cancel()
        lock.withLock { connections.forEach { $0.cancel() }; connections = [] }
    }

    private func read(_ connection: NWConnection, count: Int, next: @escaping @Sendable () -> Void) {
        connection.receive(minimumIncompleteLength: count, maximumLength: count) { data, _, _, error in
            guard error == nil, let data, !data.isEmpty else { return }
            if data.count < count { self.read(connection, count: count - data.count, next: next) }
            else { next() }
        }
    }

    private func handshake(_ connection: NWConnection) {
        connection.send(content: Data("RFB 003.008\n".utf8), completion: .contentProcessed { _ in })
        read(connection, count: 12) {
            connection.send(content: Data([1, 1]), completion: .contentProcessed { _ in })
            self.read(connection, count: 1) {
                connection.send(content: Data([0, 0, 0, 0]), completion: .contentProcessed { _ in })
                self.read(connection, count: 1) {
                    var initData = Data([0, 2, 0, 2, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0])
                    let name = Data("iOS RFB fixture".utf8)
                    initData.append(contentsOf: [0, 0, 0, UInt8(name.count)])
                    initData.append(name)
                    connection.send(content: initData, completion: .contentProcessed { _ in })
                }
            }
        }
    }
}
