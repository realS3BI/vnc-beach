import Foundation

enum ColorDepth: Int, CaseIterable, Identifiable {
    case full = 24
    case medium = 16
    case low = 8

    var id: Int { rawValue }
    var label: String {
        switch self {
        case .full: "Volle Farbtiefe"
        case .medium: "16 Bit"
        case .low: "256 Farben (langsame Leitung)"
        }
    }
}

enum InputMode: String, CaseIterable, Identifiable {
    /// Finger moves a cursor like on a trackpad; taps click where the cursor is.
    case touchpad
    /// Taps click where the finger touches.
    case direct

    var id: String { rawValue }
    var label: String {
        switch self {
        case .touchpad: "Touchpad"
        case .direct: "Direkt"
        }
    }
    var symbol: String {
        switch self {
        case .touchpad: "hand.point.up.left"
        case .direct: "hand.tap"
        }
    }
}

enum ShortcutMapping: String, CaseIterable, Identifiable {
    /// Swap ⌘ and Ctrl when the remote computer is not a Mac.
    case auto
    case swap
    case plain

    var id: String { rawValue }
    var label: String {
        switch self {
        case .auto: "Automatisch"
        case .swap: "⌘ als Strg senden"
        case .plain: "Unverändert"
        }
    }
}

/// Keys shared by the settings screen (`@AppStorage`) and the session.
enum AppSettings {
    static let colorDepthKey = "colorDepth"
    static let inputModeKey = "inputMode"
    static let shortcutMappingKey = "shortcutMapping"
    static let clipboardSyncKey = "clipboardSync"
    static let touchpadSpeedKey = "touchpadSpeed"

    static var colorDepth: ColorDepth {
        ColorDepth(rawValue: UserDefaults.standard.integer(forKey: colorDepthKey)) ?? .full
    }
    static var inputMode: InputMode {
        InputMode(rawValue: UserDefaults.standard.string(forKey: inputModeKey) ?? "") ?? .touchpad
    }
    static var shortcutMapping: ShortcutMapping {
        ShortcutMapping(rawValue: UserDefaults.standard.string(forKey: shortcutMappingKey) ?? "") ?? .auto
    }
    static var clipboardSync: Bool {
        UserDefaults.standard.object(forKey: clipboardSyncKey) as? Bool ?? true
    }
    static var touchpadSpeed: Double {
        let v = UserDefaults.standard.double(forKey: touchpadSpeedKey)
        return v == 0 ? 1.0 : v
    }
}
