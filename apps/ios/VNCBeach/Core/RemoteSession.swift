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

/// One of the server's displays inside the framebuffer.
struct RemoteScreen: Identifiable, Equatable {
    /// Position in the list; the server's own ids need not be unique.
    let id: Int
    let rect: CGRect

    var label: String { "Bildschirm \(id + 1) (\(Int(rect.width))×\(Int(rect.height)))" }
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
    /// The server's displays, if it announced them (ExtendedDesktopSize).
    private(set) var screens: [RemoteScreen] = []
    /// Index into `screens` of the display shown alone; nil shows all.
    private(set) var selectedScreen: Int?
    /// Set by the view: new pixels are waiting.
    var onFrame: (() -> Void)?
    var onClipboard: ((String) -> Void)?
    /// Set when the user chose "Trennen"; suppresses automatic reconnects.
    private(set) var closedByUser = false

    private var handle: SessionHandle?
    private var callbackBox: CallbackBox?
    private var attempt: ConnectionAttempt?
    private(set) var preparationStatus = "Verbindung wird geprüft …"
    private(set) var preparationAction = ConnectionAction.connect
    private(set) var prompt: AccessPrompt?

    func connect(address: String, password: String, depth: ColorDepth,
                 access: AccessSettings = AccessSettings(), action: ConnectionAction = .connect) async {
        close()
        state = .connecting
        closedByUser = false
        preparationAction = action
        preparationStatus = "Verbindung wird geprüft …"
        let prepared: PreparedAccess
        do { prepared = try PreparedAccess(access, action: action) }
        catch { state = .closed(error.localizedDescription); return }
        let box = CallbackBox()
        box.session = self
        callbackBox = box
        let context = Unmanaged.passRetained(box).toOpaque()
        let hosts = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("VNC Beach/ssh_hosts").path
        var error = [CChar](repeating: 0, count: 2048)
        guard let job = beach_connection_start(address, password.isEmpty ? nil : password,
            prepared.json, action.rawValue, hosts, UInt8(depth.rawValue), sessionEventCallback,
            context, &error, error.count) else {
            Unmanaged<CallbackBox>.fromOpaque(context).release()
            callbackBox = nil
            state = .closed(cString(error))
            return
        }
        let current = ConnectionAttempt(job: job, context: context, prepared: prepared)
        attempt = current
        while attempt === current, !Task.isCancelled {
            if let text = beach_connection_poll(job) {
                let data = Data(String(cString: text).utf8)
                beach_string_free(text)
                do {
                    switch try JSONDecoder().decode(PreparationEvent.self, from: data) {
                    case .progress(let text): preparationStatus = text
                    case .prompt(let question): prompt = question
                    case .failed(let message):
                        close()
                        state = .closed(message)
                        return
                    case .connected:
                        guard let pointer = beach_connection_take_session(job) else {
                            throw AccessError.message("Die VNC-Verbindung konnte nicht übernommen werden.")
                        }
                        let handle = SessionHandle(pointer)
                        handle.context = context
                        self.handle = handle
                        attempt = nil
                        beach_connection_free(job)
                        prompt = nil
                        var buffer = [CChar](repeating: 0, count: 256)
                        beach_session_name(pointer, &buffer, buffer.count)
                        name = cString(buffer)
                        remote = RemoteKind(rawValue: Int(beach_session_remote_kind(pointer))) ?? .other
                        refreshSize()
                        refreshScreens()
                        state = .connected
                        return
                    }
                } catch {
                    close()
                    state = .closed(error.localizedDescription)
                    return
                }
            } else {
                do { try await Task.sleep(for: .milliseconds(50)) }
                catch { break }
            }
        }
        // A replaced attempt belongs to its new caller; it must not be closed here.
        if attempt === current { close(byUser: true) }
    }

    func answer(_ question: AccessPrompt, value: String? = nil) {
        guard let attempt, prompt == question else { return }
        let kind: Int32
        switch question.question {
        case .trust: kind = 0
        case .secret: kind = 1
        }
        if beach_connection_answer(attempt.job, question.id, kind, value) {
            prompt = nil
        } else {
            close()
            state = .closed("Die SSH-Abfrage ist nicht mehr gültig. Bitte erneut verbinden.")
        }
    }

    /// Closes both preparation and established sessions; safe to repeat.
    func close(byUser: Bool = false) {
        if byUser { closedByUser = true }
        callbackBox = nil
        prompt = nil
        if let current = attempt {
            attempt = nil
            beach_connection_free(current.job)
            Unmanaged<CallbackBox>.fromOpaque(current.context).release()
        }
        if let handle {
            self.handle = nil
            beach_session_free(handle.pointer)
            if let context = handle.context { Unmanaged<CallbackBox>.fromOpaque(context).release() }
        }
        if state == .connected || state == .connecting {
            state = .closed(byUser ? "Verbindungsversuch abgebrochen" : "Verbindung getrennt")
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

    /// The part of the framebuffer on show: one display or all of it.
    var crop: CGRect {
        if let i = selectedScreen, i < screens.count {
            return screens[i].rect
        }
        return CGRect(origin: .zero, size: size)
    }

    /// Shows one display alone (nil: all of them) and asks the server for
    /// just that part from now on.
    func selectScreen(_ index: Int?) {
        guard let handle else { return }
        let index = index.flatMap { $0 < screens.count ? $0 : nil }
        selectedScreen = index
        if let index {
            let r = screens[index].rect
            beach_session_set_region(
                handle.pointer, UInt32(r.minX), UInt32(r.minY), UInt32(r.width), UInt32(r.height))
        } else {
            beach_session_set_region(handle.pointer, 0, 0, 0, 0)
        }
    }

    private func refreshScreens() {
        guard let handle else { return }
        let count = beach_session_screens(handle.pointer, nil, 0)
        var buffer = [BeachScreen](repeating: BeachScreen(), count: max(count, 1))
        let got = count == 0 ? 0 : beach_session_screens(handle.pointer, &buffer, buffer.count)
        let fresh = buffer.prefix(got).enumerated().map { i, s in
            RemoteScreen(
                id: i, rect: CGRect(x: Int(s.x), y: Int(s.y), width: Int(s.width), height: Int(s.height)))
        }
        guard fresh != screens else { return }
        screens = fresh
        // The library drops its region when the layout changes: choose again.
        selectScreen(selectedScreen)
    }

    fileprivate func handle(kind: Int32, text: String?, source: CallbackBox) {
        guard callbackBox === source else { return }
        switch Int(kind) {
        case BEACH_EVENT_FRAME:
            onFrame?()
        case BEACH_EVENT_RESIZED:
            refreshSize()
            refreshScreens()
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

private final class ConnectionAttempt {
    let job: OpaquePointer
    let context: UnsafeMutableRawPointer
    // The protected identity files live until the polling task has also returned.
    let prepared: PreparedAccess

    init(job: OpaquePointer, context: UnsafeMutableRawPointer, prepared: PreparedAccess) {
        self.job = job
        self.context = context
        self.prepared = prepared
    }
}

/// Reads a NUL-terminated buffer.
nonisolated private func cString(_ buffer: [CChar]) -> String {
    buffer.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
}

/// Session pointer owned by `RemoteSession`.
private final class SessionHandle {
    let pointer: OpaquePointer
    var context: UnsafeMutableRawPointer?

    init(_ pointer: OpaquePointer) {
        self.pointer = pointer
    }
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
        box.session?.handle(kind: kind, text: message, source: box)
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
