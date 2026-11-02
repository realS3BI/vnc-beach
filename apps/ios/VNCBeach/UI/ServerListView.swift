import SwiftUI

struct ServerListView: View {
    @Environment(ServerStore.self) private var store
    @Environment(UpdateChecker.self) private var updates
    @Environment(AccountStore.self) private var account
    @Environment(\.openWindow) private var openWindow
    @Environment(\.supportsMultipleWindows) private var supportsMultipleWindows
    @State private var editing: Server?
    @State private var adding = false
    @State private var active: Server?
    @State private var showSettings = false
    @State private var notice: String?
    @State private var deleting: Server?
    @State private var search = ""
    @State private var confirmUpdateAll = false

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
                        Label("Verbindung hinzufügen", systemImage: "plus")
                    }
                }
            }
        }
        .task { await updates.checkIfDue() }
        .task {
            account.favoriteStore = store
            await account.refresh()
        }
        .onChange(of: store.revision) {
            Task { await account.syncFavorites() }
        }
        .alert("Nicht erreichbar", isPresented: Binding(get: { notice != nil }, set: { if !$0 { notice = nil } })) {
            Button("OK") {}
        } message: {
            Text(notice ?? "")
        }
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

    private var grid: [GridItem] {
        [GridItem(.adaptive(minimum: 260), spacing: 14, alignment: .top)]
    }

    private var content: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 32) {
                accountSection
                favourites
            }
            .padding(.horizontal, 20)
            .padding(.top, 12)
            .padding(.bottom, 32)
            .frame(maxWidth: 1100)
            .frame(maxWidth: .infinity)
        }
        .background(Beach.canvas.ignoresSafeArea())
        .refreshable {
            // Detached from the gesture: SwiftUI cancels the refresh task
            // as soon as the list changes underneath it.
            await Task { await account.refresh() }.value
        }
        .confirmationDialog(
            "Verbindung „\(deleting?.displayName ?? "")“ löschen?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible
        ) {
            Button("Löschen", role: .destructive) {
                if let deleting { store.remove(deleting) }
                deleting = nil
            }
        } message: {
            Text("Das gespeicherte Passwort wird mit gelöscht.")
        }
    }

    // MARK: Further connections (saved by hand, next to the account's computers)

    private var filteredServers: [Server] {
        let filter = search.trimmingCharacters(in: .whitespaces).lowercased()
        guard !filter.isEmpty else { return store.servers }
        return store.servers.filter {
            $0.displayName.lowercased().contains(filter) || $0.host.lowercased().contains(filter)
        }
    }

    @ViewBuilder
    private var favourites: some View {
        VStack(alignment: .leading, spacing: 12) {
            SectionHeader(title: "Weitere Verbindungen", count: store.servers.count) {
                if store.servers.count >= 8 {
                    TextField("Verbindungen durchsuchen", text: $search)
                        .textFieldStyle(.roundedBorder)
                        .frame(maxWidth: 220)
                }
            }
            if store.servers.isEmpty {
                Text("Für Computer ohne VNC Beach oder außerhalb deines Kontos: Name, Adresse und Passwort speichern, dann verbindet ein Tippen.")
                    .font(.subheadline).foregroundStyle(Beach.weak)
                    .fixedSize(horizontal: false, vertical: true)
                LazyVGrid(columns: grid, spacing: 14) {
                    Button { adding = true } label: { AddCard() }
                        .buttonStyle(CardButtonStyle())
                        .accessibilityLabel("Neue Verbindung")
                }
            } else if filteredServers.isEmpty {
                Text("Keine Verbindung passt zur Suche.").foregroundStyle(Beach.weak)
            } else {
                LazyVGrid(columns: grid, spacing: 14) {
                    ForEach(filteredServers) { server in
                        favourite(server)
                    }
                    if search.isEmpty {
                        Button { adding = true } label: { AddCard() }
                            .buttonStyle(CardButtonStyle())
                            .accessibilityLabel("Neue Verbindung")
                    }
                }
            }
        }
    }

    private func favourite(_ server: Server) -> some View {
        Button {
            active = server
        } label: {
            ComputerCard(
                name: server.displayName, line: server.host,
                tags: server.viewOnly ? ["Nur ansehen"] : [])
        }
        .buttonStyle(CardButtonStyle())
        .contextMenu {
            Button("Verbinden", systemImage: "play") { active = server }
            if supportsMultipleWindows {
                Button("In neuem Fenster öffnen", systemImage: "rectangle.split.2x1") {
                    openWindow(value: server.id)
                }
            }
            Button("Bearbeiten", systemImage: "pencil") { editing = server }
            Divider()
            Button("Löschen", systemImage: "trash", role: .destructive) { deleting = server }
        }
    }

    // MARK: Account

    @ViewBuilder
    private var accountSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            SectionHeader(
                title: "Konto", count: account.signedIn ? account.devices.count : nil,
                detail: account.signedIn ? account.user?.name : nil
            ) {
                if account.signedIn {
                    if account.requestingUpdates {
                        ProgressView()
                        Text("Updates werden angefordert …").font(.subheadline).foregroundStyle(Beach.weak)
                    } else if !account.outdated.isEmpty {
                        Button("Alle aktualisieren (\(account.outdated.count))") { confirmUpdateAll = true }
                            .buttonStyle(.bordered)
                            .tint(Beach.accent)
                    }
                }
                if account.signedIn {
                    Button("Konto verwalten") { showSettings = true }
                        .tint(Beach.accent)
                }
            }
            .confirmationDialog(
                "\(account.outdated.count) Geräte aktualisieren?", isPresented: $confirmUpdateAll,
                titleVisibility: .visible
            ) {
                Button("Alle aktualisieren") { Task { await account.requestUpdates(account.outdated) } }
            } message: {
                Text("Die Geräte installieren die neueste Version und starten neu. Laufende Verbindungen zu ihnen brechen dabei kurz ab.")
            }
            if account.expired {
                NoticeCard(title: "Anmeldung abgelaufen", text: "Bitte neu anmelden, damit die Geräte des Kontos wieder erscheinen.") {
                    Button("Neu anmelden …") { showSettings = true }
                        .buttonStyle(.borderedProminent)
                        .tint(Beach.accent)
                }
            } else if !account.signedIn {
                NoticeCard(
                    title: "Nicht angemeldet",
                    text: "Mit einem Konto erscheinen hier alle deine Computer und die deines Teams, und du verbindest dich ohne VNC-Passwort."
                ) {
                    Button("Anmelden …") { showSettings = true }
                        .buttonStyle(.borderedProminent)
                        .tint(Beach.accent)
                }
            } else {
                if case .failed(let message) = account.status {
                    HStack(alignment: .firstTextBaseline) {
                        Text(message).font(.subheadline).foregroundStyle(Beach.coral)
                        Button("Erneut laden") { Task { await account.refresh() } }
                            .font(.subheadline)
                    }
                }
                if account.devices.isEmpty {
                    if account.status == .loading {
                        HStack(spacing: 10) {
                            ProgressView()
                            Text("Geräte werden geladen …").foregroundStyle(Beach.weak)
                        }
                    } else {
                        NoticeCard(
                            title: "Noch keine Geräte im Konto",
                            text: "Installiere VNC Beach auf einem Mac oder PC und melde dich dort an. Oder verbinde dich mit einer gespeicherten Verbindung, auf der VNC Beach läuft, und wähle im Menü „Zum Konto hinzufügen …“."
                        ) {
                            EmptyView()
                        }
                    }
                }
                ForEach(account.groups, id: \.title) { group in
                    VStack(alignment: .leading, spacing: 8) {
                        if account.groups.count > 1 || group.title != "Persönlich" {
                            Text(group.title)
                                .font(.system(size: 14, weight: .semibold))
                                .foregroundStyle(Beach.weak)
                        }
                        LazyVGrid(columns: grid, spacing: 14) {
                            ForEach(group.devices) { device in
                                deviceCard(device)
                            }
                        }
                    }
                }
            }
        }
    }

    private func deviceCard(_ device: AccountDevice) -> some View {
        Button { open(device) } label: {
            ComputerCard(
                name: device.name, line: device.status(), tags: device.updateTag.map { [$0] } ?? [],
                online: device.online, dimmed: device.addresses.isEmpty)
        }
        .buttonStyle(CardButtonStyle())
        .contextMenu {
            Button("Verbinden", systemImage: "play") { open(device) }
            if supportsMultipleWindows && !device.addresses.isEmpty {
                Button("In neuem Fenster öffnen", systemImage: "rectangle.split.2x1") {
                    openWindow(value: account.server(for: device).id)
                }
            }
            Button(device.outdated ? "Aktualisieren" : "Neueste Version installieren", systemImage: "arrow.down.circle") {
                Task { await account.requestUpdates([device]) }
            }
            .disabled(!account.mayUpdate(device) || account.requestingUpdates)
            Divider()
            if case .failed(let error) = device.update {
                Text("Update fehlgeschlagen: \(error)")
            }
            if !device.version.isEmpty {
                Text("Version \(device.version)")
            }
            if !device.canUpdate {
                Text("Diese Version lässt sich nur am Gerät selbst aktualisieren.")
            }
        }
    }

    private func open(_ device: AccountDevice) {
        let server = account.server(for: device)
        guard !server.host.isEmpty else {
            notice = "\(device.name) hat noch keine Adresse gemeldet. Läuft VNC Beach dort?"
            return
        }
        active = server
    }
}

/// Cards shrink a little while pressed, like a native button.
struct CardButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .brightness(configuration.isPressed ? -0.03 : 0)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}
