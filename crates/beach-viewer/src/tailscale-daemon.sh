#!/bin/zsh
# VNC Beach: switches Tailscale on this Mac to the open source `tailscaled`
# daemon, the only variant that runs before anyone signs in. Opened in
# Terminal by the server settings (see service.rs); messages are German
# because the user reads them.

print_step() { print "\n\033[1m$1\033[0m" }

print "VNC Beach – Tailscale vor der Anmeldung starten"
print "================================================"
print "Die Tailscale-App läuft auf dem Mac erst nach der Anmeldung. Dieses Skript"
print "ersetzt sie durch den Tailscale-Dienst (tailscaled), der mit dem Mac startet."
print ""
print "Wichtig:"
print " • Nur direkt am Mac ausführen, nicht über eine Fernverbindung: Tailscale ist"
print "   zwischendurch getrennt, und am Ende braucht es eine Anmeldung im Browser."
print " • Der Mac bekommt in Tailscale einen neuen Geräteeintrag (neue Adresse)."
print " • Es braucht Homebrew und dein Mac-Passwort."
print ""
read "answer?Weiter? [j/N] "
[[ $answer == [jJyY]* ]] || exit 0

BREW=""
for b in /opt/homebrew/bin/brew /usr/local/bin/brew; do
  [[ -x $b ]] && BREW=$b && break
done
if [[ -z $BREW ]]; then
  print "\nHomebrew fehlt. Installieren von https://brew.sh, dann diesen Schritt wiederholen."
  exit 1
fi
PREFIX=$("$BREW" --prefix)

if [[ ! -f /Library/LaunchDaemons/com.tailscale.tailscaled.plist ]]; then
  print_step "1/4 Tailscale über Homebrew installieren"
  "$BREW" install tailscale || exit 1

  print_step "2/4 Tailscale-App beenden und in den Papierkorb legen"
  osascript -e 'quit app "Tailscale"' 2>/dev/null
  sleep 2
  if [[ -d /Applications/Tailscale.app ]]; then
    osascript -e 'tell application "Finder" to delete POSIX file "/Applications/Tailscale.app"' >/dev/null \
      || print "Bitte /Applications/Tailscale.app selbst in den Papierkorb legen."
    print "Fragt macOS, ob die Netzwerkerweiterung entfernt werden soll: zustimmen."
  fi

  print_step "3/4 Tailscale-Dienst einrichten (fragt nach deinem Passwort)"
  sudo "$PREFIX/bin/tailscaled" install-system-daemon || exit 1
  sleep 3
else
  print "\nDer Tailscale-Dienst ist schon eingerichtet."
fi

print_step "4/4 Bei Tailscale anmelden"
print "Es erscheint ein Link: im Browser öffnen und anmelden."
sudo "$PREFIX/bin/tailscale" up || exit 1
"$PREFIX/bin/tailscale" status | head -5

print_step "Entsperren nach einem Neustart (FileVault)"
print "Mit FileVault startet nach dem Einschalten nichts, bis das Passwort eingegeben ist."
print "Ab macOS 26 geht das per SSH aus dem Heimnetz (Apple Silicon): dafür muss"
print "„Entfernte Anmeldung“ an sein."
if sudo systemsetup -setremotelogin on 2>/dev/null; then
  print "Entfernte Anmeldung ist an."
else
  print "Bitte unter Systemeinstellungen → Allgemein → Teilen „Entfernte Anmeldung“ einschalten."
  open "x-apple.systempreferences:com.apple.Sharing-Settings.extension"
fi

print "\nFertig. Noch im Tailscale-Adminbereich (login.tailscale.com):"
print " • beim neuen Eintrag dieses Macs „Disable key expiry“ wählen,"
print " • den alten Eintrag des Macs löschen (und den neuen bei Bedarf umbenennen)."
print "\nDieses Fenster kann geschlossen werden."
