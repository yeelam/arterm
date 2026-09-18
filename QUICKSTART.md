# arTerm quick start

arTerm has two roles: install the **host** on the remote machine and the
**client** on your notebook. Use your normal signed-in Windows account.

For development-signed downloads, first follow
[DEVELOPMENT-INSTALL.md](DEVELOPMENT-INSTALL.md) to verify the public certificate
and explicitly trust it if your organization permits it. This is separate from
signing into the tunnel service.

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

`connect TARGET` only prints a reusable command and exits, without a
remote connection. `connect TARGET REF` creates on first use and recovers the
same session on later use. Type the complete command so your shell saves it
normally; arTerm no longer inserts PSReadLine history automatically.
`MyWork` is a non-secret reference, not a resume password. Names are
case-insensitive, 1..64 ASCII letters/digits/underscores/hyphens, beginning with
a letter or digit. References are scoped to the target and Windows user.

## Disconnect and resume

| Action | Result |
| --- | --- |
| Press **Ctrl+]** (not Ctrl+plus) | Detach the client; remote shell stays alive |
| Close only the local client tab | Disconnect; remote shell stays alive |
| Type `exit` inside the remote shell | End the shell; its running state cannot be resumed |

If your terminal consumes the shortcut, close the client tab instead.
In a new local PowerShell, recall the command you typed, or run:

```powershell
arterm.exe connect my-devbox MyWork
# Existing-only recovery (including legacy 0.2 GUID records):
arterm.exe resume my-devbox YOUR-SAVED-GUID
# Explicit termination:
arterm.exe terminate my-devbox MyWork --yes
```

A resumed session has the same remote PID and variables. Repeating original
`--shell`/`--cwd` values is allowed; incompatible values fail rather than replacing
the session. Ended, missing, corrupt, or unavailable records never create a
replacement. Choose a NEW reference for a new shell. Do not delete recovery files.

## Connecting to a legacy VsTerm host

Reusable `connect` and named `resume` require the host's
`ended-session-rejection` capability. An unsupported host is rejected before
creation or attachment; the locally reserved reference remains available for
retry with a compatible host.

For a still-live 0.2 session, explicit `arterm.exe resume my-devbox YOUR-SAVED-GUID`
can use an existing unnamed record with a saved credential. It warns that the
old host may instead replay an already-ended session and return its old exit
code. This compatibility path does not apply to named records or tokenless
creation recovery. Retain the matching 0.2 client for interrupted legacy
creations. Finish old sessions before upgrading the host; do not stop or update
it while relying on live shells. No automatic host upgrade is performed.

## Moving from the earlier DevBox Remote prototype

arTerm 0.4 retains VsTerm GUID recovery records and legacy executable aliases.
Both fresh and upgraded installs keep configuration under
`%LOCALAPPDATA%\VsTerm`. It does not automatically import or delete the older
`%LOCALAPPDATA%\DevBoxRemote` state. Finish old sessions before uninstalling the
prototype host, and configure arTerm afresh. Prototype GUIDs are not imported;
existing VsTerm GUIDs and configured targets remain unchanged.

The development certificate is unchanged; `arTerm-Dev.cer` contains the same
public certificate previously named `DevBoxRemote-Dev.cer`. Its certificate
subject remains `CN=DevBoxRemote Development`; product renaming does not require
trusting a new key.
