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
struct Pending(Option<Child>);
impl Pending {
    fn output(mut self) -> std::process::Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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
    pub(crate) host: Child,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let home = std::env::temp_dir().join(format!("devbox-native-e2e-{}", Uuid::now_v7()));
        fs::create_dir_all(&home).unwrap();
        // Hosted runners may spell TEMP with an 8.3 alias; receipts use the
        // filesystem's resolved name, so establish the same fixture spelling.
        let resolved = fs::canonicalize(&home).unwrap();
        let text = resolved.to_str().unwrap();
        let home = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text));
        let host = Command::new(host_executable())
            .arg("run")
            .env("VSTERM_REMOTE_HOME", &home)
            .env("TEMP", &home).env("TMP", &home)
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
            let output = self.host_command("sessions").arg("--json").output().unwrap();
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
            .env("TEMP", &self.home).env("TMP", &self.home)
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
    relay_with_creation_gate(home, attachments, trace, None)
}
#[derive(Default)]
struct CreationGate {
    frame: &'static str,
    observed: std::sync::atomic::AtomicBool,
    release: std::sync::atomic::AtomicBool,
    occurrence: usize,
    seen: std::sync::atomic::AtomicUsize,
    hide_file_capability: bool,
    hide_metadata_capability: bool,
    hide_directory_capability: bool,
    hide_recipient_capability: bool,
    corrupt_recipient_receipt: bool,
    hold_session_exit: bool,
    exit_observed: std::sync::atomic::AtomicBool,
    exit_release: std::sync::atomic::AtomicBool,
    file_result: Mutex<Option<serde_json::Value>>,
    file_progress: Mutex<Vec<(String, Instant)>>,
}
fn relay_with_creation_gate(home: PathBuf, attachments: usize, trace: Option<Arc<Mutex<SyntheticInputTrace>>>,
    gate: Option<Arc<CreationGate>>) -> (String, mpsc::Receiver<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for _ in 0..attachments {
            let (mut incoming, _) = listener.accept().unwrap();
            let home = home.clone();
            let tx = tx.clone();
            let trace = trace.clone();
            let gate = gate.clone();
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
                let gate = gate.clone();
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
                            while let Some(mut frame) = frames.next().unwrap() {
                                if let Some(gate) = &gate {
                                    if gate.corrupt_recipient_receipt
                                        && get(&frame, "type").as_str() == Some("FileResult") {
                                        if let Value::Map(fields) = &mut frame {
                                            if let Some((_, Value::Map(body))) = fields.iter_mut().find(|(k, _)| k.as_str() == Some("body")) {
                                                if let Some((_, json)) = body.iter_mut().find(|(k, _)| k.as_str() == Some("json")) {
                                                    let mut value: serde_json::Value = serde_json::from_str(json.as_str().unwrap()).unwrap();
                                                    if gate.corrupt_recipient_receipt && value["recipient_metadata"].is_object() {
                                                        value["recipient_metadata"]["zone_identifier_absent"] = false.into();
                                                    }
                                                    *json = s(&serde_json::to_string(&value).unwrap());
                                                }
                                            }
                                        }
                                    }
                                    if get(&frame, "type").as_str() == Some("FileProgress") {
                                        gate.file_progress.lock().unwrap().push((
                                            get(get(&frame, "body"), "phase").as_str().unwrap().into(), Instant::now()));
                                    }
                                    if gate.hold_session_exit && get(&frame, "type").as_str() == Some("SessionExited") {
                                        use std::sync::atomic::Ordering;
                                        gate.exit_observed.store(true, Ordering::Release);
                                        let deadline = Instant::now() + Duration::from_secs(15);
                                        while !gate.exit_release.load(Ordering::Acquire) {
                                            assert!(Instant::now() < deadline, "owned exit gate was not released");
                                            thread::sleep(Duration::from_millis(10));
                                        }
                                    }
                                    if (gate.hide_file_capability || gate.hide_metadata_capability || gate.hide_directory_capability || gate.hide_recipient_capability) && get(&frame, "type").as_str() == Some("HelloOk") {
                                        if let Value::Map(fields) = &mut frame {
                                            if let Some((_, Value::Map(body))) = fields.iter_mut().find(|(k, _)| k.as_str() == Some("body")) {
                                                if let Some((_, Value::Array(caps))) = body.iter_mut().find(|(k, _)| k.as_str() == Some("capabilities")) {
                                                    caps.retain(|v| !(gate.hide_file_capability && v.as_str() == Some(arterm::transfer_admission::CAPABILITY))
                                                        && !(gate.hide_metadata_capability && v.as_str() == Some(arterm::transfer_payload::METADATA_CAPABILITY))
                                                        && !(gate.hide_recipient_capability && v.as_str() == Some(arterm::recipient_metadata::CAPABILITY))
                                                        && !(gate.hide_directory_capability && v.as_str() == Some(arterm::transfer_payload::DIRECTORY_CAPABILITY)));
                                                }
                                            }
                                        }
                                    }
                                    if get(&frame, "type").as_str() == Some(gate.frame) {
                                        use std::sync::atomic::Ordering;
                                        if gate.seen.fetch_add(1, Ordering::AcqRel) == gate.occurrence {
                                        if get(&frame, "type").as_str() == Some("FileResult") {
                                            let json = get(get(&frame, "body"), "json").as_str().unwrap();
                                            *gate.file_result.lock().unwrap() = Some(serde_json::from_str(json).unwrap());
                                        }
                                        gate.observed.store(true, Ordering::Release);
                                        let deadline = Instant::now() + Duration::from_secs(15);
                                        while !gate.release.load(Ordering::Acquire) {
                                            assert!(Instant::now() < deadline, "owned {} gate was not released", gate.frame);
                                            thread::sleep(Duration::from_millis(10));
                                        }
                                        }
                                    }
                                }
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

fn zone_bytes(path: &std::path::Path) -> Option<Vec<u8>> {
    let mut ads = path.as_os_str().to_os_string();
    ads.push(":Zone.Identifier");
    match fs::read(ads) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.raw_os_error() == Some(2) => None,
        Err(error) => panic!("read owned fixture Zone.Identifier: {error}"),
    }
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn recipient_metadata_all_types_both_directions_and_folder_files() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let (address, _) = relay(fixture.home.clone(), 29);
    let mut owner = fixture.client("connect", Some("zones"), &address);
    owner.command("Write-Output ('READY=' + $PID + ':zones')");
    owner.wait_for(|out, _| pid_marker(out, "READY=", ":zones").is_some());
    let transfer = |direction: &str, source: &std::path::Path, files: u64| {
        let mut command = Command::new(controller_executable());
        command.env("VSTERM_REMOTE_HOME", &fixture.home)
            .args([direction, "fixture", "zones", "--file", source.to_str().unwrap(), "--json"]);
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{} {}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["status"], "completed");
        assert_eq!(receipt["recipient_metadata"]["files"], files);
        assert_eq!(receipt["recipient_metadata"]["zone_identifier_absent"], true);
        let destination = PathBuf::from(receipt["actual_path"].as_str().unwrap());
        assert!(destination.starts_with(&fixture.home) && destination != source);
        (destination, receipt)
    };
    for direction in ["send", "receive"] {
            for zone in [None, Some(3), Some(4)] {
                for extension in ["txt", "ps1", "exe", "zip"] {
                    let source = fixture.home.join(format!("{direction}-{zone:?}.{extension}"));
                    let bytes = if extension == "zip" { vec![80, 75, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] }
                        else { b"dummy bytes; never execute\r\n\0\xff".to_vec() };
                    fs::write(&source, &bytes).unwrap();
                    if let Some(zone) = zone {
                        fs::write(format!("{}:Zone.Identifier", source.display()),
                            format!("[ZoneTransfer]\r\nZoneId={zone}\r\nHostUrl=https://fixture.invalid/private\r\n")).unwrap();
                    }
                    let original_zone = zone_bytes(&source);
                    fs::write(format!("{}:unrelated", source.display()), b"source only").unwrap();
                    let (destination, receipt) = transfer(direction, &source, 1);
                    assert_eq!(receipt["source_kind"], "file");
                    assert!(receipt.get("source_zone").is_none());
                    assert_eq!(receipt["sha256"], format!("{:x}", Sha256::digest(&bytes)));
                    assert_eq!(fs::read(&destination).unwrap(), bytes);
                    assert_eq!(fs::read(&source).unwrap(), bytes);
                    assert_eq!(zone_bytes(&source), original_zone);
                    assert_eq!(fs::read(format!("{}:unrelated", source.display())).unwrap(), b"source only");
                    assert_eq!(zone_bytes(&destination), None);
                }
            }
            let source = fixture.home.join(format!("folder-{direction}"));
            fs::create_dir_all(source.join("nested").join("empty")).unwrap();
            let names = ["plain.txt", "nested\\script.ps1", "nested\\dummy.exe",
                "nested\\ordinary.zip", "nested\\\u{65e5}\u{672c}.txt", "zero"];
            for (index, name) in names.iter().enumerate() {
                let path = source.join(name);
                fs::write(&path, if index == 5 { &b""[..] } else { &b"folder bytes"[..] }).unwrap();
                if index % 2 == 0 {
                    fs::write(format!("{}:Zone.Identifier", path.display()), b"[ZoneTransfer]\r\nZoneId=4\r\n").unwrap();
                }
            }
            let before: Vec<_> = names.iter().map(|name| zone_bytes(&source.join(name))).collect();
            let (destination, receipt) = transfer(direction, &source, names.len() as u64);
            assert_eq!(receipt["source_kind"], "directory");
            assert!(destination.join("nested").join("empty").is_dir());
            for (index, name) in names.iter().enumerate() {
                assert_eq!(Sha256::digest(fs::read(destination.join(name)).unwrap()),
                    Sha256::digest(fs::read(source.join(name)).unwrap()));
                assert_eq!(zone_bytes(&source.join(name)), before[index]);
                assert_eq!(zone_bytes(&destination.join(name)), None);
            }
            let empty = fixture.home.join(format!("empty-{direction}"));
            fs::create_dir(&empty).unwrap();
            let (destination, _) = transfer(direction, &empty, 0);
            assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }
    owner.detach();
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn recipient_metadata_old_host_rejected_before_staging() {
    let fixture = Fixture::new();
    let gate = Arc::new(CreationGate { hide_recipient_capability: true, ..Default::default() });
    let (address, _) = relay_with_creation_gate(fixture.home.clone(), 3, None, Some(gate));
    let mut owner = fixture.client("connect", Some("old-zone"), &address);
    owner.command("Write-Output ('READY=' + $PID + ':old-zone')");
    owner.wait_for(|out, _| pid_marker(out, "READY=", ":old-zone").is_some());
    let source = fixture.home.join("owned.txt");
    fs::write(&source, b"not transferred").unwrap();
    for direction in ["send", "receive"] {
            let mut command = Command::new(controller_executable());
            command.env("VSTERM_REMOTE_HOME", &fixture.home)
                .args([direction, "fixture", "old-zone", "--file", source.to_str().unwrap(), "--json"]);
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stdout).contains("recipient-unblock-v1"));
            assert!(owned_partials(&fixture.home).is_empty());
            assert_eq!(zone_bytes(&source), None);
    }
    owner.detach();
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn recipient_metadata_unconfirmed_receipt_is_unknown_and_preserves_data() {
        let fixture = Fixture::new();
        let gate = Arc::new(CreationGate { corrupt_recipient_receipt: true, ..Default::default() });
        let (address, _) = relay_with_creation_gate(fixture.home.clone(), 2, None, Some(gate));
        let mut owner = fixture.client("connect", Some("policy"), &address);
        owner.command("Write-Output ('READY=' + $PID + ':policy')");
        owner.wait_for(|out, _| pid_marker(out, "READY=", ":policy").is_some());
        let source = fixture.home.join("owned.txt");
        fs::write(&source, b"fixture").unwrap();
        let output = Command::new(controller_executable()).env("VSTERM_REMOTE_HOME", &fixture.home)
            .args(["send", "fixture", "policy", "--file", source.to_str().unwrap(), "--json"])
            .output().unwrap();
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let destination = PathBuf::from(response["actual_path"].as_str().unwrap());
        assert_eq!(zone_bytes(&destination), None);
        assert_eq!(zone_bytes(&source), None);
            assert_eq!(output.status.code(), Some(6), "{response}");
            assert_eq!(response["status"], "unknown");
            assert!(response["recipient_metadata"].is_null());
        owner.detach();
        assert_eq!(fs::read(destination).unwrap(), b"fixture");
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn recipient_metadata_marked_stage_injection_and_io_failure_both_directions() {
    use std::{os::windows::fs::OpenOptionsExt, sync::atomic::Ordering};
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_DELETE};
    for direction in ["send", "receive"] {
        for locked in [false, true] {
            let fixture = Fixture::new();
            let gate = Arc::new(CreationGate { frame: "FileResult",
                occurrence: if direction == "send" { 0 } else { 1 }, ..Default::default() });
            struct Release(Arc<CreationGate>);
            impl Drop for Release { fn drop(&mut self) { self.0.release.store(true, Ordering::Release); } }
            let _release = Release(gate.clone());
            let (address, _) = relay_with_creation_gate(fixture.home.clone(), 2, None, Some(gate.clone()));
            let mut owner = fixture.client("connect", Some("io-zone"), &address);
            owner.command("Write-Output ('READY=' + $PID + ':io-zone')");
            owner.wait_for(|out, _| pid_marker(out, "READY=", ":io-zone").is_some());
            let source = fixture.home.join("owned.txt");
            fs::write(&source, b"fixture").unwrap();
            let mut command = Command::new(controller_executable());
            command.env("VSTERM_REMOTE_HOME", &fixture.home)
                .args([direction, "fixture", "io-zone", "--file", source.to_str().unwrap(), "--json"])
                .stdout(Stdio::piped()).stderr(Stdio::piped());
            let pending = Pending(Some(command.spawn().unwrap()));
            let deadline = Instant::now() + Duration::from_secs(10);
            while !gate.observed.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "receiver staging gate was not reached");
                thread::sleep(Duration::from_millis(20));
            }
            let partials = owned_partials(&fixture.home);
            assert_eq!(partials.len(), 1);
            let destination = partials[0].parent().unwrap().join("owned.txt");
            let ads = format!("{}:Zone.Identifier", partials[0].display());
            fs::write(&ads, b"[ZoneTransfer]\r\nZoneId=4\r\n").unwrap();
            fs::write(format!("{}:unrelated", partials[0].display()), b"keep receiver stream").unwrap();
            assert!(zone_bytes(&partials[0]).is_some(), "fixture must inject a real receiver mark");
            let lock = locked.then(|| fs::OpenOptions::new().read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE).open(&ads).unwrap());
            gate.release.store(true, Ordering::Release);
            let output = pending.output();
            let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            if locked {
                assert!(!output.status.success(), "metadata failure claimed completion");
                assert_ne!(response["status"], "completed", "{response}");
                assert!(response["recipient_metadata"].is_null());
                assert!(response["error"].as_str().unwrap().contains("metadata"), "{response}");
                assert!(!destination.exists());
            } else {
                assert!(output.status.success(), "{response}");
                assert_eq!(response["recipient_metadata"]["zone_identifier_absent"], true);
                assert_eq!(zone_bytes(&destination), None);
                assert_eq!(fs::read(&destination).unwrap(), b"fixture");
                assert_eq!(fs::read(format!("{}:unrelated", destination.display())).unwrap(), b"keep receiver stream");
            }
            assert_eq!(zone_bytes(&source), None);
            assert_eq!(fs::read(&source).unwrap(), b"fixture");
            drop(lock);
            owner.detach();
            assert_eq!(destination.exists(), !locked);
        }
    }
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn single_file_roundtrip_current_owner_busy_command_and_binary_integrity() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let (address, connections) = relay(fixture.home.clone(), 10);
    let mut client = fixture.client("connect", Some("files"), &address);
    client.command("$global:FileState='retained'; Write-Output ('READY=' + $PID + ':files')");
    client.wait_for(|out, _| pid_marker(out, "READY=", ":files").is_some());
    let pid = pid_marker(&client.out, "READY=", ":files").unwrap();
    let _connection = connections.recv_timeout(Duration::from_secs(10)).unwrap();
    let control = |args: &[&str]| Command::new(controller_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home)
        .env("TEMP", &fixture.home).env("TMP", &fixture.home)
        .args(args).output().unwrap();
    let slow = control(&["send", "fixture", "files", "--command",
        "1..100 | ForEach-Object { Write-Output ('BUSY=' + $_); Start-Sleep -Milliseconds 300 }", "--json"]);
    assert!(slow.status.success(), "{}", String::from_utf8_lossy(&slow.stdout));
    let mut completed = 0;
    for (index, bytes) in [
        Vec::new(), vec![0, 255, 13, 10, 27, 128],
        (0..3 * 1024 * 1024 + 17).map(|i| (i % 251) as u8).collect(),
        vec![80, 75, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ].into_iter().enumerate() {
        let basename = if index == 3 { "ordinary.zip".into() } else { format!("binary-{index}.bin") };
        let path = fixture.home.join(&basename);
        fs::write(&path, &bytes).unwrap();
        let sent = control(&["send", "fixture", "files", "--file", path.to_str().unwrap(), "--json"]);
        assert_eq!(sent.status.code(), Some(0), "{} {}", String::from_utf8_lossy(&sent.stdout), String::from_utf8_lossy(&sent.stderr));
        let sent: serde_json::Value = serde_json::from_slice(&sent.stdout).unwrap();
        assert_eq!(sent["status"], "completed");
        assert_eq!(sent["source_kind"], "file");
        assert_eq!(sent["original_basename"], basename);
        assert!(sent["command_id"].is_null());
        let remote = PathBuf::from(sent["actual_path"].as_str().unwrap());
        assert!(remote.is_absolute() && remote.starts_with(&fixture.home));
        assert_eq!(fs::read(&remote).unwrap(), bytes);
        let received = control(&["receive", "fixture", "files", "--file", remote.to_str().unwrap(), "--json"]);
        assert_eq!(received.status.code(), Some(0), "{} {}", String::from_utf8_lossy(&received.stdout), String::from_utf8_lossy(&received.stderr));
        let received: serde_json::Value = serde_json::from_slice(&received.stdout).unwrap();
        assert_eq!(received["source_kind"], "file");
        assert_eq!(received["original_basename"], basename);
        let local = PathBuf::from(received["actual_path"].as_str().unwrap());
        assert!(local.starts_with(&fixture.home) && local != remote && local != path);
        assert_eq!(fs::read(&local).unwrap(), bytes);
        let hash = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(sent["sha256"], hash);
        assert_eq!(received["sha256"], hash);
        assert_eq!(received["bytes"], bytes.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        completed += 2;
    }
    assert_eq!(completed, 8);
    let output = control(&["read", "fixture", "files", "--json"]);
    assert!(output.status.success() && String::from_utf8_lossy(&output.stdout).contains("BUSY="));
    let interrupted = control(&["interrupt", "fixture", "files", "--json"]);
    assert!(interrupted.status.success());
    let after = control(&["send", "fixture", "files", "--command",
        "Write-Output ('AFTER=' + $PID + ':' + $global:FileState)", "--wait", "--timeout", "20s", "--json"]);
    assert!(after.status.success(), "{}", String::from_utf8_lossy(&after.stdout));
    client.wait_for(|out, _| pid_marker(out, "AFTER=", ":retained").is_some());
    assert_eq!(pid_marker(&client.out, "AFTER=", ":retained").unwrap(), pid);
    client.detach();
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn folder_roundtrip_snapshot_busy_owner_and_empty_directory() {
    use std::sync::atomic::Ordering;
    for direction in ["send", "receive"] {
        let fixture = Fixture::new();
        let gate = Arc::new(CreationGate { frame: "FileResult", ..Default::default() });
        let (address, _) = relay_with_creation_gate(fixture.home.clone(), 4, None, Some(gate.clone()));
        let mut owner = fixture.client("connect", Some("folders"), &address);
        owner.command("$global:FolderState='retained'; Write-Output ('READY=' + $PID + ':folders')");
        owner.wait_for(|out, _| pid_marker(out, "READY=", ":folders").is_some());
        let pid = pid_marker(&owner.out, "READY=", ":folders").unwrap();
        let control = |args: &[&str]| Command::new(controller_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args(args).output().unwrap();
        assert!(control(&["send", "fixture", "folders", "--command",
            "1..100 | ForEach-Object { Write-Output ('BUSY=' + $_); Start-Sleep -Milliseconds 100 }",
            "--json"]).status.success());
        let source = fixture.home.join("snapshot");
        fs::create_dir_all(source.join("nested").join("empty")).unwrap();
        fs::create_dir(source.join(".hidden-directory")).unwrap();
        let unicode = source.join("nested").join("\u{65e5}\u{672c}.txt");
        fs::write(&unicode, b"old snapshot").unwrap();
        fs::write(source.join(".hidden"), b"hidden").unwrap();
        use std::os::windows::ffi::OsStrExt;
        for name in [".hidden", ".hidden-directory"] {
            let hidden: Vec<u16> = source.join(name).as_os_str().encode_wide().chain(Some(0)).collect();
            assert_ne!(unsafe { windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(
                hidden.as_ptr(), windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN) }, 0);
        }
        let child = Command::new(controller_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args([direction, "fixture", "folders", "--file", source.to_str().unwrap(), "--json"])
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let pending = Pending(Some(child));
        let deadline = Instant::now() + Duration::from_secs(15);
        while !gate.observed.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "folder preparation gate not reached");
            thread::sleep(Duration::from_millis(20));
        }
        // FileBegin's result is after packing in both directions.
        fs::write(&unicode, b"edited after packing").unwrap();
        fs::write(source.join("new-after-pack.txt"), b"not in snapshot").unwrap();
        gate.release.store(true, Ordering::Release);
        let output = pending.output();
        assert!(output.status.success(), "{} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["source_kind"], "directory");
        assert_eq!(receipt["original_basename"], "snapshot");
        assert_eq!(receipt["extracted_bytes"], 18);
        let destination = PathBuf::from(receipt["actual_path"].as_str().unwrap());
        assert_eq!(destination.file_name().unwrap(), "snapshot");
        assert!(destination.starts_with(&fixture.home));
        assert_eq!(fs::read(destination.join("nested").join("\u{65e5}\u{672c}.txt")).unwrap(), b"old snapshot");
        assert!(destination.join("nested").join("empty").is_dir());
        assert!(destination.join(".hidden-directory").is_dir());
        assert_eq!(fs::read(destination.join(".hidden")).unwrap(), b"hidden");
        assert!(!destination.join("new-after-pack.txt").exists());
        assert_eq!(fs::read(&unicode).unwrap(), b"edited after packing");
        let empty = fixture.home.join("empty-folder");
        fs::create_dir(&empty).unwrap();
        let output = control(&[direction, "fixture", "folders", "--file", empty.to_str().unwrap(), "--json"]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["extracted_bytes"], 0);
        assert!(PathBuf::from(receipt["actual_path"].as_str().unwrap()).is_dir());
        assert!(String::from_utf8_lossy(&control(&["read", "fixture", "folders", "--json"]).stdout).contains("BUSY="));
        assert!(control(&["interrupt", "fixture", "folders", "--json"]).status.success());
        assert!(control(&["send", "fixture", "folders", "--command",
            "Write-Output ('AFTER=' + $PID + ':' + $global:FolderState)", "--wait", "--timeout", "20s", "--json"]).status.success());
        owner.wait_for(|out, _| pid_marker(out, "AFTER=", ":retained").is_some());
        assert_eq!(pid_marker(&owner.out, "AFTER=", ":retained").unwrap(), pid);
        owner.detach();
    }
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn folder_old_capability_fails_before_publication() {
    let fixture = Fixture::new();
    let gate = Arc::new(CreationGate { hide_directory_capability: true, ..Default::default() });
    let (address, _) = relay_with_creation_gate(fixture.home.clone(), 3, None, Some(gate));
    let mut owner = fixture.client("connect", Some("legacy-folder"), &address);
    owner.command("Write-Output ('READY=' + $PID + ':legacy')");
    owner.wait_for(|out, _| pid_marker(out, "READY=", ":legacy").is_some());
    let source = fixture.home.join("folder-source");
    fs::create_dir(&source).unwrap();
    for direction in ["send", "receive"] {
        let output = Command::new(controller_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args([direction, "fixture", "legacy-folder", "--file", source.to_str().unwrap(), "--json"])
            .output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stdout));
        assert!(String::from_utf8_lossy(&output.stdout).contains("unsupported"));
        assert!(owned_partials(&fixture.home).is_empty());
    }
    owner.detach();
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required; 512 MiB local fixture"]
fn folder_native_long_preparation_progress_and_caller_cancellation() {
    let fixture = Fixture::new();
    let gate = Arc::new(CreationGate::default());
    let (address, _) = relay_with_creation_gate(fixture.home.clone(), 2, None, Some(gate.clone()));
    let mut owner = fixture.client("connect", Some("long-folder"), &address);
    owner.command("Write-Output ('READY=' + $PID + ':long')");
    owner.wait_for(|out, _| pid_marker(out, "READY=", ":long").is_some());
    let source = fixture.home.join("large-folder");
    fs::create_dir(&source).unwrap();
    let mut state = 123456789u32;
    let data: Vec<u8> = (0..1024 * 1024).map(|_| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as u8
    }).collect();
    for index in 0..64 {
        let mut file = fs::File::create(source.join(format!("{index}.bin"))).unwrap();
        for _ in 0..8 { file.write_all(&data).unwrap(); }
    }
    let mut controller = Pending(Some(Command::new(controller_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home)
        .args(["receive", "fixture", "long-folder", "--file", source.to_str().unwrap(), "--json"])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()));
    let started = Instant::now();
    loop {
        let progress = gate.file_progress.lock().unwrap();
        let preparing: Vec<_> = progress.iter().filter(|(phase, _)| phase == "preparing").collect();
        if preparing.len() >= 5 {
            assert!(preparing.last().unwrap().1.duration_since(preparing[0].1) >= Duration::from_secs(20));
            break;
        }
        drop(progress);
        assert!(started.elapsed() < Duration::from_secs(90), "native preparation did not emit five progress frames");
        assert!(controller.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "native controller ended before long archive phase completed");
        thread::sleep(Duration::from_millis(50));
    }
    controller.0.as_mut().unwrap().kill().unwrap();
    controller.0.as_mut().unwrap().wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let stages = fs::read_dir(&fixture.home).unwrap().filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with("arterm-archive-"));
        if !stages { break; }
        assert!(Instant::now() < deadline, "cancelled archive preparation left an owned stage");
        thread::sleep(Duration::from_millis(50));
    }
    assert!(owned_partials(&fixture.home).is_empty());
    assert_eq!(fs::metadata(source.join("0.bin")).unwrap().len(), 8 * 1024 * 1024);
    owner.command("Write-Output ('AFTER=' + $PID + ':long')");
    owner.wait_for(|out, _| pid_marker(out, "AFTER=", ":long").is_some());
    owner.detach();
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn folder_lease_change_revokes_transfer_without_replacing_shell() {
    use std::sync::atomic::Ordering;
    for direction in ["send", "receive"] {
        let fixture = Fixture::new();
        let gate = Arc::new(CreationGate { frame: "FileResult", occurrence: 1, ..Default::default() });
        let (address, connections) = relay_with_creation_gate(fixture.home.clone(), 3, None, Some(gate.clone()));
        let mut owner = fixture.client("connect", Some("lease-folder"), &address);
        owner.command("Write-Output ('READY=' + $PID + ':lease')");
        owner.wait_for(|out, _| pid_marker(out, "READY=", ":lease").is_some());
        let pid = pid_marker(&owner.out, "READY=", ":lease").unwrap();
        let primary = connections.recv_timeout(Duration::from_secs(10)).unwrap();
        let source = fixture.home.join("lease-source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data.bin"), b"snapshot").unwrap();
        let mut controller = Pending(Some(Command::new(controller_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args([direction, "fixture", "lease-folder", "--file", source.to_str().unwrap(), "--json"])
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()));
        let _file_link = connections.recv_timeout(Duration::from_secs(10)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !gate.observed.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "folder chunk was not gated");
            thread::sleep(Duration::from_millis(20));
        }
        let partials = owned_partials(&fixture.home);
        assert_eq!(partials.len(), 1);
        let destination = partials[0].parent().unwrap().join("lease-source");
        primary.shutdown(Shutdown::Both).unwrap();
        let _replacement = connections.recv_timeout(Duration::from_secs(20)).unwrap();
        gate.release.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(10);
        while controller.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "old transfer survived attachment replacement");
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!controller.output().status.success());
        let deadline = Instant::now() + Duration::from_secs(8);
        while !owned_partials(&fixture.home).is_empty() {
            assert!(Instant::now() < deadline, "revoked folder partial remained");
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!destination.exists());
        assert_eq!(fs::read(source.join("data.bin")).unwrap(), b"snapshot");
        owner.command("Write-Output ('AFTER=' + $PID + ':lease')");
        owner.wait_for(|out, _| pid_marker(out, "AFTER=", ":lease").is_some());
        assert_eq!(pid_marker(&owner.out, "AFTER=", ":lease").unwrap(), pid);
        owner.detach();
    }
}

fn owned_partials(root: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            found.extend(owned_partials(&entry.path()));
        } else if entry.file_name() == ".arterm-partial" {
            found.push(entry.path());
        }
    }
    found
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn single_file_caller_abort_and_transport_disconnect_clean_only_partials() {
    caller_abort_and_transport_disconnect(false);
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn folder_caller_abort_and_transport_disconnect_clean_only_partials() {
    caller_abort_and_transport_disconnect(true);
}

fn caller_abort_and_transport_disconnect(folder: bool) {
    use std::sync::atomic::Ordering;
    struct Release(Arc<CreationGate>);
    impl Drop for Release {
        fn drop(&mut self) { self.0.release.store(true, Ordering::Release); }
    }
    for direction in ["send", "receive"] {
        for caller_abort in [true, false] {
            let fixture = Fixture::new();
            let gate = Arc::new(CreationGate { frame: "FileResult", occurrence: 2, ..Default::default() });
            let _release = Release(gate.clone());
            let (address, connections) = relay_with_creation_gate(fixture.home.clone(), 2, None, Some(gate.clone()));
            let mut owner = fixture.client("connect", Some("cancel"), &address);
            owner.command("Write-Output ('READY=' + $PID + ':cancel')");
            owner.wait_for(|out, _| pid_marker(out, "READY=", ":cancel").is_some());
            let _primary = connections.recv_timeout(Duration::from_secs(10)).unwrap();
            let source = fixture.home.join(if folder { "keep-folder" } else { "keep-source.bin" });
            let data_path = if folder {
                fs::create_dir(&source).unwrap();
                source.join("data.bin")
            } else { source.clone() };
            let mut state = 123456789u32;
            let data: Vec<u8> = (0..1024 * 1024).map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            }).collect();
            fs::write(&data_path, &data).unwrap();
            fs::write(format!("{}:Zone.Identifier", data_path.display()), b"[ZoneTransfer]\r\nZoneId=4\r\n").unwrap();
            let mut command = Command::new(controller_executable());
            command.env("VSTERM_REMOTE_HOME", &fixture.home)
                .args([direction, "fixture", "cancel", "--file", source.to_str().unwrap(), "--json"])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
            let mut controller = command.spawn().unwrap();
            let transfer_link = connections.recv_timeout(Duration::from_secs(10)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !gate.observed.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "file chunk gate never reached");
                assert!(controller.try_wait().unwrap().is_none(), "controller exited before file chunk gate");
                thread::sleep(Duration::from_millis(20));
            }
            let partials = owned_partials(&fixture.home);
            assert_eq!(partials.len(), 1, "{direction} must have one owned staging file");
            let destination = partials[0].parent().unwrap().join(source.file_name().unwrap());
            assert!(!destination.exists());
            if caller_abort {
                controller.kill().unwrap();
                controller.wait().unwrap();
            } else {
                transfer_link.shutdown(Shutdown::Both).unwrap();
                let output = controller.wait_with_output().unwrap();
                assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stdout));
            }
            let deadline = Instant::now() + Duration::from_secs(8);
            while !owned_partials(&fixture.home).is_empty() {
                assert!(Instant::now() < deadline, "abandoned file partial not cleaned");
                thread::sleep(Duration::from_millis(20));
            }
            assert!(!destination.exists(), "cancelled transfer must not publish");
            assert_eq!(fs::read(&data_path).unwrap(), data);
            assert_eq!(zone_bytes(&data_path), Some(b"[ZoneTransfer]\r\nZoneId=4\r\n".to_vec()));
            gate.release.store(true, Ordering::Release);
            owner.command("Write-Output ('AFTER=' + $PID + ':cancel')");
            owner.wait_for(|out, _| pid_marker(out, "AFTER=", ":cancel").is_some());
            owner.detach();
        }
    }
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn single_file_old_host_is_unsupported_without_staging() {
    for hide_metadata in [false, true] {
    let fixture = Fixture::new();
    let gate = Arc::new(CreationGate {
        hide_file_capability: !hide_metadata, hide_metadata_capability: hide_metadata, ..Default::default()
    });
    let (address, _) = relay_with_creation_gate(fixture.home.clone(), 1, None, Some(gate));
    let mut owner = fixture.client("connect", Some("legacy"), &address);
    owner.command("Write-Output ('READY=' + $PID + ':legacy')");
    owner.wait_for(|out, _| pid_marker(out, "READY=", ":legacy").is_some());
    let source = fixture.home.join("source.bin");
    fs::write(&source, b"legacy").unwrap();
    for direction in ["send", "receive"] {
        let output = Command::new(controller_executable())
            .env("VSTERM_REMOTE_HOME", &fixture.home)
            .args([direction, "fixture", "legacy", "--file", source.to_str().unwrap(), "--json"])
            .output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stdout).contains("unsupported"));
        assert!(owned_partials(&fixture.home).is_empty());
    }
    owner.command("Write-Output ('AFTER=' + $PID + ':legacy')");
    owner.wait_for(|out, _| pid_marker(out, "AFTER=", ":legacy").is_some());
    owner.detach();
    }
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn single_file_lost_commit_receipt_is_unknown_and_preserves_published_file() {
    use std::sync::atomic::Ordering;
    struct Release(Arc<CreationGate>);
    impl Drop for Release {
        fn drop(&mut self) { self.0.release.store(true, Ordering::Release); }
    }
    let fixture = Fixture::new();
    let gate = Arc::new(CreationGate { frame: "FileResult", occurrence: 2, ..Default::default() });
    let _release = Release(gate.clone());
    let (address, connections) = relay_with_creation_gate(fixture.home.clone(), 2, None, Some(gate.clone()));
    let mut owner = fixture.client("connect", Some("commit"), &address);
    owner.command("Write-Output ('READY=' + $PID + ':commit')");
    owner.wait_for(|out, _| pid_marker(out, "READY=", ":commit").is_some());
    let _primary = connections.recv_timeout(Duration::from_secs(10)).unwrap();
    let source = fixture.home.join("commit.bin");
    fs::write(&source, b"abc").unwrap();
    fs::write(format!("{}:Zone.Identifier", source.display()), b"[ZoneTransfer]\r\nZoneId=4\r\n").unwrap();
    let controller = Command::new(controller_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home)
        .args(["send", "fixture", "commit", "--file", source.to_str().unwrap(), "--json"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let transfer_link = connections.recv_timeout(Duration::from_secs(10)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gate.observed.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "commit receipt gate never reached");
        thread::sleep(Duration::from_millis(20));
    }
    transfer_link.shutdown(Shutdown::Both).unwrap();
    let output = controller.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(6), "{}", String::from_utf8_lossy(&output.stdout));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["status"], "unknown");
    assert_eq!(response["commit_started"], true);
    assert_eq!(response["automatic_retry"], false);
    assert_eq!(response["transfer_id"], response["remote_transfer_id"]);
    let destination = PathBuf::from(response["actual_path"].as_str().unwrap());
    assert!(destination.starts_with(&fixture.home) && destination != source);
    assert_eq!(fs::read(&destination).unwrap(), b"abc");
    assert_eq!(fs::read(&source).unwrap(), b"abc");
    assert_eq!(zone_bytes(&source), Some(b"[ZoneTransfer]\r\nZoneId=4\r\n".to_vec()));
    assert_eq!(zone_bytes(&destination), None);
    assert!(response["recipient_metadata"].is_null(), "lost receipt cannot confirm applied policy");
    assert!(owned_partials(&fixture.home).is_empty());
    gate.release.store(true, Ordering::Release);
    owner.detach();
    assert_eq!(fs::read(&destination).unwrap(), b"abc");
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn single_file_confirmed_termination_revokes_pending_receive() {
    confirmed_termination_revokes_pending_receive(false);
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn folder_confirmed_termination_revokes_pending_receive() {
    confirmed_termination_revokes_pending_receive(true);
}

fn confirmed_termination_revokes_pending_receive(folder: bool) {
    use std::sync::atomic::Ordering;
    struct Release(Arc<CreationGate>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release.store(true, Ordering::Release);
            self.0.exit_release.store(true, Ordering::Release);
        }
    }
    let fixture = Fixture::new();
    let gate = Arc::new(CreationGate {
        frame: "FileResult", occurrence: 2, hold_session_exit: true, ..Default::default()
    });
    let _release = Release(gate.clone());
    let (address, _) = relay_with_creation_gate(fixture.home.clone(), 3, None, Some(gate.clone()));
    let mut owner = fixture.client("connect", Some("end-race"), &address);
    owner.command("Write-Output ('READY=' + $PID + ':end-race')");
    owner.wait_for(|out, _| pid_marker(out, "READY=", ":end-race").is_some());
    let source = fixture.home.join("empty.bin");
    if folder { fs::create_dir(&source).unwrap(); }
    else { fs::write(&source, b"").unwrap(); }
    let mut controller = Pending(Some(Command::new(controller_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home)
        .args(["receive", "fixture", "end-race", "--file", source.to_str().unwrap(), "--json"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gate.observed.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "FileClose receipt was not gated");
        assert!(controller.0.as_mut().unwrap().try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(10));
    }
    let terminated = Command::new(controller_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home)
        .args(["terminate", "fixture", "end-race", "--json"]).output().unwrap();
    assert!(terminated.status.success(), "{} {}", String::from_utf8_lossy(&terminated.stdout), String::from_utf8_lossy(&terminated.stderr));
    let terminated: serde_json::Value = serde_json::from_slice(&terminated.stdout).unwrap();
    assert_eq!(terminated["status"], "terminated");
    let exit_deadline = Instant::now() + Duration::from_secs(3);
    while !gate.exit_observed.load(Ordering::Acquire) {
        assert!(Instant::now() < exit_deadline, "SessionExited was not gated");
        thread::sleep(Duration::from_millis(10));
    }
    gate.release.store(true, Ordering::Release);
    let response_deadline = Instant::now() + Duration::from_secs(5);
    while controller.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(Instant::now() < response_deadline, "revoked receive did not finish");
        thread::sleep(Duration::from_millis(10));
    }
    let response = controller.output();
    assert_eq!(response.status.code(), Some(1), "{} {}", String::from_utf8_lossy(&response.stdout), String::from_utf8_lossy(&response.stderr));
    let response: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(response["status"], "error");
    assert_eq!(response["commit_started"], false);
    assert!(!PathBuf::from(response["actual_path"].as_str().unwrap()).exists());
    assert!(owned_partials(&fixture.home).is_empty());
    assert_eq!(source.is_dir(), folder);
    assert!(source.exists());
    gate.exit_release.store(true, Ordering::Release);
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn single_file_owner_death_reports_unknown_without_waiting_for_transfer_deadline() {
    use std::sync::atomic::Ordering;
    struct Release(Arc<CreationGate>);
    impl Drop for Release {
        fn drop(&mut self) { self.0.release.store(true, Ordering::Release); }
    }
    let fixture = Fixture::new();
    let gate = Arc::new(CreationGate { frame: "FileResult", occurrence: 2, ..Default::default() });
    let _release = Release(gate.clone());
    let (address, _) = relay_with_creation_gate(fixture.home.clone(), 2, None, Some(gate.clone()));
    let mut owner = fixture.client("connect", Some("owner-loss"), &address);
    owner.command("Write-Output ('READY=' + $PID + ':owner-loss')");
    owner.wait_for(|out, _| pid_marker(out, "READY=", ":owner-loss").is_some());
    let source = fixture.home.join("committed.bin");
    fs::write(&source, b"abc").unwrap();
    let mut controller = Pending(Some(Command::new(controller_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home)
        .args(["send", "fixture", "owner-loss", "--file", source.to_str().unwrap(), "--json"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gate.observed.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "committed receipt was not gated");
        thread::sleep(Duration::from_millis(10));
    }
    let receipt = gate.file_result.lock().unwrap().clone().unwrap();
    let destination = PathBuf::from(receipt["actual_path"].as_str().unwrap());
    assert_eq!(fs::read(&destination).unwrap(), b"abc");
    owner.child.kill().unwrap();
    owner.child.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while controller.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "controller kept waiting after owner died");
        thread::sleep(Duration::from_millis(10));
    }
    let response = controller.output();
    assert_eq!(response.status.code(), Some(6), "{} {}", String::from_utf8_lossy(&response.stdout), String::from_utf8_lossy(&response.stderr));
    let response: serde_json::Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(response["status"], "unknown");
    assert_eq!(response["operation_kind"], "file_transfer");
    assert_eq!(response["commit_may_have_started"], true);
    assert_eq!(response["automatic_retry"], false);
    assert_eq!(fs::read(&destination).unwrap(), b"abc");
    gate.release.store(true, Ordering::Release);
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
        let rejected = control(&["send", "fixture", &reference, "--command", "Get-Date", "--timeout", "1s", "--json"]);
        assert_eq!(rejected.status.code(), Some(124));
        let rejected: serde_json::Value = serde_json::from_slice(&rejected.stdout).unwrap();
        assert_eq!(rejected["submitted"], false);
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
        let busy_id = Uuid::now_v7().to_string();
        let busy = control(&["send", "fixture", &reference, "--command",
            "while ($true) { Start-Sleep -Milliseconds 100 }", "--command-id", &busy_id, "--json"]);
        assert!(busy.status.success(), "{}", String::from_utf8_lossy(&busy.stdout));
        let running_deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let query = control(&["read", "fixture", &reference, "--command-id", &busy_id, "--json"]);
            assert!(query.status.success(), "{} {}", String::from_utf8_lossy(&query.stdout), String::from_utf8_lossy(&query.stderr));
            let query: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
            if query["record"]["state"] == "running" { break; }
            assert!(Instant::now() < running_deadline, "busy command never started: {query}");
            thread::sleep(Duration::from_millis(20));
        }
        writer.lock().unwrap().write_all(b"\x1b[O\x1b[I").unwrap();
        let rejected = control(&["send", "fixture", &reference, "--command", "Get-Date", "--timeout", "1s", "--json"]);
        assert_eq!(rejected.status.code(), Some(124));
        let rejected: serde_json::Value = serde_json::from_slice(&rejected.stdout).unwrap();
        assert_eq!(rejected["shell_status"], "busy");
        assert_eq!(rejected["phase"], "readiness");
        assert_eq!(rejected["submitted"], false);
        let interrupted = control(&["interrupt", "fixture", &reference, "--json"]);
        assert!(interrupted.status.success(), "interrupt: {} {}",
            String::from_utf8_lossy(&interrupted.stdout), String::from_utf8_lossy(&interrupted.stderr));
        let completion_deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let query = control(&["read", "fixture", &reference, "--command-id", &busy_id, "--json"]);
            assert!(query.status.success(), "{} {}", String::from_utf8_lossy(&query.stdout), String::from_utf8_lossy(&query.stderr));
            let query: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
            if query["record"]["state"] == "completed" {
                assert_eq!(query["record"]["interrupt_requested"], true);
                break;
            }
            assert!(Instant::now() < completion_deadline, "interrupted command did not complete: {query}");
            thread::sleep(Duration::from_millis(20));
        }
        assert!(control(&["detach", "fixture", &reference]).status.success());
        client.0.wait().unwrap();
        drop(writer);
        drop(pair.master);
        reader_thread.join().unwrap();
    }
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn local_shell_accepts_command_after_detach_failure_and_remote_exit() {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    struct Shell(Box<dyn portable_pty::Child + Send + Sync>);
    impl Drop for Shell {
        fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
    }
    let fixture = Fixture::new();
    let (address, _) = relay(fixture.home.clone(), 2);
    for outcome in ["detach", "failure", "exit"] {
        let connected = outcome != "failure";
        let rejected = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = if connected { address.clone() } else { rejected.local_addr().unwrap().to_string() };
        // An owned listener explicitly closes the failure connection; no external port.
        let reject = if connected { None } else { Some(thread::spawn(move || {
            let (socket, _) = rejected.accept().unwrap();
            socket.shutdown(Shutdown::Both).unwrap();
        })) };
        let pair = native_pty_system().openpty(PtySize { rows: 40, cols: 160, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut command = CommandBuilder::new("cmd.exe");
        command.env("VSTERM_REMOTE_HOME", &fixture.home);
        command.env("VSTERM_HISTORY_PATH", fixture.home.join("exit-history.txt"));
        command.cwd(&fixture.home);
        command.args(["/d", "/q", "/v:on"]);
        let mut shell = Shell(pair.slave.spawn_command(command).unwrap());
        drop(pair.slave);
        let mut writer = pair.master.take_writer().unwrap();
        writer.write_all(format!(
            "\"{}\" connect fixture {} --address {} --retries 0 --shell \"{}\" & echo CLIENT-EXIT=!errorlevel!\r",
            client_executable(), Uuid::now_v7(), endpoint, test_shell()).as_bytes()).unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(n) = reader.read(&mut bytes) {
                if n == 0 || tx.send(bytes[..n].to_vec()).is_err() { break; }
            }
        });
        let mut terminal = vt100::Parser::new(40, 160, 0);
        let mut tail = Vec::new();
        let mut wait = |writer: &mut Box<dyn Write + Send>, expected: &str| {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let bytes = rx.recv_timeout(Duration::from_millis(500)).unwrap_or_default();
                terminal.process(&bytes);
                tail.extend_from_slice(&bytes);
                for query in [b"\x1b[6n".as_slice(), b"\x1b[c", b"\x1b[>c"] {
                    if tail.windows(query.len()).any(|b| b == query) {
                        writer.write_all(match query {
                            b"\x1b[6n" => b"\x1b[1;1R",
                            b"\x1b[c" => b"\x1b[?1;2c",
                            _ => b"\x1b[>0;10;1c",
                        }).unwrap();
                    }
                }
                if tail.len() > 2 { tail.drain(..tail.len() - 2); }
                let screen = terminal.screen().contents();
                if screen.lines().any(|line| line.trim() == expected) { break; }
                assert!(Instant::now() < deadline, "waiting for {expected:?}; screen={screen:?}");
            }
        };
        if connected {
            // Wait for the remote PowerShell prompt before changing remote modes.
            thread::sleep(Duration::from_millis(1000));
            writer.write_all(b"[Console]::Write(([char]27+'[?9001h'+[char]27+'[?1004h'+[char]27+'[?2004h'+[char]27+'[?1003h')); Write-Output 'REMOTE-VT-READY'\r").unwrap();
            wait(&mut writer, "REMOTE-VT-READY");
            writer.write_all(if outcome == "detach" {
                b"\x1d\x1b[221;27;29;0;8;1_\x1b[I\x1b[123;_"
            } else { b"exit 7\r" }).unwrap();
        }
        wait(&mut writer, match outcome {
            "detach" => "CLIENT-EXIT=0",
            "failure" => "CLIENT-EXIT=1",
            _ => "CLIENT-EXIT=7",
        });
        writer.write_all(b"echo POST-EXIT-OK\r").unwrap();
        wait(&mut writer, "POST-EXIT-OK");
        writer.write_all(b"exit 0\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = shell.0.try_wait().unwrap() { assert!(status.success()); break; }
            assert!(Instant::now() < deadline, "local shell did not exit");
            thread::sleep(Duration::from_millis(10));
        }
        drop(writer);
        drop(pair.master);
        reader_thread.join().unwrap();
        if let Some(reject) = reject { reject.join().unwrap(); }
    }
}

#[test]
#[ignore = "explicit functional fixture feature or trusted SIGNED_CLIENT/SIGNED_HOST required"]
fn send_waits_for_readiness_and_never_runs_cancelled_or_expired_work() {
    use std::sync::atomic::Ordering;
    struct Release(Arc<CreationGate>);
    impl Drop for Release { fn drop(&mut self) { self.0.release.store(true, Ordering::Release); } }
    for frame in ["HelloOk", "SessionCreated"] {
    let fixture = Fixture::new();
    let gate = Arc::new(CreationGate { frame, ..CreationGate::default() });
    let _release = Release(gate.clone());
    let (address, _) = relay_with_creation_gate(fixture.home.clone(), 1, None, Some(gate.clone()));
    let mut owner = fixture.client("connect", Some("ReadyWait"), &address);
    let control = |args: &[&str]| Command::new(controller_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home).args(args).output().unwrap();
    let spawn = |args: &[&str]| Pending(Some(Command::new(controller_executable())
        .env("VSTERM_REMOTE_HOME", &fixture.home).args(args)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()));
    let inspect = || {
        let output = control(&["list","--client","--json"]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gate.observed.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "{frame} handshake was not observed");
        thread::sleep(Duration::from_millis(10));
    }
    if frame == "HelloOk" {
        let owners = inspect();
        assert_eq!(owners[0]["connection_state"], "connecting");
        assert_eq!(owners[0]["command_capability"], false);
        assert!(owners[0]["command_execution"].is_null());
        let started = Instant::now();
        let expired = control(&["send","fixture","readywait","--command","$RunCount=999","--timeout","1s","--json"]);
        assert_eq!(expired.status.code(), Some(124), "pre-HelloOk: {} {}", String::from_utf8_lossy(&expired.stdout), String::from_utf8_lossy(&expired.stderr));
        assert!(started.elapsed() >= Duration::from_millis(900) && started.elapsed() < Duration::from_secs(3));
        let expired: serde_json::Value = serde_json::from_slice(&expired.stdout).unwrap();
        assert_eq!(expired["phase"], "readiness");
        assert_eq!(expired["submitted"], false);
    }
    let initial = spawn(&["send","fixture","readywait","--command","$RunCount=1","--wait","--timeout","10s","--json"]);
    while inspect()[0]["control_waiters"].as_u64().unwrap() == 0 {
        assert!(Instant::now() < deadline);
    }
    assert_eq!(inspect()[0]["connection_state"], "connecting");
    assert!(control(&["read","fixture","readywait","--json"]).status.success());
    gate.release.store(true, Ordering::Release);
    let initial = initial.output();
    assert!(initial.status.success(), "{} {}", String::from_utf8_lossy(&initial.stdout), String::from_utf8_lossy(&initial.stderr));

    let first = control(&["send","fixture","readywait","--command","Start-Sleep -Seconds 3; $RunCount++","--json"]);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stdout));
    let second = spawn(&["send","fixture","readywait","--command","$RunCount++","--wait","--timeout","10s","--json"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while inspect()[0]["control_waiters"].as_u64().unwrap() == 0 {
        assert!(Instant::now() < deadline);
    }
    assert!(control(&["read","fixture","readywait","--json"]).status.success(), "waiting blocked other controls");
    let second = second.output();
    assert!(second.status.success(), "{} {}", String::from_utf8_lossy(&second.stdout), String::from_utf8_lossy(&second.stderr));

    owner.input.write_all(b"$Partial='").unwrap();
    owner.input.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while inspect()[0]["readiness_reason"] != "partial_human_input" { assert!(Instant::now() < deadline); }
    let expired = control(&["send","fixture","readywait","--command","$RunCount=999","--timeout","1s","--json"]);
    assert_eq!(expired.status.code(), Some(124), "{}", String::from_utf8_lossy(&expired.stdout));
    let expired: serde_json::Value = serde_json::from_slice(&expired.stdout).unwrap();
    assert_eq!(expired["phase"], "readiness");
    assert_eq!(expired["submitted"], false);
    owner.command("kept'; Write-Output ('PARTIAL='+$Partial)");
    owner.wait_for(|out, _| out.contains("PARTIAL=kept"));
    let query = control(&["read","fixture","readywait","--command-id",expired["command_id"].as_str().unwrap(),"--json"]);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&query.stdout).unwrap()["host"]["lookup"], "unknown");

    let deadline = Instant::now() + Duration::from_secs(10);
    while inspect()[0]["shell_status"] != "ready" { assert!(Instant::now() < deadline); }
    owner.input.write_all(b"$Cancel='").unwrap();
    owner.input.flush().unwrap();
    while inspect()[0]["readiness_reason"] != "partial_human_input" { assert!(Instant::now() < deadline); }
    let abandoned_id = Uuid::now_v7().to_string();
    let mut abandoned = spawn(&["send","fixture","readywait","--command","$RunCount=888","--command-id",&abandoned_id,"--json"]);
    while inspect()[0]["control_waiters"].as_u64().unwrap() == 0 { assert!(Instant::now() < deadline); }
    abandoned.0.as_mut().unwrap().kill().unwrap();
    abandoned.0.as_mut().unwrap().wait().unwrap();
    while inspect()[0]["control_waiters"].as_u64().unwrap() != 0 { assert!(Instant::now() < deadline); }
    owner.command("kept'; Write-Output ('CANCEL='+$Cancel)");
    owner.wait_for(|out, _| out.contains("CANCEL=kept"));
    let query = control(&["read","fixture","readywait","--command-id",&abandoned_id,"--json"]);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&query.stdout).unwrap()["host"]["lookup"], "unknown");
    let proof = control(&["send","fixture","readywait","--command",
        "if ($RunCount -ne 3 -or $Partial -ne 'kept' -or $Cancel -ne 'kept') { throw 'queued or partial input was changed' }",
        "--wait","--timeout","10s","--json"]);
    assert!(proof.status.success(), "{} {}", String::from_utf8_lossy(&proof.stdout), String::from_utf8_lossy(&proof.stderr));
    owner.detach();
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
        let timed_busy = control(&["send", "fixture", "automation", "--command", "$Counter=999", "--timeout", "1s", "--json"]);
        assert_eq!(timed_busy.status.code(), Some(124));
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&timed_busy.stdout).unwrap()["submitted"], false);
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
