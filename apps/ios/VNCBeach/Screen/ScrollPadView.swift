import SwiftUI

/// A floating pad for scrolling and cursor keys without a keyboard or
/// two-finger gestures: four arrow keys that repeat while held, and a strip
/// that scrolls at the pointer when dragged (hold an end to keep going).
/// Drag the grip to move the pad.
struct ScrollPadView: View {
    let controller: ScreenController
    @State private var offset = CGSize.zero
    @State private var dragStart: CGSize?

    var body: some View {
        VStack(spacing: 6) {
            Capsule()
                .fill(.secondary)
                .frame(width: 36, height: 5)
                .padding(.top, 8)
                .padding(.bottom, 2)
                .frame(maxWidth: .infinity)
                .contentShape(Rectangle())
                .accessibilityLabel("Scroll-Pad verschieben")
            HStack(alignment: .center, spacing: 12) {
                arrows
                ScrollStrip(controller: controller)
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .shadow(color: .black.opacity(0.35), radius: 12, y: 4)
        .offset(offset)
        .gesture(
            DragGesture()
                .onChanged { value in
                    let start = dragStart ?? offset
                    dragStart = start
                    offset = CGSize(width: start.width + value.translation.width, height: start.height + value.translation.height)
                }
                .onEnded { _ in dragStart = nil }
        )
    }

    private var arrows: some View {
        VStack(spacing: 4) {
            RepeatKey(symbol: "arrow.up", label: "Oben") { controller.type(Keysym.up) }
            HStack(spacing: 4) {
                RepeatKey(symbol: "arrow.left", label: "Links") { controller.type(Keysym.left) }
                RepeatKey(symbol: "arrow.down", label: "Unten") { controller.type(Keysym.down) }
                RepeatKey(symbol: "arrow.right", label: "Rechts") { controller.type(Keysym.right) }
            }
        }
    }
}

/// A key that fires once on tap and repeats while held.
private struct RepeatKey: View {
    let symbol: String
    let label: String
    let action: () -> Void
    @State private var holding: Task<Void, Never>?
    @State private var repeated = false
    @State private var pressed = false

    var body: some View {
        Image(systemName: symbol)
            .font(.body.weight(.semibold))
            .frame(width: 44, height: 40)
            .background(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(pressed ? Color.accentColor.opacity(0.6) : Color.primary.opacity(0.12)))
            .contentShape(Rectangle())
            .accessibilityLabel(label)
            .gesture(
                DragGesture(minimumDistance: 0)
                    .onChanged { _ in
                        guard holding == nil else { return }
                        pressed = true
                        repeated = false
                        holding = Task { @MainActor in
                            try? await Task.sleep(for: .milliseconds(400))
                            while !Task.isCancelled {
                                repeated = true
                                action()
                                try? await Task.sleep(for: .milliseconds(60))
                            }
                        }
                    }
                    .onEnded { _ in
                        holding?.cancel()
                        holding = nil
                        pressed = false
                        if !repeated { action() }
                    }
            )
    }
}

/// Drag to scroll (natural direction, like a touch screen); a tap on the
/// upper or lower half scrolls one notch; holding an end keeps scrolling.
private struct ScrollStrip: View {
    let controller: ScreenController
    @State private var lastY: CGFloat?
    @State private var moved = false
    @State private var holding: Task<Void, Never>?
    @State private var active = false

    private static let height: CGFloat = 132
    private static let gain: CGFloat = 1.6

    var body: some View {
        VStack {
            Image(systemName: "chevron.up")
            Spacer()
            Image(systemName: "arrow.up.and.down")
                .foregroundStyle(.secondary)
            Spacer()
            Image(systemName: "chevron.down")
        }
        .font(.body.weight(.semibold))
        .padding(.vertical, 8)
        .frame(width: 44, height: Self.height)
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(active ? Color.accentColor.opacity(0.35) : Color.primary.opacity(0.12)))
        .contentShape(Rectangle())
        .accessibilityLabel("Scrollen")
        .gesture(
            DragGesture(minimumDistance: 0)
                .onChanged { value in
                    active = true
                    if let lastY {
                        let dy = value.location.y - lastY
                        if abs(value.translation.height) > 6 {
                            moved = true
                            holding?.cancel()
                            holding = nil
                        }
                        if moved {
                            controller.scroll(by: CGPoint(x: 0, y: dy * Self.gain))
                        }
                    } else {
                        moved = false
                        let notch = Self.notch(at: value.location.y)
                        holding = Task { @MainActor in
                            try? await Task.sleep(for: .milliseconds(450))
                            while !Task.isCancelled {
                                controller.scroll(by: CGPoint(x: 0, y: notch))
                                try? await Task.sleep(for: .milliseconds(90))
                            }
                        }
                    }
                    lastY = value.location.y
                }
                .onEnded { value in
                    let held = holding != nil
                    holding?.cancel()
                    holding = nil
                    active = false
                    if !moved && held && abs(value.translation.height) <= 6 {
                        controller.scroll(by: CGPoint(x: 0, y: Self.notch(at: value.location.y)))
                    }
                    lastY = nil
                    moved = false
                }
        )
    }

    /// One wheel notch up (positive, content moves down) in the upper half,
    /// down in the lower half.
    private static func notch(at y: CGFloat) -> CGFloat {
        y < height / 2 ? 40 : -40
    }
}
