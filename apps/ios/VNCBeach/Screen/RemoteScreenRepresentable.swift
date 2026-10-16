import Observation
import SwiftUI

/// Lets the SwiftUI toolbar talk to the UIKit screen view.
@Observable
@MainActor
final class ScreenController {
    var inputMode: InputMode = AppSettings.inputMode
    var viewOnly = false
    var keyboardVisible = false
    /// Shrink the image above the software keyboard instead of covering it.
    var keyboardShrinks = AppSettings.keyboardShrinks {
        didSet { UserDefaults.standard.set(keyboardShrinks, forKey: AppSettings.keyboardShrinksKey) }
    }
    /// The floating pad with arrow keys and a scroll strip.
    var scrollPadVisible = false
    var zoom: CGFloat = 1
    weak var view: RemoteScreenView?

    func toggleKeyboard() {
        keyboardVisible.toggle()
        view?.setKeyboardVisible(keyboardVisible)
    }

    func resetZoom() {
        view?.resetZoom()
    }

    func type(_ keysym: UInt32) {
        view?.type(keysym: keysym)
    }

    /// Scrolls at the pointer; positive y is "natural" (content moves down).
    func scroll(by delta: CGPoint) {
        view?.scroll(by: delta)
    }

    func sendControlAltDelete() {
        view?.sendControlAltDelete()
    }

    func releaseAll() {
        view?.releaseAll()
    }
}

struct RemoteScreenRepresentable: UIViewRepresentable {
    let session: RemoteSession
    let controller: ScreenController
    let swapCommandControl: Bool
    let remote: RemoteKind

    func makeUIView(context: Context) -> RemoteScreenView {
        let view = RemoteScreenView(session: session)
        view.onViewportChange = { [weak controller] zoom in
            if controller?.zoom != zoom {
                controller?.zoom = zoom
            }
        }
        view.onKeyboardVisibleChange = { [weak controller] visible in
            if controller?.keyboardVisible != visible {
                controller?.keyboardVisible = visible
            }
        }
        controller.view = view
        return view
    }

    func updateUIView(_ view: RemoteScreenView, context: Context) {
        view.inputMode = controller.inputMode
        view.viewOnly = controller.viewOnly
        view.swapCommandControl = swapCommandControl
        view.remote = remote
        view.touchpadSpeed = AppSettings.touchpadSpeed
        view.shrinkForKeyboard = controller.keyboardShrinks
        if view.keyboardVisible != controller.keyboardVisible {
            view.setKeyboardVisible(controller.keyboardVisible)
        }
        _ = session.size
        _ = session.crop
        view.remoteSizeChanged()
    }
}
