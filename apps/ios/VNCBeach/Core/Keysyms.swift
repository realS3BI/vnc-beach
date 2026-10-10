import BeachMobile
import UIKit

/// X11 keysyms for the RFB KeyEvent message.
enum Keysym {
    static let backspace = UInt32(BEACH_KEY_BACKSPACE)
    static let tab = UInt32(BEACH_KEY_TAB)
    static let `return` = UInt32(BEACH_KEY_RETURN)
    static let escape = UInt32(BEACH_KEY_ESCAPE)
    static let insert = UInt32(BEACH_KEY_INSERT)
    static let delete = UInt32(BEACH_KEY_DELETE)
    static let home = UInt32(BEACH_KEY_HOME)
    static let end = UInt32(BEACH_KEY_END)
    static let pageUp = UInt32(BEACH_KEY_PAGE_UP)
    static let pageDown = UInt32(BEACH_KEY_PAGE_DOWN)
    static let left = UInt32(BEACH_KEY_LEFT)
    static let up = UInt32(BEACH_KEY_UP)
    static let right = UInt32(BEACH_KEY_RIGHT)
    static let down = UInt32(BEACH_KEY_DOWN)
    static let f1 = UInt32(BEACH_KEY_F1)
    static let shift = UInt32(BEACH_KEY_SHIFT_L)
    static let control = UInt32(BEACH_KEY_CONTROL_L)
    static let capsLock = UInt32(BEACH_KEY_CAPS_LOCK)
    static let alt = UInt32(BEACH_KEY_ALT_L)
    static let meta = UInt32(BEACH_KEY_META_L)
    /// Windows key / ⌘.
    static let `super` = UInt32(BEACH_KEY_SUPER_L)

    static let modifiers: Set<UInt32> = [shift, control, alt, meta, `super`]

    /// Keysym for a printable character, nil for surrogates.
    static func forScalar(_ scalar: Unicode.Scalar) -> UInt32? {
        let sym = beach_keysym_for_char(scalar.value)
        return sym == 0 ? nil : sym
    }

    static func forCharacter(_ character: Character) -> UInt32? {
        character.unicodeScalars.first.flatMap(forScalar)
    }

    enum Modifier {
        case shift, control, alt, command
    }

    /// Hardware keyboard modifier keys.
    static func modifier(for code: UIKeyboardHIDUsage) -> Modifier? {
        switch code {
        case .keyboardLeftShift, .keyboardRightShift: .shift
        case .keyboardLeftControl, .keyboardRightControl: .control
        case .keyboardLeftAlt, .keyboardRightAlt: .alt
        case .keyboardLeftGUI, .keyboardRightGUI: .command
        default: nil
        }
    }

    /// Hardware keys that never produce text.
    static func special(for code: UIKeyboardHIDUsage) -> UInt32? {
        switch code {
        case .keyboardEscape: escape
        case .keyboardTab: tab
        case .keyboardReturnOrEnter, .keypadEnter: `return`
        case .keyboardDeleteOrBackspace: backspace
        case .keyboardDeleteForward: delete
        case .keyboardInsert: insert
        case .keyboardHome: home
        case .keyboardEnd: end
        case .keyboardPageUp: pageUp
        case .keyboardPageDown: pageDown
        case .keyboardLeftArrow: left
        case .keyboardRightArrow: right
        case .keyboardUpArrow: up
        case .keyboardDownArrow: down
        case .keyboardCapsLock: capsLock
        case .keyboardF1: f1
        case .keyboardF2: f1 + 1
        case .keyboardF3: f1 + 2
        case .keyboardF4: f1 + 3
        case .keyboardF5: f1 + 4
        case .keyboardF6: f1 + 5
        case .keyboardF7: f1 + 6
        case .keyboardF8: f1 + 7
        case .keyboardF9: f1 + 8
        case .keyboardF10: f1 + 9
        case .keyboardF11: f1 + 10
        case .keyboardF12: f1 + 11
        default: nil
        }
    }
}
