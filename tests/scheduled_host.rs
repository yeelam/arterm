use std::{
    fs,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    root: std::path::PathBuf,
    child: Option<Child>,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("arterm-scheduled-io-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        Self { root, child: None }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_arterm-host"));
        command.env("VSTERM_REMOTE_HOME", &self.root);
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
            }
            child.wait().unwrap();
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn scheduled_startup_error_is_logged_and_returns_failure() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("host")).unwrap();
    fs::write(
        fixture.root.join("host").join("setup.json"),
        b"invalid fixture config",
    )
    .unwrap();
    let result = fixture
        .command()
        .args(["run", "--data-root"])
        .arg(&fixture.root)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let log = fs::read_to_string(fixture.root.join("host").join("host.stderr.log")).unwrap();
    assert!(log.contains("[host] starting pid="));
    assert!(log.contains("expected value"), "{log}");
    assert!(!log.contains("stopped normally"));
}

#[test]
#[cfg_attr(
    not(feature = "test-unsigned-ipc"),
    ignore = "requires explicit unsigned isolated fixture"
)]
fn direct_scheduled_broker_stays_alive_and_retains_logs_after_clean_stop() {
    let mut fixture = Fixture::new();
    fixture.child = Some(
        fixture
            .command()
            .args(["run", "--data-root"])
            .arg(&fixture.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            fixture
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none(),
            "scheduled broker exited before readiness"
        );
        let status = fixture
            .command()
            .args(["status", "--json"])
            .output()
            .unwrap();
        if status.status.success() {
            break;
        }
        assert!(Instant::now() < deadline, "scheduled broker was not ready");
        thread::sleep(Duration::from_millis(50));
    }
    let status = fixture.command().args(["stop", "--json"]).output().unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = fixture.child.as_mut().unwrap().try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "scheduled broker did not exit after stop"
        );
        thread::sleep(Duration::from_millis(50));
    }
    for file in ["host.stdout.log", "host.stderr.log"] {
        let text = fs::read_to_string(fixture.root.join("host").join(file)).unwrap();
        assert!(text.contains("[host] starting pid="));
    }
    let error = fs::read_to_string(fixture.root.join("host").join("host.stderr.log")).unwrap();
    assert!(error.contains("[host] stopped normally"));
}
