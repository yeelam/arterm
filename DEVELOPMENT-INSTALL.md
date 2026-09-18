# Development-signed builds: explicit local trust

These are **self-signed development builds**. Their certificate is not publicly
trusted; installation requires explicit local trust where permitted.
Organizational application-control rules still apply.

The download contains four signed EXEs, the MIT `LICENSE`, the public `arTerm-Dev.cer`,
this guide, and checksums. It must never contain a PFX or private key.

## Certificate identity

- Subject: `CN=DevBoxRemote Development`
- Thumbprint: `3F94517F423B0A6DF92F19D4B0B0467AFE10C1F2`
- Public CER SHA-256: `46B3A8A9307652D90662DB056FDEDCB93C91EDCF823F6BE3007A7E54F2AD171C`
- Initial validity ends: 2027-09-17.

Confirm this fingerprint against the repository's copy of this guide, not an
unrelated download. The public certificate cannot create signatures.

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
complete connection and resume steps.

arTerm's `arterm connect my-devbox` only prints a reusable command.
Type that command or `arterm connect my-devbox MyWork`; repeat it to recover
the same session. Existing 0.2 GUID recovery records remain supported.
Reusable connect and named resume require the host's
`ended-session-rejection` capability. Finish live 0.2 sessions using explicit
GUID resume with an existing saved credential before upgrading their host.
See QUICKSTART.md for the legacy behavior and tokenless-recovery limitations;
do not force an update that would terminate live sessions.
MIT licensing does not change certificate trust or vendor component licenses.
Signing remains a separate protected workflow; source hosting does not establish
publisher trust.

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
