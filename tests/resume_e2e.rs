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

fn client_executable() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| std::env::var("SIGNED_CLIENT").unwrap_or_else(|_| option_env!("CARGO_BIN_EXE_arterm").unwrap().into()))
}
fn host_executable() -> &'static str {
    static PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PATH.get_or_init(|| std::env::var("SIGNED_HOST").unwrap_or_else(|_| option_env!("CARGO_BIN_EXE_arterm-host").unwrap().into()))
}
fn controller_executable() -> String {
    std::env::var("SIGNED_CONTROLLER").unwrap_or_else(|_| client_executable().into())
}
fn test_shell() -> String {
    std::env::var("ARTERM_TEST_SHELL").unwrap_or_else(|_| "powershell.exe".into())
}
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
pub(crate) struct Fixture {
    pub(crate) home: PathBuf,
    host: Child,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let home = std::env::temp_dir().join(format!("devbox-native-e2e-{}", Uuid::now_v7()));
        fs::create_dir_all(&home).unwrap();
        let host = Command::new(host_executable())
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
        let registered = Command::new(client_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args([
                "add",
                "fixture",
                "--tunnel",
                "fixture",
                "--host-path",
                host_executable(),
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
        let mut command = Command::new(host_executable());
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
    pub(crate) fn client(&self, verb: &str, id: Option<&str>, addr: &str) -> RunningClient {
        let mut command = Command::new(client_executable());
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
            command.args(["--shell", &test_shell()]);
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
pub(crate) struct RunningClient {
    child: Child,
    input: ChildStdin,
    rx: mpsc::Receiver<(bool, Vec<u8>)>,
    out: String,
    err: String,
}
impl RunningClient {
    pub(crate) fn wait_for(&mut self, predicate: impl Fn(&str, &str) -> bool) {
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
    pub(crate) fn command(&mut self, command: &str) {
        self.input.write_all(command.as_bytes()).unwrap();
        self.input.write_all(b"\r\n").unwrap();
        self.input.flush().unwrap();
    }
    pub(crate) fn detach(&mut self) {
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

pub(crate) fn relay(home: PathBuf, attachments: usize) -> (String, mpsc::Receiver<TcpStream>) {
    relay_traced(home, attachments, None)
}

#[derive(Default, Debug)]
struct SyntheticInputTrace {
    packets: Vec<Vec<u8>>,
    acknowledged: u64,
}
fn relay_traced(home: PathBuf, attachments: usize, trace: Option<Arc<Mutex<SyntheticInputTrace>>>) -> (String, mpsc::Receiver<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for _ in 0..attachments {
            let (mut incoming, _) = listener.accept().unwrap();
            let home = home.clone();
            let tx = tx.clone();
            let trace = trace.clone();
            thread::spawn(move || {
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
                Some(host_executable())
            );
            let mut bridge = Command::new(host_executable())
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
                let trace = trace.clone();
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
                                if get(&frame, "type").as_str() == Some("InputAck") {
                                    if let Some(trace) = &trace { trace.lock().unwrap().acknowledged += 1; }
                                }
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
            let mut input_frames = arterm::wire::Frames::default();
            while let Ok(value) = rmpv::decode::read_value(&mut incoming) {
                let params = get(&value, "params");
                match get(params, "segment") {
                    Value::Binary(bytes) => {
                        if let Some(trace) = &trace {
                            input_frames.push(bytes).unwrap();
                            while let Some(frame) = input_frames.next().unwrap() {
                                if get(&frame, "type").as_str() == Some("Input") {
                                    let bytes = arterm::wire::binary(get(&frame, "body"), "bytes").unwrap();
                                    let mut trace = trace.lock().unwrap();
                                    if trace.packets.len() < 32 { trace.packets.push(bytes.into_iter().take(512).collect()); }
                                }
                            }
                        }
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
            });
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
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn interactive_win32_keyup_preserves_command_readiness() {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    struct Interactive(Box<dyn portable_pty::Child + Send + Sync>);
    impl Drop for Interactive { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
    let fixture = Fixture::new();
    let trace = Arc::new(Mutex::new(SyntheticInputTrace::default()));
    let (address, _) = relay_traced(fixture.home.clone(), 2, Some(trace.clone()));
    for reference in [Uuid::now_v7().to_string(), "interactive-named".into()] {
        *trace.lock().unwrap() = SyntheticInputTrace::default();
        let pair = native_pty_system().openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 }).unwrap();
        let quote = |value: &str| format!("'{}'", value.replace('\'', "''"));
        let mut command = CommandBuilder::new("powershell.exe");
        command.env("VSTERM_REMOTE_HOME", &fixture.home);
        // A parent terminal can enable focus reports before arTerm starts. This
        // negotiation never traverses the remote broker's output stream.
        command.args(["-NoLogo", "-NoProfile", "-Command", &format!(
            "[Console]::Write(([char]27+'[?1004h')); & {} connect fixture {} --address {} --retries 0 --shell {}; exit $LASTEXITCODE",
            quote(client_executable()), quote(&reference), quote(&address), quote(&test_shell()))]);
        let mut client = Interactive(pair.slave.spawn_command(command).unwrap());
        drop(pair.slave);
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let mut reader = pair.master.try_clone_reader().unwrap();
        let terminal_writer = writer.clone();
        let focus_reporting = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let terminal_focus = focus_reporting.clone();
        let reader_thread = thread::spawn(move || {
            let mut terminal = vt100::Parser::new(40, 120, 0);
            let mut tail = Vec::new();
            let mut bytes = [0; 4096];
            while let Ok(n) = reader.read(&mut bytes) {
                if n == 0 { break; }
                terminal.process(&bytes[..n]);
                let previous = tail.len();
                tail.extend_from_slice(&bytes[..n]);
                for query in [b"\x1b[6n".as_slice(), b"\x1b[c", b"\x1b[>c"] {
                    let count = tail.windows(query.len()).enumerate()
                        .filter(|(index, window)| *index + query.len() > previous && *window == query).count();
                    for _ in 0..count {
                        let (row, col) = terminal.screen().cursor_position();
                        let response = match query {
                            b"\x1b[6n" => format!("\x1b[{};{}R", row + 1, col + 1),
                            b"\x1b[c" => "\x1b[?1;2c".into(),
                            _ => "\x1b[>0;10;1c".into(),
                        };
                        if terminal_writer.lock().unwrap().write_all(response.as_bytes()).is_err() { return; }
                    }
                }
                for (index, mode) in tail.windows(8).enumerate() {
                    if index + 8 > previous && mode == b"\x1b[?1004h" { terminal_focus.store(true, std::sync::atomic::Ordering::Release); }
                    if index + 8 > previous && mode == b"\x1b[?1004l" { terminal_focus.store(false, std::sync::atomic::Ordering::Release); }
                }
                if tail.len() > 7 { tail.drain(..tail.len() - 7); }
            }
        });
        let control = |args: &[&str]| Command::new(controller_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home).args(args).output().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut ready_since = None;
        loop {
            assert!(client.0.try_wait().unwrap().is_none(), "interactive client exited");
            let listed = control(&["list", "--client", "--json"]);
            if listed.status.success() {
                let owners: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
                if owners[0]["shell_status"] == "ready" {
                    let since = ready_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > Duration::from_millis(500) { break; }
                } else { ready_since = None; }
                assert!(Instant::now() < deadline, "interactive readiness: {owners}; forwarded input={:?}", trace.lock().unwrap());
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(focus_reporting.load(std::sync::atomic::Ordering::Acquire), "parent must enable terminal focus reporting");
        writer.lock().unwrap().write_all(b"\x1b[O\x1b[I").unwrap();
        let traffic_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let observed = trace.lock().unwrap();
            if observed.packets.iter().any(|bytes| bytes.windows(3).any(|v| v == b"\x1b[O"))
                && observed.acknowledged >= observed.packets.len() as u64 { break; }
            assert!(Instant::now() < traffic_deadline, "startup traffic not acknowledged by host: {observed:?}");
            drop(observed);
            thread::sleep(Duration::from_millis(10));
        }
        let first = control(&["send", "fixture", &reference, "--command", "Get-Date", "--wait", "--timeout", "10s", "--json"]);
        assert!(first.status.success(), "first command without manual input: {} {}; synthetic input={:?}",
            String::from_utf8_lossy(&first.stdout), String::from_utf8_lossy(&first.stderr), trace.lock().unwrap());
        writer.lock().unwrap().write_all(b"$InteractiveProof=41; Write-Output ('MANUAL-OK='+$InteractiveProof)\r").unwrap();
        let manual_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let output = control(&["read", "fixture", &reference, "--lines", "20", "--json"]);
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let listed = control(&["list", "--client", "--json"]);
            let owners: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
            if String::from_utf8_lossy(&output.stdout).contains("MANUAL-OK=41")
                && owners[0]["shell_status"] == "ready" { break; }
            assert!(Instant::now() < manual_deadline, "manual interactive command did not finish: {owners}; synthetic input={:?}", trace.lock().unwrap());
            thread::sleep(Duration::from_millis(20));
        }
        writer.lock().unwrap().write_all(b"\x1b[16;42;0;0;0;1_").unwrap(); // Shift key release, no edit.
        assert!(focus_reporting.load(std::sync::atomic::Ordering::Acquire), "terminal must observe focus-mode negotiation before generating focus reports");
        writer.lock().unwrap().write_all(b"\x1b[O\x1b[I").unwrap(); // Terminal focus loss/gain, not typed text.
        thread::sleep(Duration::from_millis(250));
        let sent = control(&["send", "fixture", &reference, "--command", "$InteractiveProof++; if ($InteractiveProof -ne 42) { throw 'interactive runspace state lost' }; Get-ChildItem | Select-Object -First 1", "--wait", "--timeout", "10s", "--json"]);
        assert!(sent.status.success(), "{} {}; forwarded input={:?}", String::from_utf8_lossy(&sent.stdout), String::from_utf8_lossy(&sent.stderr), trace.lock().unwrap());
        writer.lock().unwrap().write_all(b"x").unwrap();
        writer.lock().unwrap().write_all(b"\x1b[O\x1b[I").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let listed = control(&["list", "--client", "--json"]);
            let owners: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
            if owners[0]["readiness_reason"] == "partial_human_input" { break; }
            assert!(Instant::now() < deadline, "{owners}");
            thread::sleep(Duration::from_millis(20));
        }
        let rejected = control(&["send", "fixture", &reference, "--command", "Get-Date", "--json"]);
        assert!(!rejected.status.success());
        let rejected: serde_json::Value = serde_json::from_slice(&rejected.stdout).unwrap();
        assert_eq!(rejected["readiness_reason"], "partial_human_input");
        assert_eq!(rejected["command_execution"], true);
        assert_eq!(rejected["command_capability"], true);
        assert_eq!(rejected["host_version"], env!("CARGO_PKG_VERSION"));
        writer.lock().unwrap().write_all(b"\x03").unwrap();
        loop {
            let listed = control(&["list", "--client", "--json"]);
            let owners: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
            if owners[0]["shell_status"] == "ready" { break; }
            assert!(Instant::now() < deadline, "clear interactive line: {owners}; input={:?}", trace.lock().unwrap());
        }
        let busy = control(&["send", "fixture", &reference, "--command", "Start-Sleep -Seconds 2", "--json"]);
        assert!(busy.status.success(), "{}", String::from_utf8_lossy(&busy.stdout));
        writer.lock().unwrap().write_all(b"\x1b[O\x1b[I").unwrap();
        let rejected = control(&["send", "fixture", &reference, "--command", "Get-Date", "--json"]);
        assert!(!rejected.status.success());
        let rejected: serde_json::Value = serde_json::from_slice(&rejected.stdout).unwrap();
        assert_eq!(rejected["shell_status"], "busy");
        assert!(control(&["interrupt", "fixture", &reference, "--json"]).status.success());
        assert!(control(&["detach", "fixture", &reference]).status.success());
        client.0.wait().unwrap();
        drop(writer);
        drop(pair.master);
        reader_thread.join().unwrap();
    }
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
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
    let mut legacy = fixture.client("connect", Some(&id), &addr);
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
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
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
    #[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
    pub(crate) fn headless_connection_supports_cross_cwd_read_list_and_detach() {
        struct Headless(Child);
        impl Drop for Headless {
            fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
        }
        let fixture = Fixture::new();
        let (addr, connections) = relay(fixture.home.clone(), 3);
        let mut first = fixture.client("connect", Some("Automation"), &addr);
        first.command("$global:KeepMe='ipc'; 1..600 | ForEach-Object { Write-Output ('x' * 200) }; Write-Output ('READY=' + $PID + ':ipc')");
        first.wait_for(|out, _| pid_marker(out, "READY=", ":ipc").is_some());
        let pid = pid_marker(&first.out, "READY=", ":ipc").unwrap();
        let id = first.guid();
        first.command("Write-Output ('HUMAN-SLEEP='+$PID+':ipc'); Start-Sleep -Seconds 30");
        first.wait_for(|out, _| pid_marker(out, "HUMAN-SLEEP=", ":ipc").is_some());
        let interrupted = Command::new(client_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args(["interrupt", "fixture", "automation", "--json"]).output().unwrap();
        assert!(interrupted.status.success(), "{} {}", String::from_utf8_lossy(&interrupted.stdout), String::from_utf8_lossy(&interrupted.stderr));
        first.command("Write-Output ('HUMAN-AFTER='+$PID+':ipc')");
        first.wait_for(|out, _| pid_marker(out, "HUMAN-AFTER=", ":ipc").is_some());
        assert_eq!(pid_marker(&first.out, "HUMAN-AFTER=", ":ipc").unwrap(), pid);
        let _first_connection = connections.recv_timeout(Duration::from_secs(10)).unwrap();
        first.detach();
        let mut background = Headless(Command::new(client_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .current_dir(std::env::temp_dir())
            .args(["connect", "fixture", "automation", "--address", &addr, "--retries", "0"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap());
        let _background_connection = connections.recv_timeout(Duration::from_secs(10)).unwrap();
        let control = |args: &[&str]| Command::new(controller_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .current_dir(std::env::temp_dir())
            .args(args).output().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = background.0.try_wait().unwrap() {
                let mut error = String::new();
                background.0.stderr.take().unwrap().read_to_string(&mut error).unwrap();
                panic!("headless client exited {status}: {error}");
            }
            let output = control(&["read", "fixture", "AUTOMATION", "--json"]);
            if output.status.success() && String::from_utf8_lossy(&output.stdout).contains("READY=") { break; }
            assert!(Instant::now() < deadline, "IPC read failed: {}", String::from_utf8_lossy(&output.stderr));
            thread::sleep(Duration::from_millis(50));
        }
        let listed = control(&["list", "--client", "--json"]);
        assert!(listed.status.success(), "{}", String::from_utf8_lossy(&listed.stderr));
        let owners: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        assert_eq!(owners.as_array().unwrap().len(), 1);
        assert_eq!(owners[0]["identity"]["session_id"], id);
        let human = control(&["list", "--client"]);
        let human = String::from_utf8(human.stdout).unwrap();
        assert!(human.contains("MACHINE") && human.contains("fixture") && human.contains(&id));
        assert!(!human.contains("\"identity\""));
        let command_id = Uuid::now_v7().to_string();
        let started = Instant::now();
        let sent = control(&["send", "fixture", "automation", "--command", "$KeepMe='ipc'; $Counter=1; Write-Output ('CONTROL='+$PID+':ipc')",
            "--command-id", &command_id, "--wait", "--timeout", "60s", "--json"]);
        assert!(sent.status.success(), "{} {}", String::from_utf8_lossy(&sent.stdout), String::from_utf8_lossy(&sent.stderr));
        assert!(started.elapsed() < Duration::from_secs(5), "wait did not return early");
        let completed: serde_json::Value = serde_json::from_slice(&sent.stdout).unwrap();
        assert_eq!(completed["status"], "completed");
        let human = control(&["read", "fixture", "automation", "--command-id", &command_id]);
        assert!(human.status.success());
        let human = String::from_utf8(human.stdout).unwrap();
        assert!(human.contains("State: completed") && human.contains("Succeeded: yes"));
        let timed = control(&["send", "fixture", "automation", "--command", "Start-Sleep -Seconds 2; $Counter++",
            "--wait", "--timeout", "1s", "--json"]);
        assert_eq!(timed.status.code(), Some(124), "{} {}", String::from_utf8_lossy(&timed.stdout), String::from_utf8_lossy(&timed.stderr));
        let timed: serde_json::Value = serde_json::from_slice(&timed.stdout).unwrap();
        let timed_id = timed["command_id"].as_str().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let query = control(&["read", "fixture", "automation", "--command-id", timed_id, "--json"]);
            assert!(query.status.success(), "{}", String::from_utf8_lossy(&query.stderr));
            let query: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
            if query["record"]["state"] == "completed" { break; }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(30));
        }
        let duplicate = control(&["send", "fixture", "automation", "--command",
            "$KeepMe='ipc'; $Counter=1; Write-Output ('CONTROL='+$PID+':ipc')", "--command-id", &command_id, "--json"]);
        assert!(duplicate.status.success(), "{}", String::from_utf8_lossy(&duplicate.stdout));
        for json in [false, true] {
            let mut args = vec!["send", "fixture", "automation", "--command", "$Counter=999", "--command-id", &command_id];
            if json { args.push("--json"); }
            let rejected = control(&args);
            assert_eq!(rejected.status.code(), Some(1));
            assert!(rejected.stderr.is_empty(), "response error must not be duplicated on stderr");
            if json {
                let response: serde_json::Value = serde_json::from_slice(&rejected.stdout).unwrap();
                assert_eq!(response["status"], "rejected");
                assert!(response["identity"].is_object());
            } else {
                let human = String::from_utf8(rejected.stdout).unwrap();
                assert!(human.contains("Operation failed.") && human.contains("command ID conflicts"));
                assert!(human.contains("Shell:") && !human.contains("\"identity\""));
            }
        }
        let abandoned_id = Uuid::now_v7().to_string();
        let mut waiter = Headless(Command::new(client_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args(["send", "fixture", "automation", "--command", "Start-Sleep -Seconds 2; $CallerGone='survived'",
                "--command-id", &abandoned_id, "--wait", "--timeout", "60s", "--json"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let query = control(&["read", "fixture", "automation", "--command-id", &abandoned_id, "--json"]);
            let query: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
            if query["record"]["state"] == "running" { break; }
            assert!(Instant::now() < deadline, "{query}");
        }
        waiter.0.kill().unwrap();
        waiter.0.wait().unwrap();
        loop {
            let query = control(&["read", "fixture", "automation", "--command-id", &abandoned_id, "--json"]);
            let query: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
            if query["record"]["state"] == "completed" {
                assert_eq!(query["record"]["succeeded"], true);
                break;
            }
            assert!(Instant::now() < deadline);
        }
        let slow = control(&["send", "fixture", "automation", "--command", "Start-Sleep -Seconds 30", "--json"]);
        assert!(slow.status.success(), "{}", String::from_utf8_lossy(&slow.stdout));
        let slow: serde_json::Value = serde_json::from_slice(&slow.stdout).unwrap();
        let slow_id = slow["command_id"].as_str().unwrap();
        loop {
            let query = control(&["read", "fixture", "automation", "--command-id", slow_id, "--json"]);
            let query: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
            if query["record"]["state"] == "running" { break; }
            assert!(Instant::now() < deadline);
        }
        assert!(!control(&["send", "fixture", "automation", "--command", "$Counter=999", "--json"]).status.success());
        let interrupted = control(&["interrupt", "fixture", "automation"]);
        assert!(interrupted.status.success(), "{} {}", String::from_utf8_lossy(&interrupted.stdout), String::from_utf8_lossy(&interrupted.stderr));
        assert!(String::from_utf8_lossy(&interrupted.stdout).contains("Interrupt requested"));
        loop {
            let query = control(&["read", "fixture", "automation", "--command-id", slow_id, "--json"]);
            let query: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
            if query["record"]["state"] == "completed" { break; }
            assert!(Instant::now() < deadline);
        }
        let after_command = control(&["send", "fixture", "automation", "--command", "Write-Output ('COUNTER='+$Counter)",
            "--wait", "--timeout", "60s"]);
        assert!(after_command.status.success(), "{} {}", String::from_utf8_lossy(&after_command.stdout), String::from_utf8_lossy(&after_command.stderr));
        assert!(String::from_utf8_lossy(&after_command.stdout).contains("Command completed successfully."));
        let output = control(&["read", "fixture", "automation", "--lines", "20", "--json"]);
        assert!(String::from_utf8_lossy(&output.stdout).contains("COUNTER=2"), "{}", String::from_utf8_lossy(&output.stdout));
        let plain = control(&["read", "fixture", "automation", "--lines", "20"]);
        assert!(plain.status.success());
        let plain = String::from_utf8(plain.stdout).unwrap();
        assert!(plain.contains("COUNTER=2") && !plain.contains("\"output\""));
        assert!(!control(&["read", "fixture", "missing"]).status.success());
        assert!(!control(&["read", "othermachine", "automation"]).status.success());
        assert!(!control(&["connect", "fixture", "automation", "--address", &addr, "--retries", "0"]).status.success());
        let detached = control(&["detach", "fixture", "automation"]);
        assert!(detached.status.success(), "{}", String::from_utf8_lossy(&detached.stderr));
        assert!(String::from_utf8_lossy(&detached.stdout).contains("remote shell retained"));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = background.0.try_wait().unwrap() { assert!(status.success()); break; }
            assert!(Instant::now() < deadline, "IPC detach did not stop local client");
            thread::sleep(Duration::from_millis(20));
        }
        fixture.wait_for_session_state(&id, false, false);
        let mut again = fixture.client("connect", Some("automation"), &addr);
        let _again_connection = connections.recv_timeout(Duration::from_secs(10)).unwrap();
        again.command("Write-Output ('AFTER=' + $PID + ':' + $global:KeepMe)");
        again.wait_for(|out, _| pid_marker(out, "AFTER=", ":ipc").is_some());
        assert_eq!(pid_marker(&again.out, "AFTER=", ":ipc").unwrap(), pid);
        again.detach();
    }

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn named_termination_is_durable_and_never_recreates() {
    let fixture = Fixture::new();
    let (addr, connections) = relay(fixture.home.clone(), 2);
    let mut client = fixture.client("connect", Some("TerminateMe"), &addr);
    client.command("Write-Output ('READY=' + $PID + ':terminate')");
    client.wait_for(|out, _| pid_marker(out, "READY=", ":terminate").is_some());
    let _connection = connections.recv_timeout(Duration::from_secs(5)).unwrap();
    client.detach();
    let output = Command::new(client_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home)
        .args(["terminate", "fixture", "terminateme", "--address", &addr])
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Session terminated; exit confirmed."));
    let mut later = fixture.client("connect", Some("TerminateMe"), &addr);
    later.wait_exit(1);
    assert!(later.err.contains("session has ended"), "{}", later.err);
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn server_inventory_and_exact_termination_preserve_neighbor_session() {
    use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::{OpenProcess, WaitForSingleObject}};
    struct Process(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Process { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
    let fixture = Fixture::new();
    let (addr, _connections) = relay(fixture.home.clone(), 11);
    let invoke = |args: &[&str]| Command::new(client_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home).args(args).output().unwrap();
    let inventory = || {
        let output = invoke(&["list", "--server", "fixture", "--address", &addr, "--json"]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let mut victim = fixture.client("connect", Some("Victim"), &addr);
    victim.command("$child=Start-Process powershell.exe -NoNewWindow -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 120' -PassThru; Write-Output ('CHILD='+$child.Id+':child'); Write-Output ('VICTIM='+$PID+':live')");
    victim.wait_for(|out, _| pid_marker(out, "VICTIM=", ":live").is_some());
    let child_pid: u32 = pid_marker(&victim.out, "CHILD=", ":child").unwrap().parse().unwrap();
    let child = Process(unsafe { OpenProcess(0x00100000, 0, child_pid) }); // SYNCHRONIZE, test-owned child only.
    assert!(!child.0.is_null());
    let victim_id = victim.guid();
    let mut neighbor = fixture.client("connect", Some("Neighbor"), &addr);
    neighbor.command("$Keep='neighbor'; Write-Output ('NEIGHBOR='+$PID+':alive')");
    neighbor.wait_for(|out, _| pid_marker(out, "NEIGHBOR=", ":alive").is_some());
    let neighbor_pid = pid_marker(&neighbor.out, "NEIGHBOR=", ":alive").unwrap();
    let list = inventory();
    assert_eq!(list["authorization_scope"], "host-windows-owner");
    assert_eq!(list["sessions"].as_array().unwrap().len(), 2);
    assert!(list["sessions"].as_array().unwrap().iter().any(|s| s["local_reference"] == "victim" && s["attached"] == true));
    let human = invoke(&["list", "--server", "fixture", "--address", &addr]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("Sessions on fixture") && human.contains("ATTACHED"));
    assert!(human.contains("victim") && human.contains(&victim_id) && !human.contains("\"sessions\""));
    let ended = invoke(&["terminate", "fixture", "VICTIM", "--json"]);
    assert!(ended.status.success(), "{} {}", String::from_utf8_lossy(&ended.stdout), String::from_utf8_lossy(&ended.stderr));
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&ended.stdout).unwrap()["status"], "terminated");
    victim.wait_exit(1);
    assert_eq!(unsafe { WaitForSingleObject(child.0, 0) }, 0, "session descendant survived confirmed termination");
    assert!(!inventory()["sessions"].as_array().unwrap().iter().any(|s| s["id"] == victim_id));
    neighbor.command("Write-Output ('AFTER='+$PID+':alive'); Write-Output $Keep");
    neighbor.wait_for(|out, _| pid_marker(out, "AFTER=", ":alive").is_some());
    assert_eq!(pid_marker(&neighbor.out, "AFTER=", ":alive").unwrap(), neighbor_pid);
    let mut detached = fixture.client("connect", Some("Dormant"), &addr);
    detached.command("Write-Output ('DETACHED='+$PID+':live')");
    detached.wait_for(|out, _| pid_marker(out, "DETACHED=", ":live").is_some());
    let detached_id = detached.guid();
    detached.detach();
    assert!(inventory()["sessions"].as_array().unwrap().iter().any(|s| s["id"] == detached_id && s["attached"] == false));
    let ended = invoke(&["terminate", "fixture", "dormant", "--address", &addr, "--json"]);
    assert!(ended.status.success(), "{}", String::from_utf8_lossy(&ended.stderr));
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&ended.stdout).unwrap()["status"], "terminated");
    assert_eq!(inventory()["sessions"].as_array().unwrap().len(), 1);
    assert!(!invoke(&["connect", "fixture", "victim", "--address", &addr, "--retries", "0"]).status.success());
    assert!(!invoke(&["connect", "fixture", "dormant", "--address", &addr, "--retries", "0"]).status.success());
    neighbor.detach();
    let unowned = fixture.home.join("inventory-only");
    fs::create_dir_all(unowned.join("client")).unwrap();
    fs::copy(fixture.home.join("client").join("config.json"), unowned.join("client").join("config.json")).unwrap();
    let output = Command::new(client_executable())
        .env("VSTERM_REMOTE_HOME", &unowned)
        .args(["list", "--server", "fixture", "--address", &addr, "--json"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let inventory: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inventory["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(inventory["sessions"][0]["attached"], false);
    assert_eq!(inventory["sessions"][0]["local_reference"], serde_json::Value::Null);
    assert_eq!(inventory["sessions"][0]["has_local_recovery_record"], false);
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
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
