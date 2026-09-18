# Development-signed builds: explicit local trust

These are **self-signed development builds**. Their certificate is not publicly
trusted; installation requires explicit local trust where permitted.
Organizational application-control rules still apply.

The development-signed download contains four signed EXEs, the MIT `LICENSE`,
`THIRD-PARTY-NOTICES.txt`, the public `arTerm-Dev.cer`, this guide, and checksums.
It must never contain a PFX or private key. Public CI artifacts and ordinary
local builds are unsigned; they are not substitutes for signed production IPC
peers.

## Certificate identity

- Subject: `CN=DevBoxRemote Development`
- Thumbprint: `3F94517F423B0A6DF92F19D4B0B0467AFE10C1F2`
- Public CER SHA-256: `46B3A8A9307652D90662DB056FDEDCB93C91EDCF823F6BE3007A7E54F2AD171C`
- Initial validity ends: 2027-09-17.

Confirm this fingerprint against the repository's copy of this guide, not an
unrelated download. The public certificate cannot create signatures.
This is the existing certificate, not a recreated key. Renaming the product
does not change its certificate identity or automatically alter local trust.

## Required trust for local control

In arTerm 0.5, each running `connect` owns a local named pipe. Owner and caller
mutually verify the actual OS-reported PID, image, and context. Production IPC
requires **valid Windows Authenticode chain trust**, the exact compiled public
certificate SHA-256 above, and byte-identical client binaries. Both peers must
also match user SID, logon, Windows session, integrity, and elevation.

Unsigned, tampered, wrong-certificate, and different-build peers fail closed.
A matching filename or certificate subject is insufficient; arbitrary programs
running as the same user are not trusted. A byte-identical `vsterm.exe` alias
works. Use the same official signed client bytes for the connection owner and
all control invocations; signing alone does not grant host/installer binaries
the client role.

This is an application-identity gate, not a hard boundary against administrators,
process injection, or other code invoking the legitimate CLI. There is no
production environment-variable or command-line bypass. arTerm does not import
trust automatically.

## Optional one-time trust for your test user

Only if you understand and approve trusting this development publisher, and
your organization's policy permits it, run this PowerShell command from the
extracted artifact directory:

```powershell
& {
    $certificate = (Resolve-Path .\arTerm-Dev.cer).Path
    if ((Get-FileHash -LiteralPath $certificate -Algorithm SHA256).Hash -ne
        '46B3A8A9307652D90662DB056FDEDCB93C91EDCF823F6BE3007A7E54F2AD171C') {
        throw 'Unexpected certificate; trust was not changed.'
    }
    Import-Certificate -FilePath $certificate -CertStoreLocation Cert:\CurrentUser\Root
}
```

This adds trust for this certificate to **your current Windows user**, not every
machine or every user. It allows local certificate-chain trust for code signed
with this development key; do not perform it for arbitrary certificates.
Your administrator may restrict this operation. If it is blocked, stop and
use your organization's approved deployment route; do not disable security
controls or change machine-wide trust settings.

Then run the installer for the role you need:

```powershell
.\arTerm-Client-Setup.exe
# Or, on your Dev Box:
.\arTerm-Host-Setup.exe
```

Installation and configuration are separate. Open a fresh PowerShell window
after installation, then run `arterm.exe setup` on the client or
`arterm-host.exe setup --name my-devbox` on the host. Sign into the same GitHub
account on both machines and run the host's printed registration command on
the client. Follow [QUICKSTART.md](https://github.com/yeelam/arterm/blob/main/QUICKSTART.md) for the
complete connection and automation steps.

For an already-configured host, upgrade the binaries and run `arterm-host start`;
do not repeat setup or client registration. Both `/option` and `--option`
installer spellings are accepted. `--terminate-sessions` requests graceful
shutdown. If that times out, `arTerm-Host-Setup.exe --force-stop-host` explicitly
force-stops verified host processes from that installation for the current
user/logon and their children. This destroys their active sessions, including
other data roots sharing the executable, but retains saved configuration and
credentials. Other installations/users are not force-stopped.

arTerm's `arterm connect my-devbox` only prints a reusable command.
Type that command or `arterm connect my-devbox MyWork`; repeat it to recover
the same session. Existing 0.2 GUID recovery records remain supported.
Reusable connect requires the host's
`ended-session-rejection` capability. Finish live 0.2 sessions using explicit
GUID connection with an existing saved credential before upgrading their host.
See QUICKSTART.md for the legacy behavior and tokenless-recovery limitations;
do not force an update that would terminate live sessions.
MIT licensing does not change certificate trust or vendor component licenses.
Signing remains a separate protected workflow; source hosting does not establish
publisher trust.

For source development only, unsigned functional fixtures require the explicit,
default-off debug feature:

```text
cargo test --locked --features test-unsigned-ipc -- --test-threads=1
```

Release builds omit the feature; enabling it in release fails compilation.
The separate signed CI gate tests actual signed CLI peers, rather than unsigned
fixtures. This guide does not assert that the signed gate has passed for any
particular build. See [SIGNING.md](https://github.com/yeelam/arterm/blob/main/SIGNING.md).

SmartScreen reputation is separate from certificate-chain trust, and WDAC,
AppLocker, Smart App Control, or other organizational policies can still block
execution. This guide does not promise that adding trust resolves every
"cannot run" message.

To remove this development trust later:

```powershell
Remove-Item -LiteralPath Cert:\CurrentUser\Root\3F94517F423B0A6DF92F19D4B0B0467AFE10C1F2
```

Never import or request the project's private PFX. The private key is held in
encrypted GitHub Actions secrets and is only exposed to the trusted manual
signing workflow on the disposable runner. No private key is distributed.
