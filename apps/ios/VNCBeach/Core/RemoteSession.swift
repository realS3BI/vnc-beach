import BeachMobile
import Foundation
import Observation

/// Pixel rectangle in framebuffer coordinates.
struct DirtyRect {
    var x: Int
    var y: Int
    var width: Int
    var height: Int
}

/// Owns one RFB connection from `crates/beach-mobile` and republishes its
/// events on the main actor.
@Observable
@MainActor
final class RemoteSession {
    enum State: Equatable {
        case idle
        case connecting
        case connected
        case closed(String)
    }

    private(set) var state: State = .idle
    private(set) var name = ""
    private(set) var remote = RemoteKind.other
    private(set) var size = CGSize.zero
    /// Set by the view: new pixels are waiting.
    var onFrame: (() -> Void)?
    var onClipboard: ((String) -> Void)?
    /// Set when the user chose "Trennen"; suppresses automatic reconnects.
    private(set) var closedByUser = false

    private var handle: SessionHandle?
    private var callbackBox: CallbackBox?

    func connect(address: String, password: String, depth: ColorDepth) async {
        guard handle == nil else { return }
        state = .connecting
        closedByUser = false
        let box = CallbackBox()
        box.session = self
        callbackBox = box
        let context = SendablePointer(raw: Unmanaged.passRetained(box).toOpaque())
        let depthBits = UInt8(depth.rawValue)
        let result: ConnectOutcome = await Task.detached {
            var error = [CChar](repeating: 0, count: 512)
            let pointer = beach_session_connect(
                address, password.isEmpty ? nil : password, depthBits, sessionEventCallback,
                context.raw, &error, error.count)
            if let pointer {
                return .connected(SessionHandle(pointer))
            }
            return .failed(cString(error))
        }.value
        switch result {
        case .connected(let handle):
            // `connect` may have been cancelled meanwhile (view went away).
            guard callbackBox === box else {
                beach_session_free(handle.pointer)
                Unmanaged<CallbackBox>.fromOpaque(context.raw).release()
                return
            }
            self.handle = handle
            handle.context = context.raw
            var buffer = [CChar](repeating: 0, count: 256)
            beach_session_name(handle.pointer, &buffer, buffer.count)
            name = cString(buffer)
            remote = RemoteKind(rawValue: Int(beach_session_remote_kind(handle.pointer))) ?? .other
            refreshSize()
            state = .connected
        case .failed(let message):
            Unmanaged<CallbackBox>.fromOpaque(context.raw).release()
            callbackBox = nil
            state = .closed(Self.describe(message))
        }
    }

    /// Closes the connection; safe to call repeatedly.
    func close(byUser: Bool = false) {
        if byUser {
            closedByUser = true
        }
        callbackBox = nil
        guard let handle else { return }
        self.handle = nil
        // No callback runs after free returns, so the box can go now.
        beach_session_free(handle.pointer)
        if let context = handle.context {
            Unmanaged<CallbackBox>.fromOpaque(context).release()
        }
        if state == .connected || state == .connecting {
            state = .closed(byUser ? "" : "Verbindung getrennt")
        }
    }

    /// Hands the framebuffer to `body` if pixels changed since the last call.
    func takeFrame(_ body: (UnsafePointer<UInt8>, Int, Int, DirtyRect) -> Void) -> Bool {
        guard let handle else { return false }
        return withoutActuallyEscaping(body) { escapable in
            let box = FrameBox(body: escapable)
            return withExtendedLifetime(box) {
                beach_session_take_frame(
                    handle.pointer, frameCallback, Unmanaged.passUnretained(box).toOpaque())
            }
        }
    }

    func sendPointer(buttons: UInt8, x: Int, y: Int) {
        guard let handle else { return }
        beach_session_pointer(handle.pointer, buttons, clampU16(x), clampU16(y))
    }

    func sendWheel(buttons: UInt8, wheelBit: UInt8, x: Int, y: Int) {
        guard let handle else { return }
        beach_session_wheel(handle.pointer, buttons, wheelBit, clampU16(x), clampU16(y))
    }

    func sendKey(_ keysym: UInt32, down: Bool) {
        guard let handle else { return }
        beach_session_key(handle.pointer, keysym, down)
    }

    func tapKey(_ keysym: UInt32) {
        sendKey(keysym, down: true)
        sendKey(keysym, down: false)
    }

    func sendClipboard(_ text: String) {
        guard let handle else { return }
        beach_session_cut_text(handle.pointer, text)
    }

    func refresh() {
        guard let handle else { return }
        beach_session_refresh(handle.pointer)
    }

    fileprivate func handle(kind: Int32, text: String?) {
        switch Int(kind) {
        case BEACH_EVENT_FRAME:
            onFrame?()
        case BEACH_EVENT_RESIZED:
            refreshSize()
        case BEACH_EVENT_CLIPBOARD:
            if let text {
                onClipboard?(text)
            }
        case BEACH_EVENT_CLOSED:
            let reason = text ?? ""
            close()
            state = .closed(Self.describe(reason))
        default:
            break
        }
    }

    private func refreshSize() {
        guard let handle else { return }
        var w: UInt32 = 0
        var h: UInt32 = 0
        beach_session_size(handle.pointer, &w, &h)
        size = CGSize(width: Int(w), height: Int(h))
    }

    private func clampU16(_ v: Int) -> UInt16 {
        UInt16(clamping: max(0, v))
    }

    /// German wording for the most common failures; the rest stays as the
    /// library reports it.
    static func describe(_ reason: String) -> String {
        let lower = reason.lowercased()
        if lower.contains("authentication failed") || lower.contains("auth") && lower.contains("fail") {
            return "Passwort falsch"
        }
        if lower.contains("requires a password") {
            return "Der Server verlangt ein Passwort"
        }
        if lower.contains("connection refused") {
            return "Verbindung abgelehnt: Läuft der Server und stimmt der Port?"
        }
        if lower.contains("timed out") {
            return "Zeitüberschreitung: Rechner nicht erreichbar (VPN aktiv?)"
        }
        if lower.contains("could not resolve") || lower.contains("nodename nor servname") {
            return "Name nicht auflösbar: Rechnername oder Adresse prüfen"
        }
        if lower.contains("server closed the connection") || lower.contains("connection closed") {
            return "Der Server hat die Verbindung beendet"
        }
        if lower.contains("broken pipe") || lower.contains("connection reset") {
            return "Verbindung unterbrochen"
        }
        return reason
    }
}

/// Mirrors `RemoteKind` in crates/beach-mobile.
enum RemoteKind: Int {
    case other = 0
    case beachMac = 1
    case appleScreenSharing = 2

    var isMac: Bool { self != .other }
}

nonisolated private enum ConnectOutcome: Sendable {
    case connected(SessionHandle)
    case failed(String)
}

/// Reads a NUL-terminated buffer.
nonisolated private func cString(_ buffer: [CChar]) -> String {
    buffer.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
}

/// Session pointer owned by `RemoteSession`.
nonisolated final class SessionHandle: @unchecked Sendable {
    let pointer: OpaquePointer
    var context: UnsafeMutableRawPointer?

    init(_ pointer: OpaquePointer) {
        self.pointer = pointer
    }
}

nonisolated struct SendablePointer: @unchecked Sendable {
    let raw: UnsafeMutableRawPointer
}

/// Retained by the C side as callback context; only weakly knows the session.
nonisolated final class CallbackBox: @unchecked Sendable {
    nonisolated(unsafe) weak var session: RemoteSession?
}

@MainActor
private final class FrameBox {
    let body: (UnsafePointer<UInt8>, Int, Int, DirtyRect) -> Void

    init(body: @escaping (UnsafePointer<UInt8>, Int, Int, DirtyRect) -> Void) {
        self.body = body
    }
}

// Runs on the reader thread.
nonisolated private func sessionEventCallback(
    context: UnsafeMutableRawPointer?, kind: Int32, text: UnsafePointer<CChar>?
) {
    guard let context else { return }
    let box = Unmanaged<CallbackBox>.fromOpaque(context).takeUnretainedValue()
    let message = text.map { String(cString: $0) }
    Task { @MainActor in
        box.session?.handle(kind: kind, text: message)
    }
}

// Runs synchronously inside `takeFrame`, i.e. on the main actor.
nonisolated private func frameCallback(
    context: UnsafeMutableRawPointer?, rgba: UnsafePointer<UInt8>?, width: UInt32, height: UInt32,
    dirtyX: UInt32, dirtyY: UInt32, dirtyWidth: UInt32, dirtyHeight: UInt32
) {
    guard let context, let rgba else { return }
    // Pointers aren't Sendable; pass them as integers into the isolated closure.
    let contextBits = UInt(bitPattern: context)
    let pixelBits = UInt(bitPattern: rgba)
    MainActor.assumeIsolated {
        let box = Unmanaged<FrameBox>.fromOpaque(UnsafeMutableRawPointer(bitPattern: contextBits)!)
            .takeUnretainedValue()
        box.body(
            UnsafePointer<UInt8>(bitPattern: pixelBits)!, Int(width), Int(height),
            DirtyRect(x: Int(dirtyX), y: Int(dirtyY), width: Int(dirtyWidth), height: Int(dirtyHeight)))
    }
}

extension RemoteSession {
    /// Normalises user input like `host:1` to `host:port`.
    nonisolated static func normalize(address: String) -> String {
        var buffer = [CChar](repeating: 0, count: 512)
        beach_normalize_address(address, &buffer, buffer.count)
        return cString(buffer)
    }
}
