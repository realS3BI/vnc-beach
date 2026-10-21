# VNC Beach für iPad und iPhone

Native SwiftUI-App, die den Rust-Client aus `crates/beach-mobile` als statische
Bibliothek einbindet. Protokoll, ZRLE-Dekodierung und Verbindungsaufbau sind
damit dieselben wie im Desktop-Viewer; die App steuert Darstellung (Metal),
Gesten, Tastatur und Zwischenablage bei. Gedacht vor allem fürs iPad im
Tailscale-VPN: der Windows-PC ist dann einfach unter seinem Gerätenamen
(`sebastian-pc`) erreichbar.

## Aufbau

| Pfad | Inhalt |
| --- | --- |
| `project.yml` | XcodeGen-Spezifikation; `xcodegen generate` erzeugt `VNCBeach.xcodeproj` |
| `VNCBeach/App` | Einstieg, Fenster (jede Verbindung kann ein eigenes Fenster sein) |
| `VNCBeach/Model` | Favoriten (`servers.json` in Application Support), Passwörter im Schlüsselbund, Einstellungen |
| `VNCBeach/Core` | `RemoteSession` (Swift-Hülle um die C-API), Keysym-Tabelle, Bonjour-Suche |
| `VNCBeach/Screen` | `RemoteScreenView` (UIKit: Gesten, Trackpad, Pencil, Tastatur), Metal-Renderer, Zusatztastenleiste |
| `VNCBeach/UI` | SwiftUI: Liste, Bearbeiten, Einstellungen, Sitzung mit Werkzeugleiste |
| `VNCBeachTests` | Tests für Keysyms, Favoriten, Erreichbarkeit und abbrechbare RFB-Verbindungen |
| `ci_scripts/ci_post_clone.sh` | Xcode Cloud: XcodeGen, Rust-Toolchain, Team und Buildnummer |
| `../../tools/build-ios.sh` | Baut `libbeach_mobile.a`; läuft als Build-Phase vor dem Swift-Build |

## Bauen

Voraussetzungen: Xcode 26, XcodeGen (`brew install xcodegen`) und eine
rustup-Toolchain mit iOS-Targets. Das Homebrew-Paket `rust` kennt keine
iOS-Targets, deshalb zusätzlich:

```sh
brew install rustup
/opt/homebrew/opt/rustup/bin/rustup toolchain install stable --profile minimal
/opt/homebrew/opt/rustup/bin/rustup target add aarch64-apple-ios aarch64-apple-ios-sim
```

`tools/build-ios.sh` findet diese Toolchain selbst (auch `~/.cargo/bin`).
Die Bibliothek wird mit Fat-LTO gebaut, weil Apples Linker die
Bitcode-Objekte aus Thin-LTO nicht lesen kann.

```sh
cd apps/ios
cp Config/Signing.local.xcconfig.example Config/Signing.local.xcconfig   # Team-ID eintragen
xcodegen generate
xcodebuild -project VNCBeach.xcodeproj -scheme VNCBeach \
  -destination 'platform=iOS Simulator,name=LifeOS Kitchen iPad' build
```

Tests: dasselbe mit `test` statt `build`. Die Metal-Shader werden zur
Laufzeit kompiliert; die Metal-Toolchain von Xcode ist nicht nötig.

`AccessTests` prüft alternative Adressen mit einem lokalen RFB-Testserver,
Schlüsseldateien und den Wechsel von einem abgebrochenen Versuch zur nächsten
Verbindung. `VNCBeachUITests` prüft das Bearbeitungsformular und die Aktionen
nach einer fehlgeschlagenen Verbindung. Mit Paramiko in einer separaten
Python-Umgebung prüft `python tools/test-access-auth.py --ios` vom Repository-Root
zusätzlich SSH-Gerätebestätigung und mehrstufige Anmeldung bis zur
VNC-Verbindung im iPad-Simulator. Die Testserver nutzen ausschließlich Loopback
und temporäre Zugangsdaten.

Zum Entwickeln im Simulator verbindet ein Startargument direkt:

```sh
xcrun simctl launch booted at.schlossers.vncbeach -connect "vncbeach://sebastian-pc?password=…"
```

Auf das iPad direkt (entsperrt, per Kabel oder WLAN mit Xcode gekoppelt):

```sh
xcodebuild -project VNCBeach.xcodeproj -scheme VNCBeach -destination 'id=<Geräte-ID>' \
  -allowProvisioningUpdates build
xcrun devicectl device install app --device <Geräte-ID> "DerivedData/Build/Products/Debug-iphoneos/VNC Beach.app"
```

## TestFlight

Releases laufen über `tools/release-ios.sh` (von `release.sh X.Y.Z` im
Repository-Root aufgerufen): Das Skript archiviert das Scheme `VNCBeach`
in Release, setzt die Buildnummer auf einen UTC-Zeitstempel, exportiert
die `.ipa` per `xcodebuild -exportArchive` und lädt sie mit
`xcrun altool --upload-package` zu App Store Connect hoch. Die Signierung
ist Apple-verwaltet (Apple Distribution, Profil für
`at.schlossers.vncbeach`) und läuft über die in Xcode angemeldete Apple-ID;
ein API-Schlüssel bräuchte dafür die Rolle „Admin“. Der Upload braucht einen
App-Store-Connect-API-Schlüssel (Rolle „App Manager“) in
`~/.config/vnc-beach/appstoreconnect.env` außerhalb des Repositories.
`tools/release-ios.sh --no-upload` prüft Archiv und Export ohne Upload.

Einmalig in App Store Connect (Apple-Login nötig): iOS-App „VNC Beach“
für die Bundle-ID `at.schlossers.vncbeach` anlegen, interne
TestFlight-Gruppe mit Sebastian und automatischer Verteilung, API-Schlüssel
mit Rolle „App Manager“ erzeugen. Danach verteilt jeder Upload den Build
nach Apples Verarbeitung an die Gruppe; die TestFlight-App auf dem iPad
installiert ihn.

`ci_scripts/ci_post_clone.sh` bleibt für einen späteren Xcode-Cloud-
Workflow liegen (Scheme `VNCBeachCloud`, Variable
`VNC_BEACH_DEVELOPMENT_TEAM`), wird derzeit aber nicht verwendet, weil die
Ersteinrichtung von Xcode Cloud die Xcode-Oberfläche braucht.

Die App nutzt die VNC-Passwort-Authentifizierung nach RFC 6143 und optional
SSH über libssh2 mit OpenSSL. Der VNC-Transport benötigt weiterhin ein
vertrauenswürdiges Netz oder VPN. SSH bereitet die Verbindung vor und tunnelt
den VNC-Verkehr nicht.

## Bedienung

**Computer hinzufügen:** Name, Adresse (`sebastian-pc`, `10.0.0.2:1`,
`host::5901`), Passwort. Macs mit Bildschirmfreigabe im selben WLAN
erscheinen unter „In der Nähe“ (Bonjour). Über Tailscale müssen Namen von
Hand eingetragen werden; MagicDNS löst sie auf, sobald das VPN aktiv ist.

**Erreichbarkeit & Start:** Im Bearbeitungsformular alternative VNC-Adressen,
SSH-Anmeldung und Wake-on-LAN ergänzen. **Verbinden** prüft Hauptadresse und
Alternativen der Reihe nach. Eine abgelehnte VNC-Anmeldung wird nicht über die
nächste Adresse wiederholt. Wenn keine Verbindung möglich ist, bietet die App
je nach Konfiguration **Per SSH anmelden** und **Aufwecken & verbinden** an.
Nach diesen Aktionen wartet sie bis zu 90 Sekunden auf VNC. Während des Wartens
nach dem Aufwecken kann direkt zur SSH-Anmeldung gewechselt werden.

Beim ersten SSH-Zugriff den angezeigten SHA-256-Fingerabdruck über einen
bekannten Zugang prüfen und bestätigen. Ein geänderter Geräteschlüssel stoppt
die Anmeldung. SSH-Passwörter, mehrstufige Anmeldeabfragen und
Schlüssel-Passphrasen werden erst bei Bedarf abgefragt und nicht gespeichert.
Private Schlüssel über die Dateien-App importieren. Sie bleiben im
Geräteschlüsselbund; temporäre Dateikopien sind auf einen Versuch beschränkt.

Zum Aufwecken mindestens ein **Zugangsgerät** im LAN des Zielcomputers eintragen.
Das kann ein Raspberry Pi, NAS, Mac, Linux- oder Windows-Rechner mit SSH sein.
Mehrere Geräte werden der Reihe nach versucht. macOS, Linux und NAS benötigen
Python 3 oder Perl, Windows PowerShell. Die iOS-App ist ausschließlich der
Client und muss nicht dauerhaft laufen. Direkte Broadcasts vom iPhone oder
iPad werden nicht angeboten, da Apple dafür eine
[gesonderte Berechtigung](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.networking.multicast)
verlangt.

SSH kann auf geeigneten Macs eine Anmeldung vor dem Start der
Bildschirmfreigabe ermöglichen. Ob FileVault damit entsperrt werden kann,
hängt von Hardware, macOS und der Netzverfügbarkeit vor dem Entsperren ab.
Ein SSH-Abbruch bestätigt keine Entsperrung. Ein echter FileVault-Neustart
wurde bisher nicht geprüft.

**Gesten (Finger):**

| Geste | Wirkung |
| --- | --- |
| Finger bewegen | Touchpad: Zeiger bewegen; Direkt: Zeiger folgt dem Finger |
| Tippen | Linksklick (zweimal kurz = Doppelklick) |
| Gedrückt halten, dann ziehen | Ziehen mit gehaltener linker Taste |
| Mit zwei Fingern tippen | Rechtsklick |
| Mit zwei Fingern wischen | Scrollen |
| Auf-/Zuziehen | Zoom; bei Zoom folgt der Ausschnitt dem Zeiger |

**Trackpad und Maus** (Magic Keyboard, Bluetooth-Maus): Der Zeiger ist
immer absolut; Klick, Sekundärklick und Scrollen gehen direkt an den
entfernten Rechner. Der Systemzeiger wird ausgeblendet und als Pfeil an der
Stelle gezeichnet, die der Server kennt.

**Apple Pencil:** Schweben bewegt den Zeiger, Aufsetzen drückt die linke
Taste, also auch Zeichnen und Ziehen.

**Tastatur:** Hardware-Tastaturen funktionieren sofort. Zeichen kommen als
Text an (Umlaute, `@`, `€` unabhängig vom Layout auf dem PC); Tastenkürzel
mit Strg/⌘ lösen echte Tasten aus. „Automatisch“ sendet ⌘ als Strg, wenn
der Rechner kein Mac ist (⌘C kopiert in Windows). Bei macOS-Bildschirmfreigabe
wird ⌘ als Alt_L und Option als Meta_L gesendet, wie Apple es erwartet.
Die Bildschirmtastatur hat eine Zusatzleiste mit Esc, Tab, Strg, Alt, Win,
Pfeilen, Entf, Pos1/Ende, Bild auf/ab, F1–F12 und Strg+Alt+Entf;
Modifikatoren bleiben bis zur nächsten Taste gedrückt. Standardmäßig liegt
die Tastatur über dem Bild; „Bild über der Tastatur verkleinern“ (Menü in der
Sitzung oder Einstellungen) verkleinert den entfernten Bildschirm stattdessen
so, dass die Tastatur nichts verdeckt.

**Scroll-Pad:** Der Knopf mit dem Doppelpfeil blendet ein schwebendes Pad ein
(am Griff verschiebbar) mit vier Pfeiltasten, die beim Halten wiederholen,
und einem Scrollstreifen: Ziehen scrollt an der Zeigerposition wie auf einem
Touchscreen, Tippen auf die obere oder untere Hälfte scrollt eine Raste,
Halten scrollt weiter. Zum Blättern am iPad ohne Tastatur und ohne
Zwei-Finger-Geste.

**Mehrere Bildschirme:** Meldet der Server die Anordnung seiner Bildschirme
(VNC-Beach-Server, TigerVNC), erscheint ein Bildschirm-Knopf in der
Werkzeugleiste: „Alle Bildschirme“ oder ein einzelner. Dann wird nur noch
dieser angezeigt und vom Server angefordert.

**Werkzeugleiste** (oben, einklappbar): Tastatur, Eingabemodus, Scroll-Pad,
Bildschirm (bei mehreren), Menü (Esc, Tab, Windows-Taste, Strg+Alt+Entf,
Zwischenablage senden, Bild neu laden, Bild über der Tastatur verkleinern,
Nur ansehen), Zoom zurücksetzen, Trennen.

**Zwischenablage:** Nach dem Verbinden und bei jeder Rückkehr zur App wird
kopierter Text an den Rechner gesendet; vom Rechner kopierter Text landet
in der iPad-Zwischenablage. iOS zeigt beim Lesen einen Hinweis.

**Hintergrund:** iOS kappt die Verbindung nach kurzer Zeit im
Hintergrund. Beim Zurückkehren verbindet die App automatisch neu, sofern
sie nicht per „Trennen“ beendet wurde.

**URL-Schema:** `vncbeach://sebastian-pc` öffnet den Favoriten mit diesem
Namen oder Host, etwa aus Kurzbefehlen; unbekannte Hosts werden als
Favorit angelegt (`?password=…` optional).

**Mehrere Fenster:** Im Kontextmenü eines Favoriten „In neuem Fenster
öffnen“, für zwei Rechner nebeneinander in Split View oder Stage Manager.

## Update-Prüfung

Die App fragt beim Start (höchstens alle sechs Stunden) und über
„Nach Updates suchen“ in den Einstellungen das letzte GitHub-Release ab
(`UpdateChecker.repository`, Tag `vX.Y.Z` gegen `MARKETING_VERSION`).
Gibt es eine neuere Version, erscheint in der Liste ein Banner mit
„In TestFlight öffnen“ (`itms-beta://`, sonst die TestFlight-Website);
installieren kann iOS nicht selbst. Antwortet die API nicht mit 200 (kein Release,
offline), behandelt die App das stillschweigend als „keine Informationen“.
`release.sh VERSION` im Repository-Root setzt `MARKETING_VERSION` in
`Config/Base.xcconfig` passend zum Tag.
