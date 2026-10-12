import MetalKit
import UIKit

/// The remote screen: Metal rendering, viewport (zoom/pan), touch, trackpad,
/// mouse, Apple Pencil and keyboard input.
///
/// Touch gestures (finger or Pencil):
/// - Touchpad mode: one finger moves the cursor, tap clicks, long press and
///   drag holds the left button, two-finger tap right-clicks, two-finger
///   drag scrolls, pinch zooms.
/// - Direct mode: the same, but taps and drags happen where the finger is.
/// - Pencil hover moves the cursor; the tip presses the left button.
/// Trackpad and mouse are always absolute: hover moves, clicks click
/// (secondary click = right button), scroll gestures scroll.
@MainActor
final class RemoteScreenView: UIView {
    let session: RemoteSession
    private let metalView = MTKView()
    private var renderer: ScreenRenderer?
    private let cursorLayer = CAShapeLayer()

    var inputMode: InputMode = .touchpad
    var viewOnly = false {
        didSet {
            if viewOnly { releaseAll() }
            cursorLayer.isHidden = viewOnly || !cursorKnown
        }
    }
    /// Send ⌘ as Ctrl and Ctrl as ⌘ (Windows/Linux hosts).
    var swapCommandControl = false
    var remote = RemoteKind.other
    var touchpadSpeed: CGFloat = 1
    var onViewportChange: ((CGFloat) -> Void)?
    /// Keep the image above the software keyboard instead of under it.
    var shrinkForKeyboard = false {
        didSet { if oldValue != shrinkForKeyboard { setNeedsLayout() } }
    }

    // Viewport: the remote image is drawn `scale` points per remote pixel,
    // with its top-left corner at `origin`. Only `crop` (one display or the
    // whole framebuffer) is shown; pointer coordinates are framebuffer pixels.
    private(set) var zoom: CGFloat = 1
    private var origin = CGPoint.zero
    private var fbSize = CGSize(width: 1, height: 1)
    private var crop = CGRect(x: 0, y: 0, width: 1, height: 1)
    /// Points at the bottom hidden by the software keyboard.
    private var keyboardOverlap: CGFloat = 0
    private var pinchCommitted = false
    private var pinchLastScale: CGFloat = 1
    private var pinchLastCenter = CGPoint.zero

    // Pointer state, in remote pixels.
    private var pointer = CGPoint.zero
    private var cursorKnown = false
    private var buttons: UInt8 = 0
    private var scrollRemainder = CGPoint.zero
    private var dragging = false
    private var dragAnchor = CGPoint.zero
    private var touchpadLast = CGPoint.zero
    private var pencilHovering = false
    /// The gesture in progress comes from the Pencil tip: always absolute.
    private var pencilTouch = false

    // Keyboard state.
    private var heldModifiers = Set<UInt32>()
    private(set) var stickyModifiers = Set<UInt32>()
    private var hardwareKeysDown: [UIKeyboardHIDUsage: UInt32] = [:]
    private(set) var keyboardVisible = false
    private lazy var extraKeys = ExtraKeysBar(owner: self)
    private let hiddenInputView = UIView(frame: CGRect(x: 0, y: 0, width: 0, height: 0))

    // UITextInputTraits
    var keyboardType: UIKeyboardType = .default
    var keyboardAppearance: UIKeyboardAppearance = .dark
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
    var spellCheckingType: UITextSpellCheckingType = .no

    private static let pointsPerNotch: CGFloat = 40
    private static let maxZoom: CGFloat = 8

    init(session: RemoteSession) {
        self.session = session
        super.init(frame: .zero)
        backgroundColor = .black
        isMultipleTouchEnabled = true
        setupMetal()
        setupCursor()
        setupGestures()
        addInteraction(UIPointerInteraction(delegate: self))
        session.onFrame = { [weak self] in self?.metalView.setNeedsDisplay() }
        fbSize = session.size == .zero ? CGSize(width: 1, height: 1) : session.size
        crop = session.crop.isEmpty ? CGRect(origin: .zero, size: fbSize) : session.crop
        pointer = CGPoint(x: crop.midX, y: crop.midY)
        observeKeyboard()
    }

    required init?(coder: NSCoder) { fatalError("not supported") }

    /// Selector-based observers unregister themselves when the view dies.
    private func observeKeyboard() {
        let center = NotificationCenter.default
        for name in [UIResponder.keyboardWillChangeFrameNotification, UIResponder.keyboardWillHideNotification] {
            center.addObserver(self, selector: #selector(keyboardNotification(_:)), name: name, object: nil)
        }
    }

    /// Keyboard notifications arrive on the main thread.
    @objc private func keyboardNotification(_ note: Notification) {
        let end = (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue
        let hiding = note.name == UIResponder.keyboardWillHideNotification
        keyboardFrameChanged(to: hiding ? nil : end)
    }

    /// `frame` is in screen coordinates; nil means the keyboard is gone.
    private func keyboardFrameChanged(to frame: CGRect?) {
        var overlap: CGFloat = 0
        if let frame, let window {
            let keyboard = window.convert(frame, from: nil)
            let mine = convert(bounds, to: window)
            // A floating or split iPad keyboard doesn't take the bottom.
            let docked = keyboard.width >= window.bounds.width * 0.6
            if docked {
                overlap = max(0, min(mine.maxY, window.bounds.maxY) - max(keyboard.minY, mine.minY))
            }
        }
        if overlap != keyboardOverlap {
            keyboardOverlap = overlap
            setNeedsLayout()
        }
    }

    private func setupMetal() {
        metalView.isPaused = true
        metalView.enableSetNeedsDisplay = true
        metalView.framebufferOnly = true
        metalView.isUserInteractionEnabled = false
        metalView.clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
        metalView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        if let renderer = ScreenRenderer(view: metalView) {
            metalView.device = renderer.device
            renderer.session = session
            metalView.delegate = renderer
            self.renderer = renderer
        }
        addSubview(metalView)
    }

    private func setupCursor() {
        let path = UIBezierPath()
        path.move(to: CGPoint(x: 0, y: 0))
        path.addLine(to: CGPoint(x: 0, y: 17))
        path.addLine(to: CGPoint(x: 4.5, y: 13))
        path.addLine(to: CGPoint(x: 7.5, y: 19.5))
        path.addLine(to: CGPoint(x: 10, y: 18.5))
        path.addLine(to: CGPoint(x: 7, y: 12))
        path.addLine(to: CGPoint(x: 12.5, y: 12))
        path.close()
        cursorLayer.path = path.cgPath
        cursorLayer.fillColor = UIColor.white.cgColor
        cursorLayer.strokeColor = UIColor.black.cgColor
        cursorLayer.lineWidth = 1
        cursorLayer.lineJoin = .round
        cursorLayer.shadowColor = UIColor.black.cgColor
        cursorLayer.shadowOpacity = 0.5
        cursorLayer.shadowRadius = 1.5
        cursorLayer.shadowOffset = CGSize(width: 0, height: 1)
        cursorLayer.isHidden = true
        cursorLayer.actions = ["position": NSNull(), "hidden": NSNull()]
        layer.addSublayer(cursorLayer)
    }

    private func setupGestures() {
        let finger: [NSNumber] = [
            NSNumber(value: UITouch.TouchType.direct.rawValue),
            NSNumber(value: UITouch.TouchType.pencil.rawValue),
        ]
        let tap = UITapGestureRecognizer(target: self, action: #selector(handleTap))
        let twoFingerTap = UITapGestureRecognizer(target: self, action: #selector(handleTwoFingerTap))
        twoFingerTap.numberOfTouchesRequired = 2
        let longPress = UILongPressGestureRecognizer(target: self, action: #selector(handleLongPress))
        longPress.minimumPressDuration = 0.35
        longPress.allowableMovement = 12
        let pan = UIPanGestureRecognizer(target: self, action: #selector(handlePan))
        pan.maximumNumberOfTouches = 1
        let twoFingerPan = UIPanGestureRecognizer(target: self, action: #selector(handleTwoFingerPan))
        twoFingerPan.minimumNumberOfTouches = 2
        twoFingerPan.maximumNumberOfTouches = 2
        let pinch = UIPinchGestureRecognizer(target: self, action: #selector(handlePinch))
        for g in [tap, twoFingerTap, longPress, pan, twoFingerPan, pinch] as [UIGestureRecognizer] {
            g.allowedTouchTypes = finger
            g.cancelsTouchesInView = false
            g.delegate = self
            addGestureRecognizer(g)
        }
        // Trackpad and Pencil hover.
        let hover = UIHoverGestureRecognizer(target: self, action: #selector(handleHover))
        hover.delegate = self
        addGestureRecognizer(hover)
        // Trackpad two-finger scrolling and mouse wheels.
        let scroll = UIPanGestureRecognizer(target: self, action: #selector(handleScroll))
        scroll.allowedScrollTypesMask = .all
        scroll.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.indirectPointer.rawValue)]
        scroll.cancelsTouchesInView = false
        scroll.delegate = self
        addGestureRecognizer(scroll)
    }

    // MARK: Layout and viewport

    override var canBecomeFirstResponder: Bool { true }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        if window != nil {
            becomeFirstResponder()
        }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        metalView.frame = bounds
        renderer?.contentScale = window?.screen.scale ?? traitCollection.displayScale
        let shown = session.crop.isEmpty ? CGRect(origin: .zero, size: session.size) : session.crop
        if session.size != .zero, session.size != fbSize || shown != crop {
            fbSize = session.size
            crop = shown
            zoom = 1
            if !crop.contains(pointer) {
                pointer = CGPoint(x: crop.midX, y: crop.midY)
            }
        }
        layoutViewport()
    }

    /// Remote size or display choice changed; called by the representable.
    func remoteSizeChanged() {
        setNeedsLayout()
    }

    /// The area the image is laid out in: everything, or what the keyboard
    /// leaves free.
    private var viewport: CGRect {
        guard shrinkForKeyboard, keyboardOverlap > 0 else { return bounds }
        return CGRect(x: 0, y: 0, width: bounds.width, height: max(bounds.height - keyboardOverlap, 1))
    }

    private var fitScale: CGFloat {
        min(viewport.width / crop.width, viewport.height / crop.height)
    }

    private var scale: CGFloat { fitScale * zoom }

    private var imageSize: CGSize {
        CGSize(width: crop.width * scale, height: crop.height * scale)
    }

    private func layoutViewport() {
        let image = imageSize
        let area = viewport
        origin.x = image.width <= area.width
            ? (area.width - image.width) / 2 : min(0, max(area.width - image.width, origin.x))
        origin.y = image.height <= area.height
            ? (area.height - image.height) / 2 : min(0, max(area.height - image.height, origin.y))
        renderer?.imageRect = CGRect(origin: origin, size: image)
        renderer?.uvRect = CGRect(
            x: crop.minX / fbSize.width, y: crop.minY / fbSize.height,
            width: crop.width / fbSize.width, height: crop.height / fbSize.height)
        metalView.setNeedsDisplay()
        updateCursorLayer()
        onViewportChange?(zoom)
    }

    func resetZoom() {
        zoom = 1
        layoutViewport()
    }

    /// View point → framebuffer pixel, clamped to the shown display.
    private func remotePoint(from p: CGPoint) -> CGPoint {
        clampToCrop(CGPoint(x: crop.minX + (p.x - origin.x) / scale, y: crop.minY + (p.y - origin.y) / scale))
    }

    private func clampToCrop(_ p: CGPoint) -> CGPoint {
        CGPoint(
            x: min(max(p.x, crop.minX), crop.maxX - 1),
            y: min(max(p.y, crop.minY), crop.maxY - 1))
    }

    private func viewPoint(from p: CGPoint) -> CGPoint {
        CGPoint(x: origin.x + (p.x - crop.minX) * scale, y: origin.y + (p.y - crop.minY) * scale)
    }

    /// Keeps the cursor in view when zoomed in.
    private func followCursor() {
        guard zoom > 1 else { return }
        let p = viewPoint(from: pointer)
        let area = viewport
        let margin: CGFloat = 48
        if p.x < margin { origin.x += margin - p.x }
        if p.x > area.width - margin { origin.x -= p.x - (area.width - margin) }
        if p.y < margin { origin.y += margin - p.y }
        if p.y > area.height - margin { origin.y -= p.y - (area.height - margin) }
        layoutViewport()
    }

    private func updateCursorLayer() {
        cursorLayer.position = viewPoint(from: pointer)
        cursorLayer.isHidden = viewOnly || !cursorKnown
    }

    // MARK: Pointer

    private func movePointer(to p: CGPoint, send: Bool = true) {
        pointer = p
        cursorKnown = true
        updateCursorLayer()
        if send { sendPointer() }
    }

    private func sendPointer() {
        guard !viewOnly else { return }
        session.sendPointer(buttons: buttons, x: Int(pointer.x), y: Int(pointer.y))
    }

    private func click(_ bit: UInt8) {
        guard !viewOnly else { return }
        session.sendPointer(buttons: buttons | bit, x: Int(pointer.x), y: Int(pointer.y))
        session.sendPointer(buttons: buttons, x: Int(pointer.x), y: Int(pointer.y))
    }

    private func press(_ bit: UInt8) {
        buttons |= bit
        sendPointer()
    }

    private func release(_ bit: UInt8) {
        buttons &= ~bit
        sendPointer()
    }

    /// Turns accumulated scroll distance (points, natural direction) into
    /// wheel clicks at the pointer.
    func scroll(by delta: CGPoint) {
        guard !viewOnly else { return }
        scrollRemainder.x += delta.x
        scrollRemainder.y += delta.y
        let notch = Self.pointsPerNotch
        let (x, y) = (Int(pointer.x), Int(pointer.y))
        while scrollRemainder.y >= notch {
            session.sendWheel(buttons: buttons, wheelBit: 8, x: x, y: y)
            scrollRemainder.y -= notch
        }
        while scrollRemainder.y <= -notch {
            session.sendWheel(buttons: buttons, wheelBit: 16, x: x, y: y)
            scrollRemainder.y += notch
        }
        while scrollRemainder.x >= notch {
            session.sendWheel(buttons: buttons, wheelBit: 32, x: x, y: y)
            scrollRemainder.x -= notch
        }
        while scrollRemainder.x <= -notch {
            session.sendWheel(buttons: buttons, wheelBit: 64, x: x, y: y)
            scrollRemainder.x += notch
        }
    }

    // MARK: Finger and Pencil gestures

    private var absoluteTouch: Bool { inputMode == .direct || pencilTouch }

    @objc private func handleTap(_ g: UITapGestureRecognizer) {
        guard g.state == .ended else { return }
        if absoluteTouch {
            movePointer(to: remotePoint(from: g.location(in: self)))
        }
        click(1)
    }

    @objc private func handleTwoFingerTap(_ g: UITapGestureRecognizer) {
        guard g.state == .ended else { return }
        if inputMode == .direct {
            movePointer(to: remotePoint(from: g.location(in: self)))
        }
        click(4)
    }

    @objc private func handleLongPress(_ g: UILongPressGestureRecognizer) {
        let location = g.location(in: self)
        switch g.state {
        case .began:
            dragging = true
            dragAnchor = location
            touchpadLast = location
            if absoluteTouch {
                movePointer(to: remotePoint(from: location), send: false)
            }
            UIImpactFeedbackGenerator(style: .medium).impactOccurred()
            press(1)
        case .changed:
            guard dragging else { return }
            if absoluteTouch {
                movePointer(to: remotePoint(from: location))
            } else {
                moveTouchpad(to: location)
            }
        case .ended, .cancelled, .failed:
            guard dragging else { return }
            dragging = false
            release(1)
        default:
            break
        }
    }

    @objc private func handlePan(_ g: UIPanGestureRecognizer) {
        guard !dragging else { return }
        let location = g.location(in: self)
        switch g.state {
        case .began:
            touchpadLast = location
            if pencilTouch {
                // The Pencil tip draws: hold the left button while it moves.
                movePointer(to: remotePoint(from: location), send: false)
                press(1)
            } else if inputMode == .direct {
                movePointer(to: remotePoint(from: location))
            }
        case .changed:
            if absoluteTouch {
                movePointer(to: remotePoint(from: location))
            } else {
                moveTouchpad(to: location)
            }
        case .ended, .cancelled, .failed:
            if pencilTouch && buttons & 1 != 0 {
                movePointer(to: remotePoint(from: location), send: false)
                release(1)
            }
        default:
            break
        }
    }

    /// Relative cursor movement with mild acceleration.
    private func moveTouchpad(to location: CGPoint) {
        let dx = location.x - touchpadLast.x
        let dy = location.y - touchpadLast.y
        touchpadLast = location
        let speed = hypot(dx, dy)
        let gain = touchpadSpeed * min(1 + speed / 24, 2.5) / scale
        movePointer(to: clampToCrop(CGPoint(x: pointer.x + dx * gain, y: pointer.y + dy * gain)))
        followCursor()
    }

    @objc private func handleTwoFingerPan(_ g: UIPanGestureRecognizer) {
        guard !pinchCommitted else { return }
        switch g.state {
        case .began:
            scrollRemainder = .zero
        case .changed:
            let t = g.translation(in: self)
            g.setTranslation(.zero, in: self)
            scroll(by: t)
        default:
            break
        }
    }

    @objc private func handlePinch(_ g: UIPinchGestureRecognizer) {
        let center = g.location(in: self)
        switch g.state {
        case .began:
            pinchCommitted = false
            pinchLastScale = 1
            pinchLastCenter = center
        case .changed:
            if !pinchCommitted {
                guard abs(g.scale - 1) > 0.08 else { return }
                pinchCommitted = true
                pinchLastScale = g.scale
                pinchLastCenter = center
                return
            }
            let factor = g.scale / pinchLastScale
            pinchLastScale = g.scale
            let newZoom = min(max(zoom * factor, 1), Self.maxZoom)
            let applied = newZoom / zoom
            // Keep the remote point under the fingers fixed, then follow them.
            origin.x = center.x - (center.x - origin.x) * applied + (center.x - pinchLastCenter.x)
            origin.y = center.y - (center.y - origin.y) * applied + (center.y - pinchLastCenter.y)
            pinchLastCenter = center
            zoom = newZoom
            layoutViewport()
        case .ended, .cancelled, .failed:
            pinchCommitted = false
        default:
            break
        }
    }

    @objc private func handleHover(_ g: UIHoverGestureRecognizer) {
        switch g.state {
        case .began, .changed:
            let pencil = g.numberOfTouches == 0 && g.modifierFlags.isEmpty && isPencilHover(g)
            pencilHovering = pencil
            movePointer(to: remotePoint(from: g.location(in: self)))
        case .ended, .cancelled:
            pencilHovering = false
        default:
            break
        }
    }

    private func isPencilHover(_ g: UIHoverGestureRecognizer) -> Bool {
        if #available(iOS 16.4, *) {
            return g.zOffset > 0 || g.azimuthAngle(in: self) != 0
        }
        return false
    }

    @objc private func handleScroll(_ g: UIPanGestureRecognizer) {
        guard g.numberOfTouches == 0 else { return }
        switch g.state {
        case .began:
            scrollRemainder = .zero
        case .changed:
            let t = g.translation(in: self)
            g.setTranslation(.zero, in: self)
            scroll(by: t)
        default:
            break
        }
    }

    // MARK: Trackpad and mouse buttons

    private func pointerButtons(_ event: UIEvent?) -> UInt8 {
        guard let mask = event?.buttonMask else { return 0 }
        var b: UInt8 = 0
        if mask.contains(.primary) { b |= 1 }
        if mask.contains(.secondary) { b |= 4 }
        return b
    }

    private func indirect(_ touches: Set<UITouch>) -> UITouch? {
        touches.first { $0.type == .indirectPointer }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        if let touch = indirect(touches) {
            pointer = remotePoint(from: touch.location(in: self))
            buttons = pointerButtons(event)
            movePointer(to: pointer)
            return
        }
        super.touchesBegan(touches, with: event)
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        if let touch = indirect(touches) {
            buttons = pointerButtons(event)
            movePointer(to: remotePoint(from: touch.location(in: self)))
            return
        }
        super.touchesMoved(touches, with: event)
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        if let touch = indirect(touches) {
            buttons = pointerButtons(event)
            movePointer(to: remotePoint(from: touch.location(in: self)))
            return
        }
        super.touchesEnded(touches, with: event)
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        if indirect(touches) != nil {
            buttons = 0
            sendPointer()
            return
        }
        super.touchesCancelled(touches, with: event)
    }

    // MARK: Keyboard

    override var inputView: UIView? { keyboardVisible ? nil : hiddenInputView }
    override var inputAccessoryView: UIView? { keyboardVisible ? extraKeys : nil }

    func setKeyboardVisible(_ visible: Bool) {
        keyboardVisible = visible
        if !isFirstResponder {
            becomeFirstResponder()
        }
        reloadInputViews()
    }

    private var commandHeld: Bool {
        let command = commandKeysym
        return heldModifiers.contains(Keysym.control) || heldModifiers.contains(command)
            || stickyModifiers.contains(Keysym.control) || stickyModifiers.contains(command)
    }

    /// macOS Screen Sharing reads Alt_L as ⌘ and Meta_L as Option; everyone
    /// else takes Super_L as the ⌘/Windows key.
    private var commandKeysym: UInt32 { remote == .appleScreenSharing ? Keysym.alt : Keysym.super }
    private var optionKeysym: UInt32 { remote == .appleScreenSharing ? Keysym.meta : Keysym.alt }

    private func keysym(for modifier: Keysym.Modifier) -> UInt32 {
        switch modifier {
        case .shift: Keysym.shift
        case .alt: optionKeysym
        case .control: swapCommandControl ? commandKeysym : Keysym.control
        case .command: swapCommandControl ? Keysym.control : commandKeysym
        }
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        var unhandled = Set<UIPress>()
        for press in presses {
            guard let key = press.key, !viewOnly else {
                unhandled.insert(press)
                continue
            }
            if let modifier = Keysym.modifier(for: key.keyCode) {
                let sym = keysym(for: modifier)
                heldModifiers.insert(sym)
                session.sendKey(sym, down: true)
                // The text input system needs modifiers for Shift/Option characters.
                unhandled.insert(press)
            } else if let sym = Keysym.special(for: key.keyCode) {
                hardwareKeysDown[key.keyCode] = sym
                session.sendKey(sym, down: true)
            } else if commandHeld {
                // Shortcut: press the lowercase key itself, e.g. Ctrl+S.
                let chars = key.charactersIgnoringModifiers.lowercased()
                if let scalar = chars.unicodeScalars.first, let sym = Keysym.forScalar(scalar) {
                    hardwareKeysDown[key.keyCode] = sym
                    session.sendKey(sym, down: true)
                }
            } else {
                // Plain text arrives through insertText with the layout applied.
                unhandled.insert(press)
            }
        }
        if !unhandled.isEmpty {
            super.pressesBegan(unhandled, with: event)
        }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        releasePresses(presses)
        super.pressesEnded(presses, with: event)
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        releasePresses(presses)
        super.pressesCancelled(presses, with: event)
    }

    private func releasePresses(_ presses: Set<UIPress>) {
        for press in presses {
            guard let key = press.key else { continue }
            if let modifier = Keysym.modifier(for: key.keyCode) {
                let sym = keysym(for: modifier)
                if heldModifiers.remove(sym) != nil {
                    session.sendKey(sym, down: false)
                }
            } else if let sym = hardwareKeysDown.removeValue(forKey: key.keyCode) {
                session.sendKey(sym, down: false)
            }
        }
    }

    /// Types one key with the sticky modifiers from the extra keys bar, or
    /// plain with the hardware modifiers lifted (they already shaped the
    /// character locally: Shift for capitals, Option for `@`).
    func type(keysym sym: UInt32) {
        guard !viewOnly else { return }
        if !stickyModifiers.isEmpty {
            let mods = Array(stickyModifiers)
            for m in mods { session.sendKey(m, down: true) }
            session.tapKey(sym)
            for m in mods { session.sendKey(m, down: false) }
            stickyModifiers.removeAll()
            extraKeys.refresh()
            return
        }
        for m in heldModifiers { session.sendKey(m, down: false) }
        session.tapKey(sym)
        for m in heldModifiers { session.sendKey(m, down: true) }
    }

    /// Extra keys bar: modifiers stick until the next key. The bar's Win and
    /// Alt buttons map like the hardware keys.
    func toggleSticky(_ modifier: UInt32) {
        let sym = modifier == Keysym.super ? commandKeysym : modifier == Keysym.alt ? optionKeysym : modifier
        if stickyModifiers.remove(sym) == nil {
            stickyModifiers.insert(sym)
        }
        extraKeys.refresh()
    }

    func isSticky(_ modifier: UInt32) -> Bool {
        let sym = modifier == Keysym.super ? commandKeysym : modifier == Keysym.alt ? optionKeysym : modifier
        return stickyModifiers.contains(sym)
    }

    func sendControlAltDelete() {
        guard !viewOnly else { return }
        session.sendKey(Keysym.control, down: true)
        session.sendKey(Keysym.alt, down: true)
        session.tapKey(Keysym.delete)
        session.sendKey(Keysym.alt, down: false)
        session.sendKey(Keysym.control, down: false)
    }

    /// Releases every button and key we hold, e.g. when the app goes to the background.
    func releaseAll() {
        if buttons != 0 {
            buttons = 0
            session.sendPointer(buttons: 0, x: Int(pointer.x), y: Int(pointer.y))
        }
        for sym in hardwareKeysDown.values { session.sendKey(sym, down: false) }
        hardwareKeysDown.removeAll()
        for sym in heldModifiers { session.sendKey(sym, down: false) }
        heldModifiers.removeAll()
        stickyModifiers.removeAll()
        dragging = false
    }
}

// MARK: - UIKeyInput (software keyboard and plain hardware typing)

extension RemoteScreenView: UIKeyInput {
    var hasText: Bool { true }

    func insertText(_ text: String) {
        for scalar in text.unicodeScalars {
            switch scalar {
            case "\n", "\r": type(keysym: Keysym.return)
            case "\t": type(keysym: Keysym.tab)
            default:
                guard scalar.value >= 0x20, let sym = Keysym.forScalar(scalar) else { continue }
                type(keysym: sym)
            }
        }
    }

    func deleteBackward() {
        type(keysym: Keysym.backspace)
    }
}

// MARK: - Gesture coordination

extension RemoteScreenView: UIGestureRecognizerDelegate {
    func gestureRecognizer(
        _ gestureRecognizer: UIGestureRecognizer,
        shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
    ) -> Bool {
        true
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldReceive touch: UITouch) -> Bool {
        if touch.type == .pencil {
            pencilTouch = true
        } else if touch.type == .direct {
            pencilTouch = false
        }
        return true
    }
}

// MARK: - System pointer (trackpad/mouse)

extension RemoteScreenView: UIPointerInteractionDelegate {
    func pointerInteraction(_ interaction: UIPointerInteraction, styleFor region: UIPointerRegion) -> UIPointerStyle? {
        // We draw the cursor ourselves at the position the server knows.
        .hidden()
    }
}
