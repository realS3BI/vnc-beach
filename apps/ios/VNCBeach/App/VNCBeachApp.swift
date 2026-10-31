import SwiftUI

@main
struct VNCBeachApp: App {
    @State private var store = ServerStore()
    @State private var updates = UpdateChecker()
    @State private var account = AccountStore()

    var body: some Scene {
        WindowGroup {
            ServerListView()
                .environment(store)
                .environment(updates)
                .environment(account)
        }
        .handlesExternalEvents(matching: ["vncbeach"])

        // A favourite can also open in its own window, so two computers fit
        // side by side in Split View or Stage Manager.
        WindowGroup("Sitzung", for: Server.ID.self) { $serverID in
            if let serverID, let server = store.server(id: serverID) ?? account.server(id: serverID) {
                SessionScreen(server: server)
                    .environment(store)
                    .environment(account)
            } else {
                ContentUnavailableView(
                    "Verbindung nicht gefunden", systemImage: "display.trianglebadge.exclamationmark")
            }
        }
    }
}
