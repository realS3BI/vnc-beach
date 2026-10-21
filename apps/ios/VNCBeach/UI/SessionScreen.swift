import SwiftUI
import UIKit

/// One connection, full screen, with a collapsible toolbar.
struct SessionScreen: View {
    let server: Server
    @Environment(ServerStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var session = RemoteSession()
    @State private var controller = ScreenController()
    @State private var toolbarExpanded = true
    /// The pasteboard state last sent or received; nil until the first
    /// connect, so text copied before connecting goes over too.
    @State private var pasteboardChangeCount: Int?
    @State private var wasConnected = false

    private var swapCommandControl: Bool {
        switch AppSettings.shortcutMapping {
        case .auto: !session.remote.isMac
        case .swap: true
        case .plain: false
        }
    }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            switch session.state {
            case .connected:
                RemoteScreenRepresentable(
                    session: session, controller: controller, swapCommandControl: swapCommandControl,
                    remote: session.remote
                )
                .ignoresSafeArea()
            case .idle, .connecting:
                VStack(spacing: 20) {
                    ProgressView("Verbinde mit \(server.displayName) …")
                    Text(session.preparationStatus).multilineTextAlignment(.center)
                    if session.preparationAction == .wake, server.access?.ssh != nil {
                        Button("Per SSH anmelden") { Task { await connect(action: .signIn) } }
                            .buttonStyle(.bordered)
                    }
                    Button("Abbrechen") { session.close(byUser: true) }
                        .buttonStyle(.bordered)
                }
                .padding(24).frame(maxWidth: 560)
                .tint(.white).foregroundStyle(.white)
            case .closed(let reason):
                closedView(reason)
            }
            VStack {
                toolbar
                Spacer()
            }
            if controller.scrollPadVisible, session.state == .connected {
                ScrollPadView(controller: controller)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
                    .padding(.trailing, 24)
                    .padding(.bottom, 32)
                    .transition(.scale.combined(with: .opacity))
            }
        }
        .statusBarHidden(true)
        .sheet(item: Binding(get: { session.prompt }, set: { _ in })) { prompt in
            AccessPromptView(prompt: prompt,
                answer: { session.answer(prompt, value: $0) },
                cancel: { session.close(byUser: true) })
                .id(prompt.id)
        }
        .persistentSystemOverlays(.hidden)
        .task(id: server.id) {
            controller.viewOnly = server.viewOnly
            await connect()
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background:
                controller.releaseAll()
            case .active:
                resumed()
            default:
                break
            }
        }
        .task(id: session.state == .connected) {
            // Copying in another app doesn't always change the scene phase
            // (Split View, Stage Manager, Slide Over), so watch the change
            // count; reading it doesn't trigger the paste prompt.
            while session.state == .connected, !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                if controller.view?.window?.isKeyWindow == true {
                    sendPasteboard(force: false)
                }
            }
        }
        .onAppear { UIApplication.shared.isIdleTimerDisabled = true }
        .onDisappear {
            UIApplication.shared.isIdleTimerDisabled = false
            session.close(byUser: true)
        }
    }

    private var toolbar: some View {
        HStack(spacing: 0) {
            if toolbarExpanded {
                HStack(spacing: 14) {
                    Text(session.name.isEmpty ? server.displayName : session.name)
                        .font(.subheadline.weight(.medium))
                        .lineLimit(1)
                        .frame(maxWidth: 180)
                    Divider().frame(height: 18)
                    Button {
                        controller.toggleKeyboard()
                    } label: {
                        Image(systemName: controller.keyboardVisible ? "keyboard.fill" : "keyboard")
                    }
                    .help("Tastatur")
                    Picker("Eingabe", selection: $controller.inputMode) {
                        ForEach(InputMode.allCases) { mode in
                            Label(mode.label, systemImage: mode.symbol).tag(mode)
                        }
                    }
                    .pickerStyle(.menu)
                    .labelsHidden()
                    Button {
                        withAnimation(.snappy) { controller.scrollPadVisible.toggle() }
                    } label: {
                        Image(systemName: controller.scrollPadVisible ? "arrow.up.and.down.square.fill" : "arrow.up.and.down.square")
                    }
                    .help("Scroll-Pad")
                    if session.screens.count > 1 {
                        Menu {
                            Picker("Bildschirm", selection: screenSelection) {
                                Text("Alle Bildschirme").tag(-1)
                                ForEach(session.screens) { screen in
                                    Text(screen.label).tag(screen.id)
                                }
                            }
                        } label: {
                            Image(systemName: session.selectedScreen == nil ? "rectangle.on.rectangle" : "rectangle.inset.filled")
                        }
                        .help("Bildschirm")
                    }
                    Menu {
                        Button("Esc") { controller.type(Keysym.escape) }
                        Button("Tab") { controller.type(Keysym.tab) }
                        Button("Windows-Taste") { controller.type(Keysym.super) }
                        Button("Strg+Alt+Entf") { controller.sendControlAltDelete() }
                        Divider()
                        Button("Zwischenablage senden") { sendPasteboard(force: true) }
                        Button("Bild neu laden") { session.refresh() }
                        Divider()
                        Toggle("Bild über der Tastatur verkleinern", isOn: $controller.keyboardShrinks)
                        Toggle("Nur ansehen", isOn: $controller.viewOnly)
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                    if controller.zoom > 1.01 {
                        Button {
                            controller.resetZoom()
                        } label: {
                            Image(systemName: "arrow.down.right.and.arrow.up.left")
                        }
                        .help("Zoom zurücksetzen")
                    }
                    Button(role: .destructive) {
                        session.close(byUser: true)
                        dismiss()
                    } label: {
                        Image(systemName: "xmark")
                    }
                    .help("Trennen")
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
            }
            Button {
                withAnimation(.snappy) { toolbarExpanded.toggle() }
            } label: {
                Image(systemName: toolbarExpanded ? "chevron.up" : "chevron.down")
                    .font(.caption.weight(.bold))
                    .frame(width: 44, height: 34)
                    .contentShape(Rectangle())
            }
            .accessibilityLabel(toolbarExpanded ? "Leiste einklappen" : "Leiste ausklappen")
        }
        .buttonStyle(.plain)
        .tint(.primary)
        .background(.regularMaterial, in: Capsule())
        .contentShape(Capsule())
        .padding(.top, 6)
        // Collapsed, the handle would sit under the iPad's multitasking
        // control at the top centre, which swallows the tap.
        .padding(.trailing, toolbarExpanded ? 0 : 16)
        .frame(maxWidth: .infinity, alignment: toolbarExpanded ? .center : .trailing)
    }

    /// -1 stands for "all displays"; the picker can't hold an optional.
    private var screenSelection: Binding<Int> {
        Binding(
            get: { session.selectedScreen ?? -1 },
            set: { session.selectScreen($0 < 0 ? nil : $0) })
    }

    private func closedView(_ reason: String) -> some View {
        GeometryReader { geometry in
            ScrollView {
                VStack(spacing: 16) {
                    Image(systemName: "display.trianglebadge.exclamationmark")
                        .font(.system(size: 44))
                    Text(reason.isEmpty ? "Verbindung beendet" : reason)
                        .multilineTextAlignment(.center)
                        .textSelection(.enabled)
                    HStack {
                        Button("Zurück") { dismiss() }
                        Button("Erneut verbinden") { Task { await connect() } }
                            .buttonStyle(.borderedProminent)
                    }
                    if server.access?.ssh != nil {
                        Button("Per SSH anmelden") { Task { await connect(action: .signIn) } }
                            .buttonStyle(.bordered)
                    }
                    if server.access?.wake != nil {
                        Button("Aufwecken & verbinden") { Task { await connect(action: .wake) } }
                            .buttonStyle(.bordered)
                    }
                }
                .foregroundStyle(.white)
                .buttonStyle(.bordered)
                .controlSize(.large)
                .frame(maxWidth: 560)
                .padding(.horizontal, 24)
                .padding(.top, 80)
                .padding(.bottom, 24)
                .frame(maxWidth: .infinity, minHeight: geometry.size.height)
            }
        }
    }

    private func connect(action: ConnectionAction = .connect) async {
        session.onClipboard = { text in
            guard AppSettings.clipboardSync else { return }
            UIPasteboard.general.string = text
            pasteboardChangeCount = UIPasteboard.general.changeCount
        }
        await session.connect(
            address: server.host, password: store.password(for: server), depth: AppSettings.colorDepth,
            access: server.access ?? AccessSettings(), action: action)
        if session.state == .connected {
            wasConnected = true
            sendPasteboard(force: false)
        }
    }

    /// Back from the background: the socket is usually dead by now.
    private func resumed() {
        switch session.state {
        case .connected:
            session.refresh()
            sendPasteboard(force: false)
        case .closed where wasConnected && !session.closedByUser:
            Task { await connect() }
        default:
            break
        }
    }

    private func sendPasteboard(force: Bool) {
        guard AppSettings.clipboardSync || force, session.state == .connected else { return }
        let pasteboard = UIPasteboard.general
        guard force || pasteboard.changeCount != pasteboardChangeCount else { return }
        pasteboardChangeCount = pasteboard.changeCount
        guard pasteboard.hasStrings, let text = pasteboard.string, !text.isEmpty else { return }
        session.sendClipboard(text)
    }
}
