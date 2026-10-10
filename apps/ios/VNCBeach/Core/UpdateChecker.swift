import Foundation
import Observation

/// `major.minor.patch`, as in release tags (`v0.2.0`) and `CFBundleShortVersionString`.
nonisolated struct SemanticVersion: Comparable, CustomStringConvertible, Sendable {
    let major: Int
    let minor: Int
    let patch: Int

    init?(_ text: String) {
        var s = text.trimmingCharacters(in: .whitespaces)
        if s.hasPrefix("v") || s.hasPrefix("V") {
            s.removeFirst()
        }
        let parts = s.split(separator: ".", omittingEmptySubsequences: false).map { Int($0) }
        guard (1...3).contains(parts.count), parts.allSatisfy({ $0 != nil }) else { return nil }
        major = parts[0]!
        minor = parts.count > 1 ? parts[1]! : 0
        patch = parts.count > 2 ? parts[2]! : 0
    }

    static func < (a: SemanticVersion, b: SemanticVersion) -> Bool {
        (a.major, a.minor, a.patch) < (b.major, b.minor, b.patch)
    }

    var description: String { "\(major).\(minor).\(patch)" }
}

/// Asks GitHub for the latest release. The repository is private for now, so
/// the API answers 404; that and network trouble count as "no information",
/// never as an error.
@Observable
@MainActor
final class UpdateChecker {
    nonisolated static let repository = "realS3BI/vnc-beach"
    static let testFlightURL = URL(string: "itms-beta://")!
    static let testFlightFallbackURL = URL(string: "https://testflight.apple.com")!
    private static let lastCheckKey = "lastUpdateCheck"
    private static let dismissedKey = "dismissedUpdateVersion"
    private static let interval: TimeInterval = 6 * 60 * 60

    struct Release: Equatable, Sendable {
        let version: SemanticVersion
        let url: URL
    }

    enum Status: Equatable {
        case idle
        case checking
        case upToDate
        case available(Release)
        /// 404, offline, unparsable: nothing to say.
        case unavailable
    }

    private(set) var status: Status = .idle
    let currentVersion: SemanticVersion?
    private var dismissedVersion: String?

    init(currentVersion: String? = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) {
        self.currentVersion = currentVersion.flatMap(SemanticVersion.init)
        dismissedVersion = UserDefaults.standard.string(forKey: Self.dismissedKey)
    }

    /// The release to announce in the server list, unless dismissed.
    var banner: Release? {
        guard case .available(let release) = status, dismissedVersion != release.version.description else {
            return nil
        }
        return release
    }

    func dismissBanner() {
        guard case .available(let release) = status else { return }
        dismissedVersion = release.version.description
        UserDefaults.standard.set(dismissedVersion, forKey: Self.dismissedKey)
    }

    /// At app start: at most once every six hours.
    func checkIfDue() async {
        let last = UserDefaults.standard.double(forKey: Self.lastCheckKey)
        guard Date().timeIntervalSince1970 - last >= Self.interval else { return }
        await check()
    }

    func check() async {
        guard status != .checking else { return }
        status = .checking
        UserDefaults.standard.set(Date().timeIntervalSince1970, forKey: Self.lastCheckKey)
        guard let latest = await Self.fetchLatest() else {
            status = .unavailable
            return
        }
        if let current = currentVersion, latest.version > current {
            status = .available(latest)
        } else {
            status = .upToDate
        }
    }

    private nonisolated static func fetchLatest() async -> Release? {
        guard let url = URL(string: "https://api.github.com/repos/\(repository)/releases/latest") else {
            return nil
        }
        var request = URLRequest(url: url, timeoutInterval: 15)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue("VNC-Beach", forHTTPHeaderField: "User-Agent")
        guard let (data, response) = try? await URLSession.shared.data(for: request),
            (response as? HTTPURLResponse)?.statusCode == 200
        else { return nil }
        return parse(data)
    }

    nonisolated static func parse(_ data: Data) -> Release? {
        struct Payload: Decodable {
            let tag_name: String
            let html_url: String
        }
        guard let payload = try? JSONDecoder().decode(Payload.self, from: data),
            let version = SemanticVersion(payload.tag_name), let url = URL(string: payload.html_url)
        else { return nil }
        return Release(version: version, url: url)
    }
}
