use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
};
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    pub tunnel_id: String,
    pub host_path: String,
    pub target_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    pub schema: u32,
    pub devtunnel_path: Option<PathBuf>,
    pub targets: BTreeMap<String, Target>,
}

pub fn validate_alias(alias: &str) -> Result<()> {
    ensure!(
        (1..=40).contains(&alias.len()),
        "alias must contain 1..40 characters"
    );
    ensure!(
        alias
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "alias must contain only lowercase letters, digits, and hyphens"
    );
    ensure!(
        alias
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && alias
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric),
        "alias must start and end with a letter or digit"
    );
    Ok(())
}

pub fn target_id(tunnel_id: &str, host_path: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(tunnel_id.trim().to_ascii_lowercase());
    hash.update([0]);
    hash.update(host_path.trim().replace('/', "\\").to_ascii_lowercase());
    let digest = hash.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join("client").join("config.json")
}

pub fn state_dir(root: &Path, target: &Target) -> PathBuf {
    root.join("client").join("sessions").join(&target.target_id)
}

pub fn load(root: &Path) -> Result<ClientConfig> {
    let path = config_path(root);
    if !path.exists() {
        return Ok(ClientConfig {
            schema: 1,
            ..ClientConfig::default()
        });
    }
    let config: ClientConfig =
        serde_json::from_slice(&fs::read(&path).context("read client configuration")?)
            .context("invalid client configuration")?;
    ensure!(
        config.schema == 1,
        "unsupported client configuration schema"
    );
    Ok(config)
}

pub fn save(root: &Path, config: &ClientConfig) -> Result<()> {
    ensure!(config.schema == 1, "invalid client configuration schema");
    let path = config_path(root);
    let parent = path.parent().context("configuration path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!("config-{}.tmp", uuid::Uuid::now_v7()));
    fs::write(&temp, serde_json::to_vec_pretty(config)?)?;
    let wide =
        |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain(Some(0)).collect() };
    let from = wide(&temp);
    let to = wide(&path);
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        let error = std::io::Error::last_os_error();
        let _ = fs::remove_file(&temp);
        return Err(error).context("replace client configuration");
    }
    Ok(())
}

pub fn add(
    config: &mut ClientConfig,
    alias: &str,
    tunnel_id: &str,
    host_path: &str,
) -> Result<Target> {
    validate_alias(alias)?;
    ensure!(
        !config.targets.contains_key(alias),
        "alias already exists: {alias}"
    );
    ensure!(
        !tunnel_id.trim().is_empty() && !tunnel_id.chars().any(char::is_whitespace),
        "tunnel name or ID must be non-empty and contain no whitespace"
    );
    ensure!(
        Path::new(host_path).is_absolute(),
        "host path must be absolute"
    );
    let target = Target {
        tunnel_id: tunnel_id.to_owned(),
        host_path: host_path.to_owned(),
        target_id: target_id(tunnel_id, host_path),
    };
    config.targets.insert(alias.to_owned(), target.clone());
    Ok(target)
}

pub fn target<'a>(config: &'a ClientConfig, alias: &str) -> Result<&'a Target> {
    validate_alias(alias)?;
    config
        .targets
        .get(alias)
        .with_context(|| format!("unknown alias: {alias}; register the host first with `arterm add {alias} --tunnel <host-name> --host-path <absolute-remote-host-exe>`"))
}

pub fn saved_session_count(dir: &Path) -> Result<usize> {
    if !dir.exists() {
        return Ok(0);
    }
    Ok(fs::read_dir(dir)?
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dpapi"))
        })
        .count())
}

pub fn has_active_session(dir: &Path) -> Result<bool> {
    if !dir.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
        {
            continue;
        }
        match OpenOptions::new().write(true).share_mode(0).open(&path) {
            Ok(_) => {}
            Err(error) if error.raw_os_error() == Some(32) => return Ok(true),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect session lock {}", path.display()))
            }
        }
    }
    Ok(false)
}

fn has_active_forward(root: &Path) -> Result<bool> {
    let dir = root.join("client").join("forwards");
    if !dir.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
        {
            continue;
        }
        match OpenOptions::new().write(true).share_mode(0).open(&path) {
            Ok(_) => {}
            Err(error) if error.raw_os_error() == Some(32) => return Ok(true),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect forward lock {}", path.display()))
            }
        }
    }
    Ok(false)
}

pub fn any_active_session(root: &Path, config: &ClientConfig) -> Result<bool> {
    if has_active_forward(root)? {
        return Ok(true);
    }
    for target in config.targets.values() {
        if has_active_session(&state_dir(root, target))? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn remove(config: &mut ClientConfig, root: &Path, alias: &str) -> Result<usize> {
    let target = target(config, alias)?.clone();
    let sessions = state_dir(root, &target);
    ensure!(
        !has_active_session(&sessions)?,
        "alias has a locally active session: {alias}"
    );
    let saved = saved_session_count(&sessions)?;
    config.targets.remove(alias);
    if saved > 0 {
        eprintln!(
            "[client] Removed alias {alias}; preserved {saved} recovery record(s) for target {}.",
            target.target_id
        );
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_target_identity_are_stable() {
        assert!(validate_alias("work-box1").is_ok());
        for invalid in ["", "UPPER", "-bad", "bad-", "two words"] {
            assert!(validate_alias(invalid).is_err(), "accepted {invalid:?}");
        }
        assert_eq!(
            target_id("Tunnel-A", r"C:/Tools/vsterm-host.exe"),
            target_id("tunnel-a", r"c:\Tools\VSTERM-HOST.exe")
        );
        assert_ne!(
            target_id("tunnel-a", r"C:\Tools\vsterm-host.exe"),
            target_id("tunnel-b", r"C:\Tools\vsterm-host.exe")
        );
    }
}
