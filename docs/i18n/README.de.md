# arTerm: persistentes Windows-Remote-Terminal nach Verbindungsabbruch

[English](../../README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md)

Verlieren Sie die Verbindung, nicht Ihre Arbeit. Builds, lange Befehle und Coding-Agenten laufen auf dem entfernten Windows-Rechner weiter. Nach dem Schließen des Clients, einem VPN-Abbruch oder Notebook-Neustart kehren Sie zur selben Shell zurück statt neu anzufangen.

Client und Host benötigen Windows. Der Host muss laufen und der Benutzer angemeldet bleiben. Der Client nutzt Microsoft devtunnel CLI, der Host eine kompatible native VS Code tunnel CLI wie code-tunnel.exe. Der vollständige Editor ist optional. Beide Tunnel benötigen dasselbe GitHub-Konto; die Anmeldung bei gh ersetzt das nicht. Ausgehende Verbindungen müssen erlaubt sein. Downloads von Abhängigkeiten erfolgen nur mit Zustimmung; das Host-Setup verlangt die Annahme der VS Code server-Lizenz.

Wählen Sie x64 oder ARM64 unter [Windows-Downloads](https://github.com/yeelam/arterm/releases/latest). Der überprüfte Link leitet zu v0.7.1 weiter. Die Pakete sind mit einem Entwicklungszertifikat signiert, nicht mit einem öffentlich vertrauenswürdigen Produktionszertifikat. Prüfen Sie gemäß der [Installationsanleitung](../../DEVELOPMENT-INSTALL.md) Prüfsummen und Fingerabdruck des öffentlichen Zertifikats. Vertrauen darf nur mit ausdrücklicher Zustimmung und nach Organisationsrichtlinie für den aktuellen Benutzer hinzugefügt werden. Umgehen Sie weder Authenticode noch SmartScreen oder Anwendungskontrollen.

Führen Sie auf dem Host arTerm-Host-Setup.exe aus und öffnen Sie eine neue PowerShell:

```powershell
arterm-host setup --name my-devbox
```

Schließen Sie die Tunnel-Anmeldung ab und bewahren Sie den Registrierungsbefehl auf. Installieren Sie lokal arTerm-Client-Setup.exe, öffnen Sie eine neue PowerShell und verwenden Sie dasselbe GitHub-Konto. Führen Sie die vom Host ausgegebene Registrierung mit dem tatsächlichen Host-Pfad unverändert aus. Der Client ist bereits initialisiert; arterm setup ist nicht nötig. Verbinden Sie sich lokal mit einem neuen Sitzungsnamen:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

Führen Sie in der entfernten PowerShell diese Befehle aus und notieren Sie PID und Verzeichnis:

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Drücken Sie Ctrl+], um nur den Client zu trennen. Falls das Terminal den Kurzbefehl abfängt, schließen Sie nur den lokalen Client-Tab. Halten Sie den Host am Laufen und den Benutzer angemeldet. Führen Sie lokal denselben Befehl erneut aus:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

In der wieder verbundenen entfernten PowerShell:

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

Erwartet werden dieselbe PID, kept-on-host, dasselbe Verzeichnis und True. Verwenden Sie denselben Befehl auch später zum Weiterarbeiten. Zum Beenden des Tests geben Sie exit in der entfernten Shell ein: Der Prozess endet und sein Laufzeitzustand lässt sich nicht wiederherstellen.

Prozesse werden nach Host-Neustart, Abmeldung, Absturz, Abschalten oder Beenden der Shell nicht wiederhergestellt. Der Host startet bei der Benutzeranmeldung; automatische Anmeldung und Betrieb ohne angemeldeten Benutzer werden nicht eingerichtet. Die Ausgabehistorie ist begrenzt. Der gesamte Entwicklungsbranch ist nicht releasequalifiziert: Die unabhängige Prüfung der Transferarchive und die Kriterien für direkte Eingabe sind noch offen. Beliebige TUIs sind nicht zertifiziert; automatisches Entsperren von Dateien ist keine Sicherheitsgarantie.

Bei Fehlern prüfen Sie mit arterm doctor my-devbox Konten und Netzwerk. Bei Bedarf stellt arterm --login die Client-Anmeldung ausdrücklich wieder her. Löschen Sie keine Wiederherstellungsdaten und installieren Sie einen laufenden Host nicht neu. Updates lehnen das Unterbrechen aktiver Sitzungen standardmäßig ab. Auf dem Host hält arterm-host stop --disable den Host angehalten; arterm-host start startet ihn wieder, stellt aber verlorene Shells nicht wieder her. Die Deinstallation behält Daten. Das Entfernen des Zertifikatsvertrauens beschreibt die Installationsanleitung.

Nutzer von Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi und Qwen CLI können diese Anleitung lesen und gewöhnliche Windows-Befehle ausführen, sofern ihre Werkzeuge dies erlauben. Das behauptet keine native Integration oder Zertifizierung aller sechs Clients. Ein weiteres lokales Steuerterminal erfordert denselben Benutzer, dieselbe Windows-Anmeldesitzung, denselben Integritäts-/Erhöhungskontext und einen bytegleichen, vertrauenswürdig signierten Client. Der Verbindungsprozess muss weiterlaufen.

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Troubleshooting](../../README.md#installation-and-troubleshooting-details) · [Upgrade / stop](../../QUICKSTART.md#upgrade-without-registering-again) · [Trust removal](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)
