# Windows ohne eingeschaltete Monitore

**Gilt nur für den Windows-Server.** Der macOS-Server hat diese Funktion nicht und braucht sie nach bisherigem Stand nicht: Ein Mac ohne angeschlossenen Monitor behält einen Bildschirm in Standardauflösung; für eine bestimmte Auflösung hilft dort ein HDMI-Dummy-Stecker. Die iPad-/iPhone-App ist nur ein Viewer ohne eigenen Server.

Manche Monitore melden sich beim Ausschalten vollständig von Windows ab. Windows zeigt dann nur noch einen Ersatzbildschirm mit niedriger Auflösung (oft 1024 × 768), und genau den überträgt VNC Beach. Ein größeres Viewer-Fenster ändert daran nichts.

Mit dem virtuellen Bildschirmtreiber „usbmmidd_v2“ von Amyuni Technologies stellt VNC Beach in diesem Fall selbst einen Bildschirm bereit:

- Ist kein Monitor aktiv, fügt der Server genau einen virtuellen Bildschirm hinzu und stellt ihn auf 2560 × 1440. Die tatsächlich eingestellte Auflösung wird nachgeprüft.
- Sobald Windows wieder einen echten Monitor erkennt, entfernt der Server den virtuellen Bildschirm. Die gewohnte Anordnung der echten Monitore gilt dann wieder. Echte Monitore werden nie umgestellt.
- Der Server prüft etwa einmal pro Sekunde, auch ohne verbundenen Zuschauer und im Windows-Dienst vor der Anmeldung. Laufende Verbindungen bekommen die neue Bildschirmgröße über die üblichen VNC-Meldungen (DesktopSize/ExtendedDesktopSize); Maus und Bildschirmauswahl passen sich an.
- Ohne Treiber läuft alles wie bisher: VNC Beach überträgt, was Windows anzeigt, und schreibt einmal eine Meldung ins Protokoll. Fehler werden nicht jede Sekunde wiederholt; nach einem Fehlschlag wartet der Server zehn Sekunden bis zum nächsten Versuch.

## Treiber einrichten

Einmalig pro PC, mit Administratorrechten:

- **Im Viewer:** *Dieser Computer → Einstellungen → Virtueller Bildschirm → Treiber einrichten*. Windows fragt nach Administratorrechten.
- **Oder in einer Eingabeaufforderung als Administrator:** `"%LOCALAPPDATA%\Programs\vnc-beach\vnc-beach.exe" --install-display-driver` (oder die Kopie unter `C:\Program Files\VNC Beach`).

Die Einrichtung

1. lädt das unveränderte Paket von <https://www.amyuni.com/downloads/usbmmidd_v2.zip> und bricht ab, wenn es nicht genau der geprüften Fassung entspricht (SHA-256 `629b51e9944762bae73948171c65d09a79595cf4c771a82ebc003fbba5b24f51`, Treiberversion 2.0.0.1 vom 31. August 2021),
2. prüft, dass Treiberkatalog und Treiber-DLL gültig von Microsoft („Microsoft Windows Hardware Compatibility Publisher“) signiert sind. Das Installationsprogramm `deviceinstaller64.exe` des Herstellers ist nicht signiert; seine Echtheit sichert der SHA-256-Wert des Pakets,
3. installiert den Treiber mit dem mitgelieferten `deviceinstaller64.exe install usbmmidd.inf usbmmidd`. Die Signaturprüfung von Windows bleibt dabei aktiv, die Herstellerdateien werden nicht verändert,
4. setzt die Standardauflösung des Treibers auf 2560 × 1440 (`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\WUDF\Services\usbmmIdd\Parameters\Monitors`, Standardwert `2560,1440`).

Das Paket bleibt mit seiner Lizenz (`License.txt`) in `C:\ProgramData\VNC Beach\usbmmidd_v2` liegen. Das Protokoll steht in `C:\ProgramData\VNC Beach\setup.log`. Einen virtuellen Bildschirm fügt die Einrichtung nicht hinzu. Das übernimmt der Server, sobald kein Monitor aktiv ist.

Der Treiber gehört nicht zu VNC Beach. VNC Beach liefert ihn nicht mit, sondern lädt ihn beim Hersteller. Er steht unter der Lizenz von Amyuni Technologies Inc. (© 2014–2021, <https://www.amyuni.com>). Laut dieser Lizenz kann der Hersteller eine Werbeseite anzeigen. Der Treiber läuft nur unter 64-Bit-Windows 10/11 auf x86-Prozessoren.

Ist der Treiber schon vorhanden (etwa durch eine andere Anwendung), funktioniert der virtuelle Bildschirm nach dem VNC-Beach-Update ohne weitere Einrichtung.

**Entfernen:** im Geräte-Manager unter *Grafikkarten* „USB Mobile Monitor Virtual Display“ deinstallieren, oder in `C:\ProgramData\VNC Beach\usbmmidd_v2` als Administrator `deviceinstaller64 stop usbmmidd` und `deviceinstaller64 remove usbmmidd` ausführen.

## Andere Anwendungen mit demselben Treiber

Der Treiber kann nur „einen Bildschirm hinzufügen“ und „einen Bildschirm entfernen“, ohne zu sagen, welchen. VNC Beach merkt sich deshalb in `C:\ProgramData\VNC Beach\virtual-display`, dass es den Bildschirm selbst hinzugefügt hat. Es entfernt ihn nur, solange er der einzige Bildschirm dieses Treibers ist:

- Ein virtueller Bildschirm einer anderen Anwendung (z. B. RustDesk) wird weder entfernt noch umgestellt. Ist er der einzige Bildschirm, überträgt VNC Beach ihn so, wie er ist.
- Laufen mehrere Bildschirme dieses Treibers, lässt VNC Beach alle in Ruhe und meldet das einmal im Protokoll.
- Virtuelle Bildschirme anderer Treiber zählen wie echte Monitore: Ist einer aktiv, fügt VNC Beach keinen eigenen hinzu.

## Protokolle

Im Windows-Dienst: `C:\ProgramData\VNC Beach\agent.log`. Typische Einträge:

- `no monitor attached: added a virtual display` und `virtual display \\.\DISPLAYn set to 2560x1440`
- `display arrangement changed: framebuffer 2560x1440`
- `a monitor is back: removed the virtual display`
- `virtual display: … the virtual display driver is not installed …` (Treiber fehlt, Versuch alle zehn Sekunden)

## Manuelle Tests

Ohne Hardware laufen die Zustandstests mit `cargo test --workspace` (`headless::tests`). Auf einem Windows-PC mit Treiber zusätzlich, ausdrücklich von Hand:

```bat
cargo test -p beach-server --lib headless::windows::tests::inspect_real_displays -- --exact --ignored --nocapture
cargo test -p beach-server --lib headless::windows::tests::real_virtual_monitor_cycle -- --exact --ignored --nocapture
```

Der erste Test listet nur Grafikadapter und Monitore auf. Der zweite fügt kurz einen echten virtuellen Bildschirm hinzu, stellt 2560 × 1440 ein, prüft das und entfernt ihn wieder. Er ändert die Bildschirmanordnung, also nur auf einem Rechner ausführen, den man notfalls direkt bedienen kann. Ein laufender VNC-Beach-Server hält den Testbildschirm nicht für seinen eigenen und entfernt ihn deshalb nicht.

Praxisprüfung mit einem Zuschauer: `cargo run --release -p rfb --example snapshot -- HOST:PORT PASSWORT out.ppm` holt ein vollständiges Bild. `cargo run --release -p rfb --example watch_size -- HOST:PORT PASSWORT` bleibt verbunden und meldet jede Größenänderung samt dem ersten vollständigen Bild danach.

1. Alle Monitore ausschalten: Nach wenigen Sekunden zeigt `snapshot` 2560 × 1440, und Windows meldet genau einen Bildschirm.
2. Verbindung trennen und neu verbinden, mehrere Minuten warten: Es kommt kein zweiter virtueller Bildschirm hinzu.
3. Einen, dann beide Monitore einschalten: Der virtuelle Bildschirm verschwindet, die Anordnung der echten Monitore gilt wieder, und Maus und Klicks treffen.
4. Windows ohne Monitore neu starten: Der Dienst stellt den virtuellen Bildschirm schon am Anmeldebildschirm bereit. Sperrbildschirm und UAC bleiben erreichbar.
