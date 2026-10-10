import SwiftUI

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(UpdateChecker.self) private var updates
    @AppStorage(AppSettings.colorDepthKey) private var colorDepth = ColorDepth.full.rawValue
    @AppStorage(AppSettings.inputModeKey) private var inputMode = InputMode.touchpad.rawValue
    @AppStorage(AppSettings.shortcutMappingKey) private var shortcutMapping = ShortcutMapping.auto.rawValue
    @AppStorage(AppSettings.clipboardSyncKey) private var clipboardSync = true
    @AppStorage(AppSettings.touchpadSpeedKey) private var touchpadSpeed = 1.0

    var body: some View {
        NavigationStack {
            Form {
                Section("Darstellung") {
                    Picker("Farben", selection: $colorDepth) {
                        ForEach(ColorDepth.allCases) { depth in
                            Text(depth.label).tag(depth.rawValue)
                        }
                    }
                }
                Section {
                    Picker("Fingereingabe", selection: $inputMode) {
                        ForEach(InputMode.allCases) { mode in
                            Text(mode.label).tag(mode.rawValue)
                        }
                    }
                    .pickerStyle(.segmented)
                    VStack(alignment: .leading) {
                        Text("Touchpad-Geschwindigkeit")
                        Slider(value: $touchpadSpeed, in: 0.5...2.5, step: 0.25)
                    }
                } header: {
                    Text("Eingabe")
                } footer: {
                    Text(
                        "Touchpad: Der Finger bewegt den Zeiger, Tippen klickt an der Zeigerposition. Direkt: Tippen klickt dort, wo der Finger ist. Trackpad, Maus und Apple Pencil arbeiten immer direkt."
                    )
                }
                Section {
                    Picker("Tastenkürzel", selection: $shortcutMapping) {
                        ForEach(ShortcutMapping.allCases) { mapping in
                            Text(mapping.label).tag(mapping.rawValue)
                        }
                    }
                } footer: {
                    Text(
                        "„Automatisch“ sendet ⌘ als Strg, wenn der entfernte Rechner kein Mac ist, damit ⌘C auf Windows kopiert."
                    )
                }
                Section {
                    Toggle("Zwischenablage abgleichen", isOn: $clipboardSync)
                } footer: {
                    Text(
                        "Text, der hier kopiert wurde, steht nach dem Verbinden und bei jeder Rückkehr zur App auf dem entfernten Rechner bereit und umgekehrt."
                    )
                }
                Section("Gesten") {
                    gesture("Tippen", "Linksklick")
                    gesture("Gedrückt halten und ziehen", "Ziehen mit linker Taste")
                    gesture("Mit zwei Fingern tippen", "Rechtsklick")
                    gesture("Mit zwei Fingern wischen", "Scrollen")
                    gesture("Auf- und zuziehen", "Zoom")
                    gesture("Apple Pencil schweben", "Zeiger bewegen")
                }
                Section("Über") {
                    Text("Version \(Self.version)")
                    updateRow
                    Link("VNC Beach auf GitHub", destination: URL(string: "https://github.com/\(UpdateChecker.repository)")!)
                }
            }
            .navigationTitle("Einstellungen")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Fertig") { dismiss() }
                }
            }
        }
    }

    @ViewBuilder
    private var updateRow: some View {
        switch updates.status {
        case .checking:
            HStack {
                ProgressView()
                Text("Suche nach Updates …")
            }
        case .available(let release):
            VStack(alignment: .leading, spacing: 6) {
                Text("VNC Beach \(release.version.description) ist verfügbar.")
                Button("In TestFlight öffnen") { openTestFlight() }
            }
        case .upToDate, .unavailable, .idle:
            Button("Nach Updates suchen") {
                Task { await updates.check() }
            }
            if updates.status == .upToDate {
                Text("VNC Beach ist aktuell (\(updates.currentVersion?.description ?? Self.version)).")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            } else if updates.status == .unavailable {
                Text("Keine Update-Informationen verfügbar.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func gesture(_ name: String, _ effect: String) -> some View {
        LabeledContent(name, value: effect)
    }

    private static var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (Build \(build))"
    }
}
