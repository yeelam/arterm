use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
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
    if !path.try_exists().context("inspect client configuration")? {
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

fn writer_lock(root: &Path, timeout: Duration) -> Result<fs::File> {
    fs::create_dir_all(root)?;
    let deadline = Instant::now() + timeout;
    loop {
        match OpenOptions::new().write(true).create(true).truncate(false).share_mode(0)
            .open(root.join("client-config.lock"))
        {
            Ok(file) => return Ok(file),
            Err(error) if error.raw_os_error() == Some(32) => {
                ensure!(Instant::now() < deadline,
                    "client configuration is busy; another writer holds the lock; retry the command");
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error).context("lock client configuration"),
        }
    }
}

/// Reload and mutate under the shared writer lock; callbacks must not prompt or sign in.
pub fn update<T>(root: &Path, change: impl FnOnce(&mut ClientConfig) -> Result<T>) -> Result<T> {
    let _lock = writer_lock(root, Duration::from_secs(2))?;
    let mut config = load_for_initialization(root)?;
    let result = change(&mut config)?;
    save_locked(root, &config)?;
    Ok(result)
}

fn save_locked(root: &Path, config: &ClientConfig) -> Result<()> {
    ensure!(config.schema == 1, "invalid client configuration schema");
    let path = config_path(root);
    let parent = path.parent().context("configuration path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!("config-{}.tmp", uuid::Uuid::now_v7()));
    let bytes = serde_json::to_vec_pretty(config)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
    let written = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error).context("write client configuration");
    }
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

/// Initialize local files only; sign-in and host registration are explicit commands.
pub fn initialize(root: &Path, override_path: Option<&Path>, no_download: bool) -> Result<PathBuf> {
    initialize_with(root, override_path, |requested| {
        crate::deployment::ensure_dependency(crate::deployment::Role::Client, requested, no_download)
    })
}

fn load_for_initialization(root: &Path) -> Result<ClientConfig> {
    let exists = config_path(root).try_exists().context("inspect client configuration")?;
    let client = root.join("client");
    if !exists && client.try_exists()? {
        for entry in fs::read_dir(&client)? {
            let entry = entry?;
            // Empty initialization directories can remain after a failed first save.
            let empty_directory = ["sessions", "forwards"].iter()
                .any(|name| entry.file_name() == *name)
                && entry.file_type()?.is_dir()
                && fs::read_dir(entry.path())?.next().transpose()?.is_none();
            ensure!(
                empty_directory,
                "client configuration is missing but local data exists; restore client/config.json before continuing"
            );
        }
    }
    load(root)
}

fn initialize_with(
    root: &Path,
    override_path: Option<&Path>,
    select: impl FnOnce(Option<&Path>) -> Result<PathBuf>,
) -> Result<PathBuf> {
    let config = {
        let _lock = writer_lock(root, Duration::from_secs(2))?;
        load_for_initialization(root)?
    };
    let requested = override_path.or(config.devtunnel_path.as_deref());
    if let Some(path) = requested {
        ensure!(path.is_absolute(), "devtunnel path must be absolute");
    }
    // Dependency discovery may prompt/download. Never hold the writer lock across it.
    let selected = select(requested)?;
    let _lock = writer_lock(root, Duration::from_secs(2))?;
    let mut latest = load_for_initialization(root)?;
    ensure!(
        latest.devtunnel_path == config.devtunnel_path,
        "client dependency configuration changed during initialization; retry the command"
    );
    // Keep a valid saved path's original spelling and the entire file byte-for-byte.
    let changed = latest.devtunnel_path.is_none()
        || override_path.is_some_and(|path| latest.devtunnel_path.as_deref() != Some(path));
    let client = root.join("client");
    for name in ["sessions", "forwards"] {
        fs::create_dir_all(client.join(name))
            .with_context(|| format!("create client {name} directory"))?;
    }
    if changed {
        latest.devtunnel_path = Some(selected.clone());
        save_locked(root, &latest)?;
    }
    Ok(selected)
}

pub fn add(
    config: &mut ClientConfig,
    alias: &str,
    tunnel_id: &str,
    host_path: &str,
) -> Result<Target> {
    validate_registration(alias, tunnel_id, host_path)?;
    ensure!(
        !config.targets.contains_key(alias),
        "alias already exists: {alias}"
    );
    let target = Target {
        tunnel_id: tunnel_id.to_owned(),
        host_path: host_path.to_owned(),
        target_id: target_id(tunnel_id, host_path),
    };
    config.targets.insert(alias.to_owned(), target.clone());
    Ok(target)
}

pub fn validate_registration(alias: &str, tunnel_id: &str, host_path: &str) -> Result<()> {
    validate_alias(alias)?;
    ensure!(
        !tunnel_id.trim().is_empty() && !tunnel_id.chars().any(char::is_whitespace),
        "tunnel name or ID must be non-empty and contain no whitespace"
    );
    ensure!(
        Path::new(host_path).is_absolute(),
        "host path must be absolute"
    );
    Ok(())
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

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("arterm-config-{}", uuid::Uuid::now_v7())))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if self.0.exists() {
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }

    #[test]
    fn initialization_reloads_registration_changes_after_dependency_selection() {
        let fixture = Fixture::new();
        let root = &fixture.0;
        let dependency = PathBuf::from(r"C:\Tools\devtunnel.exe");
        let selected = initialize_with(root, None, |requested| {
            assert!(requested.is_none());
            update(root, |config| add(config, "box", "my-box", r"C:\Tools\arterm-host.exe"))?;
            Ok(dependency.clone())
        }).unwrap();
        assert_eq!(selected, dependency);
        let config = load(root).unwrap();
        assert_eq!(config.targets["box"].tunnel_id, "my-box");
        assert_eq!(config.devtunnel_path, Some(dependency));

        initialize_with(root, Some(Path::new(r"C:\Other\devtunnel.exe")), |_| {
            update(root, |config| {
                remove(config, root, "box")?;
                add(config, "other", "other-box", r"C:\Tools\arterm-host.exe")
            })?;
            Ok(PathBuf::from(r"C:\Other\devtunnel.exe"))
        }).unwrap();
        let config = load(root).unwrap();
        assert!(!config.targets.contains_key("box"));
        assert_eq!(config.targets["other"].tunnel_id, "other-box");
    }

    #[test]
    fn initialization_rejects_concurrent_custom_path_without_overwriting() {
        for initial in [None, Some(PathBuf::from(r"C:\Original\devtunnel.exe"))] {
        for explicit in [None, Some(Path::new(r"C:\Requested\devtunnel.exe"))] {
            let fixture = Fixture::new();
            let root = &fixture.0;
            if let Some(path) = &initial {
                update(root, |config| {
                    config.devtunnel_path = Some(path.clone());
                    Ok(())
                }).unwrap();
            }
            let mut committed = Vec::new();
            let error = initialize_with(root, explicit, |_| {
                update(root, |config| {
                    config.devtunnel_path = Some(PathBuf::from(r"C:\Concurrent\devtunnel.exe"));
                    add(config, "box", "my-box", r"C:\Tools\arterm-host.exe")
                })?;
                committed = fs::read(config_path(root))?;
                Ok(PathBuf::from(r"C:\Selected\devtunnel.exe"))
            }).unwrap_err();
            assert!(error.to_string().contains("changed during initialization"), "{error:#}");
            assert_eq!(fs::read(config_path(root)).unwrap(), committed);
            assert!(!root.join("client\\sessions").exists());
        }
        }
    }

    #[test]
    fn initialization_leaves_latest_unchanged_configuration_bytes_intact() {
        let fixture = Fixture::new();
        let root = &fixture.0;
        let dependency = PathBuf::from(r"C:\Tools\devtunnel.exe");
        update(root, |config| {
            config.devtunnel_path = Some(dependency.clone());
            Ok(())
        }).unwrap();
        let mut committed = Vec::new();
        initialize_with(root, None, |requested| {
            assert_eq!(requested, Some(dependency.as_path()));
            update(root, |config| add(config, "box", "my-box", r"C:\Tools\arterm-host.exe"))?;
            let _lock = writer_lock(root, Duration::ZERO)?;
            let mut value = serde_json::to_value(load(root)?)?;
            value["future_metadata"] = "preserved".into();
            committed = format!(" \r\n{}\r\n", serde_json::to_string(&value)?).into_bytes();
            fs::write(config_path(root), &committed)?;
            Ok(dependency.clone())
        }).unwrap();
        assert_eq!(fs::read(config_path(root)).unwrap(), committed);
    }

    #[test]
    fn configuration_writers_serialize_and_contention_is_bounded() {
        let fixture = Fixture::new();
        let root = &fixture.0;
        let held = writer_lock(root, Duration::ZERO).unwrap();
        let start = Instant::now();
        let error = writer_lock(root, Duration::from_millis(50)).unwrap_err();
        assert!(error.to_string().contains("configuration is busy"), "{error:#}");
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!config_path(root).exists());
        drop(held);

        let barrier = std::sync::Barrier::new(8);
        thread::scope(|scope| {
            let workers: Vec<_> = (0..8).map(|index| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    update(root, |config| {
                        thread::sleep(Duration::from_millis(10));
                        add(config, &format!("box-{index}"), "my-box", r"C:\Tools\arterm-host.exe")
                    })
                })
            }).collect();
            for worker in workers { worker.join().unwrap().unwrap(); }
        });
        assert_eq!(load(root).unwrap().targets.len(), 8);
        update(root, |config| remove(config, root, "box-0")).unwrap();
        assert_eq!(load(root).unwrap().targets.len(), 7);
        // Failed mutation releases the handle and does not publish its partial changes.
        let before = fs::read(config_path(root)).unwrap();
        assert!(update(root, |config| -> Result<()> {
            config.targets.clear();
            anyhow::bail!("abort test update")
        }).is_err());
        assert_eq!(fs::read(config_path(root)).unwrap(), before);
        writer_lock(root, Duration::ZERO).unwrap();
    }

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
