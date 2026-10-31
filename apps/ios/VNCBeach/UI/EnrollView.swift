import SwiftUI

/// "Zum Konto hinzufügen": registers the computer of the running session in
/// the account, personal or for an organization.
struct EnrollView: View {
    let session: RemoteSession
    @Environment(AccountStore.self) private var account
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var orgID: String?
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                if let hello = session.enrollHello {
                    content(hello)
                } else {
                    Text("Dieser Computer kann nicht über die App hinzugefügt werden. Füge ihn am Computer selbst unter Einstellungen → Konto hinzu.")
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Zum Konto hinzufügen")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(session.enrollHello?.joined == true ? "Fertig" : "Abbrechen") { dismiss() }
                }
                if canAdd {
                    ToolbarItem(placement: .confirmationAction) {
                        if busy {
                            ProgressView()
                        } else {
                            Button("Hinzufügen", action: add)
                                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
                        }
                    }
                }
            }
            .onAppear {
                if name.isEmpty { name = session.enrollHello?.name ?? "" }
            }
            .interactiveDismissDisabled(busy)
        }
    }

    private var canAdd: Bool {
        guard let hello = session.enrollHello else { return false }
        return account.signedIn && hello.device == nil && !hello.joined
    }

    @ViewBuilder
    private func content(_ hello: EnrollHello) -> some View {
        if hello.joined {
            Section {
                Label("„\(name)“ ist jetzt im Konto.", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(Beach.green)
            } footer: {
                Text("Er erscheint in der Übersicht unter „Konto“ und verbindet sich von jedem deiner Geräte ohne VNC-Passwort.")
            }
        } else if let device = hello.device {
            Section {
                LabeledContent("Im Konto als", value: device.name)
                LabeledContent("Server", value: device.url.replacingOccurrences(of: "https://", with: ""))
            } footer: {
                Text("Dieser Computer ist schon in einem Konto. Entfernen geht am Computer selbst unter Einstellungen → Konto.")
            }
        } else if !account.signedIn {
            Section {
                Text("Melde dich zuerst in den Einstellungen unter „Konto“ an.")
                    .foregroundStyle(.secondary)
            }
        } else {
            Section {
                TextField("Name", text: $name)
                    .textInputAutocapitalization(.words)
                    .submitLabel(.done)
                    .onSubmit(add)
            } header: {
                Text("Name in der Übersicht")
            }
            Section {
                Picker("Gehört", selection: $orgID) {
                    Text("Persönlich (nur du)").tag(String?.none)
                    ForEach(account.orgs) { org in
                        Text("\(org.name) (alle Mitglieder)").tag(Optional(org.id))
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            } header: {
                Text("Wer darf sich verbinden")
            } footer: {
                Text("Wer Zugriff hat, verbindet sich über das Konto ohne VNC-Passwort. Das Passwort gilt weiterhin für andere VNC-Viewer. Der Computer meldet ab jetzt selbst seine Adressen an \(account.url.replacingOccurrences(of: "https://", with: "")).")
            }
            if let error {
                Section {
                    Text(error).foregroundStyle(Beach.coral)
                }
            }
        }
    }

    private func add() {
        guard canAdd, !busy, !name.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        busy = true
        error = nil
        let org = account.orgs.first { $0.id == orgID }
        Task {
            error = await account.enroll(session, name: name, org: org)
            busy = false
        }
    }
}
