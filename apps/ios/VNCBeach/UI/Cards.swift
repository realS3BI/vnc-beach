import SwiftUI
import UIKit

/// The desktop viewer's colours (`beach-viewer/src/theme.rs`), light and dark.
enum Beach {
    static let sky = Color(red: 63 / 255, green: 180 / 255, blue: 230 / 255)
    static let sea = Color(red: 30 / 255, green: 151 / 255, blue: 204 / 255)
    static let sun = Color(red: 1, green: 211 / 255, blue: 92 / 255)
    static let ink = Color(red: 23 / 255, green: 54 / 255, blue: 74 / 255)
    static let coral = Color(red: 240 / 255, green: 90 / 255, blue: 90 / 255)
    static let green = Color(red: 70 / 255, green: 190 / 255, blue: 120 / 255)
    static let amber = Color(red: 240 / 255, green: 180 / 255, blue: 60 / 255)

    static let accent = dynamic(light: (30, 151, 204), dark: (63, 180, 230))
    static let onAccent = dynamic(light: (255, 255, 255), dark: (9, 30, 44))
    static let canvas = dynamic(light: (247, 249, 251), dark: (22, 31, 40))
    static let card = dynamic(light: (255, 255, 255), dark: (29, 40, 52))
    static let cardStroke = dynamic(light: (220, 228, 235), dark: (45, 60, 75))
    static let button = dynamic(light: (241, 245, 248), dark: (39, 53, 67))
    static let text = dynamic(light: (23, 54, 74), dark: (230, 237, 243))
    static let weak = dynamic(light: (98, 120, 136), dark: (139, 158, 174))

    // SwiftUI resolves these on its render threads.
    nonisolated private static func dynamic(light: (Int, Int, Int), dark: (Int, Int, Int)) -> Color {
        Color(UIColor { traits in
            let (r, g, b) = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(red: CGFloat(r) / 255, green: CGFloat(g) / 255, blue: CGFloat(b) / 255, alpha: 1)
        })
    }

    /// Avatar colour, text colour and monogram, chosen by name exactly like
    /// `theme::avatar`, so a computer looks the same on every device.
    static func avatar(_ name: String) -> (fill: Color, text: Color, monogram: String) {
        let colors: [(Color, Bool)] = [
            (sky, false), (sea, true), (sun, false),
            (Color(red: 242 / 255, green: 140 / 255, blue: 110 / 255), true),
            (Color(red: 86 / 255, green: 196 / 255, blue: 168 / 255), false),
            (Color(red: 152 / 255, green: 140 / 255, blue: 232 / 255), true),
            (Color(red: 232 / 255, green: 180 / 255, blue: 90 / 255), false),
        ]
        let hash = name.utf8.reduce(UInt32(7)) { $0 &* 31 &+ UInt32($1) }
        let (fill, lightText) = colors[Int(hash % UInt32(colors.count))]
        let words = name.split(whereSeparator: \.isWhitespace).compactMap(\.first)
        var monogram = words.first.map(String.init) ?? ""
        if words.count > 1, let last = words.last { monogram.append(last) }
        if monogram.isEmpty { monogram = "?" }
        return (fill, lightText ? .white : ink, monogram.uppercased())
    }
}

/// Section title with a count pill, like `theme::section_header`.
struct SectionHeader<Trailing: View>: View {
    let title: String
    var count: Int?
    var detail: String?
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(spacing: 8) {
            Text(title).font(.system(size: 19, weight: .bold)).foregroundStyle(Beach.text)
            if let count, count > 0 {
                Text("\(count)")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Beach.weak)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(Beach.weak.opacity(0.14)))
            }
            if let detail {
                Text(detail).font(.subheadline).foregroundStyle(Beach.weak).lineLimit(1)
            }
            Spacer(minLength: 8)
            trailing
        }
        .accessibilityElement(children: .contain)
    }
}

/// One computer, like the desktop overview's card: avatar with monogram,
/// name, address or status line, tags, and the play button.
struct ComputerCard: View {
    let name: String
    let line: String
    var tags: [String] = []
    /// Online dot in front of the line: green, grey, or none.
    var online: Bool?
    /// Dimmed while the device can't be reached (no address).
    var dimmed = false

    var body: some View {
        let avatar = Beach.avatar(name)
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 12) {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(avatar.fill)
                    .frame(width: 44, height: 44)
                    .overlay(
                        Text(avatar.monogram)
                            .font(.system(size: 18, weight: .bold))
                            .foregroundStyle(avatar.text)
                    )
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(name)
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(Beach.text)
                        .lineLimit(1)
                    HStack(spacing: 6) {
                        if let online {
                            Circle()
                                .fill(online ? Beach.green : Beach.weak)
                                .frame(width: 7, height: 7)
                                .background(
                                    Circle().fill((online ? Beach.green : Beach.weak).opacity(0.25))
                                        .frame(width: 13, height: 13)
                                )
                                .frame(width: 13, height: 13)
                                .accessibilityHidden(true)
                        }
                        Text(line)
                            .font(.system(size: 13.5))
                            .foregroundStyle(Beach.weak)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 4)
                Circle()
                    .fill(Beach.accent)
                    .frame(width: 36, height: 36)
                    .overlay(
                        Image(systemName: "play.fill")
                            .font(.system(size: 13, weight: .bold))
                            .foregroundStyle(Beach.onAccent)
                            .offset(x: 1)
                    )
                    .opacity(dimmed ? 0.35 : 1)
                    .accessibilityHidden(true)
            }
            Spacer(minLength: 8)
            if !tags.isEmpty {
                HStack(spacing: 6) {
                    ForEach(tags, id: \.self) { tag in
                        TagView(text: tag, color: tag == ComputerCard.failedTag ? Beach.coral : Beach.weak)
                    }
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, minHeight: ComputerCard.height, maxHeight: ComputerCard.height, alignment: .topLeading)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Beach.card))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Beach.cardStroke))
        .opacity(dimmed ? 0.7 : 1)
        .contentShape(.contextMenuPreview, RoundedRectangle(cornerRadius: 14, style: .continuous))
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityLabel(([name, line] + tags).joined(separator: ", "))
        .accessibilityAddTraits(.isButton)
        .accessibilityHint("Verbinden")
    }

    static let height: CGFloat = 104
    static let failedTag = "Update fehlgeschlagen"
}

/// A small capsule label at the bottom of a card.
private struct TagView: View {
    let text: String
    let color: Color

    var body: some View {
        Text(text)
            .font(.system(size: 11.5, weight: .semibold))
            .foregroundStyle(color)
            .lineLimit(1)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(Capsule().fill(color.opacity(0.13)))
    }
}

/// The dashed "+ Neue Verbindung" cell at the end of the favourites.
struct AddCard: View {
    var body: some View {
        RoundedRectangle(cornerRadius: 14, style: .continuous)
            .strokeBorder(Beach.weak.opacity(0.6), style: StrokeStyle(lineWidth: 1, dash: [6, 6]))
            .frame(height: ComputerCard.height)
            .overlay(
                Label("Neue Verbindung", systemImage: "plus")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Beach.weak)
            )
            .contentShape(Rectangle())
    }
}

/// A card-shaped notice for empty or signed-out sections.
struct NoticeCard<Actions: View>: View {
    let title: String
    let text: String
    @ViewBuilder var actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.system(size: 16, weight: .semibold)).foregroundStyle(Beach.text)
            Text(text).font(.subheadline).foregroundStyle(Beach.weak)
                .fixedSize(horizontal: false, vertical: true)
            actions.padding(.top, 6)
        }
        .padding(20)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).fill(Beach.card))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Beach.cardStroke))
    }
}
