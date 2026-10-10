import UIKit

/// Keys the software keyboard lacks, shown above it. Modifier buttons stay
/// selected until the next key.
@MainActor
final class ExtraKeysBar: UIInputView {
    private unowned let owner: RemoteScreenView
    private var modifierButtons: [UInt32: UIButton] = [:]

    private struct Item {
        let title: String
        let symbol: String?
        let keysym: UInt32
        let modifier: Bool
    }

    private static let items: [Item] = [
        Item(title: "Esc", symbol: nil, keysym: Keysym.escape, modifier: false),
        Item(title: "Tab", symbol: "arrow.right.to.line", keysym: Keysym.tab, modifier: false),
        Item(title: "Strg", symbol: nil, keysym: Keysym.control, modifier: true),
        Item(title: "Alt", symbol: nil, keysym: Keysym.alt, modifier: true),
        Item(title: "Win", symbol: nil, keysym: Keysym.super, modifier: true),
        Item(title: "Umschalt", symbol: "shift", keysym: Keysym.shift, modifier: true),
        Item(title: "Links", symbol: "arrow.left", keysym: Keysym.left, modifier: false),
        Item(title: "Oben", symbol: "arrow.up", keysym: Keysym.up, modifier: false),
        Item(title: "Unten", symbol: "arrow.down", keysym: Keysym.down, modifier: false),
        Item(title: "Rechts", symbol: "arrow.right", keysym: Keysym.right, modifier: false),
        Item(title: "Entf", symbol: nil, keysym: Keysym.delete, modifier: false),
        Item(title: "Pos1", symbol: nil, keysym: Keysym.home, modifier: false),
        Item(title: "Ende", symbol: nil, keysym: Keysym.end, modifier: false),
        Item(title: "Bild ↑", symbol: nil, keysym: Keysym.pageUp, modifier: false),
        Item(title: "Bild ↓", symbol: nil, keysym: Keysym.pageDown, modifier: false),
    ]

    init(owner: RemoteScreenView) {
        self.owner = owner
        super.init(frame: CGRect(x: 0, y: 0, width: 320, height: 48), inputViewStyle: .keyboard)
        allowsSelfSizing = true
        translatesAutoresizingMaskIntoConstraints = false
        heightAnchor.constraint(equalToConstant: 48).isActive = true

        let scroll = UIScrollView()
        scroll.showsHorizontalScrollIndicator = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        addSubview(scroll)
        let stack = UIStackView()
        stack.axis = .horizontal
        stack.spacing = 6
        stack.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(stack)
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            stack.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor, constant: 8),
            stack.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor, constant: -8),
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 6),
            stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -6),
            stack.heightAnchor.constraint(equalTo: scroll.frameLayoutGuide.heightAnchor, constant: -12),
        ])

        for item in Self.items {
            let button = makeButton(item)
            stack.addArrangedSubview(button)
            if item.modifier {
                modifierButtons[item.keysym] = button
            }
        }
        let fkeys = makeButton(Item(title: "F1–F12", symbol: nil, keysym: 0, modifier: false))
        fkeys.showsMenuAsPrimaryAction = true
        fkeys.menu = UIMenu(children: (1...12).map { n in
            UIAction(title: "F\(n)") { [weak owner] _ in owner?.type(keysym: Keysym.f1 + UInt32(n - 1)) }
        })
        stack.addArrangedSubview(fkeys)
        let cad = makeButton(Item(title: "Strg+Alt+Entf", symbol: nil, keysym: 0, modifier: false))
        cad.addAction(UIAction { [weak owner] _ in owner?.sendControlAltDelete() }, for: .touchUpInside)
        stack.addArrangedSubview(cad)
        let hide = makeButton(Item(title: "Tastatur ausblenden", symbol: "keyboard.chevron.compact.down", keysym: 0, modifier: false))
        hide.addAction(UIAction { [weak owner] _ in owner?.setKeyboardVisible(false) }, for: .touchUpInside)
        stack.addArrangedSubview(hide)
    }

    required init?(coder: NSCoder) { fatalError("not supported") }

    private func makeButton(_ item: Item) -> UIButton {
        var config = UIButton.Configuration.gray()
        config.cornerStyle = .medium
        config.buttonSize = .small
        config.contentInsets = NSDirectionalEdgeInsets(top: 4, leading: 10, bottom: 4, trailing: 10)
        if let symbol = item.symbol {
            config.image = UIImage(systemName: symbol)
            config.preferredSymbolConfigurationForImage = UIImage.SymbolConfiguration(scale: .small)
        } else {
            config.title = item.title
        }
        let button = UIButton(configuration: config)
        button.accessibilityLabel = item.title
        button.changesSelectionAsPrimaryAction = false
        if item.keysym != 0 {
            button.addAction(
                UIAction { [weak self] _ in
                    guard let self else { return }
                    if item.modifier {
                        self.owner.toggleSticky(item.keysym)
                    } else {
                        self.owner.type(keysym: item.keysym)
                    }
                }, for: .touchUpInside)
        }
        return button
    }

    /// Reflects which modifiers are armed.
    func refresh() {
        for (sym, button) in modifierButtons {
            let on = owner.isSticky(sym)
            button.configuration = on ? button.configuration?.updated(for: .tinted) : button.configuration?.updated(for: .gray)
            button.isSelected = on
        }
    }
}

extension UIButton.Configuration {
    fileprivate enum Style { case tinted, gray }

    fileprivate func updated(for style: Style) -> UIButton.Configuration {
        var c = style == .tinted ? UIButton.Configuration.tinted() : UIButton.Configuration.gray()
        c.cornerStyle = cornerStyle
        c.buttonSize = buttonSize
        c.contentInsets = contentInsets
        c.title = title
        c.image = image
        c.preferredSymbolConfigurationForImage = preferredSymbolConfigurationForImage
        return c
    }
}
