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

The host starts at Windows user logon. A cold boot still requires a Windows
sign-in; network/RDP disconnection is not the same as Windows logoff.

To use an already installed standalone native VS Code CLI without downloading
the editor, supply its absolute EXE path:

```powershell
arterm-host.exe setup --name my-devbox --code-path 'C:\Tools\code-tunnel.exe' --no-download
```

`--no-download` prevents dependency installation, not license or sign-in prompts.
Do not change a running host's configuration while it has live sessions.

## Notebook: install and configure the client

From the extracted download:

```powershell
.\arTerm-Client-Setup.exe
```

Open a fresh PowerShell window, then:

```powershell
arterm.exe setup
```

Use the **same GitHub account as the remote host**. Run the registration command
the host printed in PowerShell. For example (use the actual remote host path):

```powershell
arterm.exe add my-devbox --tunnel my-devbox --host-path 'C:\Tools\VsTerm\Host\arterm-host.exe'
```

This saves the friendly name and host path, not a generated tunnel ID.
`setup` does not create this registration automatically. On every connection
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
Setup still prompts for GitHub sign-in when needed. Installer switches use
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

Omit both `--wait` and `--timeout` to return on acceptance instead of waiting.
Submit another command only after the shell is ready.

Machine always precedes session. Plain `list` lists registered machines;
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
(for example, `60s`); `--timeout` also requires `--wait`. Waiting returns early
when the matching real completion has arrived and its preceding output has
been delivered, not after sleeping for the entire timeout.

Acceptance, completion, and success are distinct. Records expose `state`,
`succeeded`, and nullable `exit_code`; a PowerShell command need not have a
native exit code. A completed wait exits 0 for success or 1 otherwise; timeout
exits 124, and an unknown outcome exits 6. **Timeout or waiter exit does not
cancel the remote command.** Query its ID after uncertainty rather than
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

Only one managed command runs at a time. Busy or partial human input rejects
new sends, and managed execution does not interleave human command bytes.
Replacing the adapter's prompt or entering nested input can prevent readiness
or completion reporting; do not treat silence as success.

The command ledger holds **256 commands for the remote session's lifetime**,
with no eviction. At capacity, new commands are rejected, not silently
forgotten or run in a replacement shell. Completed records still occupy slots.
Command text is limited to 16 KiB of UTF-8; at most four waiters are supported
per local owner. These are bounded automation facilities, not an unlimited
command queue.

File transfer is deferred to a later release.

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
