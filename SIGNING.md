# arTerm signing

arTerm source is MIT-licensed. Public CI tests and
packages unsigned Windows builds; it has no signing credentials. Development
release binaries use the existing self-signed development certificate, not a
publicly trusted publisher certificate.

## Certificate and trust

The public certificate is `signing/arTerm-Dev.cer`. Its bytes are unchanged by
the arTerm rename. SHA-256:

```text
46B3A8A9307652D90662DB056FDEDCB93C91EDCF823F6BE3007A7E54F2AD171C
```

The certificate retains its original subject; a product rename does not change
the signing identity. Self-signing does not establish public publisher trust
or guarantee that Windows SmartScreen will accept a download. See
[DEVELOPMENT-INSTALL.md](DEVELOPMENT-INSTALL.md) for verification and optional,
explicit current-user trust instructions.

## Maintainer signing procedure

Signing runs only through a restricted, manual private CI workflow on its
protected main branch. Signing credentials stay in that private repository;
they must never be copied into the public source repository. Restrict workflow
write and dispatch access to trusted signing maintainers, and protect main
against unreviewed changes.

1. Review and merge the public source to main. Supply its exact 40-hex commit
   SHA as `source_ref` to the private signing workflow.
2. CI checks out that exact public commit, verifies it is reachable from public
   main, and checks the pinned public certificate hash. The signing helper
   comes from the private workflow's own commit, not the public checkout.
3. CI runs unsigned functional fixtures with the debug-only
   `test-unsigned-ipc` feature, then builds release binaries without that feature
   or signing secrets. The ignored native functional tests are selected
   explicitly and must execute successfully. CI signs and timestamps the two
   runtime EXEs, embeds those unchanged bytes, then signs and timestamps both
   installers. The exact signed-production IPC test must execute and pass
   before download assembly and upload.
4. CI checks the strict download allowlist, recomputes eight SHA-256 checksums,
   and uploads `arterm-windows-x64-development-signed` for maintainer review.
   This workflow does not publish a release.

Secrets are scoped to the two signing steps on disposable GitHub-hosted
runners. Missing credentials fail the build. The helper checks the certificate
identity and timestamped signatures, then removes its temporary PFX, imported
private key, and temporary runner trust. Do not run it locally or regenerate,
export, or retrieve private signing material during source maintenance.

The signed IPC gate has no signing secrets. Only on a disposable GitHub-hosted
runner, it temporarily imports the fingerprint-verified public certificate
into `LocalMachine\Root` and removes that trust in `finally` if it added it.
The debug test harness receives absolute `SIGNED_CLIENT`, `SIGNED_HOST`,
`SIGNED_INSTALLER`, and `UNSIGNED_CLIENT` paths; its positive peers are the
signed release binaries, not unsigned fixtures. `SIGNED_INSTALLER` selects the
signed client installer as a same-certificate, wrong-program fixture. These
are test-harness selectors, not production authentication bypasses. Hashes of
all referenced and published EXEs and the release build outputs must remain
unchanged after testing.
Public unsigned CI exercises fixtures, not the signed-production IPC gate.

The signed download contains exactly nine flat files: `arterm.exe`,
`arterm-host.exe`, `arTerm-Client-Setup.exe`, `arTerm-Host-Setup.exe`,
`arTerm-Dev.cer`, `DEVELOPMENT-INSTALL.md`, `LICENSE`,
`THIRD-PARTY-NOTICES.txt`, and `SHA256SUMS`.
Public unsigned artifacts contain exactly seven flat files: the four EXEs,
`LICENSE`, `THIRD-PARTY-NOTICES.txt`, and `SHA256SUMS` (six hashed entries).
The recorded public source SHA is in the private workflow summary.

Keep `THIRD-PARTY-NOTICES.txt` with the distributed binaries and installers.
It inventories the locked Windows production dependency graph and reproduces
the selected dependency licenses and additional notices. Review and refresh
that inventory whenever dependencies or their versions change.

The MIT license covers this project's source, not separately installed vendor
software. Dependencies retain their own licenses. VS Code Server and other
runtime/system dependencies are installed separately under their applicable
terms; they are not embedded or re-signed as part of this download.
