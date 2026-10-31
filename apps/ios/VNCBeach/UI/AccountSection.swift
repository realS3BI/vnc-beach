import SwiftUI

/// The "Konto" section of the settings: coordination server, sign-in and -out.
struct AccountSection: View {
    @Environment(AccountStore.self) private var account
    @State private var address = ""

    var body: some View {
        Section {
            if account.signedIn {
                if let user = account.user {
                    LabeledContent(user.name, value: user.login)
                }
                LabeledContent("Server", value: account.url.replacingOccurrences(of: "https://", with: ""))
                if !account.orgs.isEmpty {
                    LabeledContent("Organisationen", value: account.orgs.map(\.name).joined(separator: ", "))
                }
                if let manage = URL(string: account.url) {
                    Link("Organisationen verwalten", destination: manage)
                }
                Button("Abmelden", role: .destructive) {
                    Task { await account.signOut() }
                }
            } else {
                TextField("Koordinationsserver", text: $address, prompt: Text("z. B. beach.example.com"))
                    .textContentType(.URL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .onSubmit(signIn)
                Button(action: signIn) {
                    HStack {
                        Text(account.signingIn ? "Anmeldung läuft …" : "Anmelden")
                        if account.signingIn {
                            Spacer()
                            ProgressView()
                        }
                    }
                }
                .disabled(account.signingIn || address.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            if case .failed(let message) = account.status {
                Text(message).font(.footnote).foregroundStyle(.red)
            }
        } header: {
            Text("Konto")
        } footer: {
            Text(
                account.signedIn
                    ? "Die Computer deines Kontos und deiner Organisationen stehen in der Übersicht und verbinden ohne VNC-Passwort. Deine Favoriten werden mit deinen anderen Geräten abgeglichen, Passwörter bleiben auf jedem Gerät. Einen Computer fügst du hinzu, indem du dich verbindest und im Menü „Zum Konto hinzufügen …“ wählst."
                    : "Mit einem Konto auf einem VNC-Beach-Koordinationsserver siehst du hier alle Computer, die du am Mac oder PC hinzugefügt hast, und verbindest dich ohne VNC-Passwort. Angemeldet wird mit GitHub oder Google im Browser."
            )
        }
        .onAppear { if address.isEmpty { address = account.url } }
    }

    private func signIn() {
        guard !account.signingIn else { return }
        Task { await account.signIn(address: address) }
    }
}

/// One account device in the computer list.
struct AccountDeviceRow: View {
    let device: AccountDevice

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: device.platform == "windows" ? "pc" : "desktopcomputer")
                .font(.title2)
                .foregroundStyle(device.online ? Color.accentColor : .secondary)
                .frame(width: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(device.name).font(.headline)
                HStack(spacing: 6) {
                    Circle()
                        .fill(device.online ? Color.green : Color.secondary.opacity(0.5))
                        .frame(width: 8, height: 8)
                        .accessibilityHidden(true)
                    Text(device.status()).font(.subheadline).foregroundStyle(.secondary)
                }
            }
            Spacer()
            Image(systemName: "chevron.right").font(.footnote).foregroundStyle(.tertiary)
        }
        .contentShape(Rectangle())
        .padding(.vertical, 4)
        .opacity(device.addresses.isEmpty ? 0.6 : 1)
    }
}
