import SwiftUI
import UniformTypeIdentifiers

struct AccessSettingsView: View {
    @Binding var settings: AccessSettings
    @Binding var importedKeys: Set<String>

    private var routes: Binding<String> {
        Binding(get: { settings.alternatives.joined(separator: "\n") }, set: {
            settings.alternatives = $0.components(separatedBy: .newlines)
        })
    }

    private var ssh: Binding<SSHSettings> {
        Binding(get: { settings.ssh ?? SSHSettings() }, set: { settings.ssh = $0 })
    }

    private var wake: Binding<WakeSettings> {
        Binding(get: { settings.wake ?? WakeSettings() }, set: { settings.wake = $0 })
    }

    var body: some View {
        Form {
            Section {
                TextEditor(text: routes)
                    .frame(minHeight: 90)
                    .accessibilityLabel("Alternative VNC-Adressen, eine pro Zeile")
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            } header: { Text("Alternative VNC-Adressen") } footer: {
                Text("Eine Adresse pro Zeile, zum Beispiel mac.local oder 100.64.0.8::5900. Sie werden der Reihe nach geprüft, wenn die Hauptadresse nicht erreichbar ist.")
            }
            Section {
                Toggle("SSH-Anmeldung anbieten", isOn: Binding(
                    get: { settings.ssh != nil },
                    set: { settings.ssh = $0 ? SSHSettings() : nil }))
                if settings.ssh != nil {
                    SSHFields(settings: ssh, importedKeys: $importedKeys)
                }
            } header: { Text("SSH-Anmeldung am Zielgerät") } footer: {
                Text("Optional vor der VNC-Verbindung anmelden, etwa wenn ein Mac nach dem Neustart auf FileVault wartet und dort SSH verfügbar ist. Passwörter werden bei Bedarf abgefragt und nicht gespeichert.")
            }
            Section {
                Toggle("Aufwecken anbieten", isOn: Binding(
                    get: { settings.wake != nil },
                    set: { settings.wake = $0 ? WakeSettings() : nil }))
                if settings.wake != nil {
                    AccessTextField("MAC-Adresse", text: wake.mac)
                        .accessibilityHint("Zum Beispiel 00:11:22:33:44:55")
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    AccessTextField("Broadcast-Adresse mit Port", text: wake.destination)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    NavigationLink {
                        WakeRelaysView(settings: wake, importedKeys: $importedKeys)
                    } label: {
                        LabeledContent("Zugangsgeräte", value: "\(settings.wake?.relays.count ?? 0)")
                    }
                    .accessibilityIdentifier("wake-relays")
                }
            } header: { Text("Wake-on-LAN") } footer: {
                Text("Auf iPhone und iPad mindestens ein erreichbares Zugangsgerät im LAN des Computers eintragen, etwa Raspberry Pi, NAS, Mac oder Windows-PC. Der Zielcomputer muss Wake-on-LAN unterstützen und dafür eingerichtet sein.")
            }
        }
        .navigationTitle("Erreichbarkeit & Start")
        .navigationBarTitleDisplayMode(.inline)
    }
}

private struct WakeRelaysView: View {
    @Binding var settings: WakeSettings
    @Binding var importedKeys: Set<String>

    var body: some View {
        List {
            Section {
                ForEach($settings.relays) { $relay in
                    NavigationLink {
                        Form {
                            Section("SSH-Verbindung") {
                                SSHFields(settings: $relay.ssh, importedKeys: $importedKeys)
                            }
                            Section {
                                Picker("Betriebssystem", selection: $relay.shell) {
                                    ForEach(RelayShell.allCases, id: \.self) { shell in
                                        Text(shell.label).tag(shell)
                                    }
                                }
                            } footer: {
                                Text("macOS, Linux und NAS benötigen Python 3 oder Perl. Windows benötigt PowerShell. Das Gerät sendet das Aufwecksignal aus seinem LAN.")
                            }
                        }
                        .navigationTitle("Zugangsgerät")
                    } label: {
                        Text(relay.ssh.address.isEmpty ? "Neues Zugangsgerät" : relay.ssh.address)
                    }
                }
                .onDelete { settings.relays.remove(atOffsets: $0) }
                .onMove { settings.relays.move(fromOffsets: $0, toOffset: $1) }
                Button("Zugangsgerät hinzufügen", systemImage: "plus") {
                    settings.relays.append(WakeRelay())
                }
            } footer: {
                Text("Die Geräte werden der Reihe nach versucht. Sobald eines das Signal gesendet hat, wird auf VNC gewartet. Über „Bearbeiten“ lässt sich die Reihenfolge ändern.")
            }
        }
        .navigationTitle("Zugangsgeräte")
        .toolbar { EditButton() }
    }
}

private struct SSHFields: View {
    @Binding var settings: SSHSettings
    @Binding var importedKeys: Set<String>
    @State private var importing = false
    @State private var error: String?

    private var useKey: Binding<Bool> {
        Binding(get: { if case .key = settings.auth { return true }; return false }, set: {
            settings.auth = $0 ? .key("") : .password
            settings.keyName = nil
        })
    }

    var body: some View {
        AccessTextField("SSH-Adresse", text: $settings.address)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .accessibilityHint("Rechnername oder IP-Adresse, optional mit SSH-Port. Standard ist 22.")
        AccessTextField("SSH-Benutzername", text: $settings.user)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .textContentType(.username)
        Toggle("SSH-Schlüssel verwenden", isOn: useKey)
        if useKey.wrappedValue {
            Button { importing = true } label: {
                Label(settings.keyName ?? "Privaten Schlüssel importieren", systemImage: "key")
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.data, .item]) { result in
                do {
                    let url = try result.get()
                    let id = try SSHKeyStore.importKey(from: url)
                    importedKeys.insert(id)
                    settings.auth = .key(id)
                    settings.keyName = url.lastPathComponent
                    error = nil
                } catch { self.error = error.localizedDescription }
            }
            Text("Im Schlüsselbund gespeichert. Eine Passphrase wird erst beim Verbinden abgefragt.")
                .font(.footnote).foregroundStyle(.secondary)
        }
        if let error { Text(error).foregroundStyle(.red).accessibilityLabel("Fehler: \(error)") }
    }
}

private struct AccessTextField: View {
    let title: String
    @Binding var text: String

    init(_ title: String, text: Binding<String>) {
        self.title = title
        self._text = text
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            TextField(title, text: $text)
                .accessibilityLabel(title)
        }
        .padding(.vertical, 3)
    }
}
