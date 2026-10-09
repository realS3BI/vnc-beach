# vnc-beach

Ein VNC-Server und -Viewer, der einfach funktioniert: auf macOS und Windows, gratis und Open Source.

Server und Viewer sprechen Standard-VNC (RFB 3.8, RFC 6143). Der vnc-beach-Viewer verbindet sich deshalb auch mit anderen VNC-Servern (TigerVNC, RealVNC, macOS-Bildschirmfreigabe mit VNC-Passwort). Umgekehrt können sich TigerVNC, RealVNC Viewer & Co. mit dem vnc-beach-Server verbinden.

## Stand

Erste lauffähige Version (0.1). Umgesetzt:

- Bildschirmübertragung mit ZRLE-Kompression; gesendet werden nur geänderte 64×64-Kacheln
- VNC-Passwort-Authentifizierung
- Maus mit drei Tasten und Scrollrad; Retina/HiDPI wird umgerechnet
- Tastatur: Zeichen werden als Text eingefügt und kommen dadurch unabhängig vom Tastaturlayout an (`@`, `€`, Umlaute). Shortcuts mit Strg/Cmd lösen echte Tasten aus.
- Zwischenablage für Text in beide Richtungen (Latin-1, wie im Standard)
- Auflösungswechsel während der Sitzung (DesktopSize)
- Viewer mit Verbindungsdialog; das Fenster passt sich an den entfernten Bildschirm an
- Serververzeichnis im Viewer: mehrere Rechner mit Name, Adresse und Passwort speichern und per Klick verbinden

Auf macOS geprüft sind Verbindung, Passwort (richtig und falsch), Bildübertragung samt Farben, Mausposition und der Viewer gegen den Server. Tastatur, Zwischenablage und Auflösungswechsel sind geschrieben, aber noch nicht von Hand durchgetestet.

Auf Windows 11 geprüft: Build und Tests, der Viewer verbindet sich über Tailscale mit dem Mac, und der Server liefert den Windows-Bildschirm aus (2560×1440). Maus- und Tastatureingaben unter Windows (`SendInput`) sind noch nicht getestet.

## Aufbau

| Crate | Inhalt |
| --- | --- |
| `crates/rfb` | Protokoll: Handshake, VNC-Auth, Nachrichten, Pixelformate, ZRLE |
| `crates/beach-server` | Server: Bildschirmaufnahme (`xcap`), Eingaben (`enigo`), Zwischenablage (`arboard`) |
| `crates/beach-viewer` | Viewer: Fenster und Eingaben (`eframe`/`egui`) |

## Nutzung

Voraussetzung ist Rust (stable).

```sh
cargo build --release
```

### Server

```sh
./target/release/beach-server                     # Port 5900, erzeugt ein Zufallspasswort und gibt es aus
./target/release/beach-server --password geheim   # eigenes Passwort (max. 8 Zeichen, VNC-Limit)
./target/release/beach-server --list-displays     # Bildschirme auflisten
./target/release/beach-server --display 1 --port 5901 --fps 60
./target/release/beach-server --view-only         # nur zuschauen, keine Eingaben
```

Das Passwort lässt sich auch über `VNC_BEACH_PASSWORD` setzen.

**macOS:** Das Programm, aus dem der Server gestartet wird (Terminal, iTerm …), braucht zwei Berechtigungen unter *Systemeinstellungen → Datenschutz & Sicherheit*:

- **Bildschirm- & Systemaudioaufnahme:** Fehlt sie, sehen Viewer nur Hintergrundbild und Menüleiste. Der Server warnt beim Start.
- **Bedienungshilfen:** Fehlt sie, haben Maus und Tastatur keine Wirkung.

### Viewer

```sh
./target/release/beach-viewer                       # Verbindungsdialog
./target/release/beach-viewer mac.local             # Port 5900
./target/release/beach-viewer 192.168.1.20:1        # Display 1 = Port 5901
./target/release/beach-viewer 192.168.1.20::5999    # Port explizit
./target/release/beach-viewer "Mac Büro"            # gespeicherter Server, per Name
```

**Serververzeichnis:** Im Verbindungsdialog lassen sich beliebig viele Server mit Name, Adresse und Passwort speichern („Speichern"). Gespeicherte Server stehen oben in einer Liste; „Verbinden" oder ein Doppelklick stellt die Verbindung her, ein Klick auf den Eintrag lädt ihn zum Bearbeiten ins Formular. Der Name eines gespeicherten Servers kann auch auf der Kommandozeile stehen, etwa für Verknüpfungen im Startmenü.

Die Liste liegt als `servers.toml` unter `~/Library/Application Support/vnc-beach/` (macOS) bzw. `%APPDATA%\vnc-beach\` (Windows) und lässt sich auch von Hand bearbeiten. Passwörter stehen darin im Format von `vncpasswd` (DES mit festem Schlüssel, wie in `~/.vnc/passwd`). Das verhindert nur das Mitlesen auf einen Blick, ist aber keine Verschlüsselung: Wer die Datei hat, kann die Passwörter zurückrechnen.

Die Strg+Alt+Entf-Taste in der Leiste schickt die Kombination an den Server. Auf dem Mac wird Cmd als Super/Meta gesendet und kommt auf einem Mac-Server wieder als Cmd an.

## Sicherheit

Klassisches VNC verschlüsselt den Bildschirminhalt nicht, und die VNC-Passwort-Authentifizierung (DES, 8 Zeichen) ist schwach. Bis vnc-beach eine eigene Verschlüsselung hat, gilt:

- nur im eigenen Netz verwenden oder über ein VPN wie Tailscale bzw. WireGuard
- den Port nie direkt ins Internet freigeben
- `--no-password` nur in vertrauenswürdigen Netzen

## Roadmap

1. Auf echtem Windows testen (Server und Viewer, HiDPI, mehrere Monitore)
2. Verschlüsselung (VeNCrypt/TLS) und ein stärkeres Passwortverfahren
3. Schnellere Aufnahme: ScreenCaptureKit (macOS) und DXGI-Streaming (Windows) statt Einzelbildern
4. Mauszeiger übertragen (Cursor-Pseudo-Encoding)
5. Viewer: 1:1-Ansicht mit Scrollen, Vollbild, Cmd↔Strg-Zuordnung bei Mac → Windows
6. Unicode-Zwischenablage (Extended Clipboard)
7. Fertige Downloads: signierte macOS-App, Windows-Installer, Server als Hintergrunddienst

## Mac als Server: eingebaute Bildschirmfreigabe reicht

Wer nur von Windows auf einen Mac zugreifen will, braucht auf dem Mac keinen vnc-beach-Server. Die macOS-Bildschirmfreigabe spricht VNC. Unter *Systemeinstellungen → Allgemein → Teilen → Bildschirmfreigabe → (i)* muss „VNC-Viewer dürfen den Bildschirm mit Passwort steuern" aktiv sein. Der vnc-beach-Viewer verbindet sich dann mit diesem Passwort auf Port 5900. Die Bildschirmfreigabe läuft auch am Anmeldebildschirm und nach einem Neustart.

## Windows bauen

Ohne Visual Studio, mit der GNU-Toolchain:

```powershell
# Rust (nur für den eigenen Benutzer)
Invoke-WebRequest https://win.rustup.rs/x86_64 -OutFile $env:TEMP\rustup-init.exe
& $env:TEMP\rustup-init.exe -y --default-host x86_64-pc-windows-gnu --profile minimal
# MinGW: liefert dlltool/as, das einige Abhängigkeiten zum Linken brauchen
winget install --id BrechtSanders.WinLibs.POSIX.UCRT --scope user
```

Danach mit dem `mingw64\bin`-Ordner von WinLibs im `PATH` `cargo build --release` ausführen. Die fertigen `.exe` brauchen nur Windows-eigene DLLs.

## Entwicklung

```sh
cargo test --workspace
cargo run -p beach-server -- --password test1234 --port 5901
cargo run -p beach-viewer -- 127.0.0.1:5901

# Headless-Test eines beliebigen VNC-Servers: ein Bild holen und als PPM speichern
cargo run --release -p rfb --example snapshot -- 127.0.0.1:5901 test1234 bild.ppm
```

Viewer und Server auf demselben Mac schieben sich gegenseitig die Maus weg, sobald der Zeiger über dem Viewer-Fenster ist. Zum Testen am selben Rechner lieber `--view-only` oder das `snapshot`-Beispiel verwenden.
