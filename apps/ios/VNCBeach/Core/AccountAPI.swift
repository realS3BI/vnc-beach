import AuthenticationServices
import BeachMobile
import CryptoKit
import Foundation
import UIKit

/// JSON bodies of the coordination server's API (`crates/beach-account/src/api.rs`).
struct AccountUser: Codable, Hashable {
    var id: String
    var name: String
    /// GitHub login or e-mail address.
    var login: String
}

struct AccountOrg: Codable, Hashable, Identifiable {
    var id: String
    var name: String
    var role: String
}

struct AccountMe: Codable {
    var user: AccountUser
    var orgs: [AccountOrg]
}

struct AccountDevice: Codable, Hashable, Identifiable {
    struct OrgRef: Codable, Hashable {
        var id: String
        var name: String
    }

    var id: String
    var name: String
    /// `macos` or `windows`.
    var platform: String
    /// nil: a personal device of the signed-in user.
    var org: OrgRef?
    /// Most useful first (Tailscale, then LAN), without port.
    var addresses: [String]
    var port: Int
    var online: Bool
    /// Unix seconds of the last heartbeat.
    var lastSeen: Int64?
    var version: String
    /// The device updates itself on request.
    var canUpdate = false
    /// A newer release exists than the device's version.
    var outdated = false
    var update = UpdateState.idle

    enum CodingKeys: String, CodingKey {
        case id, name, platform, org, addresses, port, online, version, outdated, update
        case lastSeen = "last_seen"
        case canUpdate = "can_update"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        platform = try c.decode(String.self, forKey: .platform)
        org = try c.decodeIfPresent(OrgRef.self, forKey: .org)
        addresses = try c.decode([String].self, forKey: .addresses)
        port = try c.decode(Int.self, forKey: .port)
        online = try c.decode(Bool.self, forKey: .online)
        lastSeen = try c.decodeIfPresent(Int64.self, forKey: .lastSeen)
        version = try c.decode(String.self, forKey: .version)
        canUpdate = try c.decodeIfPresent(Bool.self, forKey: .canUpdate) ?? false
        outdated = try c.decodeIfPresent(Bool.self, forKey: .outdated) ?? false
        update = (try? c.decodeIfPresent(UpdateState.self, forKey: .update)) ?? .idle
    }

    /// The tag under the status line, like `account_ui::update_tag`.
    var updateTag: String? {
        switch update {
        case .requested, .running: "Wird aktualisiert …"
        case .failed: ComputerCard.failedTag
        case .idle: outdated ? "Update verfügbar" : nil
        }
    }

    /// `host:port` for every reported address.
    var endpoints: [String] {
        addresses.map { $0.contains(":") ? "[\($0)]:\(port)" : "\($0):\(port)" }
    }

    /// Online with the first address, or when the device was last seen.
    func status(now: Date = .now) -> String {
        if online {
            return addresses.first.map { "online · \($0)" } ?? "online"
        }
        guard let lastSeen else { return "noch nie online" }
        let seconds = max(0, Int64(now.timeIntervalSince1970) - lastSeen)
        switch seconds {
        case ..<3600: return "offline · vor \(max(1, seconds / 60)) Min."
        case ..<86_400: return "offline · vor \(seconds / 3600) Std."
        default: return "offline · vor \(seconds / 86_400) Tagen"
        }
    }
}

/// Where a requested remote update stands (`beach_account::api::UpdateState`).
enum UpdateState: Codable, Hashable {
    case idle
    /// Waiting for the device's next heartbeat.
    case requested
    case running
    case failed(String)

    var pending: Bool { self == .requested || self == .running }

    private enum CodingKeys: String, CodingKey { case state, since, error }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .state) {
        case "requested": self = .requested
        case "running": self = .running
        case "failed": self = .failed(try c.decodeIfPresent(String.self, forKey: .error) ?? "")
        default: self = .idle
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .idle: try c.encode("idle", forKey: .state)
        case .requested:
            try c.encode("requested", forKey: .state)
            try c.encode(0, forKey: .since)
        case .running:
            try c.encode("running", forKey: .state)
            try c.encode(0, forKey: .since)
        case .failed(let error):
            try c.encode("failed", forKey: .state)
            try c.encode(error, forKey: .error)
        }
    }
}

/// A computer's answer to `BEACH_ENROLL` (`beach_account::api::EnrollHello`).
struct EnrollHello: Codable, Equatable {
    struct Device: Codable, Equatable {
        var url: String
        var id: String
        var name: String
    }

    /// Host name, the default device name.
    var name: String
    /// `macos` or `windows`.
    var platform: String
    /// The current registration, if any.
    var device: Device?
    /// Added from this app during the session.
    var joined = false

    enum CodingKeys: String, CodingKey { case name, platform, device }
}

/// The registration handed to the computer (`beach_account::api::Enrollment`).
struct Enrollment: Codable {
    var url: String
    var id: String
    var token: String
    var coordinatorKey: String
    var name: String
    var owner: String

    enum CodingKeys: String, CodingKey {
        case url, id, token, name, owner
        case coordinatorKey = "coordinator_key"
    }
}

struct EnrollResult: Codable {
    var error: String?
}

/// `POST /api/devices` answer.
struct Registration: Codable {
    var id: String
    var token: String
    var coordinatorKey: String

    enum CodingKeys: String, CodingKey {
        case id, token
        case coordinatorKey = "coordinator_key"
    }
}

private struct TokenResponse: Codable {
    var token: String
    var user: AccountUser
}

private struct APIError: Codable {
    var error: String
}

enum AccountError: LocalizedError, Equatable {
    /// The session token is no longer valid.
    case unauthorized
    /// The server doesn't know the path (an older coordination server).
    case notFound(String)
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .unauthorized: "Die Anmeldung ist abgelaufen. Bitte neu anmelden."
        case .notFound(let message), .failed(let message): message
        }
    }
}

/// HTTP calls to the coordination server.
enum AccountAPI {
    /// Accepts `beach.example.com` and trailing slashes; HTTPS unless the
    /// address says otherwise (plain HTTP only makes sense for testing).
    static func normalize(_ input: String) throws -> String {
        var url = input.trimmingCharacters(in: .whitespacesAndNewlines)
        while url.hasSuffix("/") { url.removeLast() }
        guard !url.isEmpty else {
            throw AccountError.failed("Bitte die Adresse des Koordinationsservers eingeben.")
        }
        if !url.contains("://") { url = "https://" + url }
        guard url.hasPrefix("https://") || url.hasPrefix("http://"),
            !url.contains(where: \.isWhitespace), URL(string: url)?.host != nil
        else {
            throw AccountError.failed("Die Adresse muss mit https:// beginnen.")
        }
        return url
    }

    static func me(url: String, token: String) async throws -> AccountMe {
        try await call("GET", url + "/api/me", token: token)
    }

    static func devices(url: String, token: String) async throws -> [AccountDevice] {
        try await call("GET", url + "/api/devices", token: token)
    }

    /// The user's favourites, including tombstones; nil if the server
    /// doesn't keep favourites (an older version).
    static func favorites(url: String, token: String) async throws -> [Favorite]? {
        do {
            return try await call("GET", url + "/api/favorites", token: token)
        } catch AccountError.notFound {
            return nil
        }
    }

    static func putFavorite(url: String, token: String, favorite: Favorite) async throws {
        let body = try JSONEncoder().encode(favorite)
        _ = try await send("PUT", url + "/api/favorites/\(favorite.id)", token: token, body: body)
    }

    /// Registers a computer; its device token goes to the computer.
    static func register(url: String, token: String, name: String, platform: String, org: String?) async throws
        -> Registration
    {
        struct Body: Encodable {
            var name: String
            var platform: String
            var org: String?
        }
        let body = try JSONEncoder().encode(Body(name: name, platform: platform, org: org))
        return try await call("POST", url + "/api/devices", token: token, body: body)
    }

    /// Removes a device with its own device token.
    static func unregister(url: String, deviceToken: String) async {
        _ = try? await send("DELETE", url + "/api/device", token: deviceToken, body: nil)
    }

    /// Asks the device to update itself to the latest release.
    static func requestUpdate(url: String, token: String, device: String) async throws {
        _ = try await send("POST", url + "/api/devices/\(device)/update", token: token, body: nil)
    }

    static func logout(url: String, token: String) async {
        _ = try? await send("POST", url + "/api/logout", token: token, body: nil)
    }

    fileprivate static func token(url: String, code: String, verifier: String) async throws -> TokenResponse {
        let body = try JSONEncoder().encode(["code": code, "verifier": verifier])
        return try await call("POST", url + "/api/token", token: nil, body: body)
    }

    private static func call<T: Decodable>(
        _ method: String, _ url: String, token: String?, body: Data? = nil
    ) async throws -> T {
        let data = try await send(method, url, token: token, body: body)
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw AccountError.failed("Unerwartete Antwort des Koordinationsservers.")
        }
    }

    private static func send(_ method: String, _ url: String, token: String?, body: Data?) async throws -> Data {
        guard let target = URL(string: url) else { throw AccountError.failed("Ungültige Adresse: \(url)") }
        let request = makeRequest(method, target, token: token, body: body)
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch let error as URLError where error.code == .cancelled {
            throw CancellationError()
        } catch {
            throw AccountError.failed("Koordinationsserver nicht erreichbar: \(error.localizedDescription)")
        }
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        switch status {
        case 200..<300: return data
        case 401: throw AccountError.unauthorized
        default:
            let reason = (try? JSONDecoder().decode(APIError.self, from: data))?.error ?? "HTTP \(status)"
            if status == 404 { throw AccountError.notFound("Koordinationsserver: \(reason)") }
            throw AccountError.failed("Koordinationsserver: \(reason)")
        }
    }

    nonisolated fileprivate static func makeRequest(
        _ method: String, _ url: URL, token: String?, body: Data?
    ) -> URLRequest {
        var request = URLRequest(url: url, timeoutInterval: 30)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("VNC-Beach", forHTTPHeaderField: "User-Agent")
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        return request
    }
}

// MARK: - Tickets

/// Holds a URLSession answer across the semaphore wait.
nonisolated private final class PostResult: @unchecked Sendable {
    var status: Int32 = 0
    var text = ""
}

/// `BeachHttpPost` for `beach_connection_start`: runs on the Rust worker
/// thread during the RFB handshake and blocks until the answer is there.
nonisolated func accountHTTPPost(
    url: UnsafePointer<CChar>?, token: UnsafePointer<CChar>?, body: UnsafePointer<CChar>?,
    answer: UnsafeMutablePointer<CChar>?, answerLength: Int
) -> Int32 {
    guard let url, let token, let body, let answer, answerLength > 0,
        let target = URL(string: String(cString: url))
    else { return 0 }
    let request = AccountAPI.makeRequest(
        "POST", target, token: String(cString: token), body: Data(String(cString: body).utf8))
    let result = PostResult()
    let done = DispatchSemaphore(value: 0)
    URLSession.shared.dataTask(with: request) { data, response, error in
        if let response = response as? HTTPURLResponse {
            result.status = Int32(response.statusCode)
            result.text = String(decoding: data ?? Data(), as: UTF8.self)
        } else {
            result.text = error?.localizedDescription ?? "keine Antwort"
        }
        done.signal()
    }.resume()
    done.wait()
    let bytes = Array(result.text.utf8.prefix(answerLength - 1))
    bytes.withUnsafeBufferPointer { buffer in
        buffer.withMemoryRebound(to: CChar.self) { chars in
            if let base = chars.baseAddress { answer.update(from: base, count: bytes.count) }
        }
    }
    answer[bytes.count] = 0
    return result.status
}

// MARK: - Sign-in

/// Signs in through the browser like the desktop viewer: the coordination
/// server sends the browser to `http://127.0.0.1:PORT/callback?code=…`,
/// answered by a loopback listener in this app, and the code plus the
/// verifier behind the challenge are traded for a session.
@MainActor
final class AccountLogin: NSObject, ASWebAuthenticationPresentationContextProviding {
    private var session: ASWebAuthenticationSession?

    func signIn(url: String) async throws -> (token: String, user: AccountUser) {
        let verifier = Self.randomVerifier()
        let challenge = Self.base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
        let login = try LoopbackLogin()
        guard let page = URL(string: "\(url)/login?app_port=\(login.port)&challenge=\(challenge)") else {
            throw AccountError.failed("Ungültige Adresse: \(url)")
        }
        let code: String = try await withCheckedThrowingContinuation { continuation in
            let resume = OnceContinuation(continuation)
            let waiter = Task.detached {
                while !Task.isCancelled {
                    guard let code = login.waitForCode(milliseconds: 250) else { continue }
                    await MainActor.run {
                        resume.resume(.success(code))
                        self.session?.cancel()
                    }
                    return
                }
            }
            // The callback scheme is never used: the code arrives on the loopback listener.
            let session = ASWebAuthenticationSession(url: page, callbackURLScheme: "vncbeach") { _, _ in
                waiter.cancel()
                Task { @MainActor in
                    resume.resume(.failure(AccountError.failed("Anmeldung abgebrochen.")))
                }
            }
            session.presentationContextProvider = self
            self.session = session
            if !session.start() {
                resume.resume(.failure(AccountError.failed("Die Anmeldeseite konnte nicht geöffnet werden.")))
            }
        }
        session = nil
        let answer = try await AccountAPI.token(url: url, code: code, verifier: verifier)
        return (answer.token, answer.user)
    }

    nonisolated func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            let active = scenes.first { $0.activationState == .foregroundActive } ?? scenes.first
            return active?.keyWindow ?? ASPresentationAnchor()
        }
    }

    private static func randomVerifier() -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        return base64URL(Data(bytes))
    }

    /// Unpadded URL-safe base64, as `beach_account::b64`.
    static func base64URL(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

/// Resumes a continuation at most once.
@MainActor
private final class OnceContinuation<T: Sendable> {
    private var continuation: CheckedContinuation<T, Error>?

    init(_ continuation: CheckedContinuation<T, Error>) {
        self.continuation = continuation
    }

    func resume(_ result: Result<T, Error>) {
        continuation?.resume(with: result)
        continuation = nil
    }
}

/// The loopback listener from `crates/beach-mobile` (`beach_login_*`).
nonisolated final class LoopbackLogin: @unchecked Sendable {
    private let handle: OpaquePointer
    let port: UInt16

    init() throws {
        var port: UInt16 = 0
        guard let handle = beach_login_start(&port) else {
            throw AccountError.failed("Die Anmeldung konnte nicht vorbereitet werden.")
        }
        self.handle = handle
        self.port = port
    }

    deinit { beach_login_free(handle) }

    /// Answers the browser for up to `milliseconds` and returns the
    /// callback code, if it came. Blocking; one caller at a time.
    func waitForCode(milliseconds: UInt32) -> String? {
        guard let text = beach_login_wait(handle, milliseconds) else { return nil }
        defer { beach_string_free(text) }
        return String(cString: text)
    }
}
