# Konto, Organisationen und Koordinationsserver

Mit einem Konto zeigt VNC Beach auf jedem Computer, auf dem du angemeldet bist, alle deine Geräte und die deiner Organisationen. Ein Klick verbindet, ohne VNC-Passwort. Organisationsgeräte teilst du mit deinem Team: Mitglieder lädst du per Link ein.

Dahinter steht ein kleiner **Koordinationsserver** (`beach-coord`). Er ist Teil dieses Repositorys, Open Source und läuft bei jedem, der ihn betreibt. VNC Beach hängt also von keinem bestimmten Anbieter ab: Im Viewer steht nur die Adresse des Servers, und die kann jeder ändern.

## Was der Server macht und was nicht

```
┌──────────────┐   Anmeldung, Geräteliste, Tickets   ┌─────────────────────┐
│ Viewer       │ ◄─────────────────────────────────► │ Koordinationsserver │
│ (Mac, Win)   │                                     │ beach-coord         │
└──────┬───────┘                                     └──────────▲──────────┘
       │ VNC direkt über LAN oder Tailscale                     │ Anmeldung des Geräts,
       ▼                                                        │ Adressen jede Minute
┌──────────────┐ ───────────────────────────────────────────────┘
│ Server-Gerät │
└──────────────┘
```

- **Gespeichert** werden Konten (Name, GitHub-Login bzw. E-Mail-Adresse und die Konto-ID des Anbieters), Organisationen, Mitgliedschaften und Rollen, Einladungen und Geräte mit ihren zuletzt gemeldeten Adressen. Sitzungs-, Geräte- und Einladungsschlüssel liegen nur als SHA-256-Hash in der Datenbank.
- **Nicht** über den Server laufen Bildschirm, Eingaben, Zwischenablage oder Dateien. Die Verbindung geht direkt zum Gerät, der Server muss dafür nicht im Tailnet sein. Er muss aber für alle Geräte erreichbar sein, also in der Regel öffentlich per HTTPS.
- Ob sich zwei Geräte erreichen, regelt weiter das Netz: dasselbe LAN oder Tailscale. Die Geräte melden ihre Adressen (Tailscale zuerst, dann LAN), der Viewer probiert sie der Reihe nach.

## Zugriff ohne VNC-Passwort: Tickets

Ein Gerät im Konto bietet zusätzlich zur normalen VNC-Authentifizierung einen eigenen Security-Type an (RFB-Typ 66, „VNC Beach Ticket“). Fremde Viewer wie TigerVNC oder RealVNC wählen weiter die VNC-Authentifizierung mit Passwort, für sie ändert sich nichts.

1. Der Server schickt im Handshake eine Zufallszahl (32 Byte) und seine Geräte-ID.
2. Der VNC-Beach-Viewer fragt den Koordinationsserver mit seiner Anmeldung nach einem Ticket für genau dieses Gerät und diese Zufallszahl.
3. Der Koordinationsserver prüft, ob du das Gerät sehen darfst (eigenes Gerät oder Mitglied der Organisation), und signiert das Ticket mit seinem Ed25519-Schlüssel. Es gilt 60 Sekunden.
4. Der Server prüft die Signatur mit dem öffentlichen Schlüssel, den er bei der Anmeldung des Geräts gespeichert hat, sowie Gerät, Zufallszahl und Gültigkeit. Dafür braucht er den Koordinationsserver nicht.

Wer aus einer Organisation entfernt wird oder sich abmeldet, bekommt keine Tickets mehr. Kein Passwort muss geändert werden. Ein Ticket gilt nur für die eine Verbindung, für die es ausgestellt wurde.

Wichtig: Wer den Koordinationsserver und seinen Schlüssel (`ticket-key`) kontrolliert, kann Tickets für alle dort angemeldeten Geräte ausstellen. Wer das niemandem anvertrauen will, betreibt den Server selbst. Die VNC-Verbindung selbst ist wie bisher unverschlüsselt. Sie gehört ins eigene Netz oder in ein VPN wie Tailscale, siehe [Sicherheit](../README.md#sicherheit).

## Benutzen

**Anmelden:** Im Viewer links unter *Konto* die Adresse des Koordinationsservers eingeben (z. B. `beach.example.com`) und *Anmelden* klicken. Der Browser öffnet sich, du meldest dich mit GitHub oder Google an, danach geht es im Viewer automatisch weiter. Die Anmeldung bleibt ein Jahr gültig oder bis *Abmelden*.

**Diesen Computer hinzufügen:** Unter der Geräteliste *Hinzufügen …* wählen, einen Namen vergeben und festlegen, ob er dir persönlich gehört oder einer Organisation. Ab dann meldet der Hintergrunddienst jede Minute die Adressen des Computers, auch der Windows-Dienst vor der Anmeldung. *Entfernen* nimmt ihn wieder aus dem Konto.

**Verbinden:** Ein Gerät in der Liste anklicken zeigt Status und Adressen, Doppelklick oder *Verbinden* verbindet. Es gelten die Standardeinstellungen des Viewers.

**Organisationen verwalten:** *Verwalten* öffnet die Weboberfläche des Koordinationsservers. Dort legst du Organisationen an, erzeugst Einladungslinks (einmalig nutzbar, 7 Tage gültig), änderst Rollen und entfernst Mitglieder oder Geräte.

| Rolle | darf |
| --- | --- |
| Mitglied | alle Geräte der Organisation sehen und sich verbinden, eigene Computer hinzufügen |
| Admin | zusätzlich einladen, Mitglieder und Admins verwalten, Geräte entfernen |
| Owner | zusätzlich Owner ernennen und die Organisation löschen. Die letzte Owner-Rolle bleibt immer besetzt. |

## Selbst betreiben

### 1. Anmeldung bei GitHub und/oder Google einrichten

Es reicht ein Anbieter. Als Rückruf-Adresse gilt jeweils `https://DEINE-ADRESSE/auth/ANBIETER/callback`.

- **GitHub:** *Settings → Developer settings → OAuth Apps → New OAuth App*. „Homepage URL“ ist `https://beach.example.com`, „Authorization callback URL“ ist `https://beach.example.com/auth/github/callback`. Danach ein Client-Secret erzeugen. Es wird nur der öffentliche Profil-Zugriff verlangt (`read:user`).
- **Google:** In der Google Cloud Console unter *APIs & Dienste → Anmeldedaten* eine „OAuth-Client-ID“ vom Typ „Webanwendung“ anlegen und als autorisierten Weiterleitungs-URI `https://beach.example.com/auth/google/callback` eintragen. Abgefragt werden `openid email profile`.

### 2. Starten

Mit Docker (Image aus diesem Repository bauen):

```sh
docker build -f crates/beach-coord/Dockerfile -t beach-coord .
docker run -d --name beach-coord --restart unless-stopped \
  -p 127.0.0.1:8080:8080 -v beach-coord:/data \
  -e BEACH_PUBLIC_URL=https://beach.example.com \
  -e GITHUB_CLIENT_ID=… -e GITHUB_CLIENT_SECRET=… \
  beach-coord
```

Oder direkt: `cargo build --release -p beach-coord`, dann `target/release/beach-coord --public-url https://beach.example.com --data /var/lib/beach-coord` mit denselben Umgebungsvariablen.

| Variable / Option | Bedeutung |
| --- | --- |
| `BEACH_PUBLIC_URL` / `--public-url` | öffentliche HTTPS-Adresse, Pflicht. Daraus entstehen Rückruf- und Einladungslinks. |
| `BEACH_LISTEN` / `--listen` | Adresse und Port, Standard `0.0.0.0:8080` (reines HTTP) |
| `BEACH_DATA` / `--data` | Verzeichnis für `beach.db` (SQLite) und `ticket-key`, Standard `data` |
| `BEACH_NAME` / `--name` | angezeigter Name, Standard „VNC Beach“ |
| `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET` | Anmeldung mit GitHub |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` | Anmeldung mit Google |
| `BEACH_DEV_LOGIN=1` / `--dev-login` | Anmeldung nur mit einem Namen, **nur zum Entwickeln**: Jeder kann sich als jeder ausgeben. |

### 3. HTTPS davor

Der Server spricht selbst nur HTTP. Davor gehört ein Proxy mit TLS, z. B. Caddy:

```
beach.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

### 4. Sichern

Sichere das Datenverzeichnis, vor allem `ticket-key`. Jedes angemeldete Gerät vertraut genau diesem Schlüssel. Geht er verloren, müssen alle Geräte neu hinzugefügt werden. Wer ihn kopiert, kann Tickets ausstellen. Die Datei ist deshalb nur für den Server-Benutzer lesbar.

## API

Die JSON-API ist in [`crates/beach-account/src/api.rs`](../crates/beach-account/src/api.rs) beschrieben, das Ticketformat in [`crates/beach-account/src/ticket.rs`](../crates/beach-account/src/ticket.rs) und der Security-Type in [`crates/rfb/src/handshake.rs`](../crates/rfb/src/handshake.rs).

## Grenzen

- Die iPad- und iPhone-App kennt das Konto noch nicht und verbindet sich weiter per Passwort.
- Die Verbindung braucht beim Aufbau den Koordinationsserver (für das Ticket). Ist er nicht erreichbar, bleibt der Weg über einen Favoriten mit VNC-Passwort.
- Einstellungen pro Gerät (Farbtiefe, Nur ansehen …) gibt es für Kontogeräte noch nicht. Es gelten die Standardeinstellungen.
- Rechte feiner als die Rollen, z. B. Gruppen oder „nur ansehen“ für bestimmte Mitglieder, sind vorbereitet (das Ticket kann „nur ansehen“ tragen, der Server setzt es durch), aber noch nicht einstellbar.
