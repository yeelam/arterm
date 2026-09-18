//! Per-user installation and dependency discovery shared by native executables.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, ERROR_SUCCESS},
    Security::Cryptography::{CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE},
    Security::WinTrust::*,
    Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
    System::Registry::*,
    UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    },
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Client,
    Host,
}

pub fn data_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("VSTERM_REMOTE_HOME") {
        return Ok(PathBuf::from(root));
    }
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unset")?)
            .join("VsTerm"),
    )
}

pub fn install_dir(role: Role) -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unset")?)
            .join("Programs")
            .join("VsTerm")
            .join(if role == Role::Client {
                "Client"
            } else {
                "Host"
            }),
    )
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn w(value: &str) -> Vec<u16> {
    wide(std::ffi::OsStr::new(value))
}

pub fn exe_name(role: Role) -> &'static str {
    if role == Role::Client {
        "arterm.exe"
    } else {
        "arterm-host.exe"
    }
}

pub fn legacy_exe_name(role: Role) -> &'static str {
    if role == Role::Client { "vsterm.exe" } else { "vsterm-host.exe" }
}

fn installed_runtimes(dir: &Path, role: Role) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for name in [exe_name(role), legacy_exe_name(role)] {
        let path = dir.join(name);
        if path.try_exists()? {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn require_closed_runtimes(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        OpenOptions::new().write(true).share_mode(0).open(path)
            .with_context(|| format!("runtime is in use or inaccessible; close it before updating: {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn stop_host_command(path: &Path, terminate_sessions: bool) -> Result<()> {
    let mut command = Command::new(path);
    command.args(host_stop_args(terminate_sessions)).stdin(Stdio::null());
    let mut child = command.spawn().context("start scoped host stop command")?;
    wait_for_stop_child(&mut child, Duration::from_secs(20))
        .context("scoped graceful shutdown did not complete; the stop helper was reaped, but an already-delivered request may still finish. Check host status, or explicitly use --force-stop-host to end matching host processes")
}

fn wait_for_stop_child(child: &mut Child, timeout: Duration) -> Result<()> {
    let result = wait_for_shutdown(timeout, || {
        let Some(status) = child.try_wait()? else { return Ok(false); };
        ensure!(
            status.success(),
            "host has live sessions or cannot be stopped; setup/update/uninstall cancelled (explicit --terminate-sessions or installer /terminate-sessions ends scoped sessions)"
        );
        Ok(true)
    });
    if result.is_err() && child.try_wait()?.is_none() {
        // This is our own controller, not the host. Do not leave timed-out stop
        // helpers running and locking the installed executable indefinitely.
        if let Err(error) = child.kill() {
            ensure!(child.try_wait()?.is_some(), "cannot clean up timed-out stop helper: {error}");
        }
        child.wait().context("reap timed-out stop helper")?;
    }
    result
}

fn wait_for_shutdown(timeout: Duration, mut stopped: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if stopped()? {
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "timed out waiting for graceful host shutdown; operation cancelled");
        thread::sleep(Duration::from_millis(50));
    }
}

fn host_stop_args(terminate_sessions: bool) -> Vec<&'static str> {
    if terminate_sessions { vec!["stop", "--terminate-sessions"] } else { vec!["stop"] }
}

pub(crate) fn force_stop_host_command(path: &Path) -> Result<()> {
    let dir = path.parent().context("host executable has no parent directory")?;
    let mut paths = installed_runtimes(dir, Role::Host)?;
    if !paths.iter().any(|p| p == path) {
        paths.push(path.to_owned());
    }
    eprintln!("WARNING: force-stopping this installation's host processes for the current user/logon. ALL their sessions will end, including hosts using other data roots.");
    crate::host_shutdown::force_stop(&paths)?;
    Ok(())
}

fn stop_installed_host(paths: &[PathBuf], terminate_sessions: bool, force_stop: bool) -> Result<()> {
    ensure!(!paths.is_empty(), "installed host executable is missing");
    if force_stop {
        eprintln!("WARNING: force-stopping this installation's host processes for the current user/logon. ALL their sessions will end, including hosts using other data roots.");
        crate::host_shutdown::force_stop(paths)?;
    } else {
        for path in paths {
            stop_host_command(path, terminate_sessions)?;
        }
    }
    // Older installed stop commands acknowledge before the executable is released.
    wait_for_shutdown(Duration::from_secs(10), || match require_closed_runtimes(paths) {
        Ok(()) => Ok(true),
        Err(error) if error.downcast_ref::<io::Error>().is_some_and(|e| {
            e.raw_os_error().is_some_and(|code| {
                code == ERROR_SHARING_VIOLATION as i32 || code == ERROR_LOCK_VIOLATION as i32
            })
        }) => Ok(false),
        Err(error) => Err(error),
    })?;
    Ok(())
}

fn dependency_candidates(role: Role) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        if role == Role::Client {
            paths.push(local.join("Microsoft\\WinGet\\Links\\devtunnel.exe"));
        } else {
            paths.push(local.join("Programs\\Microsoft VS Code\\bin\\code-tunnel.exe"));
        }
    }
    if role == Role::Host {
        if let Some(p) = std::env::var_os("ProgramFiles") {
            paths.push(PathBuf::from(p).join("Microsoft VS Code\\bin\\code-tunnel.exe"));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let name = if role == Role::Client {
                "devtunnel.exe"
            } else {
                "code-tunnel.exe"
            };
            paths.push(dir.join(name));
        }
    }
    paths
}

pub fn verify_signature(path: &Path) -> Result<()> {
    let file_name = wide(path.as_os_str());
    unsafe {
        let mut file: WINTRUST_FILE_INFO = std::mem::zeroed();
        file.cbStruct = std::mem::size_of::<WINTRUST_FILE_INFO>() as u32;
        file.pcwszFilePath = file_name.as_ptr();
        let mut data: WINTRUST_DATA = std::mem::zeroed();
        data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
        data.dwUIChoice = WTD_UI_NONE;
        data.fdwRevocationChecks = WTD_REVOKE_NONE;
        data.dwUnionChoice = WTD_CHOICE_FILE;
        data.Anonymous.pFile = &mut file;
        data.dwStateAction = WTD_STATEACTION_VERIFY;
        data.dwProvFlags = WTD_CACHE_ONLY_URL_RETRIEVAL;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        let result = WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut data as *mut _ as *mut _,
        );
        let mut publisher = String::new();
        if result == 0 {
            let provider = WTHelperProvDataFromStateData(data.hWVTStateData);
            if !provider.is_null() {
                let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
                if !signer.is_null() && (*signer).csCertChain > 0 {
                    let cert = (*(*signer).pasCertChain).pCert;
                    let len = CertGetNameStringW(
                        cert,
                        CERT_NAME_SIMPLE_DISPLAY_TYPE,
                        0,
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        0,
                    );
                    if len > 1 {
                        let mut name = vec![0u16; len as usize];
                        CertGetNameStringW(
                            cert,
                            CERT_NAME_SIMPLE_DISPLAY_TYPE,
                            0,
                            std::ptr::null(),
                            name.as_mut_ptr(),
                            len,
                        );
                        name.pop();
                        publisher = String::from_utf16_lossy(&name);
                    }
                }
            }
        }
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut data as *mut _ as *mut _,
        );
        ensure!(
            result == 0,
            "dependency signature/trust check failed for {}: 0x{:08x}",
            path.display(),
            result as u32
        );
        ensure!(
            publisher == "Microsoft Corporation",
            "dependency publisher must be Microsoft Corporation, got {publisher:?}"
        );
    }
    Ok(())
}

fn validate_dependency(path: &Path, role: Role) -> Result<PathBuf> {
    ensure!(
        path.is_file()
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("exe")),
        "dependency must be a native EXE: {}",
        path.display()
    );
    let path = path.canonicalize()?;
    verify_signature(&path)?;
    let output = Command::new(&path).arg("--help").output()?;
    let help = String::from_utf8_lossy(&output.stdout);
    ensure!(
        output.status.success(),
        "dependency --help failed: {}",
        path.display()
    );
    if role == Role::Host {
        ensure!(help.contains("--cli-data-dir") && help.contains("tunnel"),
            "requires native code CLI with --cli-data-dir isolation (select code-tunnel.exe, not code.cmd)");
    } else {
        ensure!(
            help.contains("connect") && help.contains("user"),
            "incompatible devtunnel CLI"
        );
    }
    Ok(path)
}

pub fn ensure_dependency(
    role: Role,
    override_path: Option<&Path>,
    no_download: bool,
) -> Result<PathBuf> {
    if let Some(path) = override_path {
        return validate_dependency(path, role);
    }
    for candidate in dependency_candidates(role) {
        if candidate.is_file() {
            return validate_dependency(&candidate, role);
        }
    }
    let package = if role == Role::Client {
        "Microsoft.devtunnel"
    } else {
        "Microsoft.VisualStudioCode"
    };
    ensure!(!no_download, "{package} is missing; install it using your approved software channel or rerun interactive setup");
    eprintln!(
        "Dependency {package} is missing. WinGet will install the vendor package for this user."
    );
    eprintln!("For the host this installs VS Code, including its standalone native CLI. Vendor terms apply.");
    eprint!("Install this dependency and accept its WinGet/package agreements? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    ensure!(
        answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes"),
        "dependency installation not approved"
    );
    let status = Command::new("winget.exe")
        .args([
            "install",
            "--id",
            package,
            "--exact",
            "--scope",
            "user",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ])
        .status()
        .context(
            "WinGet unavailable; install the dependency through your approved software channel",
        )?;
    ensure!(
        status.success(),
        "WinGet failed; no dependency has been assumed installed"
    );
    for candidate in dependency_candidates(role) {
        if candidate.is_file() {
            return validate_dependency(&candidate, role);
        }
    }
    bail!("dependency installed but native EXE not found; supply an explicit setup path")
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn open_key(path: &str, write: bool) -> Result<Key> {
    let mut key = std::ptr::null_mut();
    let name = w(path);
    let result = unsafe {
        if write {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                name.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        } else {
            RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, KEY_READ, &mut key)
        }
    };
    ensure!(result == ERROR_SUCCESS, "registry open failed: {result}");
    Ok(Key(key))
}
fn read_string(key: &Key, name: &str) -> Result<String> {
    let name = w(name);
    let mut size = 0;
    let result = unsafe {
        RegGetValueW(
            key.0,
            std::ptr::null(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(String::new());
    }
    ensure!(result == ERROR_SUCCESS, "registry read failed: {result}");
    let mut bytes = vec![0u16; size as usize / 2];
    let result = unsafe {
        RegGetValueW(
            key.0,
            std::ptr::null(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            std::ptr::null_mut(),
            bytes.as_mut_ptr() as *mut _,
            &mut size,
        )
    };
    ensure!(result == ERROR_SUCCESS, "registry read failed: {result}");
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    Ok(String::from_utf16(&bytes)?)
}
fn write_string(key: &Key, name: &str, value: &str, expand: bool) -> Result<()> {
    let name = w(name);
    let value = w(value);
    let result = unsafe {
        RegSetValueExW(
            key.0,
            name.as_ptr(),
            0,
            if expand { REG_EXPAND_SZ } else { REG_SZ },
            value.as_ptr() as *const u8,
            (value.len() * 2) as u32,
        )
    };
    ensure!(result == ERROR_SUCCESS, "registry write failed: {result}");
    Ok(())
}
fn delete_value(key: &Key, name: &str) -> Result<()> {
    let result = unsafe { RegDeleteValueW(key.0, w(name).as_ptr()) };
    ensure!(
        result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND,
        "registry delete failed: {result}"
    );
    Ok(())
}
pub fn set_startup(exe: Option<&Path>) -> Result<()> {
    let key = open_key("Software\\Microsoft\\Windows\\CurrentVersion\\Run", true)?;
    if let Some(exe) = exe {
        let exe = exe.canonicalize()?;
        ensure!(
            !exe.to_string_lossy().contains('"'),
            "invalid executable path"
        );
        write_string(
            &key,
            "VsTermHost",
            &format!("\"{}\" start", exe.display()),
            false,
        )
    } else {
        delete_value(&key, "VsTermHost")
    }
}
fn update_path(dir: &Path, install: bool) -> Result<()> {
    let key = open_key("Environment", true)?;
    let before = read_string(&key, "Path")?;
    let entry = dir.to_string_lossy();
    let mut entries: Vec<&str> = before
        .split(';')
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case(&entry))
        .collect();
    if install {
        entries.push(&entry);
    }
    write_string(&key, "Path", &entries.join(";"), true)?;
    let environment = w("Environment");
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            2000,
            std::ptr::null_mut(),
        );
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Installed {
    publisher: String,
    role: String,
    version: String,
}
const INSTALL_PUBLISHER: &str = "arTerm";
const VSTERM_INSTALL_PUBLISHER: &str = "VsTerm";
// Existing installation markers must remain eligible for upgrade and uninstall.
const LEGACY_INSTALL_PUBLISHER: &str = "yeelam-gordon";
fn role_name(role: Role) -> &'static str {
    if role == Role::Client {
        "Client"
    } else {
        "Host"
    }
}

fn verify_install_owner(dir: &Path, role: Role) -> Result<()> {
    let installed: Installed = serde_json::from_slice(
        &fs::read(dir.join("installed.json"))
            .context("refusing to modify an unowned installation")?,
    )?;
    ensure!(
        (installed.publisher == INSTALL_PUBLISHER
            || installed.publisher == VSTERM_INSTALL_PUBLISHER
            || installed.publisher == LEGACY_INSTALL_PUBLISHER)
            && installed.role == role_name(role),
        "installation ownership mismatch"
    );
    Ok(())
}

pub fn write_payload(dir: &Path, role: Role, bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.starts_with(b"MZ"),
        "installer payload is not a Windows executable"
    );
    fs::create_dir_all(dir)?;
    let marker = dir.join("installed.json");
    let existing = installed_runtimes(dir, role)?;
    if !existing.is_empty() || marker.try_exists()? {
        verify_install_owner(dir, role).context("refusing to overwrite an unowned installation")?;
    }
    require_closed_runtimes(&existing)?;
    for name in [exe_name(role), legacy_exe_name(role)] {
    let temp = dir.join(format!("{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let from = wide(temp.as_os_str());
        let dest = wide(dir.join(name).as_os_str());
        ensure!(
            unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    dest.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } != 0,
            "cannot replace executable (close running clients or stop host sessions first): {}",
            io::Error::last_os_error()
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    }
    fs::write(
        &marker,
        serde_json::to_vec_pretty(&Installed {
            publisher: INSTALL_PUBLISHER.into(),
            role: role_name(role).into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })?,
    )?;
    Ok(())
}

fn uninstall(dir: &Path, role: Role, terminate_sessions: bool, force_stop: bool) -> Result<()> {
    verify_install_owner(dir, role)?;
    let existing = installed_runtimes(dir, role)?;
    if role == Role::Host {
        stop_installed_host(&existing, terminate_sessions, force_stop)?;
        set_startup(None)?;
    }
    require_closed_runtimes(&existing)?;
    for path in existing {
        fs::remove_file(path).context("executable is in use; uninstall cancelled")?;
    }
    fs::remove_file(dir.join("installed.json"))?;
    update_path(dir, false)?;
    let key_name = format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\VsTerm.{}",
        role_name(role)
    );
    let result = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, w(&key_name).as_ptr()) };
    ensure!(
        result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND,
        "failed to remove uninstall registration: {result}"
    );
    // Only remove the exact directory if empty. Never recurse into user state or vendor dependencies.
    if fs::read_dir(dir)?.next().is_none() {
        fs::remove_dir(dir)?;
    }
    println!(
        "Removed {}. User recovery data, installer cache, and vendor dependencies were retained.",
        role_name(role)
    );
    Ok(())
}

fn register_uninstall(dir: &Path, role: Role) -> Result<()> {
    let cache = data_root()?.join("installer-cache");
    fs::create_dir_all(&cache)?;
    let cached = cache.join(format!("{}-Setup.exe", role_name(role)));
    let current = std::env::current_exe()?;
    if cached.canonicalize().ok().as_ref() != Some(&current.canonicalize()?) {
        fs::copy(&current, &cached).context("cannot cache installer for Windows Apps uninstall")?;
    }
    let key = open_key(
        &format!(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\VsTerm.{}",
            role_name(role)
        ),
        true,
    )?;
    write_string(
        &key,
        "DisplayName",
        &format!("arTerm {}", role_name(role)),
        false,
    )?;
    write_string(&key, "DisplayVersion", env!("CARGO_PKG_VERSION"), false)?;
    write_string(&key, "Publisher", INSTALL_PUBLISHER, false)?;
    write_string(&key, "InstallLocation", &dir.to_string_lossy(), false)?;
    write_string(
        &key,
        "UninstallString",
        &format!("\"{}\" /uninstall", cached.display()),
        false,
    )?;
    write_string(
        &key,
        "QuietUninstallString",
        &format!("\"{}\" /uninstall /quiet", cached.display()),
        false,
    )?;
    Ok(())
}

pub fn installer(role: Role, payload: &[u8]) -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    installer_with_args(role, payload, &args)
}

pub fn installer_with_args(role: Role, payload: &[u8], args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "/?") {
        println!("{}", installer_help(role));
        return Ok(());
    }
    let mut quiet = false;
    let mut no_download = false;
    let mut remove = false;
    let mut terminate_sessions = false;
    let mut force_stop = false;
    let mut log = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].to_ascii_lowercase().as_str() {
            "/quiet" | "--quiet" => quiet = true,
            "/no-download" | "--no-download" => no_download = true,
            "/uninstall" | "--uninstall" => remove = true,
            "/terminate-sessions" | "--terminate-sessions" => {
                ensure!(role == Role::Host, "--terminate-sessions is host-only");
                terminate_sessions = true;
            }
            "/force-stop-host" | "--force-stop-host" => {
                ensure!(role == Role::Host, "--force-stop-host is host-only");
                force_stop = true;
            }
            "/log" | "--log" => {
                i += 1;
                let path = PathBuf::from(args.get(i).context("/log requires a path")?);
                ensure!(path.is_absolute(), "/log path must be absolute");
                log = Some(OpenOptions::new().create(true).append(true).open(path)?);
            }
            other => bail!("unknown installer parameter: {other}"),
        }
        i += 1;
    }
    let dir = install_dir(role)?;
    let result = (|| -> Result<()> {
        if remove {
            return uninstall(&dir, role, terminate_sessions, force_stop);
        }
        ensure_dependency(role, None, no_download || quiet)?;
        let existing = installed_runtimes(&dir, role)?;
        if role == Role::Host && !existing.is_empty() {
            verify_install_owner(&dir, role)?;
            stop_installed_host(&existing, terminate_sessions, force_stop)?;
        }
        write_payload(&dir, role, payload)?;
        update_path(&dir, true)?;
        register_uninstall(&dir, role)?;
        println!(
            "Installed {} to {}. Existing user configuration and credentials were retained. Open a new terminal and run {} {}.",
            role_name(role),
            dir.display(),
            exe_name(role),
            if role == Role::Host && data_root()?.join("host").join("setup.json").try_exists()? {
                "start (already configured; no registration required)"
            } else if role == Role::Host { "setup --name <your-box-name>" } else { "setup" }
        );
        println!("No account was signed in and no existing vendor tunnel was changed.");
        Ok(())
    })();
    if let Some(log) = &mut log {
        writeln!(
            log,
            "{} {}: {}",
            role_name(role),
            dir.display(),
            if result.is_ok() {
                "success".into()
            } else {
                format!("{:#}", result.as_ref().unwrap_err())
            }
        )?;
    }
    result
}

fn installer_help(role: Role) -> String {
    let host = if role == Role::Host {
        "\n--terminate-sessions requests graceful shutdown of the current user/logon/data-root host, ending its sessions.\n--force-stop-host bypasses an unresponsive control pipe and force-stops only matching installed host executable paths for the current user/Windows logon. ALL sessions of those processes are lost, including hosts using other data roots. Other installations/users are not killed.\nForce-stop is never automatic; without it an unresponsive host cancels the operation."
    } else { "" };
    format!("arTerm {} installer\n--quiet --no-download --log <absolute-path> --uninstall{host}\n\
        Both --option and /option spellings are accepted.\n\
        Existing configuration and credentials are retained.\n\
        Per-user installation. Signing credentials are not bundled. Login occurs in setup, never in the installer.",
        role_name(role))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().is_ok_and(|status| status.is_none()) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }

    fn hung_fixture(path: &Path, ready: &Path, descendant: Option<&Path>, gate: Option<&Path>) -> OwnedChild {
        let mut command = Command::new(path);
        command.args(["--ignored", "--exact", "deployment::tests::force_shutdown_fixture_child"])
            .env("ARTERM_FORCE_TEST_READY", ready)
            .env_remove("ARTERM_FORCE_TEST_DESCENDANT")
            .env_remove("ARTERM_FORCE_TEST_SPAWN_GATE")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(path) = descendant {
            command.env("ARTERM_FORCE_TEST_DESCENDANT", path);
        }
        if let Some(path) = gate {
            command.env("ARTERM_FORCE_TEST_SPAWN_GATE", path);
        }
        let mut child = OwnedChild(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(child.0.try_wait().unwrap().is_none(), "owned fixture exited before ready");
            assert!(Instant::now() < deadline, "owned fixture failed to start");
            thread::sleep(Duration::from_millis(10));
        }
        child
    }

    #[test]
    #[ignore = "child of the isolated force-stop regression only"]
    fn force_shutdown_fixture_child() {
        let ready = std::env::var_os("ARTERM_FORCE_TEST_READY").expect("parent test readiness path");
        if let Some(gate) = std::env::var_os("ARTERM_FORCE_TEST_SPAWN_GATE") {
            fs::write(&ready, b"ready before spawning child").unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !Path::new(&gate).exists() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
        }
        let descendant = std::env::var_os("ARTERM_FORCE_TEST_DESCENDANT").map(|path| {
            let child_ready = PathBuf::from(&ready).with_extension("child-ready");
            let child = hung_fixture(Path::new(&path), &child_ready, None, None);
            fs::write(PathBuf::from(&ready).with_extension("child-pid"), child.0.id().to_string()).unwrap();
            child
        });
        fs::write(&ready, b"ready").unwrap();
        thread::sleep(Duration::from_secs(90));
        drop(descendant);
    }

    #[test]
    fn explicit_force_recovers_locked_install_without_killing_neighbor_or_resetting_state() {
        let root = std::env::temp_dir().join(format!("arterm-force-test-{}", uuid::Uuid::now_v7()));
        let dir = root.join("host");
        let neighbor = root.join("neighbor");
        fs::create_dir_all(&neighbor).unwrap();
        let payload = fs::read(std::env::current_exe().unwrap()).unwrap();
        write_payload(&dir, Role::Host, &payload).unwrap();
        let paths = installed_runtimes(&dir, Role::Host).unwrap();
        let other = neighbor.join(exe_name(Role::Host));
        fs::write(&other, &payload).unwrap();
        let descendant_path = root.join("owned-tunnel-fixture.exe");
        fs::write(&descendant_path, &payload).unwrap();
        let state = root.join("saved-config.json");
        fs::write(&state, b"preserved host name, paths and credentials").unwrap();
        let mut host = hung_fixture(&paths[0], &root.join("host-ready"), Some(&descendant_path), None);
        let mut alias = hung_fixture(&paths[1], &root.join("alias-ready"), None, None);
        let mut stop_helper = hung_fixture(&paths[0], &root.join("helper-ready"), None, None);
        let mut unrelated = hung_fixture(&other, &root.join("neighbor-ready"), None, None);
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE}};
        let child_pid: u32 = fs::read_to_string(root.join("host-ready.child-pid")).unwrap().parse().unwrap();
        let child_handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child_pid) };
        assert!(!child_handle.is_null());
        let child_handle = unsafe { OwnedHandle::from_raw_handle(child_handle) };
        assert!(require_closed_runtimes(&paths).is_err(), "fixture must really lock the installed image");
        assert!(write_payload(&dir, Role::Host, b"MZreplacement").is_err());
        assert!(host.0.try_wait().unwrap().is_none(), "ordinary payload update must not kill anything");
        stop_installed_host(&paths, true, true).unwrap();
        for child in [&mut host, &mut alias, &mut stop_helper] {
            assert!(child.0.try_wait().unwrap().is_some(), "matching installation process survived force-stop");
        }
        assert!(unrelated.0.try_wait().unwrap().is_none(), "same-name neighbor was killed");
        assert_eq!(unsafe { WaitForSingleObject(child_handle.as_raw_handle(), 0) }, WAIT_OBJECT_0,
            "verified owned tunnel descendant survived host force-stop");
        write_payload(&dir, Role::Host, b"MZreplacement").unwrap();
        for path in paths {
            assert_eq!(fs::read(path).unwrap(), b"MZreplacement");
        }
        assert_eq!(fs::read(&state).unwrap(), b"preserved host name, paths and credentials");
        drop((host, alias, stop_helper, unrelated));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timed_out_stop_helper_is_terminated_and_reaped() {
        let root = std::env::temp_dir().join(format!("arterm-stop-helper-test-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let mut child = hung_fixture(&std::env::current_exe().unwrap(), &root.join("ready"), None, None);
        let error = wait_for_stop_child(&mut child.0, Duration::from_millis(30)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(child.0.try_wait().unwrap().is_some(), "timed-out helper remains alive");
        drop(child);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn force_stop_catches_tunnel_spawned_after_initial_discovery() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE}};
        let root = std::env::temp_dir().join(format!("arterm-late-child-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let host_path = root.join("arterm-host.exe");
        let tunnel_path = root.join("owned-tunnel.exe");
        let payload = fs::read(std::env::current_exe().unwrap()).unwrap();
        fs::write(&host_path, &payload).unwrap();
        fs::write(&tunnel_path, &payload).unwrap();
        let ready = root.join("ready");
        let gate = root.join("spawn");
        let mut host = hung_fixture(&host_path, &ready, Some(&tunnel_path), Some(&gate));
        let mut descendant = None;
        let stopped = crate::host_shutdown::force_stop_with_hook(&[host_path], || {
            fs::write(&gate, b"go")?;
            let deadline = Instant::now() + Duration::from_secs(10);
            let pid_file = ready.with_extension("child-pid");
            while !pid_file.exists() {
                ensure!(Instant::now() < deadline, "late descendant did not start");
                thread::sleep(Duration::from_millis(5));
            }
            let pid: u32 = fs::read_to_string(pid_file)?.parse()?;
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            ensure!(!handle.is_null(), "late descendant exited unexpectedly");
            descendant = Some(unsafe { OwnedHandle::from_raw_handle(handle) });
            Ok(())
        }).unwrap();
        assert_eq!(stopped, 2);
        assert!(host.0.try_wait().unwrap().is_some());
        assert_eq!(unsafe { WaitForSingleObject(descendant.as_ref().unwrap().as_raw_handle(), 0) }, WAIT_OBJECT_0);
        drop((host, descendant));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn graceful_shutdown_wait_is_bounded_and_propagates_failures() {
        let mut polls = 0;
        wait_for_shutdown(Duration::from_secs(1), || {
            polls += 1;
            Ok(polls == 2)
        }).unwrap();
        assert_eq!(polls, 2);
        let error = wait_for_shutdown(Duration::ZERO, || Ok(false)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let error = wait_for_shutdown(Duration::from_secs(1), || bail!("mock refused live sessions")).unwrap_err();
        assert!(error.to_string().contains("mock refused live sessions"));
        wait_for_shutdown(Duration::ZERO, || Ok(true)).unwrap();
    }

    #[test]
    fn installer_help_documents_actual_host_only_switch_and_scope() {
        let help = installer_help(Role::Host);
        assert!(help.contains("--terminate-sessions"));
        assert!(help.contains("--force-stop-host"));
        assert!(help.contains("Both --option and /option"));
        assert!(help.contains("current user/logon/data-root"));
        assert!(help.contains("Force-stop is never automatic"));
        assert!(!installer_help(Role::Client).contains("--terminate-sessions"));
        assert!(!installer_help(Role::Client).contains("--force-stop-host"));
    }

    #[test]
    fn host_shutdown_arguments_require_explicit_termination() {
        assert_eq!(host_stop_args(false), ["stop"]);
        assert_eq!(host_stop_args(true), ["stop", "--terminate-sessions"]);
        assert!(installer_with_args(Role::Client, b"MZtest", &["/terminate-sessions".into()]).is_err());
        for switch in ["/terminate-sessions", "/TERMINATE-SESSIONS", "--terminate-sessions",
            "/force-stop-host", "--force-stop-host", "--FORCE-STOP-HOST"] {
            let error = installer_with_args(Role::Host, b"MZtest",
                &[switch.into(), "/invalid-before-any-install".into()]).unwrap_err();
            assert!(error.to_string().contains("unknown installer parameter: /invalid-before-any-install"));
        }
        assert!(installer_with_args(Role::Client, b"MZtest", &["--force-stop-host".into()]).is_err());
    }

    #[test]
    fn repeated_host_payload_upgrade_preserves_all_adjacent_state() {
        let root = std::env::temp_dir().join(format!("arterm-upgrade-preserve-{}", uuid::Uuid::now_v7()));
        let files = [
            r"host\setup.json", r"code-cli\code_tunnel.json", r"code-cli\token.json",
            r"client\config.json", r"client\sessions\target-id\saved.dpapi",
            r"client\sessions\target-id\ref-work.json", r"host\requests\saved.dpapi", r"identity.cer",
        ];
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file.as_bytes()).unwrap();
        }
        for payload in [b"MZinitial".as_slice(), b"MZupgrade", b"MZupgrade"] {
            write_payload(&root, Role::Host, payload).unwrap();
            for file in files {
                assert_eq!(fs::read(root.join(file)).unwrap(), file.as_bytes());
            }
            assert_eq!(fs::read(root.join(exe_name(Role::Host))).unwrap(), payload);
            assert_eq!(fs::read(root.join(legacy_exe_name(Role::Host))).unwrap(), payload);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn install_owners_accept_only_exact_current_or_legacy_publisher_and_role() {
        let root = std::env::temp_dir().join(format!("arterm-owner-test-{}", uuid::Uuid::now_v7()));
        write_payload(&root, Role::Client, b"MZoriginal").unwrap();
        for (publisher, role, accepted) in [
            (INSTALL_PUBLISHER, "Client", true),
            (VSTERM_INSTALL_PUBLISHER, "Client", true),
            (LEGACY_INSTALL_PUBLISHER, "Client", true),
            ("unrelated", "Client", false),
            ("arterm", "Client", false),
            (INSTALL_PUBLISHER, "Host", false),
            (LEGACY_INSTALL_PUBLISHER, "Host", false),
        ] {
            let marker = serde_json::to_vec(&Installed {
                publisher: publisher.into(),
                role: role.into(),
                version: "0.3.0".into(),
            }).unwrap();
            fs::write(root.join("installed.json"), &marker).unwrap();
            let before = fs::read(root.join("arterm.exe")).unwrap();
            assert_eq!(verify_install_owner(&root, Role::Client).is_ok(), accepted);
            assert_eq!(write_payload(&root, Role::Client, b"MZupdated").is_ok(), accepted);
            if accepted {
                let installed: Installed = serde_json::from_slice(
                    &fs::read(root.join("installed.json")).unwrap()).unwrap();
                assert_eq!(installed.publisher, INSTALL_PUBLISHER);
                assert_eq!(installed.version, env!("CARGO_PKG_VERSION"));
            } else {
                assert_eq!(fs::read(root.join("installed.json")).unwrap(), marker);
                assert_eq!(fs::read(root.join("arterm.exe")).unwrap(), before);
            }
        }
        fs::remove_file(root.join("arterm.exe")).unwrap();
        fs::remove_file(root.join("vsterm.exe")).unwrap();
        assert!(write_payload(&root, Role::Client, b"MZorphan-marker").is_err());
        fs::remove_file(root.join("installed.json")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn payload_ownership_update_and_non_executable_rejection() {
        let root =
            std::env::temp_dir().join(format!("devbox-installer-test-{}", uuid::Uuid::now_v7()));
        assert!(write_payload(&root, Role::Client, b"not an executable").is_err());
        write_payload(&root, Role::Client, b"MZtest-payload-one").unwrap();
        write_payload(&root, Role::Client, b"MZtest-payload-two").unwrap();
        assert_eq!(
            fs::read(root.join("arterm.exe")).unwrap(),
            b"MZtest-payload-two"
        );
        fs::remove_file(root.join("installed.json")).unwrap();
        assert!(write_payload(&root, Role::Client, b"MZunowned-overwrite").is_err());
        fs::remove_file(root.join("arterm.exe")).unwrap();
        fs::remove_file(root.join("vsterm.exe")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn unsigned_dependency_is_rejected_before_execution() {
        let path =
            std::env::temp_dir().join(format!("devbox-unsigned-{}.exe", uuid::Uuid::now_v7()));
        fs::write(&path, b"MZnot-a-signed-vendor-executable").unwrap();
        assert!(validate_dependency(&path, Role::Client).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "read-only smoke check against the locally installed Microsoft dependencies"]
    fn installed_vendor_dependencies_pass_native_verification() {
        ensure_dependency(Role::Client, None, true).unwrap();
        ensure_dependency(Role::Host, None, true).unwrap();
    }
}
