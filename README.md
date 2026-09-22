# arTerm

[![arTerm - remote terminal access, automatic reconnect, client-reboot recovery, and an agent-ready CLI](.github/arterm-github-cover.jpg)](https://github.com/yeelam/arterm/releases/latest)

arTerm keeps a Windows remote shell running independently of
your local terminal. Return to the same shell with the same named command,
or control an attached session from another local CLI or agent.

Reach configured Windows Dev Boxes, VMs and Windows Sandbox hosts across networks
through an authenticated tunnel—no same-LAN connection or direct inbound host
access required. Client and host still need permitted outbound connectivity under
organizational network policy.

## Why use it?

- **Long-running remote CLI agents and builds:** leave work running on a remote
  Windows machine, then reattach to its existing process and shell environment.
- **Network or VPN drops:** reconnect to the same session when connectivity
  returns, rather than start another shell.
- **Local tab loss or client reboot:** reopen your terminal and rerun the saved
  command from the same Windows user account with its local recovery data intact.
- **Automation alongside a human:** send a command, wait for its correlated
  completion, or read output without creating a second remote shell.
- **Files and folders:** send or receive either through the existing connected
  session, with SHA-256 verification and unique TEMP destinations. Directories
  are automatically packed, transferred, and extracted; ordinary ZIP files stay files.
  Received files are automatically unblocked on the receiving endpoint,
  including newly extracted folder files. Sources are unchanged.
  **Development release gate:** independent archive review remains blocked;
  this integration is not release-qualified.

**Limit:** the remote host must stay running and logged in. Remote Windows
reboot/logoff, host crash/shutdown, or remote shell `exit` ends that process.
arTerm does not checkpoint or resurrect processes after those events.
Managed command execution requires a newly created, supported PowerShell/pwsh
session; existing sessions are not retrofitted.

## What you need

| Requirement | Local client | Remote host |
| --- | --- | --- |
| Operating system | Windows | Windows |
| arTerm component | `arTerm-Client-Setup.exe` | `arTerm-Host-Setup.exe` |
| Tunnel dependency | Microsoft **devtunnel CLI** | Compatible native **VS Code tunnel CLI**, such as `code-tunnel.exe` or standalone `code.exe` |
| Tunnel sign-in | Your GitHub account | **The same GitHub account** |

The full VS Code editor is optional when a compatible standalone native CLI is
provided. Tunnel sign-in is separate from `gh` CLI authentication.

Installers detect dependencies and offer downloads only with consent; vendor
binaries are not bundled. Host setup requires acceptance of the VS Code server
license terms. Host startup is at **Windows user logon**, not a SYSTEM service:
a cold boot requires Windows sign-in, and arTerm does not configure autologon.
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

## Get connected

1. Download the installers from [Releases](https://github.com/yeelam/arterm/releases/latest).
   Use an official signed build: local control requires valid Windows
   Authenticode chain trust and matching client binaries. Public CI artifacts
   and local builds are unsigned, not production IPC-ready. For self-signed
   development builds, follow
   [DEVELOPMENT-INSTALL.md](DEVELOPMENT-INSTALL.md) first. Their certificate is
   not publicly trusted; any local trust must be explicitly approved and
   permitted by your organization's policy.

2. **On the remote host**, run `arTerm-Host-Setup.exe`. Open a fresh PowerShell,
   then configure a unique host name:

   ```powershell
   arterm-host setup --name my-devbox
   ```

   Complete tunnel sign-in and keep the name-based client registration command
   printed by setup.

3. **On the local client**, run `arTerm-Client-Setup.exe`. Open a fresh PowerShell:

   ```powershell
   arterm login # Only if not already signed in
   ```

   Client Setup completes local initialization; `arterm setup` is not required.
   Sign in with the same GitHub account if needed. **Copy and run the host's printed
   registration command once**; client setup does not register the host for you.
   Then create your session:

   ```powershell
   arterm connect my-devbox MyWork
   ```

**Repeat that exact command to reattach.** `MyWork` is a non-secret session
reference, not a password. Press **Ctrl+]** to detach and leave the shell running;
type **`exit` inside the remote shell** to end it. Choose a new reference for a
new shell; ended or unavailable sessions are not silently replaced.

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

Without a reference, `arterm connect my-devbox` **only prints** a complete
reusable command and exits; it does not start a remote shell.

## Control an attached session

Leave `arterm connect my-devbox MyWork` running, then use another local terminal:

```powershell
arterm list --client
arterm list --server my-devbox
arterm send my-devbox MyWork --command 'Get-Location' --wait --timeout 60s
arterm read my-devbox MyWork --lines 20
```

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

`connect` is a normal running native process. Your caller, Copilot, or OS owns
backgrounding; arTerm has no `--background`, `start`, or `resume` command.
Each connection owns its own local named pipe, not a broadcast endpoint or
client daemon. Local controls require that exact active owner; missing or
ambiguous owners are errors. Authorized server inventory and termination also
work while detached.

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
Use `--json` on `send`, `read`, `list --client`, `list --server`, `interrupt`,
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

For standalone dependency paths, setup options, and troubleshooting, see
[QUICKSTART.md](QUICKSTART.md). See [SIGNING.md](SIGNING.md) for signing policy.

## Session and recovery details

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
transport. If credentials have expired, run `arterm login` with the host's
GitHub account, then retry. Do not reinstall the host or delete session records.

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
been cross-built and their PE architecture checked; native ARM64 functional,
installer lifecycle, and signed-production IPC validation are still pending.
Do not treat cross-compilation as a native support certification.

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
These jobs have been configured, not executed as part of this local change.
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
