use super::*;
use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;

struct Link {
    pair: host_pipe::PipePair,
    frames: Frames,
    last_sent: Instant,
    last_request: String,
    last_received: String,
    heartbeat: bool,
    nonce: u64,
    pongs: u64,
}
impl Link {
    fn connect(root: &Path) -> Self {
        Self {
            pair: host_pipe::connect(&pipe_name(root).unwrap(), Duration::from_secs(3)).unwrap(),
            frames: Frames::default(),
            last_sent: Instant::now(),
            last_request: "connect".into(),
            last_received: "none".into(),
            heartbeat: true,
            nonce: 0,
            pongs: 0,
        }
    }
    fn send(&mut self, value: Value) {
        let kind = text(&value, "type").unwrap();
        if !matches!(kind, "Ping" | "Pong") { self.last_request = kind.into(); }
        self.pair
            .input
            .write_all(&wire::encode(&value).unwrap())
            .unwrap();
        self.pair.input.flush().unwrap();
        self.last_sent = Instant::now();
    }
    fn recv(&mut self) -> Value {
        self.recv_result().unwrap_or_else(|error| panic!(
            "broker receive failed after request={}, last_frame={}, heartbeats={}, pongs={}: {error:#}",
            self.last_request, self.last_received, self.nonce, self.pongs))
    }
    fn recv_result(&mut self) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            ensure!(Instant::now() < deadline, "broker response deadline exceeded");
            if self.heartbeat && self.last_sent.elapsed() >= Duration::from_secs(5) {
                self.nonce += 1;
                self.send(message("Ping", map(vec![("nonce", self.nonce.into()), ("sent_at_ms", now_ms().into())])));
            }
            if let Some(value) = self.frames.next()? {
                let kind = text(&value, "type")?;
                self.last_received = kind.into();
                match kind {
                    "Pong" => {
                        let nonce = num(get(&value, "body")?, "nonce")?;
                        ensure!(nonce == self.pongs + 1 && nonce <= self.nonce, "unexpected heartbeat nonce");
                        self.pongs += 1;
                        continue;
                    }
                    "Ping" => {
                        self.send(message("Pong", map(vec![("nonce", get(get(&value, "body")?, "nonce")?.clone()),
                            ("broker_time_ms", now_ms().into())])));
                        continue;
                    }
                    _ => return Ok(value),
                }
            }
            let mut available = 0;
            let ok = unsafe { windows_sys::Win32::System::Pipes::PeekNamedPipe(
                self.pair.output.as_raw_handle(), std::ptr::null_mut(), 0, std::ptr::null_mut(),
                &mut available, std::ptr::null_mut()) };
            if ok == 0 { return Err(std::io::Error::last_os_error()).context("broker output pipe closed"); }
            if available == 0 {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            let mut bytes = [0; 8192];
            let limit = (available as usize).min(bytes.len());
            let count = self.pair.output.read(&mut bytes[..limit]).context("read broker frame")?;
            ensure!(count > 0, "broker output reached EOF");
            self.frames.push(&bytes[..count])?;
        }
    }
    fn hello(&mut self, client: &[u8]) -> Vec<u8> {
        self.send(message(
            "Hello",
            map(vec![
                ("min_version", 1.into()),
                ("max_version", 1.into()),
                ("client_version", s("test")),
                ("client_instance_id", Value::Binary(client.to_vec())),
                ("correlation_id", Value::Binary(vec![1; 16])),
                (
                    "capabilities",
                    Value::Array(CAPS.iter().map(|v| s(v)).collect()),
                ),
                ("max_receive_frame", (wire::MAX_FRAME as u64).into()),
            ]),
        ));
        let v = self.recv();
        assert_eq!(text(&v, "type").unwrap(), "HelloOk");
        binary(get(&v, "body").unwrap(), "broker_instance_id").unwrap()
    }
}
fn create(request: Uuid, id: Uuid, claim: &[u8], broker: &[u8]) -> Value {
    message(
        "CreateSession",
        map(vec![
            ("request_id", Value::Binary(request.as_bytes().to_vec())),
            ("requested_session_id", s(&id.to_string())),
            ("create_claim", Value::Binary(claim.to_vec())),
            ("origin_broker_instance_id", Value::Binary(broker.to_vec())),
            ("connection_epoch", 1.into()),
            ("shell", s("powershell.exe")),
            ("args", Value::Array(vec![s("-NoLogo"), s("-NoProfile")])),
            ("cwd", Value::Nil),
            ("env", map(vec![])),
            ("cols", 100.into()),
            ("rows", 30.into()),
            ("attach_mode", s("writer")),
            ("after_output_seq", 0.into()),
        ]),
    )
}
fn input(
    id: Uuid,
    attachment: &[u8],
    lease: &[u8],
    client: &[u8],
    seq: u64,
    bytes: &[u8],
) -> Value {
    message(
        "Input",
        map(vec![
            ("session_id", s(&id.to_string())),
            ("attachment_id", Value::Binary(attachment.to_vec())),
            ("lease_id", Value::Binary(lease.to_vec())),
            ("client_instance_id", Value::Binary(client.to_vec())),
            ("input_seq", seq.into()),
            ("bytes", Value::Binary(bytes.to_vec())),
        ]),
    )
}
fn marker(link: &mut Link, id: Uuid, cursor: &mut u64, needle: &str) -> String {
    let mut all = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(Instant::now() < deadline, "shell marker deadline exceeded; last_frame={}, pongs={}", link.last_received, link.pongs);
        let v = link.recv_result().unwrap_or_else(|error| {
            let tail = &all[all.len().saturating_sub(4096)..];
            panic!("shell marker receive failed after request={}, last_frame={}, pongs={}: {error:#}; output_tail={:?}",
                link.last_request, link.last_received, link.pongs, String::from_utf8_lossy(tail));
        });
        if text(&v, "type").unwrap() == "Error" {
            let body = get(&v, "body").unwrap();
            panic!("broker rejected marker wait after {}: code={}, detail={}", link.last_request,
                text(body, "code").unwrap(), text(body, "detail").unwrap());
        }
        if text(&v, "type").unwrap() == "Output" {
            let b = get(&v, "body").unwrap();
            assert_eq!(text(b, "session_id").unwrap(), id.to_string());
            let seq = num(b, "output_seq").unwrap();
            if seq > *cursor {
                *cursor = seq;
                all.extend(binary(b, "bytes").unwrap());
                assert!(all.len() <= wire::MAX_FRAME, "shell marker not found within bounded output");
            }
            let rendered = String::from_utf8_lossy(&all);
            if rendered.contains(needle) {
                return rendered.into_owned();
            }
        }
    }
}
fn pid(text: &str, prefix: &str) -> String {
    text.match_indices(prefix)
        .find_map(|(start, _)| {
            let rest = &text[start + prefix.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() && rest[digits.len()..].starts_with(":resumable") {
                Some(digits)
            } else {
                None
            }
        })
        .unwrap()
}

struct TestBroker {
    root: PathBuf,
    server: Option<thread::JoinHandle<Result<()>>>,
}
impl TestBroker {
    fn start() -> Self {
        let root = std::env::temp_dir().join(format!("arterm-host-liveness-test-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let server_root = root.clone();
        let broker = Self { root, server: Some(thread::spawn(move || run_at(server_root))) };
        let deadline = Instant::now() + Duration::from_secs(10);
        while control_at(&broker.root, "status", None, false).is_err() {
            assert!(Instant::now() < deadline, "owned broker did not start");
            thread::sleep(Duration::from_millis(10));
        }
        broker
    }
    fn finish(&mut self) -> Result<()> {
        let Some(server) = self.server.as_ref() else { return Ok(()) };
        if !server.is_finished() { stop_at(&self.root, true)?; }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server.is_finished() {
            ensure!(Instant::now() < deadline, "owned broker teardown timed out");
            thread::sleep(Duration::from_millis(10));
        }
        self.server.take().unwrap().join().map_err(|_| anyhow::anyhow!("owned broker panicked"))??;
        fs::remove_dir_all(&self.root)?;
        Ok(())
    }
}
impl Drop for TestBroker {
    fn drop(&mut self) {
        if let Err(error) = self.finish() { eprintln!("owned broker cleanup failed: {error:#}"); }
    }
}

#[test]
fn real_conpty_pid_and_state_survive_pipe_disconnect() {
    let mut fixture = TestBroker::start();
    let root = fixture.root.clone();
    let id = Uuid::now_v7();
    let request = Uuid::now_v7();
    let client = Uuid::now_v7().as_bytes().to_vec();
    let claim = vec![9; 32];
    let mut one = Link::connect(&root);
    let broker = one.hello(&client);
    one.send(create(request, id, &claim, &broker));
    let made = one.recv();
    assert_eq!(text(&made, "type").unwrap(), "SessionCreated");
    let body = get(&made, "body").unwrap();
    let token = binary(body, "resume_token").unwrap();
    let attach = binary(body, "attachment_id").unwrap();
    let lease = binary(body, "lease_id").unwrap();
    // Simulate a quiet/cold shell for longer than the production watchdog.
    // The test peer must maintain liveness; increasing the watchdog would hide this bug.
    let quiet_command = format!("Start-Sleep -Seconds {}; $global:KeepMe='resumable'; Write-Output ('FIRST=' + $PID + ':' + $global:KeepMe)\r\n",
        PEER_IDLE_TIMEOUT.as_secs() + 1);
    let quiet_started = Instant::now();
    one.send(input(
        id,
        &attach,
        &lease,
        &client,
        1,
        quiet_command.as_bytes(),
    ));
    let mut cursor = 0;
    let first = marker(&mut one, id, &mut cursor, ":resumable");
    assert!(quiet_started.elapsed() >= PEER_IDLE_TIMEOUT, "regression did not cross the real watchdog interval");
    assert!(one.pongs > 0, "slow marker wait must exchange real heartbeats");
    let shell_pid = pid(&first, "FIRST=");
    assert!(first.contains(&format!("FIRST={shell_pid}:resumable")));
    drop(one);
    let mut two = Link::connect(&root);
    assert_eq!(two.hello(&client), broker);
    two.send(create(request, id, &claim, &broker));
    let recovered = two.recv();
    assert_eq!(text(&recovered, "type").unwrap(), "SessionCreated");
    assert_eq!(
        get(get(&recovered, "body").unwrap(), "recovered")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    two.send(message(
        "AttachSession",
        map(vec![
            ("session_id", s(&id.to_string())),
            ("resume_token", Value::Binary(token)),
            ("mode", s("writer")),
            ("takeover", false.into()),
            ("after_output_seq", cursor.into()),
            ("client_instance_id", Value::Binary(client.clone())),
            ("connection_epoch", 2.into()),
            ("last_input_ack", 1.into()),
            ("cols", 100.into()),
            ("rows", 30.into()),
        ]),
    ));
    let attached = two.recv();
    assert_eq!(text(&attached, "type").unwrap(), "SessionAttached");
    let body = get(&attached, "body").unwrap();
    let attach2 = binary(body, "attachment_id").unwrap();
    let lease2 = binary(body, "lease_id").unwrap();
    assert_eq!(num(body, "input_committed_through").unwrap(), 1);
    two.send(input(
        id,
        &attach2,
        &lease2,
        &client,
        2,
        b"Write-Output ('SECOND=' + $PID + ':' + $global:KeepMe)\r\n",
    ));
    let second = marker(&mut two, id, &mut cursor, ":resumable");
    assert!(second.contains(&format!("SECOND={shell_pid}:resumable")));
    let sessions = control_at(&root, "sessions", None, false).unwrap();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
    assert!(control_at(&root, "stop", None, false).is_err());
    control_at(&root, "terminate", Some(&id.to_string()), false).unwrap();
    drop(two);
    fixture.finish().unwrap();
}

#[test]
fn silent_peer_is_closed_at_unchanged_watchdog_without_wire_error() {
    let mut fixture = TestBroker::start();
    let mut link = Link::connect(&fixture.root);
    link.heartbeat = false;
    link.hello(Uuid::now_v7().as_bytes());
    let started = Instant::now();
    assert!(link.recv_result().is_err(), "idle peer must close, not send a fatal protocol Error");
    assert!(started.elapsed() >= PEER_IDLE_TIMEOUT - Duration::from_secs(1));
    assert!(started.elapsed() < PEER_IDLE_TIMEOUT + Duration::from_secs(10));
    drop(link);
    fixture.finish().unwrap();
}

#[test]
fn stop_wakeup_closing_does_not_fail_broker_shutdown() {
    for _ in 0..32 {
        let mut fixture = TestBroker::start();
        fixture.finish().unwrap();
        assert!(!fixture.root.join("host").join("broker.json").exists());
    }
}

#[test]
fn setup_scope_routes_distinct_data_roots_without_starting_a_host() {
    let root = std::env::temp_dir().join(format!("arterm-setup-scope-{}", Uuid::now_v7()));
    let one = pipe_name(&root.join("one")).unwrap();
    let two = pipe_name(&root.join("two")).unwrap();
    assert_ne!(one, two);
    assert_eq!(one, pipe_name(&root.join("one")).unwrap());
    assert!(!root.exists());
}

#[test]
fn stop_is_successful_when_scoped_host_is_absent() {
    let root = std::env::temp_dir().join(format!("devbox-host-absent-{}", Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    stop_at(&root, false).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

fn short_create(request: Uuid, id: Uuid, broker: &[u8]) -> Value {
    message(
        "CreateSession",
        map(vec![
            ("request_id", Value::Binary(request.as_bytes().to_vec())),
            ("requested_session_id", s(&id.to_string())),
            ("create_claim", Value::Binary(vec![3; 32])),
            ("origin_broker_instance_id", Value::Binary(broker.to_vec())),
            ("connection_epoch", 1.into()),
            ("shell", s("powershell.exe")),
            (
                "args",
                Value::Array(vec![
                    s("-NoLogo"),
                    s("-NoProfile"),
                    s("-Command"),
                    s("exit 0"),
                ]),
            ),
            ("cwd", Value::Nil),
            ("env", map(vec![])),
            ("cols", 80.into()),
            ("rows", 24.into()),
            ("attach_mode", s("writer")),
            ("after_output_seq", 0.into()),
        ]),
    )
}

#[test]
fn seventeen_completed_shells_do_not_exhaust_live_limit() {
    let root = std::env::temp_dir().join(format!("devbox-host-limit-{}", Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    let broker = Broker::new(root.clone(), pipe_name(&root).unwrap()).unwrap();
    let client = Uuid::now_v7().as_bytes().to_vec();
    for _ in 0..17 {
        let id = Uuid::now_v7();
        let request = Uuid::now_v7();
        let create = short_create(request, id, &broker.instance);
        let (session, recovered, _) = broker
            .create(get(&create, "body").unwrap(), &client)
            .unwrap();
        assert!(!recovered);
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.state.lock().unwrap().exit.is_none() {
            assert!(Instant::now() < deadline, "short-lived shell did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }
    drop(broker);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exited_history_cleanup_expires_oldest_and_enforces_cap() {
    let now = Instant::now();
    let old = Uuid::now_v7();
    let recent = (0..4).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
    let mut entries = vec![(old, now - Duration::from_secs(901))];
    entries.extend(
        recent
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, now - Duration::from_secs((4 - index) as u64))),
    );
    let removed = exited_history_removals(entries, now, Duration::from_secs(900), 2);
    assert!(removed.contains(&old));
    assert!(removed.contains(&recent[0]));
    assert!(removed.contains(&recent[1]));
    assert!(!removed.contains(&recent[2]));
    assert!(!removed.contains(&recent[3]));
}

fn live_create(request: Uuid, id: Uuid, broker: &[u8]) -> Value {
    message(
        "CreateSession",
        map(vec![
            ("request_id", Value::Binary(request.as_bytes().to_vec())),
            ("requested_session_id", s(&id.to_string())),
            ("create_claim", Value::Binary(vec![4; 32])),
            ("origin_broker_instance_id", Value::Binary(broker.to_vec())),
            ("connection_epoch", 1.into()),
            ("shell", s("powershell.exe")),
            (
                "args",
                Value::Array(vec![
                    s("-NoLogo"),
                    s("-NoProfile"),
                    s("-Command"),
                    s("Start-Sleep -Seconds 30"),
                ]),
            ),
            ("cwd", Value::Nil),
            ("env", map(vec![])),
            ("cols", 80.into()),
            ("rows", 24.into()),
            ("attach_mode", s("writer")),
            ("after_output_seq", 0.into()),
        ]),
    )
}

#[test]
fn sixteen_live_sessions_enforce_admission_limit() {
    let root = std::env::temp_dir().join(format!("devbox-host-live-limit-{}", Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    let broker = Broker::new(root.clone(), pipe_name(&root).unwrap()).unwrap();
    let client = Uuid::now_v7().as_bytes().to_vec();
    let mut live = Vec::new();
    for _ in 0..MAX_SESSIONS {
        let create = live_create(Uuid::now_v7(), Uuid::now_v7(), &broker.instance);
        live.push(
            broker
                .create(get(&create, "body").unwrap(), &client)
                .unwrap()
                .0,
        );
    }
    let rejected = live_create(Uuid::now_v7(), Uuid::now_v7(), &broker.instance);
    assert!(format!(
        "{:#}",
        broker
            .create(get(&rejected, "body").unwrap(), &client)
            .err()
            .unwrap()
    )
    .contains("SessionLimit"));
    for session in &live {
        session.terminate().unwrap();
    }
    drop(live);
    drop(broker);
    fs::remove_dir_all(root).unwrap();
}
