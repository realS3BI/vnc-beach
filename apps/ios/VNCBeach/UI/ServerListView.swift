import SwiftUI

struct ServerListView: View {
    @Environment(ServerStore.self) private var store
    @Environment(UpdateChecker.self) private var updates
    @Environment(\.openWindow) private var openWindow
    @Environment(\.supportsMultipleWindows) private var supportsMultipleWindows
    @State private var editing: Server?
    @State private var adding = false
    @State private var active: Server?
    @State private var showSettings = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if let release = updates.banner {
                    UpdateBanner(release: release) { updates.dismissBanner() }
                }
                content
            }
            .navigationTitle("VNC Beach")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { showSettings = true } label: {
                        Label("Einstellungen", systemImage: "gearshape")
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { adding = true } label: {
                        Label("Computer hinzufügen", systemImage: "plus")
                    }
                }
            }
        }
        .task { await updates.checkIfDue() }
        .sheet(isPresented: $adding) {
            ServerEditView(server: Server(name: "", host: ""), password: "", isNew: true)
        }
        .sheet(item: $editing) { server in
            ServerEditView(server: server, password: store.password(for: server), isNew: false)
        }
        .sheet(isPresented: $showSettings) {
            SettingsView()
        }
        .fullScreenCover(item: $active) { server in
            SessionScreen(server: server)
        }
        .onOpenURL { url in
            if let server = store.server(for: url) {
                active = server
            }
        }
        .onAppear {
            #if DEBUG
                // Development: `simctl launch … -connect "vncbeach://host?password=…"`.
                let args = ProcessInfo.processInfo.arguments
                if let i = args.firstIndex(of: "-connect"), i + 1 < args.count,
                    let url = URL(string: args[i + 1]), let server = store.server(for: url)
                {
                    active = server
                }
            #endif
        }
    }

    @ViewBuilder
    private var content: some View {
        Group {
            if store.servers.isEmpty {
                ContentUnavailableView {
                    Label("Noch keine Computer", systemImage: "desktopcomputer")
                } description: {
                    Text(
                        "Füge einen Computer hinzu, auf dem VNC Beach oder ein anderer VNC-Server läuft. Über Tailscale genügt der Gerätename, zum Beispiel „sebastian-pc“."
                    )
                } actions: {
                    Button("Computer hinzufügen") { adding = true }
                        .buttonStyle(.borderedProminent)
                }
            } else {
                List {
                    ForEach(store.servers) { server in
                        Button {
                            active = server
                        } label: {
                            row(server)
                        }
                        .buttonStyle(.plain)
                        .contextMenu {
                            Button("Verbinden", systemImage: "play") { active = server }
                            if supportsMultipleWindows {
                                Button("In neuem Fenster öffnen", systemImage: "rectangle.split.2x1") {
                                    openWindow(value: server.id)
                                }
                            }
                            Button("Bearbeiten", systemImage: "pencil") { editing = server }
                            Button("Löschen", systemImage: "trash", role: .destructive) { store.remove(server) }
                        }
                        .swipeActions(edge: .trailing) {
                            Button(role: .destructive) { store.remove(server) } label: {
                                Label("Löschen", systemImage: "trash")
                            }
                            Button { editing = server } label: {
                                Label("Bearbeiten", systemImage: "pencil")
                            }
                            .tint(.accentColor)
                        }
                    }
                    .onMove { store.move(from: $0, to: $1) }
                }
            }
        }
    }

    private func row(_ server: Server) -> some View {
        HStack(spacing: 14) {
            Image(systemName: "desktopcomputer")
                .font(.title2)
                .foregroundStyle(Color.accentColor)
                .frame(width: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(server.displayName).font(.headline)
                Text(server.host).font(.subheadline).foregroundStyle(.secondary)
            }
            Spacer()
            if server.viewOnly {
                Image(systemName: "eye").foregroundStyle(.secondary)
                    .accessibilityLabel("Nur ansehen")
            }
            Image(systemName: "chevron.right").font(.footnote).foregroundStyle(.tertiary)
        }
        .contentShape(Rectangle())
        .padding(.vertical, 4)
    }
}
