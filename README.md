# arTerm — Persistent remote Windows terminal

<a id="languages"></a>
<details>
<summary>Languages / 语言 / 言語 / اللغات (16)</summary>

[English](README.md) | [简体中文](docs/i18n/README.zh-CN.md) | [繁體中文](docs/i18n/README.zh-TW.md) | [日本語](docs/i18n/README.ja.md) | [한국어](docs/i18n/README.ko.md) | [Español](docs/i18n/README.es.md) | [Português (Brasil)](docs/i18n/README.pt-BR.md) | [Français](docs/i18n/README.fr.md) | [Deutsch](docs/i18n/README.de.md) | [Italiano](docs/i18n/README.it.md) | [Русский](docs/i18n/README.ru.md) | [Türkçe](docs/i18n/README.tr.md) | [Tiếng Việt](docs/i18n/README.vi.md) | [Bahasa Indonesia](docs/i18n/README.id.md) | [हिन्दी](docs/i18n/README.hi.md) | [العربية](docs/i18n/README.ar.md)

</details>

Keep builds and coding agents running through client/VPN loss; reattach to the same interactive shell, variables and working directory; catch up on retained output—bounded replay, not a complete log. Both ends Windows; host stays running and logged in. No host reboot/logoff recovery.

[Set up](#get-connected) · [Verify downloads and trust](DEVELOPMENT-INSTALL.md) · [Prove same-shell continuity](#prove-that-you-returned-to-the-same-shell)

<img src=".github/arterm-before-after.png" width="700" alt="Before: a terminal-bound workflow loses context when the client disconnects. After: arTerm keeps work on the remote host so you can reconnect and continue.">

*Workflow illustration with English labels, not a live-session screenshot or
test proof. The remote Windows host must stay running and logged in; arTerm
survives client-side loss, not host reboot or shutdown.*

- **Keep work running when the client disappears.** Builds, coding agents and long-running commands stay on the remote Windows host through client closure, VPN loss or notebook restart.
- **Return to your work, not a blank shell.** Reattach to the same interactive shell process, with its variables and working directory intact.
- **Catch up on what happened while away.** Reconnection replays retained terminal output; retention is bounded, so gaps are possible and this is not a complete log.

For ordinary noninteractive jobs, your existing approved remote-command channel may already be sufficient. Choose arTerm when you need to reattach to the **same host-owned interactive Windows shell and environment** after client closure, VPN loss or notebook restart, using its documented connectivity and local controls. Both ends must be Windows, and the host must remain running and logged in; this is not host-reboot recovery.

**Fits:** Windows on both ends, the same GitHub account for both tunnels,
and permitted outbound connectivity. The current downloads are
**development-signed, not publicly trusted production builds**;
[verify downloads and review the trust decision](DEVELOPMENT-INSTALL.md)
before installation. Never bypass OS warnings or your organization's policy.
**Does not survive:** host reboot, logoff, crash/shutdown, or remote shell `exit`.

<details>
<summary>File-transfer capabilities and network details</summary>

- **Reach your Windows machines across networks.** Connect to configured Dev
  Boxes and VMs through GitHub-authenticated tunnels, without needing the same
  LAN or direct inbound host access. A configured **Windows Sandbox** is only a
  candidate: separately validate your provisioned guest, tunnel connectivity and
  guest/shell lifetime. No inspected versioned Sandbox proof is provided here;
  do not assume support or import desktop-minimization advice from other tools.
- **Move results without terminal paste.** Send files or whole folders through
  the attached session with integrity checks and unique receiving destinations.


</details>

Client and host need permitted outbound connectivity. See the
[quick start](QUICKSTART.md) for setup and
[development-signed installation guide](DEVELOPMENT-INSTALL.md) for trust requirements.



## What you need

| Requirement | Local client | Remote host |
| --- | --- | --- |
| Operating system | Windows | Windows |
| arTerm component | `arTerm-Client-Setup.exe` | `arTerm-Host-Setup.exe` |
| Tunnel dependency | Microsoft **devtunnel CLI** | Compatible native **VS Code tunnel CLI**, such as `code-tunnel.exe` or standalone `code.exe` |
| Tunnel sign-in | Your GitHub account | **The same GitHub account** |

The full VS Code editor is optional when a compatible standalone native CLI is
provided. Tunnel sign-in is separate from `gh` CLI authentication.

Host setup also requires policy-permitted **Windows Script Host with VBScript** for its per-user bootstrap. If blocked, stop and use an approved route; do not enable it against policy or use a fallback.

Installers detect dependencies and offer downloads only with consent; vendor
binaries are not bundled. Host setup requires acceptance of the VS Code server
license terms. Host startup is at **Windows user logon**, not a SYSTEM service:
a cold boot requires Windows sign-in, and arTerm does not configure autologon.

### Released downloads versus development gates

The existing download route above redirects to the published
[v0.7.1 release](https://github.com/yeelam-gordon/arterm/releases/tag/v0.7.1).
It offers **development-signed**, not publicly trusted production-signed, x64
and ARM64 archives. Verify checksums and the public certificate using
[DEVELOPMENT-INSTALL.md](DEVELOPMENT-INSTALL.md); local trust is a separate,
explicit, policy-permitted decision. Do not bypass SmartScreen, application
control, Authenticode checks, dependency consent, or the server license prompt.

For source `73c59e0682fbe9a4d966dd48468436248b94e3e7`, the public
[native Windows CI run](https://github.com/yeelam-gordon/arterm/actions/runs/37918662460)
completed successfully. The release author reports protected signing gates on
both architectures; those private results are not independently verified here,
and the release explicitly excludes real-cloud terminal testing. The pending
ARM64 rollout discussion in [SIGNING.md](SIGNING.md) describes earlier
source-change scope, not the current download inventory. Neither the published
archives nor CI success qualifies this entire development branch: independent
file-transfer archive review and direct-input gates above remain unresolved;
arbitrary TUIs, custom read-line handlers, and mixed-architecture end-to-end
operation are not certified by this documentation.



## Get connected

**Before trust or installation:** follow the concrete
[downloaded ZIP checksum comparison](DEVELOPMENT-INSTALL.md#verify-the-downloaded-release-payload-before-trust-or-installation)
against the public release manifest. Verifying only the CER fingerprint does
not verify the archive or executable payloads. Stop if the published payload
checksum is unavailable or mismatched; a match is not a safety or trust approval.
Then perform the [read-only Authenticode inspection](DEVELOPMENT-INSTALL.md#inspect-extracted-executable-signatures-without-running-them)
of the extracted installers and clients. Require the pinned development signer
and `Valid` Windows trust before running them; non-`Valid` results must stop, not
trigger a warning bypass. Manual certificate trust remains a separate,
explicit, policy-permitted decision.

**Keep session data private.** Retained terminal output, normal shell history,
and recovery files may expose commands, paths, code or secrets. Protect them on
both machines; redact sensitive content before sharing logs, screenshots or
support bundles. DPAPI protection of credentials does not mean every retained
artifact is encrypted or content-free. The narrower readiness-diagnostic
content exclusions do not apply to all session data.

1. Choose the Windows **x64 or ARM64 ZIP archive** matching the machine on
   which you will install from [Releases](https://github.com/yeelam/arterm/releases/latest).
   The published v0.7.1 assets are
   `arTerm-0.7.1-windows-x64-development-signed.zip` and
   `arTerm-0.7.1-windows-arm64-development-signed.zip`.
   Complete the [payload and signature verification](DEVELOPMENT-INSTALL.md)
   described above before trust or installation. Local control requires valid
   Windows Authenticode trust and matching signed client binaries; unsigned
   public CI/local builds cannot replace them. Any development-certificate trust
   needs separate explicit approval permitted by your organization's policy.

2. **On the remote host**, run `arTerm-Host-Setup.exe`. Open a fresh PowerShell,
   then configure a unique host name:

   ```powershell
   arterm-host setup --name my-devbox
   ```

   Complete tunnel sign-in and keep the name-based client registration command
   printed by setup.

3. **On the local client**, run `arTerm-Client-Setup.exe`. Open a fresh PowerShell:

   ```powershell
   arterm --login # Optional explicit sign-in; arterm login is also supported
   ```

   Client Setup completes local initialization; `arterm setup` is not required.
   Use the same GitHub account as the host. Connections automatically attempt
   GitHub sign-in when credentials are missing or expired. **Copy and run the host's printed
   registration command once**; client setup does not register the host for you.
   Then create your session:

   ```powershell
   arterm connect my-devbox MyWork
   ```

**Repeat that exact command to reattach.** `MyWork` is a non-secret session
reference, not a password. Press **Ctrl+]** to detach and leave the shell running;
type **`exit` inside the remote shell** to end it. Starting with 0.7.1, a session
name can be reused after its old session is known ended or has been
authoritatively retired. Reuse creates a fresh GUID and clean session data;
the old GUID never addresses the replacement. Unknown or unavailable sessions
are not silently replaced.

Without a reference, `arterm connect my-devbox` **only prints** a complete
reusable command and exits; it does not start a remote shell.

## Prove that you returned to the same shell

After the setup and registration above, use a new reference for this small
PowerShell check on the **local client**:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

In the **attached remote PowerShell**, run:

```powershell
$artermProof = 'kept-on-host'
$artermPid = $PID
$artermCwd = (Get-Location).Path
$PID; $artermProof; (Get-Location).Path
```

Note the PID and directory. Press **Ctrl+]** (or close only the local client
tab). Keep the remote Windows host running and logged in. In a local terminal,
repeat the **same** command:

```powershell
arterm connect my-devbox MyProof --shell powershell
```

In the reattached **remote** shell, run:

```powershell
$PID; $artermProof; (Get-Location).Path
($PID -eq $artermPid) -and ($artermProof -eq 'kept-on-host') -and ((Get-Location).Path -eq $artermCwd)
```

Replay is bounded; keep a remote build log if you need complete output. Reconnection is not an exact screen/history checkpoint.

Expected: the same PID, `kept-on-host`, the same directory, and `True`.
This demonstrates client detachment, not host reboot recovery. Finish this
test with `exit` **inside the remote shell**; that ends it deliberately.
If discovery or sign-in fails, run `arterm doctor my-devbox`, check both tunnel
accounts and outbound access, and use `arterm --login` for explicit client
authentication recovery. Do not delete saved recovery records or reinstall a
running host just because the client disconnected. See
[troubleshooting](#installation-and-troubleshooting-details),
[safe upgrades and stopping](QUICKSTART.md#upgrade-without-registering-again),
and [trust removal](DEVELOPMENT-INSTALL.md). Ordinary `arterm-host stop` may restart at the next scheduled check; use `arterm-host stop --disable` for a persistent stop after safely finishing sessions. `arterm-host start` restarts the host, not lost shells.

<details>
<summary>Console restoration and parser limitations</summary>

On detach, remote exit, or connection failure, the interactive client restores
the local console flags, code pages, and inherited keyboard, focus, paste, and
mouse input modes. The client's buffered attachment input is discarded on exit;
unrelated native input records are preserved. Scripted `--stdio` clients
do not change these console modes. Mode discovery waits at most 100ms. If the
console does not report a mode, remote changes to that input mode are suppressed
instead of guessing the parent shell's state; other output remains available.
Outstanding local mode replies remain tracked across fragmented input and
connection failure. Exit allows a further bounded 200ms for outstanding replies,
preserves unrelated startup typeahead, and reports queries still unanswered.

This does not reset the outer ConPTY input parser. An input stream ending in an
unterminated CSI can consume the next typed character even without arTerm;
restoring console modes and clearing queued input records cannot repair that
separate parser state.

</details>

## Release and workflow scope

Public release metadata checked **2026-10-10** lists **v0.7.1** x64/ARM64
**development-signed ZIPs** and `SHA256SUMS`. The existing
[release route](https://github.com/yeelam/arterm/releases/latest) redirects to
that release today; later versions need their own evidence. Metadata/checksums
are not local binary execution or whole-product certification.

| Workflow | First-use decision and evidence boundary |
| --- | --- |
| Attach, detach, reattach to the same shell | Use the unchanged PID/variable/cwd procedure above; `True` is expected, not a claimed run here. Host must stay running/logged in. |
| Managed `send` / `read` | Requires a **newly created supported PowerShell/pwsh** session and the matching trusted local owner context. Existing shells are not retrofitted; other shells/unknown versions are not qualified by this example. |
| Direct input and file/folder transfer | Implemented in the current branch, but its direct-input and independent transfer-archive review gates remain open. Published archives/CI do not close those gates or establish every branch capability for every release. |

## Control an attached session

<details>
<summary>Technical details: session lifetime, shell integration, and development gates</summary>

**File-transfer development gate:** independent archive review remains blocked;
this integration is not release-qualified by that review.

**Limit:** the remote host must stay running and logged in. Remote Windows
reboot/logoff, host crash/shutdown, or remote shell `exit` ends that process.
arTerm does not checkpoint or resurrect processes after those events.
Managed command execution requires a newly created, supported PowerShell/pwsh
session; existing sessions are not retrofitted.
The direct-input integration additionally requires PSReadLine's F24 handler,
buffer inspection, insertion, and accept-line APIs. An authenticated, bounded
in-memory pipe delivers command data only to the owned shell process; the handler
checks the real edit buffer and inserts the original source for ordinary
top-level execution. Base64 is transport data, not an execution wrapper.
A read-only `PSConsoleHostReadLine` adapter arms execution tracking only after
the original reader returns the accepted source. Prompt redraws during editing
or acceptance cannot report command completion or managed readiness.
Managed source is excluded from PSReadLine history with a one-line handler that
restores the exact current user handler before the next manual line. Manual
history, save settings, and user filters are not globally disabled or replaced.
Keyboard input is permitted while a managed command is running (including
`Read-Host` answers) and while its profile prompt is finishing, but remains guarded before dispatch; other automated
commands wait. Partial edit buffers are never cleared or overwritten.
Command completion is published even when a preserved partial line keeps
automation not ready; waiting for completion does not require submitting that line.
Unsupported integration and invalid syntax fail explicitly without a wrapper
fallback. Replacing the prompt or PSReadLine integration can stop managed
completion/readiness.
Cancellation is checked at the existing client queue-to-delivery boundary.
Once dispatched, caller exit does not cancel queued work; delivery uncertainty
must be queried by command ID, never retried under a new identity. Admission is
not proof of execution: a real-buffer rejection, invalid syntax, or a hook that
does not accept within five seconds records `not_submitted` (`submitted:false`).
The payload is revoked and cannot run later. Timeout disables managed readiness
instead of leaving an indefinitely accepted command or guessing the edit buffer.
Mailbox commitment is distinct from confirmed PSReadLine acceptance. If start
confirmation is missing for five seconds after commitment, the outcome becomes
`unknown`, never `not_submitted`: the command may have executed. Its identity
cannot be replayed. Manual input remains usable; a subsequent real prompt can
restore readiness without falsely completing the unknown record. Late start or
completion evidence is accepted only for the same still-eligible command ID.

**Advanced local-host opt-in:** `ARTERM_SHELL_HISTORY_PATH` may specify an absolute
PowerShell history path for newly created supported interactive shells, including
unmanaged ones. The host validates and encodes this path as data and applies it
before the first ReadLine; it does not change history save style or user filters.
Unset preserves the user's normal path. This is a host-process setting, not a
remote-client option; `VSTERM_HISTORY_PATH` remains unsupported. Test fixtures set
unique owned paths explicitly because changing `APPDATA` alone does not isolate
PSReadLine history on either supported shell.

**Direct-input development gate:** this branch is not release-qualified.
Fresh host/client/controller tests cover two `Read-Host` answers and recovery
through the existing interrupt request on Windows PowerShell and PowerShell 7.
Earlier minimal F24-only interruption probes are archived outside shipping tests;
arbitrary TUI behavior and all custom profile/read-line handlers are not certified.

</details>

### Agent-neutral command access

Copilot CLI, Claude Code, Codex, Gemini CLI, Kimi, and Qwen CLI users can inspect
these public instructions and, where their tools permit ordinary Windows CLI
execution, use the same attach/control workflow. This is **not a claim of native
integration or six-client runtime certification**. Keep one attachment process
running; controller commands require the same local Windows user, logon/session,
integrity/elevation context, and byte-identical trusted signed client. Your caller
owns backgrounding; there is no arTerm `--background` or `resume` command.

**Before managed control:** create a new supported PowerShell/pwsh session; keep its matching, trusted local attachment running. Review the [release/workflow scope](#release-and-workflow-scope) and open development gates; do not assume an old or arbitrary shell gained integration.

Leave `arterm connect my-devbox MyWork` running, then use another local terminal:

```powershell
arterm list --client
arterm list --server my-devbox
arterm send my-devbox MyWork --command 'Get-Location' --wait --timeout 60s
arterm read my-devbox MyWork --lines 20
```

**Before transfer:** the current branch automatically removes recipient `Zone.Identifier` (Mark-of-the-Web). This is not malware scanning, benign-content assurance or certificate trust. The independent archive-review gate remains open; see [workflow scope](#release-and-workflow-scope).

File transfers use the same active owner:

```powershell
arterm send my-devbox MyWork --file C:\work\report.txt
arterm receive my-devbox MyWork --file C:\work\results --json
```

All received files are **automatically unblocked** at the **host** for send and
the **client** for receive, including every newly extracted regular folder file.
Before publication, arTerm removes exactly `Zone.Identifier` if present and
verifies it is absent; an already absent stream is valid. Source files and
their marks are unchanged. No option is required. Both endpoints must support
`recipient-unblock-v1`; older peers fail explicitly rather than falsely
confirm unblocking.

This removes Mark-of-the-Web; it does **not** scan for malware, make files
benign, execute them, release open-file locks, or change execution policy,
antivirus, or certificate trust. Review files before opening or running them;
a signature or matching hash does not prove benign content. Byte/ZIP transport
does not preserve source ADS, URLs or ACLs. See [transfer details](QUICKSTART.md#automation) for publication,
receipts, limits, and unknown-outcome handling.

<details>
<summary>Controller lifecycle, readiness, inventory, and diagnostic details</summary>

`connect` is a normal running native process. Your caller, Copilot, or OS owns
backgrounding; arTerm has no `--background`, `start`, or `resume` command.
Each connection owns its own local named pipe, not a broadcast endpoint or
client daemon. Local controls require that exact active owner; missing or
ambiguous owners are errors. Authorized server inventory and termination also
work while detached.

Plain `arterm list` shows locally known session names and IDs, except sessions
the client knows have ended. A session with uncertain status remains visible:
an unreachable host, server reboot, or changed broker is not itself proof of
termination. These entries are not claims that a remote process is still alive.
Default listing does not connect or sign in. Use `list --server MACHINE` for
current authorized host inventory and `--json` for structured output.
Known-ended or authoritatively stale entries are retired from the session
inventory. Only the identity guard needed for safe old-GUID handling remains.
Reusing a retired name atomically publishes fresh session data, not the old
credential or broker binding.

`send` waits for a safe prompt for up to **30 seconds** by default, then returns
a command ID and acceptance, not proof of success. `--timeout 5s` overrides this
readiness budget even without `--wait`. With `--wait`, an explicit positive,
finite timeout is required and covers readiness plus completion; it returns
early on the matching real completion **after output delivery**.
A readiness timeout exits 124 with `phase: readiness` and `submitted: false`;
the command is discarded without changing partial interactive input. Exiting
while awaiting readiness also discards the pending request. Once dispatch starts,
timeout or waiter exit does not cancel execution. Query the command ID rather
than blindly resending. Readiness/completion waiters are bounded and do not block
the owner's input, heartbeat, or other controls. See [automation and limits](QUICKSTART.md#automation)
for status, interruption, termination, and idempotency.

Controls and client/server inventories print readable summaries by default.
Use `--json` on `send`, `read`, `list`, `list --client`, `list --server`, `interrupt`,
`detach`, or `terminate` when parsing output in scripts. JSON retains the
structured response schema and the same exit codes.

If readiness times out or a send is rejected, the human-readable result includes readiness details;
`arterm list --client --json` and `send ... --json` also report
`shell_status`, `readiness_reason`, `command_capability`, `command_execution`, and
the remote `host_version` when reported. Connected does not imply ready: an old
host, a session created without integration, startup/profile initialization,
pending interactive input, and a running managed command are distinct cases.
An absent host version is unknown, not the local client's version. Win32 key-up
events do not edit a line and must not block sends; key-down edits, partial
sequences, and unclassified terminal input remain guarded. Existing sessions
are never silently replaced or retrofitted.

Support is established by the shell's correlated integration lifecycle marker,
not arbitrary VT traffic. Startup waiting uses the same finite readiness budget;
capability that is still unknown before the host handshake is not a rejection.
If no first ready marker arrives, the timeout reports `IntegrationNotEstablished`
and submits nothing. Pre-marker input is not described as a busy command.

Focus-in/out notifications are treated as non-editing, including when the local
parent terminal enabled focus reporting before arTerm started. The remote host
cannot observe that local negotiation. Other unrecognized terminal input remains
guarded, and focus notifications never clear a pending edit or busy command.

Readiness diagnostics are written automatically beneath the data root in
`client\diagnostics` and `host\diagnostics`. Failed command responses include
the owning client's exact log path. Logs correlate session/command IDs with
state transitions and input categories, **not input text, command contents,
terminal output, key codes, hashes of commands, credentials, or tokens**.
`partial_human_input` describes the host tracker's state, not proof that visible
text remains in the prompt. See the diagnostic collection steps in QUICKSTART.
Pure modifier-key presses carrying no character are now nonediting, like key
releases and focus reports; actual edits remain guarded.

</details>

For standalone dependency paths, setup options, and troubleshooting, see
[QUICKSTART.md](QUICKSTART.md). See [SIGNING.md](SIGNING.md) for signing policy.

## Session and recovery details

<details>
<summary>Host startup and ownership details</summary>

The per-user Task Scheduler task uses InteractiveToken/LeastPrivilege and runs
under the current process's Windows token SID, including Entra/AAD `S-1-12-1`
identities. Both the principal and logon trigger use that SID; no local-account
name is derived or substituted. Registration validates persisted principal,
trigger, executable and data-root ownership. Scheduler-returned account names
are translated to SIDs only for verification; rejection is reported without
password, SYSTEM, service, batch-logon or local-account fallback. At user logon,
and every **10 minutes** afterward (`PT10M`, no repetition duration), the task
runs a short, idempotent `ensure-running --data-root ...` check. A responsive
broker is left untouched; an absent broker starts in the background, and the
check exits. A locked/unresponsive broker is reported, never killed or replaced.
There is no continuous monitor or crash-retry backoff. Task Scheduler's
`RestartOnFailure` is not used. InteractiveToken prevents checks without a
logged-in user; no service or unattended account logon is configured.
The check and broker append startup/PID, shutdown, and error diagnostics
to `host\host.stdout.log` and `host\host.stderr.log` under its explicit data root;
the tunnel retains its separate `code-tunnel.*.log` files. A small owned VBScript
bootstrap runs under GUI `wscript.exe`, launches the check with window style 0,
waits for that bounded check and propagates its exit status. The check creates
its broker with `CREATE_NO_WINDOW`. Shared terminal windows are never hidden or
killed; Task Scheduler's Hidden flag is not relied upon. Registration probes
Windows Script Host/VBScript availability and fails explicitly if unavailable
or policy-blocked, with no visible-console fallback.
**Ordinary `arterm-host stop` permits automatic startup at the next 10-minute
check (or next logon). Use `stop --disable` to keep it stopped until explicit
`start`.** Recovery creates a new broker, not restored terminal sessions.
Install/update/uninstall only manage ownership-verified tasks for this installation.
Runtime setup holds that pause through shutdown, explicit authentication, configuration
save and task registration. A failed setup restores the prior configuration before
restoring task state; unsafe recovery leaves checks disabled and reports the error.
The installed `arterm-host.exe` and `vsterm-host.exe` aliases resolve the same
SID/data-root task only when both belong to the same ownership-marked host
installation. The registered action remains exact; another directory or publisher
is not treated as an alias.
Legacy `VsTermHost` Run entries are removed only after successful task registration
and only when their command matches the owned installed executable.

</details>

<details>
<summary>Names, retries, replay, and command history</summary>

References contain 1..64 ASCII letters, digits, `_`, or `-`, beginning with a
letter or digit. Names are case-insensitive (`MyWork` equals `mywork`); GUIDs
use canonical hyphenated form. References are scoped to the registered target
identity and current Windows user. They are not portable credentials.

The host owns a persistent ConPTY and drains output while detached. Retries
reuse the same internal GUID and creation request. Saved authorization uses
random credentials protected by Windows DPAPI, never secrets derived from names.
Repeat `arterm connect my-devbox MyWork` to recover it; saved legacy GUID
references remain supported by `connect`.

Mapping and protected state are persisted before remote creation. Exclusive
locks prevent concurrent local use from creating duplicate sessions. Missing,
corrupt, incomplete, expired, or unauthorized recovery records fail closed.
Do not delete them to troubleshoot a connection.

Repeating original `--shell` and `--cwd` values is supported when they exactly
match saved creation values. Incompatible overrides fail. `--retries` controls
attachment retries. Loopback `--address` and `--stdio` remain isolated-test
surfaces and are preserved in printed commands.

Session lifetime is separate from output retention. Replay is bounded: a
long-running process may outlive retained terminal history. A new console
replays the available output, not an exact screen checkpoint after eviction.
Input acknowledgments indicate acceptance into the host's ordered input path,
not completion of the shell command.

Type the complete reusable command so your shell records it normally. arTerm
does not append history, inject keystrokes, launch parent-shell wrappers, or
restore individual tabs. Legacy history environment variables do not enable
history injection.

</details>

## Installation and troubleshooting details

<details>
<summary>Dependency selection, discovery, and installer options</summary>

Installers are per-user and validate dependency Authenticode trust, Microsoft
publisher identity, and required CLI capabilities. Interactive installation
can offer the official WinGet packages `Microsoft.devtunnel` or
`Microsoft.VisualStudioCode` with explicit agreement consent. The latter
includes the editor; use a compatible standalone CLI and its setup path option
if you do not need it.

Installer `/quiet` or `/no-download` requires suitable preinstalled dependencies
and never starts a sign-in prompt. `/log <absolute-path>` records results.
`/uninstall` removes the product role, not vendor dependencies. Host updates and
uninstall refuse live sessions; finish them explicitly before upgrading.
Recovery data and the installer cache are retained.

Host setup uses isolated code CLI metadata rather than replacing an existing
managed tunnel. Use its printed registration command: it stores the friendly
host name, not a generated service ID. Connections and retries resolve that
name afresh through `devtunnel list`. Give hosts unique names under the account;
ambiguous names fail. Exact service IDs remain an advanced override, not the
default registration route.

`arterm doctor my-devbox` checks discovery and reachability. The host's
`code tunnel` and the client's `devtunnel connect` are two ends of the same
transport. Starting with 0.7.0, the client attempts a bounded
`devtunnel user login --github` under the current Windows user when credentials
are confirmed missing or expired, then checks authentication again before
continuing. Valid sign-in is left unchanged. Concurrent client operations
coordinate authentication recovery; there is no periodic login task or
keep-alive traffic.

The automatic login process has no console window, but GitHub/browser sign-in
or approval can still appear. Existing browser sign-in may make it complete
without typing credentials; browser-free or unattended authentication is not
guaranteed. Network errors, malformed status responses, and timeouts are not
treated as permission to launch login. Recovery is bounded rather than an
endless login/reconnect loop. If recovery fails, run `arterm --login` (or
`arterm login`) explicitly with the host's GitHub account, then retry.
Installation and host authentication behavior are unchanged.
Do not reinstall the host or delete session records.

</details>

## Upgrade and storage compatibility

<details>
<summary>Existing VsTerm installations, saved commands, and older hosts</summary>

arTerm 0.5 installs canonical `arterm.exe` / `arterm-host.exe` binaries and
byte-identical `vsterm.exe` / `vsterm-host.exe` compatibility aliases in the same
role directories. Existing absolute commands and registered host paths remain
valid without rewriting target configuration or recovery records.

The following remain stable compatibility identifiers, even on fresh installs:

- Data and protected state: `%LOCALAPPDATA%\VsTerm`.
- Installed binaries: `%LOCALAPPDATA%\Programs\VsTerm\Client` or `Host`.
- Existing registry keys, pipe identity, and session protocol (legacy startup migrates to Task Scheduler).

New ownership markers use arTerm. Both legacy VsTerm publisher markers remain
accepted for upgrade/uninstall with the matching role. Installers check both
canonical and legacy runtime filenames and do not bypass live-session guards
when only an old executable name exists. Older DevBox Remote prototype state
is left untouched and is not automatically imported.

Reusable `connect` requires the host capability
`ended-session-rejection`, supplied by arTerm and compatible VsTerm 0.3 hosts.
Unsupported hosts are rejected before creation or attachment. The local
reservation remains available for retry with a compatible host; protocol
version 1 alone does not establish support.

For a still-live VsTerm 0.2 session, `connect` with its saved GUID
can use an existing unnamed record with a saved credential. This legacy path
warns that an old host may replay a retained exited session and return its old
exit code instead of rejecting it as already ended. It never creates a
replacement. A named record's GUID does not bypass capability negotiation.

Tokenless creation recovery on a 0.2 host still requires the matching 0.2 client.
Finish legacy sessions before upgrading their host; restarting the broker does
not preserve running shells. A failed `terminate` lookup of an unused GUID does
not reserve it or prevent its later first `connect`. Existing remote sessions
do not gain the new in-memory command adapter when a client reconnects.

</details>

## Build and test

<details>
<summary>Native Windows/MSVC builds and isolated verification</summary>

Use Windows and a host-native stable MSVC Rust toolchain. The C runtime is
statically linked on both x64 and ARM64. ARM64 runtimes and installers have
been cross-built and their PE architecture checked. Native ARM64 functional,
installer lifecycle, and signed-production IPC validation were pending in the
earlier source-change notes. See [released-download evidence](#released-downloads-versus-development-gates)
for the later v0.7.1 native CI and attributed signing results; these do not close
every development gate. Do not treat cross-compilation as native certification.

```text
cargo test --locked --features test-unsigned-ipc -- --test-threads=1
cargo run --locked --release --bin package
```

The package builder writes `arterm.exe`, `arterm-host.exe`,
`arTerm-Client-Setup.exe`, `arTerm-Host-Setup.exe`, `LICENSE`,
`THIRD-PARTY-NOTICES.txt`, and `SHA256SUMS`
to `dist`. Installers embed only the project's own executables. For runtime-only
builds:

```text
cargo build --locked --release --bin arterm --bin arterm-host
```

For architecture-labeled packages, run the package builder on the build host
and pass the payload target **after** `--`:

```text
cargo run --locked --release --bin package -- --target x86_64-pc-windows-msvc
cargo run --locked --release --bin package -- --target aarch64-pc-windows-msvc
```

These write separate `dist\windows-x64` and `dist\windows-arm64` directories,
each with its own six-entry `SHA256SUMS` and the same seven filenames above.
No `--target` still selects x64 and the legacy flat `dist` output. The builder
always uses an explicit Cargo target for runtimes and installers; intermediate
outputs are `target\<triple>\release` (or under `CARGO_TARGET_DIR`).
Cross-compilation requires the target's Rust standard library and MSVC/Windows
SDK libraries; add `rustup target add aarch64-pc-windows-msvc` if missing.
Do not run an ARM64 package builder on an x64 host: the builder itself must
remain host-runnable. Avoid an ambient `CARGO_BUILD_TARGET` when launching it,
or explicitly pass the host target to `cargo run` before `--`.

The builder checks PE32+ executable headers, section-table and raw-data bounds,
a file-backed executable entry point, certificate-table bounds when present,
and matching PE machines in both runtime payloads and resulting installers.
Truncated runtime inputs are rejected before the installer build. These
structural checks do not replace Authenticode verification or native execution.
`--target <triple> --payload-dir <signed-directory>` preserves the supplied
runtime bytes without rebuilding, with the same architecture checks.
Public CI uses native `windows-2022` x64 and `windows-11-arm` ARM64 jobs,
explicit target triples, and a required nonzero ignored-functional-test count.
These jobs were configured but not executed as part of the earlier local source
change; the later release-linked public run is recorded above. No CI or runtime
tests were executed for this documentation change.
For feature-branch validation, open a draft pull request against `main` after
review: the existing `pull_request` trigger runs both native jobs on opening
and subsequent pushes. No signing credentials or production trust are available
to that workflow. A cross-build alone must not be reported as a native CI pass.

The remote MessagePack protocol uses architecture-independent framing; a
golden-byte test runs on both CI targets. An ARM64 client connecting to an x64
remote host still needs mixed-architecture end-to-end validation. Local IPC
peers must remain byte-identical signed images from the same architecture and
build; neither certificate nor image-hash checks are relaxed.

`test-unsigned-ipc` is an explicit debug-only fixture feature, off by default.
Release builds must omit it; enabling it for release fails compilation.
Production has no environment-variable or CLI authentication bypass.
Unsigned functional tests do not establish signed-production IPC readiness.
The separate signed CI gate exercises the actual signed CLI; see
[SIGNING.md](SIGNING.md) for its procedure, not a claim that a run has passed.

Tests exercise actual ConPTY processes, PID/state recovery, exit behavior,
history non-insertion, and console alignment. Installer lifecycle tests use
temporary files and redirected per-process HKCU; ignored checks require
installed signed vendor dependencies. The test-only `VSTERM_REMOTE_HOME`
selects isolated data. Never point tests at production state.
Tests do not sign into cloud accounts or change existing tunnels.

</details>

## License and signing

The source is [MIT licensed](LICENSE). Vendor components retain their own
licenses. VS Code Server/control RPC usage remains subject to Microsoft's
license and compatibility constraints; its Server binary is not redistributed.

Self-signed development downloads require the explicit trust procedure in
[DEVELOPMENT-INSTALL.md](DEVELOPMENT-INSTALL.md). SmartScreen reputation and
organizational application-control policy are separate from certificate trust.
Local package builds are unsigned by default. Neither a GitHub download nor
source availability establishes Authenticode trust.

<details>
<summary>Local IPC application identity and trust limits</summary>

The owner and caller mutually verify the actual OS-reported peer PID, image,
and token context. Both need valid Windows Authenticode chain trust, the exact
compiled public-certificate SHA-256, and byte-identical client images. Matching
user SID, logon, Windows session, integrity, and elevation are also required.
Filename or certificate subject alone, and arbitrary same-user programs, are
not trusted. Unsigned, tampered, wrong-certificate, or different-build peers
fail closed; a byte-identical `vsterm.exe` compatibility alias works.

This is an application-identity gate, not a hard boundary against administrators,
process injection, or other code invoking the legitimate CLI. arTerm does not
automatically import a certificate or change trust. Use the existing
[development trust guide](DEVELOPMENT-INSTALL.md) where policy permits it;
the certificate has not been recreated.

</details>

<details>
<summary>Maintainer signing and packaging sequence</summary>

A separate private, manual development-signing workflow signs runtime
executables before embedding them, then signs installers. Downloads include
the public certificate and instructions, never private signing material.
See [SIGNING.md](SIGNING.md) for policy and safeguards.

For an approved signing pipeline, build/sign the runtime first, then package
those exact bytes without rebuilding:

```text
target\release\package.exe --payload-dir <directory-containing-signed-runtime-exes>
```

Sign the resulting installers and regenerate checksums afterward. The default
package command is not a signing flow. Never commit signing keys or bypass
organizational software policy.

</details>
