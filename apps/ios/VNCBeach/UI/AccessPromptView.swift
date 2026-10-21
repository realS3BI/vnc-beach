import SwiftUI

struct AccessPromptView: View {
    let prompt: AccessPrompt
    let answer: (String?) -> Void
    let cancel: () -> Void
    @State private var input = ""
    @State private var showInput = false
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            Form {
                switch prompt.question {
                case .trust(let address, let fingerprint):
                    Section {
                        Text("Erste SSH-Verbindung zu \(address). Prüfe den Fingerabdruck am Gerät oder über einen bekannten Zugang.")
                        Text(fingerprint)
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                    } header: { Text("SSH-Gerät bestätigen") }
                    Section {
                        Button("Vertrauen & fortfahren") { answer(nil) }
                    }
                case .secret(let title, let label, let echo):
                    Section {
                        Text(label).font(.caption).foregroundStyle(.secondary)
                        if echo || showInput {
                            TextField(label, text: $input).focused($focused)
                        } else {
                            SecureField(label, text: $input).focused($focused)
                        }
                        if !echo { Toggle("Eingabe anzeigen", isOn: $showInput) }
                    } header: { Text(title) } footer: {
                        Text("Diese Eingabe wird nur für diesen Versuch verwendet und nicht gespeichert.")
                    }
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .onSubmit { submit() }
                    Section { Button("Anmelden") { submit() } }
                }
            }
            .navigationTitle("SSH-Anmeldung")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Abbrechen") { input = ""; cancel() }
                }
            }
            .task { if case .secret = prompt.question { focused = true } }
        }
        .interactiveDismissDisabled()
        .onDisappear { input = "" }
    }

    private func submit() {
        let value = input
        input = ""
        answer(value)
    }
}
