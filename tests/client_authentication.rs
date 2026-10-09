use arterm::client_config;
use std::{
    fs,
    net::TcpListener,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

static AUTHENTICATION_TESTS: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    vendor: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("client-auth-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        // Preserve the extended prefix for production's raw Win32 atomic replacements.
        let root = fs::canonicalize(&root).unwrap();
        let vendor = root.join("vendor.exe");
        let build = Command::new("rustc")
            .args(["--edition=2021", "tests\\fixtures\\auth_vendor.rs", "-o"])
            .arg(&vendor)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        Self { root, vendor }
    }

    fn home(&self, mode: &str, initially_ready: bool) -> PathBuf {
        self.home_under(&self.root, mode, initially_ready)
    }

    fn home_under(&self, parent: &Path, mode: &str, initially_ready: bool) -> PathBuf {
        for file in ["calls", "ready", "discovered", "denied"] {
            let _ = fs::remove_file(self.root.join(file));
        }
        fs::write(self.root.join("mode"), mode).unwrap();
        if initially_ready {
            fs::write(self.root.join("ready"), b"ready").unwrap();
        }
        let home = parent.join(format!("home-{}", Uuid::now_v7()));
        client_config::update(&home, |config| {
            config.devtunnel_path = Some(self.vendor.clone());
            client_config::add(config, "fixture", "fixture", r"C:\Tools\arterm-host.exe")?;
            Ok(())
        })
        .unwrap();
        home
    }

    fn calls(&self, call: &str) -> usize {
        fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == call)
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn command(home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arterm"));
    command
        .env("VSTERM_REMOTE_HOME", home)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run(home: &Path, args: &[&str]) -> Output {
    command(home, args).output().unwrap()
}

fn fallback(output: &Output) {
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("arterm --login"), "{diagnostic}");
    assert!(
        diagnostic.contains("same GitHub account as the host"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("SECRET_OAUTH"), "{diagnostic}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SECRET_OAUTH"));
}

#[test]
fn long_private_checkout_fixture_preserves_atomic_saved_sessions_and_cleanup() {
    use arterm::store::{SessionReference, Store};
    let _guard = AUTHENTICATION_TESTS.lock().unwrap();
    let fixture = Fixture::new();
    // Keep linker output short while exercising extended state paths beyond MAX_PATH.
    let checkout = fixture
        .root
        .join("private-checkout")
        .join("source")
        .join("long-checkout-component-".repeat(5));
    let home = fixture.home_under(&checkout, "unauthorized", true);
    let config = client_config::load(&home).unwrap();
    let target = &config.targets["fixture"];
    let dir = client_config::state_dir(&home, target);
    let (store, state) = Store::resolve(
        &dir,
        &target.target_id,
        &SessionReference::parse("saved").unwrap(),
        true,
        None,
        None,
    )
    .unwrap();
    let record = dir.join(format!("{}.dpapi", state.id));
    let temporary = record.with_extension(format!("{}.tmp", Uuid::now_v7()));
    let wide: Vec<u16> = temporary.as_os_str().encode_wide().collect();
    assert_eq!(
        &wide[..4],
        &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]
    );
    assert!(
        wide.len() - 4 > 260,
        "replacement path must exceed MAX_PATH"
    );
    store.save(&state).unwrap();
    assert_eq!(
        store.load(&target.target_id, state.id).unwrap().id,
        state.id
    );
    let before = fs::read(&record).unwrap();
    drop(store);
    #[cfg(feature = "test-unsigned-ipc")]
    {
        fallback(&run(
            &home,
            &["connect", "fixture", "saved", "--stdio", "--retries", "1"],
        ));
        assert_eq!(fixture.calls("login-hidden"), 0);
        assert_eq!(fixture.calls("connect"), 1);
    }
    assert_eq!(fs::read(&record).unwrap(), before);
    let owned_root = fixture.root.clone();
    let shared_parent = owned_root.parent().unwrap().to_owned();
    drop(fixture);
    assert!(!owned_root.try_exists().unwrap());
    assert!(
        shared_parent.is_dir(),
        "cleanup must preserve the shared parent"
    );
}

fn control_connection(
    fixture: &Fixture,
    bridge_ready: bool,
) -> anyhow::Result<arterm::transport::TunnelLink> {
    use arterm::{
        transport::{Forward, TunnelLink},
        wire::{map, s, text},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    fs::write(fixture.root.join("address"), &address).unwrap();
    let server = thread::spawn(move || {
        for connection in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "fixture control connection timed out"
                        );
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let version = rmpv::decode::read_value(&mut socket).unwrap();
            assert_eq!(text(&version, "method").unwrap(), "version");
            rmpv::encode::write_value(
                &mut socket,
                &map(vec![("id", 1.into()), ("result", map(vec![]))]),
            )
            .unwrap();
            if connection == 1 {
                let spawn = rmpv::decode::read_value(&mut socket).unwrap();
                assert_eq!(text(&spawn, "method").unwrap(), "spawn");
                let reply = if bridge_ready {
                    map(vec![
                        ("method", s("streams_started")),
                        (
                            "params",
                            map(vec![(
                                "stream_ids",
                                rmpv::Value::Array(vec![11.into(), 12.into(), 13.into()]),
                            )]),
                        ),
                    ])
                } else {
                    map(vec![("method", s("spawn_failed"))])
                };
                rmpv::encode::write_value(&mut socket, &reply).unwrap();
            }
        }
    });
    let result = Forward::start(&fixture.vendor, "fixture").and_then(|(forward, address)| {
        TunnelLink::connect(&address, r"C:\Tools\arterm-host.exe", Some(forward))
    });
    server.join().unwrap();
    result
}

#[test]
fn authentication_rearms_only_after_an_established_control_connection() {
    let _guard = AUTHENTICATION_TESTS.lock().unwrap();
    let fixture = Fixture::new();
    let home = fixture.home("forward_success", false);
    struct HomeGuard(Option<std::ffi::OsString>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(home) => std::env::set_var("VSTERM_REMOTE_HOME", home),
                None => std::env::remove_var("VSTERM_REMOTE_HOME"),
            }
        }
    }
    let _home_guard = HomeGuard(std::env::var_os("VSTERM_REMOTE_HOME"));
    std::env::set_var("VSTERM_REMOTE_HOME", &home);
    for episode in 1..=2 {
        let link = control_connection(&fixture, true).unwrap();
        assert_eq!(fixture.calls("login-hidden"), episode);
        drop(link);
        fs::remove_file(fixture.root.join("ready")).unwrap();
    }
    assert!(control_connection(&fixture, false).is_err());
    assert_eq!(fixture.calls("login-hidden"), 3);
    fs::remove_file(fixture.root.join("ready")).unwrap();
    let error = match arterm::transport::Forward::start(&fixture.vendor, "fixture") {
        Ok(_) => panic!("an unestablished episode must not permit another automatic sign-in"),
        Err(error) => error,
    };
    assert!(error.is::<arterm::transport::LoginRequired>());
    assert_eq!(fixture.calls("login-hidden"), 3);
}

#[test]
fn automatic_authentication_cli_is_positive_bounded_hidden_and_serialized() {
    let _guard = AUTHENTICATION_TESTS.lock().unwrap();
    let fixture = Fixture::new();
    let home = fixture.home("expired", false);
    for alias in ["unknown", "Bad Alias"] {
        assert!(!run(&home, &["doctor", alias]).status.success());
        assert_eq!(fixture.calls("status"), 0);
        assert_eq!(fixture.calls("login-hidden"), 0);
    }
    for mode in ["valid", "expired", "missing"] {
        let home = fixture.home(mode, false);
        let output = run(&home, &["doctor"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fixture.calls("login-hidden"),
            usize::from(mode != "valid"),
            "{mode}"
        );
        assert_eq!(fixture.calls("login-console"), 0);
        assert!(!String::from_utf8_lossy(&output.stdout).contains("SECRET_OAUTH"));
    }
    for mode in [
        "malformed",
        "unknown",
        "network",
        "truncated",
        "status_hang",
    ] {
        let home = fixture.home(mode, false);
        assert!(!run(&home, &["doctor"]).status.success(), "{mode}");
        assert_eq!(fixture.calls("login-hidden"), 0, "{mode}");
    }
    for mode in ["login_fail", "false_success"] {
        let home = fixture.home(mode, false);
        fallback(&run(&home, &["doctor"]));
        assert_eq!(fixture.calls("login-hidden"), 1, "{mode}");
    }
    for mode in ["expired", "concurrent_fail"] {
        let home = fixture.home(mode, false);
        let other_home = fixture.home(mode, false);
        let first = command(&home, &["doctor"]).spawn().unwrap();
        let second = command(&other_home, &["doctor"]).spawn().unwrap();
        let outputs = [
            first.wait_with_output().unwrap(),
            second.wait_with_output().unwrap(),
        ];
        for output in outputs {
            if mode == "expired" {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            } else {
                fallback(&output);
            }
        }
        assert_eq!(fixture.calls("login-hidden"), 1, "{mode}");
    }
    let home = fixture.home("concurrent_fail", false);
    fallback(&run(&home, &["doctor"]));
    fallback(&run(&home, &["doctor"]));
    assert_eq!(
        fixture.calls("login-hidden"),
        2,
        "later independent commands can retry"
    );
    let home = fixture.home("expired", false);
    for args in [vec!["--login", "extra"], vec!["login", "extra"]] {
        let output = run(&home, &args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("accepts no extra arguments"));
        assert_eq!(fixture.calls("login-hidden"), 0);
    }
    for alias in ["--login", "login"] {
        let home = fixture.home("expired", false);
        assert!(run(&home, &[alias]).status.success());
        assert_eq!(
            fixture.calls("login-hidden") + fixture.calls("login-console"),
            1
        );
    }
    let home = fixture.home("expired", false);
    assert!(run(&home, &["list"]).status.success());
    assert!(run(&home, &["connect", "fixture"]).status.success());
    assert_eq!(fixture.calls("status"), 0);
    assert!(
        !run(&home, &["doctor", "fixture", "--address", "127.0.0.1:1"])
            .status
            .success()
    );
    assert_eq!(fixture.calls("status"), 0);
    assert_eq!(fixture.calls("login-hidden"), 0);
    for mode in ["discovery_expiry", "forward_expiry", "continued_failure"] {
        let home = fixture.home(mode, true);
        let output = run(&home, &["doctor", "fixture"]);
        assert!(!output.status.success());
        assert_eq!(
            fixture.calls("login-hidden"),
            1,
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if mode != "discovery_expiry" {
            fallback(&output);
        }
        assert!(!home.join("client").join("sessions").exists());
    }
    for mode in ["unauthorized", "unauthorized_expiry"] {
        let home = fixture.home(mode, true);
        let output = run(&home, &["doctor", "fixture"]);
        assert!(!output.status.success());
        assert_eq!(
            fixture.calls("login-hidden"),
            usize::from(mode == "unauthorized_expiry")
        );
        fallback(&output);
    }
    let home = fixture.home("login_hang", false);
    let started = Instant::now();
    fallback(&run(&home, &["doctor"]));
    assert!(started.elapsed() >= Duration::from_secs(59));
    assert!(started.elapsed() < Duration::from_secs(70));
    assert_eq!(fixture.calls("login-hidden"), 1);

    #[cfg(feature = "test-unsigned-ipc")]
    for mode in ["continued_failure", "unauthorized", "valid"] {
        use arterm::store::{SessionReference, Store};
        let home = fixture.home(mode, mode != "valid");
        let config = client_config::load(&home).unwrap();
        let target = &config.targets["fixture"];
        let dir = client_config::state_dir(&home, target);
        let (store, state) = Store::resolve(
            &dir,
            &target.target_id,
            &SessionReference::parse("saved").unwrap(),
            true,
            None,
            None,
        )
        .unwrap();
        let record = dir.join(format!("{}.dpapi", state.id));
        let before = fs::read(&record).unwrap();
        drop(store);
        let output = run(
            &home,
            &["connect", "fixture", "saved", "--stdio", "--retries", "1"],
        );
        assert!(!output.status.success());
        assert_eq!(fs::read(&record).unwrap(), before);
        if mode == "continued_failure" {
            fallback(&output);
            assert_eq!(fixture.calls("login-hidden"), 1);
            assert!(!String::from_utf8_lossy(&output.stderr).contains("Reconnecting"));
        } else if mode == "unauthorized" {
            fallback(&output);
            assert_eq!(fixture.calls("login-hidden"), 0);
            assert_eq!(fixture.calls("connect"), 1);
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(diagnostic.contains("permissions"));
            assert!(!diagnostic.contains("Reconnecting"));
        } else {
            assert_eq!(fixture.calls("login-hidden"), 0);
            assert_eq!(fixture.calls("connect"), 2);
            assert!(String::from_utf8_lossy(&output.stderr).contains("Reconnecting 1/1"));
        }
    }
}
