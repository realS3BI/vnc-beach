import SwiftUI
import UIKit

/// "A newer VNC Beach exists": iOS can't install it, TestFlight can.
struct UpdateBanner: View {
    let release: UpdateChecker.Release
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "arrow.down.circle.fill")
                .font(.title2)
                .foregroundStyle(Color.accentColor)
            Text("VNC Beach \(release.version.description) ist verfügbar.")
                .font(.subheadline.weight(.medium))
            Spacer()
            Button("In TestFlight öffnen") { openTestFlight() }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark")
                    .font(.footnote.weight(.bold))
                    .foregroundStyle(.secondary)
            }
            .accessibilityLabel("Hinweis ausblenden")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.regularMaterial)
    }
}

/// The TestFlight app, or its website if it isn't installed.
@MainActor
func openTestFlight() {
    UIApplication.shared.open(UpdateChecker.testFlightURL) { opened in
        if !opened {
            UIApplication.shared.open(UpdateChecker.testFlightFallbackURL)
        }
    }
}
