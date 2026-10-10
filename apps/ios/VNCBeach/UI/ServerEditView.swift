import SwiftUI

struct ServerEditView: View {
    @State var server: Server
    @State var password: String
    let isNew: Bool
    @Environment(ServerStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var browser = ServiceBrowser()
    @State private var resolving: String?

    private var canSave: Bool {
        !server.host.trimmingCharacters(in: .whitespaces).isEmpty
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $server.name)
                        .textInputAutocapitalization(.words)
                    TextField("Adresse", text: $server.host)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                    SecureField("Passwort", text: $password)
                } footer: {
                    Text(
                        "Rechnername, IP-Adresse oder Tailscale-Name, optional mit Display („pc:1“) oder Port („pc::5901“). Ohne Angabe wird Port 5900 verwendet."
                    )
                }
                Section {
                    Toggle("Nur ansehen", isOn: $server.viewOnly)
                } footer: {
                    Text("Zeigt den Bildschirm, ohne Maus und Tastatur zu senden.")
                }
                if !browser.services.isEmpty {
                    Section("In der Nähe") {
                        ForEach(browser.services) { service in
                            Button {
                                use(service)
                            } label: {
                                HStack {
                                    Label(service.name, systemImage: "bonjour")
                                    Spacer()
                                    if resolving == service.id {
                                        ProgressView()
                                    }
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle(isNew ? "Computer hinzufügen" : "Computer bearbeiten")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Abbrechen") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Sichern") {
                        server.host = server.host.trimmingCharacters(in: .whitespaces)
                        store.save(server, password: password)
                        dismiss()
                    }
                    .disabled(!canSave)
                }
            }
            .onAppear { browser.start() }
            .onDisappear { browser.stop() }
        }
    }

    private func use(_ service: DiscoveredService) {
        resolving = service.id
        if server.name.isEmpty {
            server.name = service.name
        }
        Task {
            if let address = await ServiceBrowser.resolve(service.endpoint) {
                server.host = address
            }
            resolving = nil
        }
    }
}
