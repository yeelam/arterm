use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    X64,
    Arm64,
}

impl Target {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "x86_64-pc-windows-msvc" => Ok(Self::X64),
            "aarch64-pc-windows-msvc" => Ok(Self::Arm64),
            _ => bail!("unsupported package target {value:?}; use x86_64-pc-windows-msvc or aarch64-pc-windows-msvc"),
        }
    }

    fn triple(self) -> &'static str {
        match self {
            Self::X64 => "x86_64-pc-windows-msvc",
            Self::Arm64 => "aarch64-pc-windows-msvc",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::X64 => "windows-x64",
            Self::Arm64 => "windows-arm64",
        }
    }

    fn machine(self) -> u16 {
        match self {
            Self::X64 => 0x8664,
            Self::Arm64 => 0xaa64,
        }
    }
}

#[derive(Debug)]
struct Options {
    target: Target,
    explicit_target: bool,
    payload_dir: Option<PathBuf>,
}

fn options(args: &[String]) -> Result<Options> {
    let mut result = Options {
        target: Target::X64,
        explicit_target: false,
        payload_dir: None,
    };
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with("--"))
            .context("usage: package [--target <MSVC-triple>] [--payload-dir <directory>]")?;
        match flag.as_str() {
            "--target" if !result.explicit_target => {
                result.target = Target::parse(value)?;
                result.explicit_target = true;
            }
            "--payload-dir" if result.payload_dir.is_none() => {
                result.payload_dir = Some(value.into())
            }
            _ => bail!("unknown or repeated package option {flag:?}"),
        }
    }
    Ok(result)
}

fn release_dir(root: &Path, target_dir: Option<PathBuf>, target: Target) -> PathBuf {
    root.join(target_dir.unwrap_or_else(|| "target".into()))
        .join(target.triple())
        .join("release")
}

fn validate_pe(bytes: &[u8], target: Target) -> Result<()> {
    ensure!(
        bytes.len() >= 64 && &bytes[..2] == b"MZ",
        "invalid or truncated DOS header"
    );
    let offset = u32::from_le_bytes(bytes[60..64].try_into()?) as usize;
    ensure!(offset >= 64, "PE header overlaps DOS header");
    let header = offset
        .checked_add(24)
        .and_then(|end| bytes.get(offset..end))
        .context("invalid or truncated PE header")?;
    ensure!(&header[..4] == b"PE\0\0", "invalid PE signature");
    let machine = u16::from_le_bytes(header[4..6].try_into()?);
    ensure!(
        matches!(machine, 0x8664 | 0xaa64),
        "unsupported PE machine 0x{machine:04x}"
    );
    ensure!(
        machine == target.machine(),
        "PE architecture mismatch: expected {} (0x{:04x}), found 0x{machine:04x}",
        target.triple(),
        target.machine()
    );
    let characteristics = u16::from_le_bytes(header[22..24].try_into()?);
    ensure!(
        characteristics & 0x0002 != 0 && characteristics & 0x2000 == 0,
        "PE must be an executable image, not a DLL"
    );
    let section_count = u16::from_le_bytes(header[6..8].try_into()?) as usize;
    ensure!(
        (1..=96).contains(&section_count),
        "invalid PE section count"
    );
    let optional_size = u16::from_le_bytes(header[20..22].try_into()?) as usize;
    ensure!(
        optional_size >= 112,
        "missing or short PE32+ optional header"
    );
    let optional_start = offset
        .checked_add(24)
        .context("PE header offset overflow")?;
    let section_start = optional_start
        .checked_add(optional_size)
        .context("PE optional header size overflow")?;
    let optional = bytes
        .get(optional_start..section_start)
        .context("truncated PE32+ optional header")?;
    ensure!(
        u16::from_le_bytes(optional[..2].try_into()?) == 0x20b,
        "PE32+ optional header required"
    );
    let directory_count = u32::from_le_bytes(optional[108..112].try_into()?);
    let directory_end = directory_count
        .checked_mul(8)
        .and_then(|size| size.checked_add(112))
        .context("PE data directory size overflow")?;
    ensure!(
        directory_end as usize <= optional.len(),
        "truncated PE data directories"
    );
    let section_end = section_count
        .checked_mul(40)
        .and_then(|size| section_start.checked_add(size))
        .context("PE section table size overflow")?;
    let sections = bytes
        .get(section_start..section_end)
        .context("truncated PE section table")?;
    let headers_size = u32::from_le_bytes(optional[60..64].try_into()?);
    let image_size = u32::from_le_bytes(optional[56..60].try_into()?);
    ensure!(
        headers_size as usize >= section_end && headers_size as usize <= bytes.len(),
        "invalid or truncated PE SizeOfHeaders"
    );
    ensure!(image_size > headers_size, "invalid PE SizeOfImage");
    let entry = u32::from_le_bytes(optional[16..20].try_into()?);
    let mut executable_entry = false;
    for section in sections.chunks_exact(40) {
        let virtual_size = u32::from_le_bytes(section[8..12].try_into()?);
        let address = u32::from_le_bytes(section[12..16].try_into()?);
        let raw_size = u32::from_le_bytes(section[16..20].try_into()?);
        let raw_offset = u32::from_le_bytes(section[20..24].try_into()?);
        let flags = u32::from_le_bytes(section[36..40].try_into()?);
        let virtual_end = address
            .checked_add(virtual_size.max(raw_size))
            .context("PE section virtual range overflow")?;
        ensure!(
            address >= headers_size && virtual_end <= image_size,
            "PE section virtual range outside image"
        );
        if raw_size != 0 {
            let raw_end = raw_offset
                .checked_add(raw_size)
                .context("PE section raw range overflow")?;
            ensure!(
                raw_offset >= headers_size && raw_end as usize <= bytes.len(),
                "invalid or truncated PE section raw data"
            );
            if flags & 0x2000_0000 != 0
                && entry
                    .checked_sub(address)
                    .is_some_and(|delta| delta < raw_size)
            {
                executable_entry = true;
            }
        }
    }
    ensure!(
        executable_entry,
        "PE entry point must be backed by executable section data"
    );
    // Unlike other directories, the certificate table uses a file offset, not an RVA.
    if directory_count > 4 {
        let certificate_offset = u32::from_le_bytes(optional[144..148].try_into()?);
        let certificate_size = u32::from_le_bytes(optional[148..152].try_into()?);
        if certificate_offset != 0 || certificate_size != 0 {
            let certificate_end = certificate_offset
                .checked_add(certificate_size)
                .context("PE certificate table range overflow")?;
            ensure!(
                certificate_offset >= headers_size
                    && certificate_size >= 8
                    && certificate_end as usize <= bytes.len(),
                "invalid or truncated PE certificate table"
            );
        }
    }
    Ok(())
}

fn validate_file(path: &Path, target: Target) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    validate_pe(&bytes, target).with_context(|| format!("validating {}", path.display()))?;
    Ok(bytes)
}

fn build_command(root: &Path, target_dir: &Path, target: Target) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            target.triple(),
            "--target-dir",
        ])
        .arg(target_dir);
    command
}

fn dist_dir(root: &Path, options: &Options) -> PathBuf {
    if options.explicit_target {
        root.join("dist").join(options.target.label())
    } else {
        root.join("dist")
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--help"] {
        println!("package [--target <x86_64-pc-windows-msvc|aarch64-pc-windows-msvc>] [--payload-dir <directory>]\nDefault: x64 in dist. Explicit targets use dist\\windows-x64 or dist\\windows-arm64.\nWith --payload-dir, embed supplied matching-architecture runtime EXEs without rebuilding them. Installer signing remains a separate step.");
        return Ok(());
    }
    let options = options(&args)?;
    let payload_dir = &options.payload_dir;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = root.join(
        std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| "target".into()),
    );
    let release = release_dir(&root, Some(target_dir.clone()), options.target);
    if payload_dir.is_none() {
        let status = build_command(&root, &target_dir, options.target)
            .args(["--bin", "arterm", "--bin", "arterm-host"])
            .status()?;
        ensure!(
            status.success(),
            "{} executable build failed",
            options.target.triple()
        );
    }
    let runtime = payload_dir.as_ref().unwrap_or(&release).canonicalize()?;
    let client = runtime.join("arterm.exe").canonicalize()?;
    let host = runtime.join("arterm-host.exe").canonicalize()?;
    let client_hash = Sha256::digest(validate_file(&client, options.target)?);
    let host_hash = Sha256::digest(validate_file(&host, options.target)?);
    let status = build_command(&root, &target_dir, options.target)
        .env("VSTERM_CLIENT_PAYLOAD", &client)
        .env("VSTERM_HOST_PAYLOAD", &host)
        .args([
            "--features",
            "installers",
            "--bin",
            "arTerm-Client-Setup",
            "--bin",
            "arTerm-Host-Setup",
        ])
        .status()?;
    ensure!(status.success(), "installer build failed");
    ensure!(
        Sha256::digest(fs::read(&client)?) == client_hash
            && Sha256::digest(fs::read(&host)?) == host_hash,
        "runtime payload changed while building installers"
    );
    for name in ["arTerm-Client-Setup.exe", "arTerm-Host-Setup.exe"] {
        validate_file(&release.join(name), options.target)?;
    }
    let dist = dist_dir(&root, &options);
    fs::create_dir_all(&dist)?;
    let mut hashes = String::new();
    for name in [
        "arterm.exe",
        "arterm-host.exe",
        "arTerm-Client-Setup.exe",
        "arTerm-Host-Setup.exe",
    ] {
        let source = if name == "arterm.exe" || name == "arterm-host.exe" {
            runtime.join(name)
        } else {
            release.join(name)
        };
        let destination = dist.join(name);
        if destination.canonicalize().ok().as_ref() != Some(&source.canonicalize()?) {
            fs::copy(&source, &destination).with_context(|| format!("packaging {name}"))?;
        }
        hashes.push_str(&format!(
            "{:x}  {name}\n",
            Sha256::digest(fs::read(source)?)
        ));
    }
    for name in ["LICENSE", "THIRD-PARTY-NOTICES.txt"] {
        fs::copy(root.join(name), dist.join(name)).with_context(|| format!("packaging {name}"))?;
        hashes.push_str(&format!(
            "{:x}  {name}\n",
            Sha256::digest(fs::read(dist.join(name))?)
        ));
    }
    fs::write(dist.join("SHA256SUMS"), hashes)?;
    println!(
        "arTerm packages in {}. Installers are not signed by this builder.",
        dist.display()
    );
    if payload_dir.is_some() {
        println!("Supplied runtime payloads were preserved without rebuilding.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_payload_mode_is_explicit_and_strict() {
        assert!(options(&[]).unwrap().payload_dir.is_none());
        assert_eq!(
            options(&["--payload-dir".into(), r"C:\signed".into()])
                .unwrap()
                .payload_dir,
            Some(PathBuf::from(r"C:\signed"))
        );
        assert!(options(&["--payload-dir".into()]).is_err());
        assert!(options(&["--skip-build".into()]).is_err());
    }

    #[test]
    fn target_selection_and_paths_are_isolated() {
        let root = Path::new(r"C:\work");
        let default = options(&[]).unwrap();
        assert_eq!(default.target, Target::X64);
        assert_eq!(dist_dir(root, &default), root.join("dist"));
        for target in [Target::X64, Target::Arm64] {
            let opts = options(&[
                "--payload-dir".into(),
                r"C:\signed".into(),
                "--target".into(),
                target.triple().into(),
            ])
            .unwrap();
            assert_eq!(opts.target, target);
            assert_eq!(
                release_dir(root, None, target),
                root.join("target").join(target.triple()).join("release")
            );
            assert_eq!(
                release_dir(root, Some(r"C:\isolated".into()), target),
                Path::new(r"C:\isolated")
                    .join(target.triple())
                    .join("release")
            );
            assert_eq!(
                release_dir(root, Some("out".into()), target),
                root.join("out").join(target.triple()).join("release")
            );
            assert_eq!(
                dist_dir(root, &opts),
                root.join("dist").join(target.label())
            );
        }
        for args in [
            vec!["--target"],
            vec!["--target", "arm64"],
            vec!["--target", "i686-pc-windows-msvc"],
            vec![
                "--target",
                "x86_64-pc-windows-msvc",
                "--target",
                "aarch64-pc-windows-msvc",
            ],
            vec!["--payload-dir", "--target"],
            vec!["--payload-dir", "a", "--payload-dir", "b"],
        ] {
            assert!(options(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }

    fn pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0; 1024];
        bytes[..2].copy_from_slice(b"MZ");
        put32(&mut bytes, 60, 64);
        bytes[64..68].copy_from_slice(b"PE\0\0");
        put16(&mut bytes, 68, machine);
        put16(&mut bytes, 70, 1);
        put16(&mut bytes, 84, 240);
        put16(&mut bytes, 86, 0x22);
        let optional = 88;
        put16(&mut bytes, optional, 0x20b);
        put32(&mut bytes, optional + 4, 512);
        put32(&mut bytes, optional + 16, 4096);
        put32(&mut bytes, optional + 20, 4096);
        bytes[optional + 24..optional + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        put32(&mut bytes, optional + 32, 4096);
        put32(&mut bytes, optional + 36, 512);
        put16(&mut bytes, optional + 40, 6);
        put16(&mut bytes, optional + 48, 6);
        put32(&mut bytes, optional + 56, 8192);
        put32(&mut bytes, optional + 60, 512);
        put16(&mut bytes, optional + 68, 3);
        for (offset, size) in [(72, 0x10_0000u64), (80, 4096), (88, 0x10_0000), (96, 4096)] {
            bytes[optional + offset..optional + offset + 8].copy_from_slice(&size.to_le_bytes());
        }
        put32(&mut bytes, optional + 108, 16);
        let section = optional + 240;
        bytes[section..section + 5].copy_from_slice(b".text");
        put32(&mut bytes, section + 8, 4);
        put32(&mut bytes, section + 12, 4096);
        put32(&mut bytes, section + 16, 512);
        put32(&mut bytes, section + 20, 512);
        put32(&mut bytes, section + 36, 0x6000_0020);
        if machine == 0xaa64 {
            put32(&mut bytes, 512, 0xd65f_03c0);
        } else {
            bytes[512] = 0xc3;
        }
        bytes
    }

    fn put16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn payload_machine_must_match_target() {
        for target in [Target::X64, Target::Arm64] {
            validate_pe(&pe(target.machine()), target).unwrap();
        }
        assert!(validate_pe(&pe(0x8664), Target::Arm64)
            .unwrap_err()
            .to_string()
            .contains("mismatch"));
        assert!(validate_pe(&pe(0xaa64), Target::X64)
            .unwrap_err()
            .to_string()
            .contains("mismatch"));
        for machine in [0x014c, 0xa641, 0, 0xffff] {
            assert!(validate_pe(&pe(machine), Target::Arm64)
                .unwrap_err()
                .to_string()
                .contains("unsupported PE machine"));
        }
    }

    #[test]
    fn malformed_payloads_fail_closed() {
        for target in [Target::X64, Target::Arm64] {
            let valid = pe(target.machine());
            for length in 0..valid.len() {
                assert!(
                    validate_pe(&valid[..length], target).is_err(),
                    "{} accepted truncation at {length}",
                    target.triple()
                );
            }
        }
        let valid = pe(0xaa64);
        let mut bytes = valid.clone();
        bytes[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(validate_pe(&bytes, Target::Arm64).is_err());
        let mut bytes = valid;
        bytes[64] = b'X';
        assert!(validate_pe(&bytes, Target::Arm64)
            .unwrap_err()
            .to_string()
            .contains("PE signature"));
    }

    #[test]
    fn invalid_executable_headers_and_sections_fail_closed() {
        for target in [Target::X64, Target::Arm64] {
            for (offset, value, reason) in [
                (70, 0, "section count"),
                (70, 97, "section count"),
                (70, u16::MAX, "section count"),
                (84, 0, "optional header"),
                (84, 111, "optional header"),
                (84, u16::MAX, "optional header"),
                (86, 0, "executable image"),
                (86, 0x2002, "executable image"),
                (88, 0x10b, "PE32+"),
            ] {
                let mut bytes = pe(target.machine());
                put16(&mut bytes, offset, value);
                let error = validate_pe(&bytes, target).unwrap_err().to_string();
                assert!(error.contains(reason), "{offset}: {error}");
            }
            for (offset, value, reason) in [
                (60, 0, "overlaps DOS"),
                (60, u32::MAX, "PE header"),
                (196, 17, "data directories"),
                (196, u32::MAX, "directory size overflow"),
                (148, 327, "SizeOfHeaders"),
                (148, u32::MAX, "SizeOfHeaders"),
                (144, 512, "SizeOfImage"),
                (340, u32::MAX, "virtual range overflow"),
                (336, u32::MAX, "virtual range overflow"),
                (340, 8192, "virtual range outside"),
                (344, u32::MAX, "virtual range overflow"),
                (348, u32::MAX, "raw range overflow"),
                (348, 0, "section raw data"),
                (348, 1024, "section raw data"),
                (104, 0, "entry point"),
                (104, 4608, "entry point"),
                (344, 0, "entry point"),
                (364, 0x4000_0040, "entry point"),
            ] {
                let mut bytes = pe(target.machine());
                put32(&mut bytes, offset, value);
                let error = validate_pe(&bytes, target).unwrap_err().to_string();
                assert!(error.contains(reason), "{offset}: {error}");
            }
            let mut bytes = pe(target.machine());
            put16(&mut bytes, 70, 18);
            assert!(validate_pe(&bytes, target)
                .unwrap_err()
                .to_string()
                .contains("section table"));
        }
    }

    #[test]
    fn certificate_overlay_is_allowed_but_must_not_be_truncated() {
        for target in [Target::X64, Target::Arm64] {
            let mut bytes = pe(target.machine());
            bytes.resize(1040, 0);
            put32(&mut bytes, 232, 1024);
            put32(&mut bytes, 236, 16);
            put32(&mut bytes, 1024, 16);
            put16(&mut bytes, 1028, 0x0200);
            put16(&mut bytes, 1030, 2);
            validate_pe(&bytes, target).unwrap();
            assert!(validate_pe(&bytes[..1039], target)
                .unwrap_err()
                .to_string()
                .contains("certificate table"));
            put32(&mut bytes, 232, u32::MAX);
            assert!(validate_pe(&bytes, target)
                .unwrap_err()
                .to_string()
                .contains("certificate table range overflow"));
        }
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
