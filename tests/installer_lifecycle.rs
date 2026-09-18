//! Native installer path exercised with redirected per-process HKCU and temporary directories.
use arterm::deployment::{self, Role};
use std::{ffi::OsString, fs, path::PathBuf};
use windows_sys::Win32::{Foundation::ERROR_SUCCESS, System::Registry::*};

#[cfg(feature = "installers")]
#[test]
fn setup_help_uses_neutral_product_branding_without_installing() {
    for (exe, expected) in [
        (env!("CARGO_BIN_EXE_arTerm-Client-Setup"), "arTerm Client installer"),
        (env!("CARGO_BIN_EXE_arTerm-Host-Setup"), "arTerm Host installer"),
    ] {
        let output = std::process::Command::new(exe).arg("--help").output().unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.lines().next(), Some(expected));
        assert!(output.stderr.is_empty());
    }
}

struct Sandbox {
    home: PathBuf,
    registry_path: Vec<u16>,
    key: HKEY,
    vars: Vec<(&'static str, Option<OsString>)>,
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn uninstall_value(role: &str, name: &str) -> String {
    let path = wide(&format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\VsTerm.{role}"
    ));
    let mut buffer = [0u16; 256];
    let mut bytes = std::mem::size_of_val(&buffer) as u32;
    unsafe {
        assert_eq!(RegGetValueW(
            HKEY_CURRENT_USER, path.as_ptr(), wide(name).as_ptr(), RRF_RT_REG_SZ,
            std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut bytes,
        ), ERROR_SUCCESS);
    }
    String::from_utf16(&buffer[..bytes as usize / 2 - 1]).unwrap()
}
fn assert_branding(dir: &std::path::Path, role: &str) {
    let marker: serde_json::Value = serde_json::from_slice(
        &fs::read(dir.join("installed.json")).unwrap()).unwrap();
    assert_eq!(marker["publisher"], "arTerm");
    assert_eq!(marker["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(uninstall_value(role, "DisplayName"), format!("arTerm {role}"));
    assert_eq!(uninstall_value(role, "Publisher"), "arTerm");
    assert_eq!(uninstall_value(role, "DisplayVersion"), env!("CARGO_PKG_VERSION"));
}
fn use_legacy_marker(dir: &std::path::Path, publisher: &str) {
    let path = dir.join("installed.json");
    let mut marker: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    marker["publisher"] = publisher.into();
    marker["version"] = "0.3.0".into();
    fs::write(path, serde_json::to_vec(&marker).unwrap()).unwrap();
}
impl Sandbox {
    fn new(dependencies: &[PathBuf]) -> Self {
        let id = uuid::Uuid::now_v7();
        let home = std::env::temp_dir().join(format!("devbox-install-e2e-{id}"));
        fs::create_dir_all(&home).unwrap();
        let registry_path = wide(&format!("Software\\VsTerm\\InstallerTests\\{id}"));
        let mut key = std::ptr::null_mut();
        unsafe {
            assert_eq!(
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    registry_path.as_ptr(),
                    0,
                    std::ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_ALL_ACCESS,
                    std::ptr::null(),
                    &mut key,
                    std::ptr::null_mut()
                ),
                ERROR_SUCCESS
            );
            assert_eq!(RegOverridePredefKey(HKEY_CURRENT_USER, key), ERROR_SUCCESS);
        }
        let vars = vec![
            ("LOCALAPPDATA", std::env::var_os("LOCALAPPDATA")),
            ("VSTERM_REMOTE_HOME", std::env::var_os("VSTERM_REMOTE_HOME")),
            ("PATH", std::env::var_os("PATH")),
        ];
        let mut paths: Vec<PathBuf> = dependencies
            .iter()
            .map(|p| p.parent().unwrap().to_owned())
            .collect();
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::set_var("LOCALAPPDATA", &home);
        std::env::set_var("VSTERM_REMOTE_HOME", home.join("data"));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        Self {
            home,
            registry_path,
            key,
            vars,
        }
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        for (key, value) in &self.vars {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        unsafe {
            RegOverridePredefKey(HKEY_CURRENT_USER, std::ptr::null_mut());
            RegCloseKey(self.key);
            RegDeleteTreeW(HKEY_CURRENT_USER, self.registry_path.as_ptr());
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

#[test]
#[ignore = "requires installed signed Microsoft host dependency; isolated legacy host and HKCU"]
fn live_legacy_host_blocks_upgrade_and_uninstall_without_replacing_files() {
    use arterm::wire::{self, binary, get, map, message, s, text};
    use rmpv::Value;
    use std::{io::{Read, Write}, process::{Command, Stdio}, thread, time::{Duration, Instant}};
    struct OwnedProcess(std::process::Child);
    impl Drop for OwnedProcess {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    fn receive(reader: &mut impl Read) -> Value {
        let mut length = [0; 4];
        reader.read_exact(&mut length).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
        reader.read_exact(&mut bytes).unwrap();
        rmpv::decode::read_value(&mut &bytes[..]).unwrap()
    }
    let dependency = deployment::ensure_dependency(Role::Host, None, true).unwrap();
    let _sandbox = Sandbox::new(&[dependency]);
    let dir = deployment::install_dir(Role::Host).unwrap();
    let payload = fs::read(env!("CARGO_BIN_EXE_arterm-host")).unwrap();
    deployment::write_payload(&dir, Role::Host, &payload).unwrap();
    use_legacy_marker(&dir, "VsTerm");
    fs::remove_file(dir.join(deployment::exe_name(Role::Host))).unwrap();
    let legacy = dir.join(deployment::legacy_exe_name(Role::Host));
    let mut host = OwnedProcess(Command::new(&legacy).arg("run")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(host.0.try_wait().unwrap().is_none());
        if Command::new(&legacy).arg("sessions").output().unwrap().status.success() { break; }
        assert!(Instant::now() < deadline, "isolated legacy host did not start");
        thread::sleep(Duration::from_millis(50));
    }
    let mut bridge = OwnedProcess(Command::new(&legacy)
        .args(["bridge", "--protocol", "vsterm-session-v1"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap());
    let mut input = bridge.0.stdin.take().unwrap();
    let mut output = bridge.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for _ in 0..2 {
            if tx.send(receive(&mut output)).is_err() { break; }
        }
    });
    input.write_all(&wire::encode(&message("Hello", map(vec![
        ("min_version", 1.into()), ("max_version", 1.into()),
        ("client_instance_id", Value::Binary(vec![1; 16])),
    ]))).unwrap()).unwrap();
    let hello = rx.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_eq!(text(&hello, "type").unwrap(), "HelloOk");
    let broker = binary(get(&hello, "body").unwrap(), "broker_instance_id").unwrap();
    let id = uuid::Uuid::now_v7();
    input.write_all(&wire::encode(&message("CreateSession", map(vec![
        ("request_id", Value::Binary(uuid::Uuid::now_v7().as_bytes().to_vec())),
        ("requested_session_id", s(&id.to_string())),
        ("create_claim", Value::Binary(vec![2; 32])),
        ("origin_broker_instance_id", Value::Binary(broker)),
        ("connection_epoch", 1.into()), ("shell", s("powershell.exe")),
        ("args", Value::Array(vec![s("-NoLogo"), s("-NoProfile")])),
        ("cwd", Value::Nil), ("cols", 80.into()), ("rows", 24.into()),
        ("after_output_seq", 0.into()),
    ]))).unwrap()).unwrap();
    assert_eq!(text(&rx.recv_timeout(Duration::from_secs(15)).unwrap(), "type").unwrap(), "SessionCreated");
    let marker = fs::read(dir.join("installed.json")).unwrap();
    for args in [
        vec!["/quiet".into(), "/no-download".into()],
        vec!["/quiet".into(), "/uninstall".into()],
    ] {
        let error = deployment::installer_with_args(Role::Host, &payload, &args).unwrap_err();
        assert!(error.to_string().contains("live sessions"), "{error:#}");
        assert!(host.0.try_wait().unwrap().is_none());
        assert!(!dir.join(deployment::exe_name(Role::Host)).exists());
        assert_eq!(fs::read(&legacy).unwrap(), payload);
        assert_eq!(fs::read(dir.join("installed.json")).unwrap(), marker);
        let status = Command::new(&legacy).args(["sessions", "--json"]).output().unwrap();
        let sessions: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert!(sessions.as_array().unwrap().iter().any(|v| v["id"] == id.to_string() && v["exited"] == false));
    }
}

#[test]
#[ignore = "requires installed signed Microsoft dependencies; uses isolated HKCU and files, no sign-in"]
fn both_roles_install_upgrade_and_uninstall_without_touching_real_profile() {
    let dependencies = [
        deployment::ensure_dependency(Role::Client, None, true).unwrap(),
        deployment::ensure_dependency(Role::Host, None, true).unwrap(),
    ];
    let _sandbox = Sandbox::new(&dependencies);
    let retained = deployment::data_root().unwrap().join("client");
    fs::create_dir_all(retained.join("sessions")).unwrap();
    fs::write(retained.join("config.json"), b"unchanged legacy target configuration").unwrap();
    fs::write(retained.join("sessions").join("retained.dpapi"), b"unchanged protected record").unwrap();
    for (role, source) in [
        (Role::Client, env!("CARGO_BIN_EXE_arterm")),
        (Role::Host, env!("CARGO_BIN_EXE_arterm-host")),
    ] {
        let payload = fs::read(source).unwrap();
        let args = vec!["/quiet".into(), "/no-download".into()];
        deployment::installer_with_args(role, &payload, &args).unwrap();
        let dir = deployment::install_dir(role).unwrap();
        assert_eq!(
            fs::read(dir.join(deployment::exe_name(role))).unwrap(),
            payload
        );
        assert_eq!(fs::read(dir.join(deployment::legacy_exe_name(role))).unwrap(), payload);
        assert!(dir.join("installed.json").exists());
        let role_name = if role == Role::Client { "Client" } else { "Host" };
        assert_branding(&dir, role_name);
        use_legacy_marker(&dir, "VsTerm");
        fs::remove_file(dir.join(deployment::exe_name(role))).unwrap();
        deployment::installer_with_args(role, &payload, &args).unwrap();
        assert_branding(&dir, role_name);
        assert_eq!(fs::read(dir.join(deployment::legacy_exe_name(role))).unwrap(), payload);
        deployment::installer_with_args(role, &payload, &["/uninstall".into(), "/quiet".into()])
            .unwrap();
        assert!(!dir.join(deployment::exe_name(role)).exists());
        assert!(!dir.join("installed.json").exists());
        assert!(!dir.join(deployment::legacy_exe_name(role)).exists());
        assert_eq!(fs::read(retained.join("config.json")).unwrap(), b"unchanged legacy target configuration");
        assert_eq!(fs::read(retained.join("sessions").join("retained.dpapi")).unwrap(), b"unchanged protected record");
        deployment::installer_with_args(role, &payload, &args).unwrap();
        use_legacy_marker(&dir, "yeelam-gordon");
        fs::remove_file(dir.join(deployment::exe_name(role))).unwrap();
        deployment::installer_with_args(role, &payload, &["/uninstall".into(), "/quiet".into()])
            .unwrap();
        assert!(!dir.join(deployment::exe_name(role)).exists());
        assert!(!dir.join("installed.json").exists());
        assert!(!dir.join(deployment::legacy_exe_name(role)).exists());
    }
}
