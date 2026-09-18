//! Explicit private-CI gate. Select the exact ignored test, not every child test.
//! The test binary may use the debug fixture feature; all SIGNED_* and
//! UNSIGNED_CLIENT product artifacts must be built WITHOUT that feature.
//! No signing, key access, certificate imports, or trust modification occurs here.
#[path = "resume_e2e.rs"]
mod native;

use arterm::{
    local_control::{Identity, Operation, Owner, Request},
    peer_auth,
};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows_sys::Win32::{Foundation::*, Storage::FileSystem::*, System::Pipes::*};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("arterm-signed-gate-{}", Uuid::now_v7()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove exact owned signed-test directory");
    }
}
fn artifact(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("required CI artifact selector: {name}")),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "{name} must be an existing absolute artifact path"
    );
    path
}
fn cli(exe: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(exe)
        .env("VSTERM_REMOTE_HOME", home)
        .args(args)
        .output()
        .unwrap()
}
fn active_identity(home: &Path) -> Identity {
    let path = fs::read_dir(home.join("client").join("active"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .expect("actual signed owner published no discovery record");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn raw_open(identity: &Identity) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
        .open(&identity.pipe)
        .unwrap()
}
fn rejected_raw_client(identity: &Identity) {
    let mut pipe = raw_open(identity);
    let request = Request {
        version: identity.version,
        operation_id: Uuid::now_v7(),
        instance_id: identity.instance_id,
        scope_id: identity.scope_id.clone(),
        target_id: identity.target_id.clone(),
        session_id: identity.session_id,
        action: Operation::Send {
            command: "$GateProof=999".into(),
            timeout_ms: None,
        },
    };
    let bytes = serde_json::to_vec(&request).unwrap();
    let mut frame = (bytes.len() as u32).to_le_bytes().to_vec();
    frame.extend(bytes);
    // Rejection may race the write. Either outcome must produce no RPC reply.
    let _ = pipe.write_all(&frame);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut available = 0;
        let ok = unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = std::io::Error::last_os_error().raw_os_error();
            assert!(
                matches!(error, Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32),
                "unexpected pipe failure: {error:?}"
            );
            break;
        }
        assert_eq!(
            available, 0,
            "signed owner replied to unsigned arbitrary-process caller"
        );
        assert!(
            Instant::now() < deadline,
            "signed owner did not reject unsigned caller"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn reject_fake_owner(signed_client: &Path, signed_host: &Path) {
    let home = Temp::new();
    assert!(cli(
        signed_client,
        &home.0,
        &[
            "add",
            "fixture",
            "--tunnel",
            "fixture",
            "--host-path",
            signed_host.to_str().unwrap()
        ]
    )
    .status
    .success());
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(home.0.join("client").join("config.json")).unwrap())
            .unwrap();
    // Fixture API is used ONLY to construct discovery metadata for a fake owner.
    let owner = Owner::start(
        &home.0,
        config["targets"]["fixture"]["target_id"].as_str().unwrap(),
        "fixture",
        Uuid::now_v7(),
        Some("fake".into()),
    )
    .unwrap();
    let identity = owner.identity.clone();
    drop(owner);
    let name = identity
        .pipe
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(handle, INVALID_HANDLE_VALUE);
    let pipe = unsafe { File::from_raw_handle(handle) };
    unsafe {
        ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut());
    }
    fs::write(
        home.0
            .join("client")
            .join("active")
            .join(format!("{}.json", identity.instance_id)),
        serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let observer = thread::spawn(move || {
        let mut maximum = 0;
        while !stopped.load(Ordering::Acquire) {
            let mut available = 0;
            unsafe {
                PeekNamedPipe(
                    pipe.as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                );
            }
            maximum = maximum.max(available);
            thread::sleep(Duration::from_millis(2));
        }
        maximum
    });
    let output = cli(
        signed_client,
        &home.0,
        &["read", "fixture", "fake", "--json"],
    );
    stop.store(true, Ordering::Release);
    let written = observer.join().unwrap();
    assert!(
        !output.status.success(),
        "signed controller trusted unsigned fake owner"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("authenticate local pipe owner"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        written, 0,
        "controller sent application bytes before authenticating owner"
    );
}

#[test]
#[ignore = "requires debug fixture feature, trusted signed production artifacts, and unsigned production artifact"]
fn actual_signed_product_ipc() {
    assert!(cfg!(all(feature = "test-unsigned-ipc", debug_assertions)),
        "build only the TEST harness with --features test-unsigned-ipc; product artifacts must not use it");
    let signed_client = artifact("SIGNED_CLIENT");
    let signed_host = artifact("SIGNED_HOST");
    let signed_installer = artifact("SIGNED_INSTALLER");
    let unsigned_client = artifact("UNSIGNED_CLIENT");
    let client_image = peer_auth::verify_image(&signed_client)
        .expect("OS trust and pinned certificate are mandatory");
    let host_image = peer_auth::verify_image(&signed_host).expect("host signature prerequisite");
    let installer_image =
        peer_auth::verify_image(&signed_installer).expect("installer signature prerequisite");
    assert_eq!(
        peer_auth::verify_same_client_build(&client_image, &host_image)
            .unwrap_err()
            .reason,
        peer_auth::Rejection::DifferentClientBuild
    );
    assert_eq!(
        peer_auth::verify_same_client_build(&client_image, &installer_image)
            .unwrap_err()
            .reason,
        peer_auth::Rejection::DifferentClientBuild
    );
    let unsigned_error = match peer_auth::verify_image(&unsigned_client) {
        Ok(_) => panic!("UNSIGNED_CLIENT must be genuinely unsigned"),
        Err(error) => error,
    };
    assert_eq!(
        unsigned_error.reason,
        peer_auth::Rejection::UntrustedSignature
    );
    assert_eq!(
        unsigned_error.status,
        Some(0x800b0100),
        "require an unsigned original, never execute a tampered image"
    );
    let copies = Temp::new();
    let alias = copies.0.join("vsterm.exe");
    fs::copy(&signed_client, &alias).unwrap();
    let alias_image = peer_auth::verify_image(&alias).unwrap();
    peer_auth::verify_same_client_build(&client_image, &alias_image).unwrap();
    std::env::set_var("SIGNED_CONTROLLER", &alias);
    native::headless_connection_supports_cross_cwd_read_list_and_detach();
    std::env::remove_var("SIGNED_CONTROLLER");
    drop(alias_image);

    let fixture = native::Fixture::new();
    let (address, _connections) = native::relay(fixture.home.clone(), 1);
    let mut owner = fixture.client("connect", Some("Signed"), &address);
    owner.command("$GateProof=0; Write-Output ('SIGNED-READY='+($GateProof -eq 0))");
    owner.wait_for(|out, _| out.contains("SIGNED-READY=True"));
    let identity = active_identity(&fixture.home);
    rejected_raw_client(&identity);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let listed = cli(
            &signed_client,
            &fixture.home,
            &["list", "--client", "--json"],
        );
        assert!(
            listed.status.success(),
            "{}",
            String::from_utf8_lossy(&listed.stderr)
        );
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        if listed[0]["shell_status"] == "ready" {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    let proof = cli(
        &signed_client,
        &fixture.home,
        &[
            "send",
            "fixture",
            "signed",
            "--command",
            "Write-Output ('GATE-UNCHANGED='+($GateProof -eq 0))",
            "--wait",
            "--timeout",
            "10s",
            "--json",
        ],
    );
    assert!(
        proof.status.success(),
        "{} {}",
        String::from_utf8_lossy(&proof.stdout),
        String::from_utf8_lossy(&proof.stderr)
    );
    let read = cli(
        &signed_client,
        &fixture.home,
        &["read", "fixture", "signed", "--json"],
    );
    assert!(String::from_utf8_lossy(&read.stdout).contains("GATE-UNCHANGED=True"));

    let unsigned_owner = copies.0.join("unsigned-owner.exe");
    let unsigned_controller = copies.0.join("unsigned-controller.exe");
    fs::copy(&unsigned_client, &unsigned_owner).unwrap();
    fs::copy(&unsigned_client, &unsigned_controller).unwrap();
    assert_eq!(
        fs::read(&unsigned_owner).unwrap(),
        fs::read(&unsigned_controller).unwrap()
    );
    for (exe, args) in [
        (
            &unsigned_owner,
            vec![
                "connect",
                "fixture",
                "unsigned",
                "--address",
                &address,
                "--retries",
                "0",
            ],
        ),
        (
            &unsigned_controller,
            vec!["read", "fixture", "signed", "--json"],
        ),
    ] {
        let output = cli(exe, &fixture.home, &args);
        assert!(
            !output.status.success(),
            "unsigned production client accepted"
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("authenticate local client program")
                && error.contains("UntrustedSignature"),
            "must reject the certificate, not just a different image: {error}"
        );
    }
    owner.detach();
    reject_fake_owner(&signed_client, &signed_host);

    let tampered = copies.0.join("never-executed-tampered.exe");
    let mut bytes = fs::read(&signed_client).unwrap();
    assert!(bytes.len() > 4096);
    bytes[4096] ^= 1;
    fs::write(&tampered, bytes).unwrap();
    let rejected = match peer_auth::verify_image(&tampered) {
        Ok(_) => panic!("tampered file accepted"),
        Err(error) => error,
    };
    assert_eq!(rejected.reason, peer_auth::Rejection::UntrustedSignature);
}
