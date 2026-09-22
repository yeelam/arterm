# arTerm quick start

arTerm has two roles: install the **host** on the remote machine and the
**client** on your notebook. Use your normal signed-in Windows account.

For development-signed downloads, first follow
[DEVELOPMENT-INSTALL.md](DEVELOPMENT-INSTALL.md) to verify the public certificate
and explicitly trust it if your organization permits it. This is separate from
signing into the tunnel service.
Use the official signed client build on the local machine: both the connection
owner and control commands require Windows chain trust and byte-identical client
images. Unsigned public CI artifacts are for development, not production IPC.

## Remote machine: install and configure the host

From the extracted download:

```powershell
.\arTerm-Host-Setup.exe
```

Open a fresh PowerShell window, then run the one configuration command:

```powershell
arterm-host.exe setup --name my-devbox
```

Choose a unique lowercase name, accept the server license only if you agree, and complete
GitHub sign-in. Keep the client registration command printed by setup.
It uses the friendly name (`--tunnel my-devbox`), not the generated service ID.

```powershell
arterm-host.exe status
arterm-host.exe doctor
```

Host `status` prints a retained-session count and setup details; `sessions` prints a
table of retained sessions (including exited sessions). For scripts, use
`arterm-host.exe sessions --json` to retain the original JSON array unchanged.
`status --json` emits the broker response object (`ok` and numeric `sessions`) without
setup prose. `terminate SESSION-ID --yes` and `stop [--terminate-sessions]`
print request acknowledgements; add `--json` for an `{"ok":true}` acknowledgement.
Acknowledgement does not assert confirmed process exit. Failures retain exit 1
and a single stderr diagnostic; successful commands retain exit 0.

The host starts at Windows user logon. A cold boot still requires a Windows
sign-in; network/RDP disconnection is not the same as Windows logoff.

To use an already installed standalone native VS Code CLI without downloading
the editor, supply its absolute EXE path:

```powershell
arterm-host.exe setup --name my-devbox --code-path 'C:\Tools\code-tunnel.exe' --no-download
```

`--no-download` prevents dependency installation, not license or sign-in prompts.
Do not change a running host's configuration while it has live sessions.

### Upgrade without registering again

Rerun `arTerm-Host-Setup.exe` to update the binaries. Configured, enabled hosts
receive an immediate ensure-running check through their per-user Task Scheduler
registrations without rerunning setup. An expired sign-in or a failed dependency/
network readiness probe is reported but does not disable a previously enabled
schedule. Later checks can recover after explicit `arterm-host login` or network
recovery; no login or provider change is performed automatically. Intentionally
disabled tasks remain disabled.
Fresh installations register a disabled task until explicit named setup, sign-in,
and license acceptance are complete. Task Scheduler policy/permission failures
are errors; there is no silent Run-key or elevated service fallback.
The hidden task checks at user logon and every **10 minutes** afterward. It exits
after leaving an existing broker alone or starting an absent broker; it does not
monitor continuously. `arterm-host stop` allows the next check to start it again;
use `arterm-host stop --disable` for a persistent stop. Windows Script Host with
VBScript is required for the hidden bootstrap and is checked before registration.
The installer does not reset setup, tunnel identity, registered client boxes,
protected credentials, named session references, or certificates. Uninstall also
retains user data. Keep the same Windows user and `VSTERM_REMOTE_HOME` (default:
`%LOCALAPPDATA%\VsTerm`); another user/root reads different state, not a migration.
Do not remove that directory or register the box again just to upgrade.

Repeated `arterm-host setup` retains the saved name and Code CLI path; `--name`
is required only for first setup. Explicit name/path arguments reconfigure them.
Setup does not reset the isolated tunnel sign-in.

If the bridge reports `cannot connect to broker pipe ... (os error 2)`, the
executable ran but its scoped broker pipe was absent. This does not establish
that the registered executable path is wrong or that registration was lost.
On the remote host, use `arterm-host start`, then `arterm-host status --json`
and `arterm-host sessions --json`. These commands are available in 0.5.3.
Run them as the same Windows user, in the same logon session, with the same
`VSTERM_REMOTE_HOME` (or default data root) as the bridge. A stopped broker or
a mismatch in any of these scopes can produce this error. Starting a broker
does not recover sessions lost when the previous broker was killed; use a new
session reference for new work. An arbitrary attachment loss alone does not
prove whether the remote session is still alive.

By default, an upgrade refuses to stop a host with live sessions; unchanged
runtime setup leaves the running host alone. To deliberately end its sessions:

```powershell
.\arTerm-Host-Setup.exe --terminate-sessions
# Or, to stop and restart as part of runtime setup:
arterm-host setup --terminate-sessions
```

**These flags end ALL live sessions of the scoped host, including detached shells;
their running state cannot be recovered.** Saved references and credentials are
retained, but ended sessions need new references. Shutdown uses the existing
control command scoped to the current Windows user, logon session and data root,
not process-name or all-user kills. Owned tasks sharing this installation are
paused before shutdown/write, including other data roots. Their brokers in this
logon are stopped with the same session-refusal rules. Other logons are not killed;
if they hold installed executables open, the upgrade fails rather than killing them.
Setup waits at most 20 seconds for each stop command and installer upgrades wait
up to 10 seconds for executable release. An unresponsive host cancels the ordinary
operation. Timed-out controller processes are terminated and reaped, but a stop
request already delivered to the broker may still finish: check status before retrying.
This retains the existing scoped host control
boundary; it does not add pipe image attestation.
Both `--option` and `/option` installer spellings are accepted, including
`/terminate-sessions`. The option also applies to host `--uninstall`.

If the host will not respond, explicitly force-stop it while upgrading:

```powershell
.\arTerm-Host-Setup.exe --force-stop-host
arterm-host start
# For runtime reconfiguration rather than a binary upgrade:
arterm-host setup --force-stop-host
```

**Force-stop bypasses the control pipe and destroys ALL sessions of the matching
host processes.** It verifies exact installed executable paths and the current
Windows user/logon, holds process handles, and also stops their verified child
processes (including the owned tunnel). This scope includes other data roots
using the same host installation. Same-named executables in other installations
are not killed; it is not an all-user process-name kill. Run from a separate
local terminal, not from a remote session owned by the host being stopped.
Force-stop is never automatic and does not erase configuration or credentials.
After upgrading a configured host, use `start`, not `setup` or re-registration.
No certificate trust or
authentication policy is changed. Live installer/sign-in E2E requires a dedicated
machine; temporary-fixture tests do not exercise it.

## Notebook: install and configure the client

From the extracted download:

```powershell
.\arTerm-Client-Setup.exe
```

Open a fresh PowerShell window, then:

```powershell
arterm.exe login # Only if not already signed in
```

The installer completes local initialization without inspecting or changing sign-in;
no `arterm setup` is required. Use the **same GitHub account as the remote host**.
Already signed-in users can go directly to registration. Run the registration command
the host printed in PowerShell. For example (use the actual remote host path):

```powershell
arterm.exe add my-devbox --tunnel my-devbox --host-path 'C:\Tools\VsTerm\Host\arterm-host.exe'
```

This saves the friendly name and host path, not a generated tunnel ID.
The installer does not create this registration automatically. On every connection
and reconnect, the client discovers the current ID from the host name in
`devtunnel list`. Hosts must have unique names under the signed-in account.
Then:

```powershell
arterm.exe doctor my-devbox
arterm.exe connect my-devbox
# The preceding command only prints. Type its output, or choose your own name:
arterm.exe connect my-devbox MyWork
```

For an explicitly provisioned devtunnel CLI, use
`arterm.exe setup --devtunnel-path 'C:\Tools\devtunnel.exe' --no-download`.
This optional repair/custom-path command preserves registrations and does not sign in.
Upgrades retain the configured dependency path and configuration bytes.
Missing configuration alongside existing local data, invalid configuration, or an
unavailable configured dependency fails explicitly rather than resetting state.
Client configuration writers share a short-lived lock (up to two seconds of
waiting); dependency download prompts do not hold it. Initialization reloads
registrations before saving. If another command changes the configured dependency
during selection, initialization fails without overwriting it; retry the command.
Use `arterm login` explicitly if sign-in is needed. Installer switches use
slashes (`/quiet`, `/no-download`); runtime setup switches use double hyphens.

`connect MACHINE` only prints a reusable command and exits, without a
remote connection. `connect MACHINE SESSION` creates on first use and recovers the
same session on later use. Type the complete command so your shell saves it
normally; arTerm no longer inserts PSReadLine history automatically.
`MyWork` is a non-secret reference, not a resume password. Names are
case-insensitive, 1..64 ASCII letters/digits/underscores/hyphens, beginning with
a letter or digit. References are scoped to the target and Windows user.

## Disconnect and reconnect

| Action | Result |
| --- | --- |
| Press **Ctrl+]** (not Ctrl+plus) | Detach the client; remote shell stays alive |
| Close only the local client tab | Disconnect; remote shell stays alive |
| Type `exit` inside the remote shell | End the shell; its running state cannot be resumed |

If your terminal consumes the shortcut, close the client tab instead.
In a new local PowerShell, recall the command you typed, or run:

```powershell
arterm.exe connect my-devbox MyWork
# Explicit termination:
arterm.exe terminate my-devbox MyWork
```

A reattached session has the same remote PID, variables, and working directory.
Repeating original `--shell`/`--cwd` values is allowed; incompatible values fail
rather than replacing the session. Ended, missing, corrupt, or unavailable
records never create a replacement. Choose a NEW reference for a new shell.
Do not delete recovery files.

## Automation

Keep `arterm.exe connect my-devbox MyWork` running in one terminal. It is a normal
native process, not a launcher that exits after starting a client daemon. Your
caller, Copilot, or OS handles backgrounding. There is no arTerm `--background`,
`start`, or `resume` command.

From a second local terminal, in the same Windows logon/session and
integrity/elevation context:

```powershell
arterm.exe list --client
arterm.exe list --server my-devbox
arterm.exe send my-devbox MyWork --command '$work = Get-Location; $work' --wait --timeout 60s
arterm.exe read my-devbox MyWork --lines 20
```

Omit `--wait` to return on acceptance rather than completion. A send waits up
to 30 seconds for safe readiness by default; use `--timeout 5s` to choose a
different readiness budget. Busy or initializing sessions can become ready
during this wait; no command is inserted into pending interactive input.
Startup support requires an observed, correlated shell integration ready marker,
not VT capability traffic. If that marker never arrives, the finite readiness
timeout reports `IntegrationNotEstablished` with `submitted: false`; unknown
startup input is not classified as a busy managed command. Explicitly unsupported
sessions fail immediately instead of consuming the readiness budget.

Machine always precedes session. Plain `list` lists registered machines and
locally saved **session names and session IDs**; `list --json` provides the same
inventory structurally. This reads public local name mappings and recovery-file
presence, not credentials, and does not connect or sign in. A saved record is
not proof that the remote session still exists or is resumable. GUID-only
sessions are labelled `(unnamed)`; incomplete mappings are `reservation only`.
`list --client` lists active managed local connections; `list --server MACHINE`
queries the authorized host inventory, including detached sessions. Each
`connect` owns a separate local named pipe: no broadcast or shared client daemon.
`send`, `read`, `interrupt`, and `detach` require the exact active local owner
or fail. A detached session must be reattached before using those controls.
`list --server` and `terminate` do not need an active local owner, but still
require authorization; termination needs the saved session credential.

Controls and client/server inventories use readable summaries and tables by
default. Add `--json` to `send`, `read`, `list --client`, `list --server`,
`interrupt`, `detach`, or `terminate` for the existing structured response
schema. Scripts parsing output must explicitly request `--json`. Exit codes
are the same in both modes. Response failures are printed once, in the selected
format; argument, setup, and transport failures still report diagnostics on
stderr and exit 1 (they have no structured response).

`send` returns a command ID and acceptance. Save that ID. `read`
without options prints the last **20** retained logical lines; `--lines N`
accepts 1..2000. Add `--json` to inspect truncation, replay gaps, and alternate
screen metadata. Output is a bounded terminal snapshot, not a complete log.

To query command status instead of output, substitute the ID returned by `send`:

```powershell
arterm.exe read my-devbox MyWork --command-id 01234567-89ab-4cde-8fab-0123456789ab
```

`--command-id` and `--lines` are mutually exclusive on `read`. Status-query
success means the query worked: inspect the command record, not just the
query's process exit code.

<details>
<summary>Waiting, idempotency, shell support, and advanced limits</summary>

`--wait` requires an explicit `--timeout` in positive, finite whole seconds
(for example, `60s`). With `--wait`, that one budget covers readiness,
submission, and completion. Without `--wait`, it bounds readiness and acceptance.
Waiting returns early
when the matching real completion has arrived and its preceding output has
been delivered, not after sleeping for the entire timeout.

Acceptance, completion, and success are distinct. Records expose `state`,
`succeeded`, and nullable `exit_code`; a PowerShell command need not have a
native exit code. A completed wait exits 0 for success or 1 otherwise; timeout
exits 124, and an unknown outcome exits 6. A pre-submission timeout reports
`phase: readiness`, `submitted: false`, and discards the request. A caller
that disconnects while awaiting readiness cannot leave work to execute later.
Once dispatch starts, **timeout or waiter exit does not cancel the remote
command.** Query its ID after uncertainty rather than
blindly resending. Reattach first if the owner has exited.

For retry correlation,
`arterm.exe send my-devbox MyWork --command TEXT --command-id UUID` accepts a
caller-chosen ID. Reusing the same ID and exact text consults the retained
record without executing again; different text with that ID is a conflict.
An unknown result is not evidence that the command never ran.

Managed execution uses an in-memory adapter in newly created supported
PowerShell/pwsh sessions, preserving the shell PID, variables, and cwd.
Ordinary interactive startup is supported (only `-NoLogo`/`-NoProfile` startup
arguments); old sessions are not retrofitted. Unsupported sessions reject
managed execution before command bytes are sent. Completion is correlated
through adapter events, never inferred from visible prompt text, idle time,
or input acknowledgments.

Only one managed command runs at a time. Sends wait for busy/initializing
sessions and partial human input to reach a safe prompt, up to the deadline.
They never clear the line or interleave command bytes. At most four readiness
or completion waiters are admitted per owner, leaving other controls and
interactive/protocol processing available.
Replacing the adapter's prompt or entering nested input can prevent readiness
or completion reporting; do not treat silence as success.

### Readiness diagnostic logs

When `send --command` times out, keep its **Command ID** and the printed
**Readiness log** path. Starting with 0.6.1, an owning client and host write
privacy-limited JSONL diagnostics automatically:

- Client: `%LOCALAPPDATA%\VsTerm\client\diagnostics`
- Host: `%LOCALAPPDATA%\VsTerm\host\diagnostics`

If `VSTERM_REMOTE_HOME` is set, use that root instead. Collect the matching
`readiness-v1-*.jsonl` and rotated `.jsonl.1` files from both endpoints soon after
the failure. Each role retains at most eight process-instance pairs of 1 MiB
files; active instances are not evicted. Queue losses appear as `dropped_events`;
storage failures produce a warning rather than holding up the terminal.

Events contain timestamps, process/session/command IDs, readiness flags, and
categories such as text present, modifier, editing, key release, focus, or
unclassified control input. They never contain typed characters/key codes,
command text or its hash, shell output, integration nonces, or credentials.
They still describe activity, so treat them as internal diagnostic data.

A `partial_human_input` timeout means the tracker recorded a possible edit;
it does not prove visible text remains. Logs show which category changed that
state and whether later shell markers cleared it. These are observations, not
permission to clear another person's input automatically.

Update both endpoints for complete input-origin evidence. A new client can log
waits against an older host, but cannot reconstruct events that happened before
diagnostics were installed. Upgrading the host still protects live sessions;
do not force-stop useful work merely to collect logs.

The command ledger holds **256 commands for the remote session's lifetime**,
with no eviction. At capacity, new commands are rejected, not silently
forgotten or run in a replacement shell. Completed records still occupy slots.
Command text is limited to 16 KiB of UTF-8; at most four waiters are supported
per local owner. These are bounded automation facilities, not an unlimited
command queue.

**Development release gate:** files and automatic directory ZIP/extraction are
integrated together. Independent archive review was blocked by service screening
and remains a release blocker; this worktree is **not release-qualified**.

The shared interface uses the already-running local connect owner:

```powershell
arterm send my-devbox MyWork --file C:\work\artifact.zip
arterm receive my-devbox MyWork --file C:\work\remote-result.bin --json
```

Use exactly one of `send --command` and `send --file`. File operations do not
accept command IDs, `--wait`, or command `--timeout`; command semantics are
unchanged. Receive requires an absolute remote source path. Neither operation
connects implicitly or pastes file bytes into the terminal. `--file PATH` is the
file-or-directory interface: the source endpoint classifies a pinned filesystem
object. Directories are automatically packed, transferred, and extracted.
No manual ZIP step is needed for directories.
An explicitly supplied ZIP file remains a regular file and is never extracted
based on its extension.

**Automatic receiver-side unblocking:** every received regular file is
unblocked on the **host destination for send** or the **client destination for
receive**, without an option. This has Windows `Unblock-File` semantics: remove
exactly `Zone.Identifier` if present and verify its absence. Byte/ZIP transport
normally drops source ADS already; an absent mark is a valid already-unblocked
result, not an error. arTerm does not add an artificial mark first or read,
parse, or transport source-zone provenance.

The check/removal runs only on new owned staging files with identity pins
retained, before publication. For folders it also runs on every newly extracted
regular file before the atomic directory rename, not just the transport ZIP.
It never recursively unblocks an existing directory, changes a source mark, or
removes unrelated streams. Source ADS, URLs and ACLs are not preserved.

Automatic unblocking is **not a malware scan or safety verdict** and does not
make files benign or execute them. It does not release open-file locks or
change execution policy, antivirus, certificate trust, or permissions. Review
received content before opening or running it; signing and matching SHA-256
do not prove it benign.

The destination is a unique per-session/per-transfer directory under the
receiving endpoint's TEMP directory. The result reports its actual absolute
path, source, transfer ID, byte count, SHA-256 and completion status; `--json`
preserves structured fields and keeps transfer IDs separate from command IDs.
Human and JSON results include `recipient_metadata`:
`zone_identifier_absent: true` and the number of regular `files` checked.
An empty folder reports zero. Metadata errors prevent final publication and
completion; unknown outcomes never claim confirmed unblocking.
Unblocking does not change main-stream bytes or their hash.
Existing files are never replaced. Publication is an atomic no-replace rename
after size and SHA-256 validation. Traversal, ADS, reserved device names and
reparse-point paths are rejected.

Chunks are at most 64 KiB with one outstanding chunk request. Limits are 8 GiB
per file and 16 GiB of retained payloads plus extracted-byte publication charges
per manager, two active primitive transfers
and 256 retained records per manager; the local owner accepts one file operation
at a time. Each operation has a four-hour deadline and each remote response a
20-second timeout. These are protocol deadlines, not a guarantee against an
unresponsive filesystem.

File control uses the existing strict same-build signed local IPC and a separate
remote capability, bound to the session secret and current client/attachment/
lease/epoch. `file-transfer-v1`, `transfer-source-metadata-v1`, and
`recipient-unblock-v1` are all required and negotiated before staging.
The immutable metadata contains `kind` (`file` or `directory`) and
`original_basename`, and is checked in admission and receipts. The receiver
always unblocks; completion receipts must confirm `Zone.Identifier` absence.
Directory
preparation additionally requires `directory-transfer-zip-v1`, advertised only
with the installed archive adapter. Old hosts
fail explicitly before local staging publication. Busy commands do not block file control. Caller exit or connection
loss stops transfer and removes only owned partials; completed files survive.
There is no interrupted-transfer resume or automatic retry. Losing the response
after commit begins can leave an unknown outcome: inspect the reported destination
before retrying, rather than assuming nothing was written.

The archive integration points are `transfer_payload::prepare_source` and
`complete_payload`. A `PreparedArchive` owns its immutable ZIP and cleanup until
transfer ends, and implements `payload_path` plus `verify_sources(check)`.
Packing audits the original tree and then releases its pins. Later edits to
the original do not change the transferred snapshot; this is a source snapshot,
not an atomic filesystem snapshot. The receiver's publication callback extracts
into an owned private stage, preserve the original basename and nested/empty
directories, recheck cancellation/lease authorization immediately around its
atomic no-replace directory rename, and return the actual published directory.
The core never treats the intermediate ZIP receipt as directory completion.
The extraction callback receives the remaining expansion budget (capped at the
per-file limit) and must enforce it against actual streamed bytes, not ZIP header
claims. Its final authorization factory must admit those measured bytes into
the receiver's quota **before** the atomic directory rename. Both the retained
compressed payload and expanded content count toward the 16 GiB budget, so many
small ZIPs cannot bypass the quota. This uses the existing file-manager lock;
no session/terminal lock is held during archive I/O. An admitted publication's
charge is conservatively retained on later failure/uncertainty, and duplicate
extraction is rejected rather than blindly published again.
It rechecks the received directory payload's size/SHA-256 and retains its pinned
handle through extraction, preventing a replaced payload path from being treated
as the already-verified archive.
The reported byte count and SHA-256 describe the transferred payload, not a
recursive directory digest. A completed directory additionally reports
`extracted_bytes`; a source-stream/ZIP-only receipt cannot satisfy this completion
contract. Explicit ZIP files are charged and returned only as ordinary files.

Archive code should reuse `file_transfer::{validate_basename,
validate_absolute_path, extended_path, pin_source, pin_directories,
pin_directory, create_private_directory, verify_path_identity, actual_path,
rename_no_replace}`. Keep ancestor pins alive around path operations.
`pin_source` rejects reparse objects and holds write/delete-denying handles;
its `verify_unchanged` checks that object, **not an entire directory tree**.
The archive adapter validates and tracks each descendant. Folder limits are
8 GiB for both the ZIP payload and actual expanded bytes, 10,000 entries,
1,024 UTF-16 units per relative path, depth 64, and 16 MiB central-directory
metadata. Symlinks and other reparse points are rejected. Nested, empty, Unicode,
and hidden entries retain their contents; ACLs, timestamps, attributes,
alternate streams, and hard-link relationships are not promised.
Long adapter phases must invoke their cancellation callback regularly during
enumeration and bounded I/O. The core uses five-second correlated keepalives
during local preparation and request-correlated `FileProgress` frames during
remote preparation, verification and publication. Progress renews only the
20-second response-idle budget, never the original four-hour operation deadline;
the host also expires its authorization at that deadline. No background progress
threads or session locks around archive I/O are added. Missing progress fails
explicitly, and a progress frame is never a completion receipt. Native functional
coverage includes preparation lasting over 20 seconds with correlated progress
and caller cancellation. The blocked independent archive review remains a
release requirement.

</details>

### Interrupt, detach, or terminate

| Command or action | Meaning |
| --- | --- |
| `arterm.exe interrupt my-devbox MyWork` or human Ctrl+C | Best-effort interruption; not proof the command stopped |
| `arterm.exe detach my-devbox MyWork` or Ctrl+] | Disconnect the owner; keep the remote shell alive |
| `arterm.exe terminate my-devbox MyWork` | Authorize termination of that exact session job, not neighboring sessions |

`interrupt_requested` and `detach_requested` acknowledge requests, not confirmed
command completion or process exit. Query command status after interruption.
`terminate` takes no confirmation flag: invoke it only when you intend to end
the session. Its JSON `terminated` status reports confirmed exit;
`termination_accepted` means the request was accepted but exit was not confirmed
(including older hosts or loss of confirmation). Do not interpret process exit
0 alone as confirmation.

### Local IPC trust

Both pipe ends verify the OS-reported peer PID, image, and token context:
valid Windows Authenticode chain trust, the exact compiled certificate SHA-256,
and byte-identical client binaries, with matching user SID, logon, Windows
session, integrity, and elevation. An unsigned, tampered, wrong-certificate,
or different-build client fails closed. Filenames, certificate subjects,
and same-user access alone do not authorize a program. A byte-identical
`vsterm.exe` alias is compatible.

Use matching official signed builds and the existing
[development trust guide](DEVELOPMENT-INSTALL.md); do not bypass policy.
This application-identity gate is not protection against administrators,
process injection, or code invoking the legitimate CLI.

## Compatibility

<details>
<summary>Legacy hosts and existing installations</summary>

Reusable `connect` requires the host's
`ended-session-rejection` capability. An unsupported host is rejected before
creation or attachment; the locally reserved reference remains available for
retry with a compatible host.

For a still-live 0.2 session, `connect` with its saved GUID
can use an existing unnamed record with a saved credential. It warns that the
old host may instead replay an already-ended session and return its old exit
code. This compatibility path does not apply to named records or tokenless
creation recovery. Retain the matching 0.2 client for interrupted legacy
creations. Finish old sessions before upgrading the host; do not stop or update
it while relying on live shells. No automatic host upgrade is performed.

arTerm 0.5 retains VsTerm GUID recovery records and byte-identical legacy
executable aliases (`vsterm.exe` / `vsterm-host.exe`).
Both fresh and upgraded installs keep configuration under
`%LOCALAPPDATA%\VsTerm`. It does not automatically import or delete the older
`%LOCALAPPDATA%\DevBoxRemote` state. Finish old sessions before uninstalling the
prototype host, and configure arTerm afresh. Prototype GUIDs are not imported;
existing VsTerm GUIDs and configured targets remain unchanged.

The development certificate is unchanged; `arTerm-Dev.cer` contains the same
public certificate previously named `DevBoxRemote-Dev.cer`. Its certificate
subject remains `CN=DevBoxRemote Development`; product renaming does not require
trusting a new key.

</details>
