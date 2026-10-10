import Foundation
import Network
import Observation

struct DiscoveredService: Identifiable, Hashable {
    let name: String
    let endpoint: NWEndpoint
    var id: String { name }
}

/// Finds VNC servers announced via Bonjour (`_rfb._tcp`), e.g. macOS Screen
/// Sharing on the local network. Tailscale peers don't announce themselves;
/// their MagicDNS names are typed by hand.
@Observable
@MainActor
final class ServiceBrowser {
    private(set) var services: [DiscoveredService] = []
    private var browser: NWBrowser?

    func start() {
        guard browser == nil else { return }
        let browser = NWBrowser(for: .bonjour(type: "_rfb._tcp", domain: nil), using: .tcp)
        browser.browseResultsChangedHandler = { results, _ in
            let found = results.compactMap { result -> DiscoveredService? in
                guard case .service(let name, _, _, _) = result.endpoint else { return nil }
                return DiscoveredService(name: name, endpoint: result.endpoint)
            }
            .sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
            Task { @MainActor [weak self] in
                self?.services = found
            }
        }
        browser.start(queue: .global(qos: .utility))
        self.browser = browser
    }

    func stop() {
        browser?.cancel()
        browser = nil
        services = []
    }

    /// Resolves a Bonjour service to `host:port` by opening a connection.
    nonisolated static func resolve(_ endpoint: NWEndpoint) async -> String? {
        await withCheckedContinuation { continuation in
            let connection = NWConnection(to: endpoint, using: .tcp)
            let done = Resolved()
            connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    var address: String?
                    if case .hostPort(let host, let port)? = connection.currentPath?.remoteEndpoint {
                        address = "\(Self.hostString(host)):\(port.rawValue)"
                    }
                    connection.cancel()
                    done.finish { continuation.resume(returning: address) }
                case .failed, .cancelled:
                    done.finish { continuation.resume(returning: nil) }
                default:
                    break
                }
            }
            connection.start(queue: .global(qos: .utility))
        }
    }

    private nonisolated static func hostString(_ host: NWEndpoint.Host) -> String {
        switch host {
        case .ipv4(let a): "\(a)"
        case .ipv6(let a): "[\(a)]"
        case .name(let n, _): n
        @unknown default: "\(host)"
        }
    }
}

/// Makes sure a continuation resumes once.
nonisolated private final class Resolved: @unchecked Sendable {
    private let lock = NSLock()
    private var finished = false

    nonisolated func finish(_ body: () -> Void) {
        lock.lock()
        defer { lock.unlock() }
        guard !finished else { return }
        finished = true
        body()
    }
}
