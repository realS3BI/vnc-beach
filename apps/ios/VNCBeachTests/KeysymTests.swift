import Testing
import UIKit

@testable import VNCBeach

struct KeysymTests {
    @Test func charactersMapLikeTheDesktopViewer() {
        #expect(Keysym.forCharacter("a") == 0x61)
        #expect(Keysym.forCharacter("ü") == 0xfc)
        #expect(Keysym.forCharacter("€") == 0x0100_20ac)
        #expect(Keysym.forCharacter("@") == 0x40)
    }

    @Test func specialKeys() {
        #expect(Keysym.special(for: .keyboardEscape) == Keysym.escape)
        #expect(Keysym.special(for: .keyboardF12) == Keysym.f1 + 11)
        #expect(Keysym.special(for: .keyboardA) == nil)
        #expect(Keysym.modifier(for: .keyboardLeftGUI) == .command)
        #expect(Keysym.modifier(for: .keyboardRightControl) == .control)
    }

    @Test func addressesNormalise() {
        #expect(RemoteSession.normalize(address: "sebastian-pc") == "sebastian-pc:5900")
        #expect(RemoteSession.normalize(address: "10.0.0.2:1") == "10.0.0.2:5901")
        #expect(RemoteSession.normalize(address: "pc::5901") == "pc:5901")
    }
}
