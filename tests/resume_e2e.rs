//! Actual native client + host + ConPTY. Only the outer cloud relay is a local fixture.
use rmpv::Value;
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

fn s(v: &str) -> Value {
    v.into()
}
fn map(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(fields.into_iter().map(|(k, v)| (s(k), v)).collect())
}
fn get<'a>(v: &'a Value, name: &str) -> &'a Value {
    &v.as_map()
        .unwrap()
        .iter()
        .find(|(k, _)| k.as_str() == Some(name))
        .unwrap()
        .1
}
fn send(socket: &Arc<Mutex<TcpStream>>, value: Value) {
    let mut socket = socket.lock().unwrap();
    let _ = rmpv::encode::write_value(&mut *socket, &value);
}
struct Fixture {
    home: PathBuf,
    host: Child,
}
impl Fixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!("devbox-native-e2e-{}", Uuid::now_v7()));
        fs::create_dir_all(&home).unwrap();
        let host = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
            .arg("run")
            .env("VSTERM_REMOTE_HOME", &home)
            .stdin(Stdio::null())
            .stdout(fs::File::create(home.join("host.stdout")).unwrap())
            .stderr(fs::File::create(home.join("host.stderr")).unwrap())
            .spawn()
            .unwrap();
        let mut fixture = Self { home, host };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                fixture.host.try_wait().unwrap().is_none(),
                "host exited: {}",
                fs::read_to_string(fixture.home.join("host.stderr")).unwrap()
            );
            let status = fixture.host_command("status").output().unwrap();
            if status.status.success() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "host never became ready: {}",
                String::from_utf8_lossy(&status.stderr)
            );
            thread::sleep(Duration::from_millis(100));
        }
        let registered = Command::new(env!("CARGO_BIN_EXE_arterm"))
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args([
                "add",
                "fixture",
                "--tunnel",
                "fixture",
                "--host-path",
                env!("CARGO_BIN_EXE_arterm-host"),
            ])
            .output()
            .unwrap();
        assert!(
            registered.status.success(),
            "register failed: {}",
            String::from_utf8_lossy(&registered.stderr)
        );
        fixture
    }
    fn host_command(&self, verb: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_arterm-host"));
        command.env("VSTERM_REMOTE_HOME", &self.home).arg(verb);
        command
    }
    fn wait_for_session_state(&self, id: &str, exited: bool, attached: bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let output = self.host_command("sessions").output().unwrap();
            assert!(
                output.status.success(),
                "session status failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let sessions: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let session = sessions.as_array().unwrap().iter()
                .find(|session| session["id"] == id)
                .expect("fixture session disappeared");
            if session["exited"] == exited && session["attached"] == attached {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "session {id} did not reach exited={exited}, attached={attached}: {session}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
    fn client(&self, verb: &str, id: Option<&str>, addr: &str) -> RunningClient {
        let mut command = Command::new(env!("CARGO_BIN_EXE_arterm"));
        command
            .env("VSTERM_REMOTE_HOME", &self.home)
            .env("VSTERM_HISTORY_PATH", self.home.join("powershell-history.txt"))
            .args([verb, "fixture"]);
        if let Some(id) = id {
            command.arg(id);
        } else if verb == "connect" {
            command.arg(Uuid::now_v7().to_string());
        }
        command.args(["--address", addr, "--stdio", "--retries", "2"]);
        if verb == "connect" {
            command.args(["--shell", "powershell.exe"]);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        for (stderr, mut reader) in [
            (false, Box::new(out) as Box<dyn Read + Send>),
            (true, Box::new(err) as Box<dyn Read + Send>),
        ] {
            let tx = tx.clone();
            thread::spawn(move || {
                let mut bytes = [0; 4096];
                while let Ok(n) = reader.read(&mut bytes) {
                    if n == 0 || tx.send((stderr, bytes[..n].to_vec())).is_err() {
                        break;
                    }
                }
            });
        }
        RunningClient {
            child,
            input,
            rx,
            out: String::new(),
            err: String::new(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self
            .host_command("stop")
            .arg("--terminate-sessions")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.host.kill();
        let _ = self.host.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}
struct RunningClient {
    child: Child,
    input: ChildStdin,
    rx: mpsc::Receiver<(bool, Vec<u8>)>,
    out: String,
    err: String,
}
impl RunningClient {
    fn wait_for(&mut self, predicate: impl Fn(&str, &str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !predicate(&self.out, &self.err) {
            assert!(
                Instant::now() < deadline,
                "client timeout\nOUT:{}\nERR:{}",
                self.out,
                self.err
            );
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok((is_err, bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    if is_err {
                        self.err.push_str(&text);
                    } else {
                        self.out.push_str(&text);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => panic!("client ended early\nOUT:{}\nERR:{}", self.out, self.err),
            }
        }
    }
    fn command(&mut self, command: &str) {
        self.input.write_all(command.as_bytes()).unwrap();
        self.input.write_all(b"\r\n").unwrap();
        self.input.flush().unwrap();
    }
    fn detach(&mut self) {
        self.input.write_all(&[0x1d]).unwrap();
        self.input.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "client did not detach");
            thread::sleep(Duration::from_millis(50));
        }
    }
    fn guid(&self) -> String {
        self.err
            .split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
            .find(|v| Uuid::parse_str(v).is_ok())
            .expect("client did not print a GUID")
            .to_owned()
    }
    fn wait_exit(&mut self, expected: i32) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                for (is_err, bytes) in self.rx.iter() {
                    if is_err {
                        self.err.push_str(&String::from_utf8_lossy(&bytes));
                    } else {
                        self.out.push_str(&String::from_utf8_lossy(&bytes));
                    }
                }
                assert_eq!(status.code(), Some(expected), "OUT:{}\nERR:{}", self.out, self.err);
                return;
            }
            assert!(Instant::now() < deadline, "client did not exit\nOUT:{}\nERR:{}", self.out, self.err);
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for RunningClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn relay(home: PathBuf, attachments: usize) -> (String, mpsc::Receiver<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for _ in 0..attachments {
            let (mut incoming, _) = listener.accept().unwrap();
            incoming
                .set_read_timeout(Some(Duration::from_secs(45)))
                .unwrap();
            let out = Arc::new(Mutex::new(incoming.try_clone().unwrap()));
            let version = rmpv::decode::read_value(&mut incoming).unwrap();
            assert_eq!(get(&version, "method").as_str(), Some("version"));
            send(
                &out,
                map(vec![
                    ("id", 1.into()),
                    (
                        "result",
                        map(vec![("version", s("local-real-host-fixture"))]),
                    ),
                ]),
            );
            let spawn = rmpv::decode::read_value(&mut incoming).unwrap();
            assert_eq!(get(&spawn, "method").as_str(), Some("spawn"));
            let params = get(&spawn, "params");
            assert_eq!(
                get(params, "command").as_str(),
                Some(env!("CARGO_BIN_EXE_arterm-host"))
            );
            let mut bridge = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
                .args(["bridge", "--protocol", "vsterm-session-v1"])
                .env("VSTERM_REMOTE_HOME", &home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            send(
                &out,
                map(vec![
                    ("method", s("streams_started")),
                    (
                        "params",
                        map(vec![(
                            "stream_ids",
                            Value::Array(vec![11.into(), 12.into(), 13.into()]),
                        )]),
                    ),
                ]),
            );
            let _ = tx.send(incoming.try_clone().unwrap());
            let stdout = bridge.stdout.take().unwrap();
            let stderr = bridge.stderr.take().unwrap();
            for (id, mut reader) in [
                (12, Box::new(stdout) as Box<dyn Read + Send>),
                (13, Box::new(stderr) as Box<dyn Read + Send>),
            ] {
                let out = out.clone();
                thread::spawn(move || {
                    let mut data = [0; 4096];
                    let mut frames = arterm::wire::Frames::default();
                    while let Ok(n) = reader.read(&mut data) {
                        if n == 0 {
                            break;
                        }
                        let mut segments = Vec::new();
                        if id == 12 {
                            frames.push(&data[..n]).unwrap();
                            while let Some(frame) = frames.next().unwrap() {
                                if get(&frame, "type").as_str() == Some("SessionExited") {
                                    // VS Code pumps each stream independently of its spawn result.
                                    send(&out, map(vec![
                                        ("method", s("stream_ended")),
                                        ("params", map(vec![("stream", 11.into())])),
                                    ]));
                                    send(&out, map(vec![
                                        ("id", 10.into()),
                                        ("result", map(vec![("exit_code", 0.into())])),
                                    ]));
                                }
                                segments.push(arterm::wire::encode(&frame).unwrap());
                            }
                        } else {
                            segments.push(data[..n].to_vec());
                        }
                        for segment in segments {
                            send(&out, map(vec![
                                ("method", s("stream_data")),
                                ("params", map(vec![
                                    ("stream", id.into()),
                                    ("segment", Value::Binary(segment)),
                                ])),
                            ]));
                        }
                    }
                    send(&out, map(vec![
                        ("method", s("stream_ended")),
                        ("params", map(vec![("stream", id.into())])),
                    ]));
                });
            }
            let mut stdin = bridge.stdin.take().unwrap();
            while let Ok(value) = rmpv::decode::read_value(&mut incoming) {
                let params = get(&value, "params");
                match get(params, "segment") {
                    Value::Binary(bytes) => {
                        if stdin.write_all(bytes).is_err() {
                            break;
                        }
                    }
                    _ => panic!("expected binary transport"),
                }
            }
            drop(stdin);
            let _ = incoming.shutdown(Shutdown::Both);
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until && bridge.try_wait().unwrap().is_none() {
                thread::sleep(Duration::from_millis(20));
            }
            let _ = bridge.kill();
            let _ = bridge.wait();
        }
    });
    (addr, rx)
}
fn pid_marker(text: &str, prefix: &str, suffix: &str) -> Option<String> {
    text.match_indices(prefix).find_map(|(start, _)| {
        let rest = &text[start + prefix.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && rest[digits.len()..].starts_with(suffix) {
            Some(digits)
        } else {
            None
        }
    })
}

#[test]
fn real_shell_survives_network_loss_detach_and_resume() {
    let fixture = Fixture::new();
    let (addr, connections) = relay(fixture.home.clone(), 5);
    let mut client = fixture.client("connect", Some("MyWork"), &addr);
    client.command(
        "$global:KeepMe='resumable'; Write-Output ('FIRST=' + $PID + ':' + $global:KeepMe)",
    );
    client.wait_for(|out, _| pid_marker(out, "FIRST=", ":resumable").is_some());
    let pid = pid_marker(&client.out, "FIRST=", ":resumable").unwrap();
    let id = client.guid();
    let history_path = fixture.home.join("powershell-history.txt");
    assert!(!history_path.exists(), "named connect must not insert history");
    let first_socket = connections.recv_timeout(Duration::from_secs(5)).unwrap();
    first_socket.shutdown(Shutdown::Both).unwrap();
    let _replacement = connections.recv_timeout(Duration::from_secs(20)).unwrap();
    client.command("Write-Output ('SECOND=' + $PID + ':' + $global:KeepMe)");
    client.wait_for(|out, _| pid_marker(out, "SECOND=", ":resumable").is_some());
    assert_eq!(
        pid_marker(&client.out, "SECOND=", ":resumable").unwrap(),
        pid
    );
    assert_eq!(client.guid(), id);
    client.detach();
    let config: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.home.join("client").join("config.json")).unwrap()).unwrap();
    let target = config["targets"]["fixture"]["target_id"].as_str().unwrap();
    let store = arterm::store::Store::open(
        &fixture.home.join("client").join("sessions").join(target),
        Uuid::parse_str(&id).unwrap()).unwrap();
    let state = store.load(target, Uuid::parse_str(&id).unwrap()).unwrap();
    for secret in [&state.claim, state.token.as_ref().unwrap()] {
        let hex: String = secret.iter().map(|byte| format!("{byte:02x}")).collect();
        assert!(!client.err.contains(&hex));
        assert!(!client.err.contains(&format!("{secret:?}")));
    }
    drop(store);
    let mut resumed = fixture.client("connect", Some("MyWork"), &addr);
    let _attached = connections.recv_timeout(Duration::from_secs(10)).unwrap();
    resumed.command("Write-Output ('THIRD=' + $PID + ':' + $global:KeepMe)");
    resumed.wait_for(|out, _| pid_marker(out, "THIRD=", ":resumable").is_some());
    assert_eq!(
        pid_marker(&resumed.out, "THIRD=", ":resumable").unwrap(),
        pid
    );
    assert_eq!(resumed.guid(), id);
    resumed.detach();
    let mut legacy = fixture.client("resume", Some(&id), &addr);
    let _legacy = connections.recv_timeout(Duration::from_secs(10)).unwrap();
    legacy.command("Write-Output ('LEGACY=' + $PID + ':' + $global:KeepMe)");
    legacy.wait_for(|out, _| pid_marker(out, "LEGACY=", ":resumable").is_some());
    assert_eq!(pid_marker(&legacy.out, "LEGACY=", ":resumable").unwrap(), pid);
    legacy.detach();
    assert!(!history_path.exists());
    let mut fresh = fixture.client("connect", None, &addr);
    let _new = connections.recv_timeout(Duration::from_secs(10)).unwrap();
    fresh.command("Write-Output ('NEW=' + $PID + ':new')");
    fresh.wait_for(|out, _| pid_marker(out, "NEW=", ":new").is_some());
    assert_ne!(fresh.guid(), id);
    assert_ne!(pid_marker(&fresh.out, "NEW=", ":new").unwrap(), pid);
    fresh.detach();
    assert!(!history_path.exists());
}

#[test]
fn intentional_remote_exit_ends_the_client_without_recovery_instructions() {
    for code in [0, 7] {
        let fixture = Fixture::new();
        let (addr, connections) = relay(fixture.home.clone(), 1);
        let mut client = fixture.client("connect", Some("ExitTest"), &addr);
        client.command("Write-Output ('READY=' + $PID + ':exit')");
        client.wait_for(|out, _| pid_marker(out, "READY=", ":exit").is_some());
        let _connection = connections.recv_timeout(Duration::from_secs(5)).unwrap();
        client.command(&format!("exit {code}"));
        client.wait_exit(code);
        assert!(client.err.contains(&format!("Remote exit code: {code}")), "{}", client.err);
        assert!(!client.err.contains("Reconnecting"), "{}", client.err);
        assert!(!client.err.contains("Attachment lost"), "{}", client.err);
        assert_eq!(client.err.matches("Resume command:").count(), 1,
            "only initial startup may print the resume command: {}", client.err);
        let mut later = fixture.client("connect", Some("ExitTest"), &addr);
        later.wait_exit(1);
        assert!(later.err.contains("session has ended"), "{}", later.err);
        assert!(!later.err.contains("Reconnecting"));
    }
}

#[test]
fn named_termination_is_durable_and_never_recreates() {
    let fixture = Fixture::new();
    let (addr, connections) = relay(fixture.home.clone(), 2);
    let mut client = fixture.client("connect", Some("TerminateMe"), &addr);
    client.command("Write-Output ('READY=' + $PID + ':terminate')");
    client.wait_for(|out, _| pid_marker(out, "READY=", ":terminate").is_some());
    let _connection = connections.recv_timeout(Duration::from_secs(5)).unwrap();
    client.detach();
    let output = Command::new(env!("CARGO_BIN_EXE_arterm"))
        .env("VSTERM_REMOTE_HOME", &fixture.home)
        .args(["terminate", "fixture", "terminateme", "--yes", "--address", &addr])
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let mut later = fixture.client("connect", Some("TerminateMe"), &addr);
    later.wait_exit(1);
    assert!(later.err.contains("session has ended"), "{}", later.err);
}

#[test]
fn exit_while_detached_is_an_error_on_later_connect() {
    let fixture = Fixture::new();
    let (addr, connections) = relay(fixture.home.clone(), 2);
    let mut client = fixture.client("connect", Some("DetachedExit"), &addr);
    client.command("Write-Output ('READY=' + $PID + ':detached')");
    client.wait_for(|out, _| pid_marker(out, "READY=", ":detached").is_some());
    let _connection = connections.recv_timeout(Duration::from_secs(5)).unwrap();
    let id = client.guid();
    let allow_exit = fixture.home.join("allow-detached-exit");
    let quoted_path = allow_exit.to_string_lossy().replace('\'', "''");
    client.command(&format!(
        "Write-Output ('ARMED=' + $PID + ':detached'); while (-not (Test-Path -LiteralPath '{quoted_path}')) {{ Start-Sleep -Milliseconds 25 }}; exit 0"
    ));
    client.wait_for(|out, _| pid_marker(out, "ARMED=", ":detached").is_some());
    client.detach();
    // Release exit only after detach, then observe the same broker flag attach checks.
    fixture.wait_for_session_state(&id, false, false);
    fs::write(&allow_exit, b"exit").unwrap();
    fixture.wait_for_session_state(&id, true, false);
    let mut later = fixture.client("connect", Some("DetachedExit"), &addr);
    later.wait_exit(1);
    assert!(later.err.contains("session has ended"), "{}", later.err);
    let mut again = fixture.client("connect", Some("DetachedExit"), &addr);
    again.wait_exit(1);
    assert!(!again.err.contains("Resume command:"), "known ended state must fail before connecting");
}
