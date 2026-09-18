use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command};

fn payload_option(args: &[String]) -> Result<Option<PathBuf>> {
    match args {
        [] => Ok(None),
        [flag, path] if flag == "--payload-dir" && !path.is_empty() => {
            Ok(Some(PathBuf::from(path)))
        }
        _ => bail!("usage: package [--payload-dir <directory-with-runtime-exes>]"),
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--help"] {
        println!("package [--payload-dir <directory>]\nWith --payload-dir, embed supplied runtime EXEs without rebuilding them. Installer signing remains a separate step.");
        return Ok(());
    }
    let payload_dir = payload_option(&args)?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if payload_dir.is_none() {
        let status = Command::new("cargo")
            .current_dir(&root)
            .args([
                "build",
                "--locked",
                "--release",
                "--bin",
                "arterm",
                "--bin",
                "arterm-host",
            ])
            .status()?;
        ensure!(status.success(), "native executable build failed");
    }
    let release = root.join("target\\release");
    let runtime = payload_dir.as_ref().unwrap_or(&release).canonicalize()?;
    let client = runtime.join("arterm.exe").canonicalize()?;
    let host = runtime.join("arterm-host.exe").canonicalize()?;
    let client_hash = Sha256::digest(fs::read(&client)?);
    let host_hash = Sha256::digest(fs::read(&host)?);
    let status = Command::new("cargo")
        .current_dir(&root)
        .env("VSTERM_CLIENT_PAYLOAD", &client)
        .env("VSTERM_HOST_PAYLOAD", &host)
        .args([
            "build",
            "--locked",
            "--release",
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
    let dist = root.join("dist");
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
    fs::copy(root.join("LICENSE"), dist.join("LICENSE")).context("packaging MIT license")?;
    hashes.push_str(&format!(
        "{:x}  LICENSE\n",
        Sha256::digest(fs::read(dist.join("LICENSE"))?)
    ));
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
        assert!(payload_option(&[]).unwrap().is_none());
        assert_eq!(
            payload_option(&["--payload-dir".into(), r"C:\signed".into()]).unwrap(),
            Some(PathBuf::from(r"C:\signed"))
        );
        assert!(payload_option(&["--payload-dir".into()]).is_err());
        assert!(payload_option(&["--skip-build".into()]).is_err());
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
