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

### ARM64 signing rollout (pending)

The public package builder accepts `--target aarch64-pc-windows-msvc` and
`--target x86_64-pc-windows-msvc`. Explicit targets produce separate
`dist\windows-arm64` / `dist\windows-x64` directories, each retaining the flat
allowlist and independent checksums. The private workflow has **not** been
changed or run for ARM64 by this source change.

The private maintainer workflow needs a target/architecture matrix for the
same exact approved public `source_ref`. For each target:

1. Build the host-runnable package tool separately, then build the two runtimes
   with `cargo build --locked --release --target <triple> --bin arterm --bin arterm-host`.
   Do not enable `test-unsigned-ipc` in release builds.
2. Sign only that target's `target\<triple>\release` runtimes. Invoke the host
   package tool with `--target <triple> --payload-dir <absolute-signed-runtime-directory>`.
   Sign that target's two installers afterward. Never share a payload directory
   between architectures or rebuild signed runtime inputs.
3. On a matching native runner, run the unsigned functional fixtures and the
   existing signed-production IPC gate with absolute, same-architecture fixture
   paths. Preserve exact-image/certificate enforcement, temporary public-CER
   trust cleanup, and pre/post-test hash checks. Require nonzero test execution.
4. Check the nine-file signed allowlist, recompute the eight hashed entries,
   and upload `arterm-windows-<x64|arm64>-development-signed` separately.
   Keep the source SHA and target in the workflow summary; do not publish
   downloads until both required native gates pass.

Private workflow patch contract (recommendation only; retain existing protected
environment, certificate/key secret names, signer, source checks, and cleanup):

```yaml
strategy:
  fail-fast: false
  matrix:
    include:
      - runner: windows-2022
        arch: x64
        target: x86_64-pc-windows-msvc
      - runner: windows-11-arm
        arch: arm64
        target: aarch64-pc-windows-msvc
runs-on: ${{ matrix.runner }}
env:
  RUSTUP_TOOLCHAIN: stable-${{ matrix.target }}
```

Use the public CI's native OS/Rust-host assertions and its two debug fixture
commands, including `--test resume_e2e -- --ignored --test-threads=1` with the
nonzero-passed/zero-failed/zero-ignored output check. Do not set `SIGNED_*`
selectors for that unsigned step. Keep all release build commands free of
`test-unsigned-ipc`.

Run build commands from the checked-out public source directory; set
`CARGO_TARGET_DIR` to its absolute `target` directory, consistently for every
step. Resolve these paths to absolute paths from that directory:

| Purpose / environment variable | Path |
| --- | --- |
| Host package tool | `target\release\package.exe` (build with `cargo build --locked --release --bin package`, no cross-target environment override) |
| Runtime signing inputs | `target\<triple>\release\arterm.exe`, `target\<triple>\release\arterm-host.exe` |
| `UNSIGNED_CLIENT` | `target\<triple>\unsigned-production\arterm.exe` (copy release client **before** runtime signing; never sign this copy) |
| `SIGNED_CLIENT` | `dist\windows-<arch>\arterm.exe` |
| `SIGNED_HOST` | `dist\windows-<arch>\arterm-host.exe` |
| `SIGNED_INSTALLER` | `dist\windows-<arch>\arTerm-Client-Setup.exe` |

After signing the runtime inputs, execute the host package tool with
`--target <triple> --payload-dir <absolute-runtime-signing-input-directory>`.
Then sign **both** `dist\windows-<arch>\arTerm-*-Setup.exe` files using the
existing signing helper. Verify all four distributed signatures, the pinned
certificate, timestamps, and PE machines before the gate. Snapshot hashes only
after signing is complete; compare the signed runtime input/output copies and
retain pre/post-gate hashes for all fixture, distributed, and build-output EXEs.

Inside the existing disposable-runner public-CER trust `try`/`finally` wrapper,
set the four absolute fixture selectors from the table and run exactly:

```powershell
cargo test --locked --target ${{ matrix.target }} --features test-unsigned-ipc --test signed_ipc_e2e actual_signed_product_ipc -- --ignored --exact --test-threads=1 2>&1 |
  Tee-Object -Variable signedOutput
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if (($signedOutput -join "`n") -notmatch '(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored;') {
  throw 'The exact signed-production IPC gate must execute and pass.'
}
```

This builds a debug fixture harness but executes the signed, feature-free
release products for positive IPC peers. Do not select every ignored test in
`signed_ipc_e2e`: it also includes other fixtures. Scope selectors to this gate
and retain the existing temporary trust removal in `finally`, including on
failure. Assemble the nine-file allowlist and eight checksums only afterward;
upload `arterm-windows-${{ matrix.arch }}-development-signed`. Both matrix jobs
must pass before maintainers approve either architecture for publication.

[GitHub's runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
currently lists standard `windows-11-arm` runners for **both public and private**
repositories. Public standard runners are free; private usage consumes included
minutes and may be billed beyond the allowance. Availability documentation does
not establish that the private repository's policy, quota, or signing helper
permits that job. No runner purchase, billing change, or private-key access is
part of this change. Verify existing allowance and helper compatibility first.
If native private validation cannot run within authorized resources, keep the
ARM64 signed-release gate blocked rather than reporting a cross-build as E2E.

A possible fallback is private x64 cross-build/signing followed by native
public ARM64 validation of a maintainer-staged draft release asset. This is
not implemented or verified here. It would require a manual, protected-main-only
workflow in the public repository, its own repository-scoped `GITHUB_TOKEN`,
an approved exact source SHA and independently supplied artifact SHA-256,
fixed release/asset IDs, and the existing public-certificate fingerprint.
Never import private key material there. Download via the
[release asset API](https://docs.github.com/en/rest/releases/assets#get-a-release-asset),
verify source provenance and hashes before executing, and remove temporary
public-CER trust in `finally`. The
[release API](https://docs.github.com/en/rest/releases/releases#list-releases)
restricts draft listings to push-capable callers; do not assume the current
public CI's `contents: read` token can retrieve drafts. Confirm the required
own-repository token permissions and draft download behavior before choosing
this route. It cannot use that token to read private-repository artifacts.
No draft publication or credential changes are included in this source change.

Keep `THIRD-PARTY-NOTICES.txt` with the distributed binaries and installers.
It inventories the locked Windows production dependency graph and reproduces
the selected dependency licenses and additional notices. Review and refresh
that inventory whenever dependencies or their versions change.

The MIT license covers this project's source, not separately installed vendor
software. Dependencies retain their own licenses. VS Code Server and other
runtime/system dependencies are installed separately under their applicable
terms; they are not embedded or re-signed as part of this download.
