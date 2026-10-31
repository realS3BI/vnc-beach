import Foundation
import Observation

/// The VNC Beach account: the coordination server, the session token
/// (Keychain) and the devices of the account and its organizations. The
/// last device list is kept in Application Support, so the list shows up
/// at once and while the server can't be reached.
@Observable
@MainActor
final class AccountStore {
    enum Status: Equatable {
        case idle
        case loading
        case failed(String)
    }

    /// The coordination server, e.g. `https://beach.example.com`. Kept after
    /// signing out, so the next sign-in starts there.
    private(set) var url: String
    private(set) var user: AccountUser?
    private(set) var orgs: [AccountOrg] = []
    private(set) var devices: [AccountDevice] = []
    private(set) var status = Status.idle
    private(set) var signingIn = false
    /// Set when the server rejected the session.
    private(set) var expired = false
    private(set) var requestingUpdates = false
    /// The favourites kept in step with the account.
    weak var favoriteStore: ServerStore?
    private var syncing = false

    private var token: String?
    private let file: URL
    private let keychain: Keychain
    private let defaults: UserDefaults
    /// Stable favourite IDs for devices during this run (windows refer to them).
    private var serverIDs: [String: UUID] = [:]

    private static let urlKey = "account.url"
    private static let tokenAccount = "session"

    init(directory: URL? = nil, keychainService: String = "at.schlossers.vncbeach.account",
         defaults: UserDefaults = .standard) {
        let base =
            directory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("VNC Beach", isDirectory: true)
        file = base.appendingPathComponent("account.json")
        keychain = Keychain(service: keychainService)
        self.defaults = defaults
        url = defaults.string(forKey: Self.urlKey) ?? ""
        token = keychain.read(account: Self.tokenAccount)
        if token != nil { load() }
    }

    var signedIn: Bool { token != nil && !url.isEmpty }

    /// Devices grouped for the list: personal first, then each organization.
    var groups: [(title: String, devices: [AccountDevice])] {
        let personal = devices.filter { $0.org == nil }
        var result: [(String, [AccountDevice])] = personal.isEmpty ? [] : [("Persönlich", personal)]
        var seen: [String] = []
        for device in devices {
            if let org = device.org, !seen.contains(org.id) { seen.append(org.id) }
        }
        for id in seen {
            let members = devices.filter { $0.org?.id == id }
            result.append((members[0].org!.name, members))
        }
        return result
    }

    func signIn(address: String) async {
        let url: String
        do { url = try AccountAPI.normalize(address) } catch {
            status = .failed(error.localizedDescription)
            return
        }
        signingIn = true
        status = .idle
        defer { signingIn = false }
        do {
            let answer = try await AccountLogin().signIn(url: url)
            self.url = url
            defaults.set(url, forKey: Self.urlKey)
            token = answer.token
            keychain.write(answer.token, account: Self.tokenAccount)
            user = answer.user
            expired = false
            devices = []
            await refresh()
        } catch {
            status = .failed(error.localizedDescription)
        }
    }

    /// Sends favourites that are newer here and takes over those that are
    /// newer on the server (`FavoriteSync`). Passwords stay on the device.
    func syncFavorites() async {
        guard let token, !url.isEmpty, let store = favoriteStore, !syncing else { return }
        syncing = true
        defer { syncing = false }
        do {
            guard let remote = try await AccountAPI.favorites(url: url, token: token) else { return }
            let merged = FavoriteSync.merge(local: store.favorites, remote: remote)
            for favorite in merged.push {
                try await AccountAPI.putFavorite(url: url, token: token, favorite: favorite)
            }
            store.apply(merged.apply)
        } catch is CancellationError {
        } catch {
            NSLog("favorites sync: \(error.localizedDescription)")
        }
    }

    /// Whether this account may ask `device` to update: personal devices,
    /// or organizations where the user is admin or owner.
    func mayUpdate(_ device: AccountDevice) -> Bool {
        guard device.canUpdate, !device.update.pending else { return false }
        guard let org = device.org else { return true }
        return orgs.contains { $0.id == org.id && ($0.role == "admin" || $0.role == "owner") }
    }

    /// Outdated devices "Alle aktualisieren" applies to.
    var outdated: [AccountDevice] { devices.filter { $0.outdated && mayUpdate($0) } }

    /// Asks the devices to update themselves to the latest release.
    func requestUpdates(_ targets: [AccountDevice]) async {
        guard let token, !targets.isEmpty, !requestingUpdates else { return }
        requestingUpdates = true
        defer { requestingUpdates = false }
        // Shown right away; the next refresh brings the server's view.
        for i in devices.indices where targets.contains(where: { $0.id == devices[i].id }) {
            devices[i].update = .requested
        }
        var failure: String?
        for device in targets {
            do { try await AccountAPI.requestUpdate(url: url, token: token, device: device.id) } catch {
                failure = error.localizedDescription
            }
        }
        await refresh()
        if let failure { status = .failed(failure) }
    }

    /// Adds the computer `session` is connected to: registers it here and
    /// hands it the registration. Returns an error in words, nil on success.
    func enroll(_ session: RemoteSession, name: String, org: AccountOrg?) async -> String? {
        guard let token, let hello = session.enrollHello else { return "Nicht angemeldet." }
        let name = name.trimmingCharacters(in: .whitespaces)
        let registration: Registration
        do {
            registration = try await AccountAPI.register(
                url: url, token: token, name: name, platform: hello.platform, org: org?.id)
        } catch {
            if case AccountError.unauthorized = error {
                forget()
                expired = true
            }
            return error.localizedDescription
        }
        let enrollment = Enrollment(
            url: url, id: registration.id, token: registration.token,
            coordinatorKey: registration.coordinatorKey, name: name, owner: org?.name ?? "Persönlich")
        let json = String(decoding: (try? JSONEncoder().encode(enrollment)) ?? Data(), as: UTF8.self)
        if let error = await session.enroll(json) {
            // The computer didn't take it: don't leave a dead device behind.
            await AccountAPI.unregister(url: url, deviceToken: registration.token)
            return error
        }
        await refresh()
        return nil
    }

    func signOut() async {
        if let token { await AccountAPI.logout(url: url, token: token) }
        forget()
        expired = false
    }

    /// Reloads profile, organizations and devices.
    func refresh() async {
        guard let token, !url.isEmpty, status != .loading else { return }
        status = .loading
        do {
            async let me = AccountAPI.me(url: url, token: token)
            async let list = AccountAPI.devices(url: url, token: token)
            let (profile, devices) = try await (me, list)
            user = profile.user
            orgs = profile.orgs
            self.devices = devices.sorted { ($0.online ? 0 : 1, $0.name.lowercased()) < ($1.online ? 0 : 1, $1.name.lowercased()) }
            status = .idle
            persist()
            await syncFavorites()
        } catch AccountError.unauthorized {
            forget()
            expired = true
            status = .failed(AccountError.unauthorized.localizedDescription)
        } catch is CancellationError {
            status = .idle
        } catch {
            status = .failed(error.localizedDescription)
        }
    }

    /// A connectable favourite for `device`: the first address, the others
    /// as alternatives. Its `device` makes the session ask for a ticket.
    func server(for device: AccountDevice) -> Server {
        let id = serverIDs[device.id] ?? UUID()
        serverIDs[device.id] = id
        var endpoints = device.endpoints
        let host = endpoints.isEmpty ? "" : endpoints.removeFirst()
        var access = AccessSettings()
        access.alternatives = endpoints
        return Server(id: id, name: device.name, host: host, access: access, device: device.id)
    }

    func server(id: Server.ID) -> Server? {
        guard let deviceID = serverIDs.first(where: { $0.value == id })?.key,
            let device = devices.first(where: { $0.id == deviceID })
        else { return nil }
        return server(for: device)
    }

    /// `{"url","token","device"}` for `beach_connection_start`, or nil if
    /// `server` isn't an account device or nobody is signed in.
    func ticketTarget(for server: Server) -> String? {
        guard let device = server.device, let token, !url.isEmpty else { return nil }
        let data = try? JSONEncoder().encode(["url": url, "token": token, "device": device])
        return data.map { String(decoding: $0, as: UTF8.self) }
    }

    private func forget() {
        token = nil
        keychain.delete(account: Self.tokenAccount)
        user = nil
        orgs = []
        devices = []
        try? FileManager.default.removeItem(at: file)
    }

    private struct Snapshot: Codable {
        var url: String
        var user: AccountUser?
        var orgs: [AccountOrg]
        var devices: [AccountDevice]
    }

    private func load() {
        guard let data = try? Data(contentsOf: file),
            let snapshot = try? JSONDecoder().decode(Snapshot.self, from: data), snapshot.url == url
        else { return }
        user = snapshot.user
        orgs = snapshot.orgs
        devices = snapshot.devices
    }

    private func persist() {
        do {
            try FileManager.default.createDirectory(
                at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            let snapshot = Snapshot(url: url, user: user, orgs: orgs, devices: devices)
            try JSONEncoder().encode(snapshot).write(to: file, options: [.atomic, .completeFileProtection])
        } catch {
            NSLog("saving account failed: \(error)")
        }
    }
}
