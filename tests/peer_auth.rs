#![cfg(windows)]
#[path = "../src/peer_auth.rs"]
mod peer_auth;

use peer_auth::{verify_image, ClientIdentity, Rejection};

#[test]
fn unsigned_test_program_is_not_an_authenticated_client() {
    let path = std::env::current_exe().unwrap();
    let error = match verify_image(&path) {
        Ok(_) => panic!("this test requires an unsigned test harness"),
        Err(error) => error,
    };
    assert_eq!(error.reason, Rejection::UntrustedSignature);
    assert!(ClientIdentity::current().is_err());
}

#[test]
fn missing_image_fails_explicitly() {
    let absent =
        std::env::temp_dir().join(format!("arterm-absent-peer-{}.exe", std::process::id()));
    assert!(!absent.exists());
    let error = match verify_image(&absent) {
        Ok(_) => panic!("nonexistent image accepted"),
        Err(error) => error,
    };
    assert_eq!(error.reason, Rejection::Os);
    assert!(error.status.is_some());
}

#[test]
#[ignore = "requires ARTERM_PUBLIC_SIGNED_FIXTURES containing public EXEs only"]
fn public_signed_fixture_and_nonexecuted_tampered_copy() {
    use std::{fs, io::Write, time::SystemTime};
    let root = std::path::PathBuf::from(
        std::env::var_os("ARTERM_PUBLIC_SIGNED_FIXTURES")
            .expect("provide a read-only public fixture directory"),
    );
    let source = root.join("arterm.exe");
    match verify_image(&source) {
        Ok(image) => {
            assert_ne!(image.sha256(), [0; 32]);
            peer_auth::verify_same_client_build(&image, &image).unwrap();
            eprintln!("public fixture: OS trusted and exact signer pin verified");
        }
        Err(error) => {
            assert_eq!(error.reason, Rejection::RootNotTrusted);
            assert_eq!(error.status, Some(0x800b0109), "{error}");
            eprintln!("public fixture: correctly rejected CERT_E_UNTRUSTEDROOT");
        }
    }
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "arterm-tampered-{}-{nonce}.exe",
        std::process::id()
    ));
    let mut bytes = fs::read(source).unwrap();
    assert!(bytes.len() > 4096);
    bytes[4096] ^= 1;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    file.write_all(&bytes).unwrap();
    drop(file);
    let result = verify_image(&path);
    fs::remove_file(&path).expect("remove exact non-executed tampered fixture");
    let error = match result {
        Ok(_) => panic!("tampered PE accepted"),
        Err(error) => error,
    };
    assert_eq!(error.reason, Rejection::UntrustedSignature);
    assert_eq!(error.status, Some(0x80096010), "{error}");
}

#[test]
#[ignore = "requires an existing OS-trusted public wrong-certificate signed PE"]
fn trusted_wrong_certificate_file_is_rejected() {
    let path = std::path::PathBuf::from(
        std::env::var_os("ARTERM_WRONG_CERT_SIGNED_FILE")
            .expect("provide a public signed PE; no key or certificate generation"),
    );
    let error = match verify_image(&path) {
        Ok(_) => panic!("wrong signing certificate accepted"),
        Err(error) => error,
    };
    assert_eq!(error.reason, Rejection::WrongCertificate, "{error}");
}

// Product acceptance is tests/signed_ipc_e2e.rs::actual_signed_product_ipc.
// That harness is unsigned and launches signed production artifacts; it needs no
// helper signing. The older optional signed-harness diagnostics below are not
// a substitute for product acceptance.
// Optional diagnostic recipe (all env inputs below exist ONLY in this test executable):
// 1. Build this test executable, then sign it using the EXISTING project signing
//    workflow/key. Use a disposable runner with separately provisioned OS chain
//    trust. This test never imports certificates. Run the exact ignored
//    signed_two_process_pipe_roundtrip test; it exercises the production module
//    unmodified in two real processes, in both pipe roles, with revalidation.
// 2. Set ARTERM_PEER_TEST_CHILD to a byte-identical copy named vsterm.exe and
//    repeat. Then use a separately built/signed copy of this SAME harness for
//    DifferentClientBuild, an unsigned copy for UntrustedSignature, and an
//    independently OS-trusted wrong-cert harness for WrongCertificate. Set
//    ARTERM_PEER_EXPECT_REJECTION to the corresponding variant name. Never
//    execute a tampered file: use the file-only tamper test above.
// 3. Run the module's exact ignored signed_file_role_binding... test with the
//    signed runtime fixtures for native same-cert host/installer rejection.
// 4. Root's product E2Es must separately drive the signed production client CLI
//    against real sessions. SIGNED_CLIENT/SIGNED_HOST may select artifact paths
//    ONLY inside that test harness; the actual programs never inspect these env
//    vars for authorization. Both release artifacts MUST be compiled WITHOUT
//    test-unsigned-ipc. After existing-key signing, CI may temporarily trust the
//    PUBLIC CER only on its disposable runner and must tear that runner down.
//    A signed TEST harness is not an actual arterm client/host product E2E.
//    Cover cross-user/logon, cross-elevation, restricted/AppContainer, endpoint
//    churn, elevated/elevated success, and cross-version rejection there.
// Without OS trust, require RootNotTrusted, never silently skip a positive test.
// For unsigned local product E2Es, core must explicitly select test_fixture::
// FixtureIdentity at COMPILE TIME only in debug test-unsigned-ipc builds. The
// feature defaults OFF; without debug_assertions it is a compile error.
// ClientIdentity/verify_image remain strict even with the feature enabled.
// No runtime/env/CLI switch selects this policy.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsHandle, AsRawHandle, FromRawHandle},
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime},
};
use windows_sys::Win32::{Foundation::*, Storage::FileSystem::*, System::Pipes::*};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().expect("query owned test child").is_none() {
            self.0.kill().expect("terminate only owned test child");
        }
        self.0.wait().expect("reap owned test child");
    }
}

fn receive_byte(pipe: &File) -> u8 {
    let deadline = Instant::now() + Duration::from_secs(15);
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
        assert_ne!(ok, 0, "peek pipe: {}", std::io::Error::last_os_error());
        if available != 0 {
            let mut byte = [0];
            (&*pipe).read_exact(&mut byte).unwrap();
            return byte[0];
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for owned pipe"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "strict signed two-process harness; requires existing OS chain trust"]
fn signed_two_process_pipe_roundtrip() {
    let owner = ClientIdentity::current().unwrap_or_else(|e| panic!("signed CI prerequisite: {e}"));
    let child_path = std::env::var_os("ARTERM_PEER_TEST_CHILD")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_exe().unwrap());
    let (server, mut child) = connect_owned_child(&child_path, "signed_pipe_child");
    let auth = owner.authenticate(server.as_handle(), peer_auth::PeerEnd::Client);
    if let Some(expected) = std::env::var_os("ARTERM_PEER_EXPECT_REJECTION") {
        let error = match auth {
            Ok(_) => panic!("unexpected peer acceptance"),
            Err(e) => e,
        };
        assert_eq!(format!("{:?}", error.reason), expected.to_string_lossy());
        (&server).write_all(&[0]).unwrap();
    } else {
        let peer = auth.unwrap_or_else(|e| panic!("strict peer acceptance failed: {e}"));
        assert_eq!(peer.pid(), child.0.id());
        assert!(peer.creation_time() > 0);
        peer.revalidate().unwrap();
        (&server).write_all(&[1]).unwrap();
        assert_eq!(receive_byte(&server), b'A');
        peer.revalidate().unwrap();
        (&server).write_all(b"Q").unwrap();
        assert_eq!(receive_byte(&server), b'Q');
        peer.revalidate().unwrap();
        (&server).write_all(b"X").unwrap();
    }
    finish_child(&mut child);
}

fn connect_owned_child(child_path: &std::path::Path, entry: &str) -> (File, OwnedChild) {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!(
        r"\\.\pipe\arterm-peer-auth-e2e-{}-{nonce}",
        std::process::id()
    );
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(handle, INVALID_HANDLE_VALUE, "create owned test pipe");
    let server = unsafe { File::from_raw_handle(handle) };
    let listening = unsafe { ConnectNamedPipe(server.as_raw_handle(), std::ptr::null_mut()) };
    assert!(listening != 0 || unsafe { GetLastError() } == ERROR_PIPE_LISTENING);
    let mut child = OwnedChild(
        Command::new(child_path)
            .args(["--exact", entry, "--ignored", "--nocapture"])
            .env("ARTERM_PEER_TEST_PIPE", &name)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut pid = 0;
        let connected = unsafe { GetNamedPipeClientProcessId(server.as_raw_handle(), &mut pid) };
        let error = unsafe { GetLastError() };
        if connected != 0 {
            break;
        }
        assert!(
            matches!(
                error,
                ERROR_PIPE_LISTENING | ERROR_PIPE_NOT_CONNECTED | ERROR_NOT_FOUND
            ),
            "connect owned pipe: {error}"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "owned child exited before connecting"
        );
        assert!(Instant::now() < deadline, "owned pipe connect timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(receive_byte(&server), b'R');
    (server, child)
}

fn finish_child(child: &mut OwnedChild) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "owned child exit timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(child.0.wait().unwrap().success());
}

#[test]
#[ignore = "owned signed/negative fixture child; invoked only by parent test"]
fn signed_pipe_child() {
    let pipe = open_child_pipe();
    (&pipe).write_all(b"R").unwrap();
    if receive_byte(&pipe) == 0 {
        return;
    }
    let owner = ClientIdentity::current().unwrap();
    let peer = owner
        .authenticate(pipe.as_handle(), peer_auth::PeerEnd::Server)
        .unwrap();
    peer.revalidate().unwrap();
    (&pipe).write_all(b"A").unwrap();
    assert_eq!(receive_byte(&pipe), b'Q');
    peer.revalidate().unwrap();
    (&pipe).write_all(b"Q").unwrap();
    assert_eq!(receive_byte(&pipe), b'X');
}

fn open_child_pipe() -> File {
    let name = std::env::var_os("ARTERM_PEER_TEST_PIPE").expect("owned pipe name");
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
        .open(name)
        .unwrap()
}

#[test]
fn unsigned_fixture_two_process_roundtrip_does_not_weaken_production() {
    let owner = peer_auth::test_fixture::FixtureIdentity::current().unwrap();
    assert!(ClientIdentity::current().is_err());
    assert!(verify_image(&std::env::current_exe().unwrap()).is_err());
    let (server, mut child) = connect_owned_child(
        &std::env::current_exe().unwrap(),
        "unsigned_fixture_pipe_child",
    );
    let peer = owner
        .authenticate(server.as_handle(), peer_auth::PeerEnd::Client)
        .unwrap();
    assert_eq!(peer.pid(), child.0.id());
    assert!(peer.creation_time() > 0);
    peer.revalidate().unwrap();
    (&server).write_all(&[1]).unwrap();
    assert_eq!(receive_byte(&server), b'A');
    peer.revalidate().unwrap();
    (&server).write_all(b"Q").unwrap();
    assert_eq!(receive_byte(&server), b'Q');
    peer.revalidate().unwrap();
    (&server).write_all(b"X").unwrap();
    finish_child(&mut child);
    assert!(
        peer.revalidate().is_err(),
        "exited peer must not remain authorized"
    );
}

#[test]
#[ignore = "owned unsigned fixture child; invoked only by parent test"]
fn unsigned_fixture_pipe_child() {
    let pipe = open_child_pipe();
    (&pipe).write_all(b"R").unwrap();
    if receive_byte(&pipe) == 0 {
        return;
    }
    let owner = peer_auth::test_fixture::FixtureIdentity::current().unwrap();
    assert!(ClientIdentity::current().is_err());
    assert!(verify_image(&std::env::current_exe().unwrap()).is_err());
    let peer = owner
        .authenticate(pipe.as_handle(), peer_auth::PeerEnd::Server)
        .unwrap();
    peer.revalidate().unwrap();
    assert!(peer.creation_time() > 0);
    assert_ne!(peer.pid(), std::process::id());
    (&pipe).write_all(b"A").unwrap();
    assert_eq!(receive_byte(&pipe), b'Q');
    peer.revalidate().unwrap();
    (&pipe).write_all(b"Q").unwrap();
    assert_eq!(receive_byte(&pipe), b'X');
}

#[test]
#[ignore = "requires a separately compiled unsigned copy of this test harness"]
fn unsigned_fixture_rejects_different_image() {
    let path = std::path::PathBuf::from(
        std::env::var_os("ARTERM_DIFFERENT_TEST_IMAGE").expect("separately compiled test harness"),
    );
    let owner = peer_auth::test_fixture::FixtureIdentity::current().unwrap();
    let (server, mut child) = connect_owned_child(&path, "unsigned_fixture_pipe_child");
    let error = match owner.authenticate(server.as_handle(), peer_auth::PeerEnd::Client) {
        Ok(_) => panic!("different fixture image was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.reason, Rejection::DifferentClientBuild);
    (&server).write_all(&[0]).unwrap();
    finish_child(&mut child);
}
