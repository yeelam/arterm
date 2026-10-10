# arTerm: persistentes Windows-Remote-Terminal nach Verbindungsabbruch

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](../../README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Italiano](README.it.md) | [Русский](README.ru.md) | [Türkçe](README.tr.md) | [Tiếng Việt](README.vi.md) | [Bahasa Indonesia](README.id.md) | [हिन्दी](README.hi.md) | [العربية](README.ar.md)

</details>

Builds und Coding-Agenten laufen bei Client-/VPN-Abbruch weiter; verbinden Sie sich wieder mit derselben interaktiven Shell, ihren Variablen und dem Arbeitsverzeichnis; lesen Sie gespeicherte Ausgabe nach: begrenzte Wiedergabe, kein vollständiges Log. Beide Seiten Windows; Host läuft, Benutzer bleibt angemeldet. Keine Wiederherstellung nach Host-Neustart oder Abmeldung.

[Einrichten](#native-setup) · [Download und Vertrauen prüfen](../../DEVELOPMENT-INSTALL.md) · [Dieselbe Shell prüfen](../../README.md#prove-that-you-returned-to-the-same-shell) · [Technische Referenz (Englisch)](../../README.md#get-connected)

<img src="../../.github/arterm-before-after.png" width="700" alt="Vorher und nachher: Nach Client-Ende, VPN-Abbruch oder Notebook-Neustart zurück zur selben Remote-Shell, ihren Variablen und der Arbeit; der Host läuft weiter und der Benutzer bleibt angemeldet.">

*Ablaufillustration, kein Screenshot einer echten Sitzung und kein Testnachweis. Die Bildbeschriftungen sind auf Englisch. Der Nutzen: Nach einem Client-Abbruch zur selben Shell, ihren Variablen und der Arbeit zurückkehren, solange der Host läuft und der Benutzer angemeldet bleibt. Ein Host-Neustart wird nicht überbrückt.*

- **Die Arbeit läuft ohne den Client weiter.** Builds, Agenten und lange Befehle laufen auf dem Windows-Remote-Host auch bei Client-Ende, VPN-Abbruch oder Notebook-Neustart weiter.
- **Zurück zur Arbeit, nicht in eine leere Shell.** Verbinden Sie sich mit demselben interaktiven Shell-Prozess samt Variablen und Arbeitsverzeichnis.
- **Lesen Sie nach, was während Ihrer Abwesenheit geschah.** Beim Wiederverbinden wird gespeicherte Terminalausgabe wiedergegeben; die Aufbewahrung ist begrenzt, Lücken sind möglich, und es ist kein vollständiges Log.

Für gewöhnliche nichtinteraktive Aufgaben kann Ihr bereits genehmigter Remote-Befehlskanal ausreichen. Wählen Sie arTerm, wenn Sie nach Client-Ende, VPN-Abbruch oder Notebook-Neustart wieder **dieselbe auf dem Host laufende interaktive Windows-Shell samt Umgebung** benötigen; beachten Sie die dokumentierten Verbindungsbedingungen und lokalen Steuerungsgrenzen. Beide Seiten müssen Windows nutzen, der Host muss laufen und angemeldet bleiben. Ein Host-Neustart wird nicht überlebt.

<a id="native-setup"></a>

## Voraussetzungen und Vertrauen

Geeignet für Windows auf beiden Seiten, dasselbe GitHub-Konto für die Tunnel und erlaubte ausgehende Verbindungen. Der Host muss laufen und der Benutzer angemeldet bleiben. Host-Neustart, Abmeldung, Absturz, Abschalten oder Shell-exit beenden den Prozess ohne Wiederherstellung. Aktuelle Downloads sind entwicklungssigniert, keine öffentlich vertrauenswürdigen Produktionsbuilds.

[Download prüfen und Vertrauensentscheidung vor der Installation abwägen](../../DEVELOPMENT-INSTALL.md). Umgehen Sie keine OS-Warnungen oder Organisationsrichtlinien.

Client und Host benötigen Windows. Der Host muss laufen und der Benutzer angemeldet bleiben. Der Client nutzt Microsoft devtunnel CLI, der Host eine kompatible native VS Code tunnel CLI wie code-tunnel.exe. Der vollständige Editor ist optional. Beide Tunnel benötigen dasselbe GitHub-Konto; die Anmeldung bei gh ersetzt das nicht. Ausgehende Verbindungen müssen erlaubt sein. Downloads von Abhängigkeiten erfolgen nur mit Zustimmung; das Host-Setup verlangt die Annahme der VS Code server-Lizenz.

Wählen Sie x64 oder ARM64 unter [Windows-Downloads](https://github.com/yeelam/arterm/releases/latest). Der überprüfte Link leitet zu v0.7.1 weiter. Die Pakete sind mit einem Entwicklungszertifikat signiert, nicht mit einem öffentlich vertrauenswürdigen Produktionszertifikat. Prüfen Sie gemäß der [Installationsanleitung](../../DEVELOPMENT-INSTALL.md) Prüfsummen und Fingerabdruck des öffentlichen Zertifikats. Vertrauen darf nur mit ausdrücklicher Zustimmung und nach Organisationsrichtlinie für den aktuellen Benutzer hinzugefügt werden. Umgehen Sie weder Authenticode noch SmartScreen oder Anwendungskontrollen.

**Vor Vertrauensfreigabe oder Installation:** folgen Sie dem [ZIP-Prüfsummenvergleich](../../DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation) mit Get-FileHash und der öffentlich veröffentlichten Archivprüfsumme. Der CER-Fingerabdruck prüft weder ZIP noch Programmdateien. Fehlt die öffentliche Prüfsumme oder weicht sie ab, stoppen Sie. Eine Übereinstimmung garantiert weder Sicherheit noch Vertrauensfreigabe.

Der Host braucht außerdem richtlinienkonformes Windows Script Host/VBScript. Vor Vertrauen/Installation prüfen; bei Sperre stoppen, keine Richtlinie umgehen.

## Einrichtung

**Sitzungsdaten privat halten:** Gespeicherte Terminalausgabe, normale Shell-Historie und Wiederherstellungsdateien können Befehle, Pfade, Code oder Geheimnisse enthalten. Schützen Sie sie auf beiden Rechnern und entfernen Sie sensible Inhalte vor dem Teilen von Logs, Screenshots oder Supportpaketen. DPAPI schützt Zugangsdaten, nicht automatisch alle Dateien vor Klartext oder Inhalt. Die engeren Inhaltsausschlüsse für Bereitschaftsdiagnosen gelten nicht für sämtliche Sitzungsdaten.

Prüfen Sie vor der Ausführung die entpackten Installer und Clients mit Get-AuthenticodeSignature gemäß der [rein lesenden Signaturprüfung](../../DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them). ZIP-Prüfsumme, CER-Identität und Windows-Vertrauen sind getrennte Prüfungen. Erforderlich sind der festgelegte Entwicklungssignierer und Status Valid; unbekannter Signierer oder nicht Valid bedeutet stoppen, nicht Warnungen umgehen. Vertrauen braucht separat ausdrückliche Zustimmung und Richtlinienerlaubnis; danach erneut prüfen. Steuerung ist auf denselben lokalen Benutzer, dieselbe Sitzung, Integrität/Erhöhung und den identischen signierten Client beschränkt, nicht beliebiges Agententeilen zwischen Rechnern.

Führen Sie auf dem Host arTerm-Host-Setup.exe aus und öffnen Sie eine neue PowerShell:

```powershell
arterm-host setup --name my-devbox
```

Schließen Sie die Tunnel-Anmeldung ab und bewahren Sie den Registrierungsbefehl auf. Installieren Sie lokal arTerm-Client-Setup.exe, öffnen Sie eine neue PowerShell und verwenden Sie dasselbe GitHub-Konto. Führen Sie die vom Host ausgegebene Registrierung mit dem tatsächlichen Host-Pfad unverändert aus. Der Client ist bereits initialisiert; arterm setup ist nicht nötig. Verbinden Sie sich lokal mit einem neuen Sitzungsnamen:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

## Dieselbe Shell prüfen

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

## Grenzen

Prozesse werden nach Host-Neustart, Abmeldung, Absturz, Abschalten oder Beenden der Shell nicht wiederhergestellt. Der Host startet bei der Benutzeranmeldung; automatische Anmeldung und Betrieb ohne angemeldeten Benutzer werden nicht eingerichtet. Die Ausgabehistorie ist begrenzt. Der gesamte Entwicklungsbranch ist nicht releasequalifiziert: Die unabhängige Prüfung der Transferarchive und die Kriterien für direkte Eingabe sind noch offen. Beliebige TUIs sind nicht zertifiziert; automatisches Entsperren von Dateien ist keine Sicherheitsgarantie.

Wiedergabe ist begrenzt; für vollständige Ausgabe ein Remote-Buildlog behalten. arterm-host stop kann automatisch neu starten; arterm-host stop --disable hält den Host gestoppt.

## Wiederherstellung und Rücknahme

Bei Fehlern prüfen Sie mit arterm doctor my-devbox Konten und Netzwerk. Bei Bedarf stellt arterm --login die Client-Anmeldung ausdrücklich wieder her. Löschen Sie keine Wiederherstellungsdaten und installieren Sie einen laufenden Host nicht neu. Updates lehnen das Unterbrechen aktiver Sitzungen standardmäßig ab. Auf dem Host hält arterm-host stop --disable den Host angehalten; arterm-host start startet ihn wieder, stellt aber verlorene Shells nicht wieder her. Die Deinstallation behält Daten. Das Entfernen des Zertifikatsvertrauens beschreibt die Installationsanleitung.

## Erweiterte lokale Steuerung

Verwaltete send/read benötigen eine neu erstellte unterstützte PowerShell/pwsh-Sitzung und die passende vertrauenswürdige lokale Verbindungsidentität. Bestehende Shells werden nicht nachgerüstet. Die Veröffentlichung schließt Entwicklungsfreigaben für direkte Eingabe/Transferarchive nicht ab; unbekannte Versionen bleiben ungeprüft.

Nutzer von Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi und Qwen CLI können diese Anleitung lesen und gewöhnliche Windows-Befehle ausführen, sofern ihre Werkzeuge dies erlauben. Das behauptet keine native Integration oder Zertifizierung aller sechs Clients. Ein weiteres lokales Steuerterminal erfordert denselben Benutzer, dieselbe Windows-Anmeldesitzung, denselben Integritäts-/Erhöhungskontext und einen bytegleichen, vertrauenswürdig signierten Client. Der Verbindungsprozess muss weiterlaufen.

[README](../../README.md) · [Quick start](../../QUICKSTART.md) · [Automation](../../QUICKSTART.md#automation) · [Fehlerbehebung](../../README.md#installation-and-troubleshooting-details) · [Aktualisieren und stoppen](../../QUICKSTART.md#upgrade-without-registering-again) · [Zertifikatsvertrauen entfernen](../../DEVELOPMENT-INSTALL.md) · [Release evidence / gates](../../README.md#released-downloads-versus-development-gates) · [Signing](../../SIGNING.md)
