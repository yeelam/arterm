use arterm::wire::{self, binary, get, map, message, num, s, text};
use rmpv::Value;
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Host {
    home: PathBuf,
    child: Child,
}
impl Host {
    fn start() -> Self {
        let home = std::env::temp_dir().join(format!("devbox-host-core-{}", Uuid::now_v7()));
        fs::create_dir_all(&home).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
            .arg("run")
            .env("VSTERM_REMOTE_HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut host = Self { home, child };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if host.child.try_wait().unwrap().is_some() {
                panic!("host exited before ready");
            }
            let status = host.command(&["status"]);
            if status.status.success() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "host did not become ready: {}",
                String::from_utf8_lossy(&status.stderr)
            );
            thread::sleep(Duration::from_millis(50));
        }
        host
    }
    fn bridge(&self) -> Bridge {
        Bridge::start(&self.home)
    }
    fn command(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_arterm-host"))
            .args(args)
            .env("VSTERM_REMOTE_HOME", &self.home)
            .output()
            .unwrap()
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.command(&["stop", "--terminate-sessions"]);
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}
struct Bridge {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}
impl Bridge {
    fn start(home: &PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
            .args(["bridge", "--protocol", "vsterm-session-v1"])
            .env("VSTERM_REMOTE_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        Self {
            child,
            input: Some(input),
            output,
        }
    }
    fn send(&mut self, value: Value) {
        let input = self.input.as_mut().unwrap();
        input.write_all(&wire::encode(&value).unwrap()).unwrap();
        input.flush().unwrap();
    }
    fn recv(&mut self) -> Value {
        let mut len = [0u8; 4];
        self.output.read_exact(&mut len).unwrap();
        let mut bytes = vec![0u8; u32::from_be_bytes(len) as usize];
        self.output.read_exact(&mut bytes).unwrap();
        rmpv::decode::read_value(&mut &bytes[..]).unwrap()
    }
    fn hello(&mut self, client: &[u8]) -> Vec<u8> {
        self.send(message(
            "Hello",
            map(vec![
                ("min_version", 1.into()),
                ("max_version", 1.into()),
                ("client_version", s("test")),
                ("client_instance_id", Value::Binary(client.to_vec())),
                ("correlation_id", Value::Binary(vec![8; 16])),
                ("capabilities", Value::Array(vec![s("client-session-id")])),
                ("max_receive_frame", (wire::MAX_FRAME as u64).into()),
            ]),
        ));
        let reply = self.recv();
        assert_eq!(text(&reply, "type").unwrap(), "HelloOk");
        let body = get(&reply, "body").unwrap();
        assert!(num(body, "max_input_window_bytes").unwrap() >= 4096);
        binary(body, "broker_instance_id").unwrap()
    }
    fn close_input(&mut self) {
        self.input.take();
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn writer_message(
    id: Uuid,
    attachment: &[u8],
    lease: &[u8],
    client: &[u8],
    kind: &str,
    extra: Vec<(&str, Value)>,
) -> Value {
    let mut fields = vec![
        ("session_id", s(&id.to_string())),
        ("attachment_id", Value::Binary(attachment.to_vec())),
        ("lease_id", Value::Binary(lease.to_vec())),
        ("client_instance_id", Value::Binary(client.to_vec())),
    ];
    fields.extend(extra);
    message(kind, map(fields))
}
fn collect_marker(bridge: &mut Bridge, id: Uuid, after: &mut u64, needle: &str) -> String {
    let mut all = Vec::new();
    loop {
        let value = bridge.recv();
        if text(&value, "type").unwrap() == "Output" {
            let body = get(&value, "body").unwrap();
            assert_eq!(text(body, "session_id").unwrap(), id.to_string());
            let seq = num(body, "output_seq").unwrap();
            if seq > *after {
                *after = seq;
                if let Value::Binary(bytes) = get(body, "bytes").unwrap() {
                    all.extend(bytes);
                }
            }
            let rendered = String::from_utf8_lossy(&all);
            if rendered.contains(needle) {
                return rendered.into_owned();
            }
        }
    }
}
fn marker_pid(text: &str, prefix: &str) -> String {
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
fn create_message(request: Uuid, id: Uuid, claim: &[u8], broker: &[u8]) -> Value {
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

#[test]
fn real_conpty_survives_bridge_loss_and_duplicate_create() {
    let host = Host::start();
    let id = Uuid::now_v7();
    let request = Uuid::now_v7();
    let client = Uuid::now_v7().as_bytes().to_vec();
    let claim = vec![7; 32];
    let mut first = host.bridge();
    let broker = first.hello(&client);
    first.send(create_message(request, id, &claim, &broker));
    let created = first.recv();
    assert_eq!(text(&created, "type").unwrap(), "SessionCreated");
    let body = get(&created, "body").unwrap();
    assert_eq!(get(body, "requires_attach").unwrap().as_bool(), Some(false));
    let token = binary(body, "resume_token").unwrap();
    let attachment = binary(body, "attachment_id").unwrap();
    let lease = binary(body, "lease_id").unwrap();
    first.send(writer_message(id,&attachment,&lease,&client,"Input",vec![("input_seq",1.into()),("bytes",Value::Binary(b"$global:KeepMe='resumable'; Write-Output ('FIRST=' + $PID + ':' + $global:KeepMe)\r\n".to_vec()))]));
    let mut cursor = 0;
    let first_text = collect_marker(&mut first, id, &mut cursor, ":resumable");
    let pid = marker_pid(&first_text, "FIRST=");
    assert!(!pid.is_empty());
    assert!(first_text.contains(&format!("FIRST={pid}:resumable")));
    first.close_input();
    drop(first);
    thread::sleep(Duration::from_millis(150));
    let mut second = host.bridge();
    assert_eq!(second.hello(&client), broker);
    second.send(create_message(request, id, &claim, &broker));
    let recovered = second.recv();
    assert_eq!(text(&recovered, "type").unwrap(), "SessionCreated");
    let recovered_body = get(&recovered, "body").unwrap();
    assert_eq!(
        get(recovered_body, "recovered").unwrap().as_bool(),
        Some(true)
    );
    assert_eq!(
        get(recovered_body, "requires_attach").unwrap().as_bool(),
        Some(true)
    );
    assert_eq!(binary(recovered_body, "resume_token").unwrap(), token);
    second.send(message(
        "AttachSession",
        map(vec![
            ("session_id", s(&id.to_string())),
            ("resume_token", Value::Binary(token.clone())),
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
    let attached = second.recv();
    assert_eq!(text(&attached, "type").unwrap(), "SessionAttached");
    let attached_body = get(&attached, "body").unwrap();
    let attachment2 = binary(attached_body, "attachment_id").unwrap();
    let lease2 = binary(attached_body, "lease_id").unwrap();
    assert_eq!(num(attached_body, "input_committed_through").unwrap(), 1);
    second.send(writer_message(
        id,
        &attachment2,
        &lease2,
        &client,
        "Input",
        vec![
            ("input_seq", 2.into()),
            (
                "bytes",
                Value::Binary(
                    b"Write-Output ('SECOND=' + $PID + ':' + $global:KeepMe)\r\n".to_vec(),
                ),
            ),
        ],
    ));
    let second_text = collect_marker(&mut second, id, &mut cursor, ":resumable");
    assert!(second_text.contains(&format!("SECOND={pid}:resumable")));
    let sessions = host.command(&["sessions"]);
    assert!(sessions.status.success());
    let listed = String::from_utf8_lossy(&sessions.stdout);
    assert_eq!(listed.matches(&id.to_string()).count(), 1);
    second.send(message(
        "Detach",
        map(vec![
            ("session_id", s(&id.to_string())),
            ("attachment_id", Value::Binary(attachment2)),
            ("last_output_ack", cursor.into()),
            ("last_input_ack", 2.into()),
            ("keep_running", true.into()),
        ]),
    ));
    let stopped = host.command(&["stop"]);
    assert!(!stopped.status.success());
    let mut terminator = host.bridge();
    assert_eq!(terminator.hello(&client), broker);
    terminator.send(message(
        "TerminateSession",
        map(vec![
            ("session_id", s(&id.to_string())),
            ("resume_token", Value::Binary(token)),
            ("grace_ms", 5000.into()),
            ("reason", s("explicit")),
        ]),
    ));
    let accepted = terminator.recv();
    assert_eq!(text(&accepted, "type").unwrap(), "TerminateAccepted");
    assert_eq!(
        text(get(&accepted, "body").unwrap(), "session_id").unwrap(),
        id.to_string()
    );
    drop(terminator);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let listed = host.command(&["sessions"]);
        let sessions: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        if sessions
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == id.to_string() && item["exited"] == true)
        {
            break;
        }
        assert!(Instant::now() < deadline, "terminated shell did not exit");
        thread::sleep(Duration::from_millis(25));
    }
    assert!(host.command(&["stop"]).status.success());
}

fn attach_message(id: Uuid, token: &[u8], client: &[u8], epoch: u64) -> Value {
    message(
        "AttachSession",
        map(vec![
            ("session_id", s(&id.to_string())),
            ("resume_token", Value::Binary(token.to_vec())),
            ("mode", s("writer")),
            ("takeover", false.into()),
            ("after_output_seq", 0.into()),
            ("client_instance_id", Value::Binary(client.to_vec())),
            ("connection_epoch", epoch.into()),
            ("last_input_ack", 0.into()),
            ("cols", 100.into()),
            ("rows", 30.into()),
        ]),
    )
}

#[test]
fn concurrent_identical_create_requests_spawn_one_session() {
    let host = Host::start();
    let id = Uuid::now_v7();
    let request = Uuid::now_v7();
    let client = Uuid::now_v7().as_bytes().to_vec();
    let claim = vec![11; 32];
    let mut first = host.bridge();
    let broker = first.hello(&client);
    let mut second = host.bridge();
    assert_eq!(second.hello(&client), broker);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let run = |mut bridge: Bridge, barrier: std::sync::Arc<std::sync::Barrier>, create: Value| {
        thread::spawn(move || {
            barrier.wait();
            bridge.send(create);
            bridge.recv()
        })
    };
    let first_run = run(
        first,
        barrier.clone(),
        create_message(request, id, &claim, &broker),
    );
    let second_run = run(
        second,
        barrier.clone(),
        create_message(request, id, &claim, &broker),
    );
    barrier.wait();
    let responses = [first_run.join().unwrap(), second_run.join().unwrap()];
    assert!(responses
        .iter()
        .all(|value| text(value, "type").unwrap() == "SessionCreated"));
    let recovered = responses
        .iter()
        .map(|value| {
            get(get(value, "body").unwrap(), "recovered")
                .unwrap()
                .as_bool()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(recovered.iter().filter(|value| !**value).count(), 1);
    assert_eq!(recovered.iter().filter(|value| **value).count(), 1);
    let sessions = host.command(&["sessions"]);
    let sessions: serde_json::Value = serde_json::from_slice(&sessions.stdout).unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    assert_eq!(sessions[0]["id"], id.to_string());
    let mut retry = host.bridge();
    assert_eq!(retry.hello(&client), broker);
    retry.send(create_message(request, id, &claim, &broker));
    let retry_response = retry.recv();
    assert_eq!(
        get(get(&retry_response, "body").unwrap(), "recovered")
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let cleanup = host.command(&["terminate", &id.to_string(), "--yes"]);
    assert!(
        cleanup.status.success(),
        "terminate failed: {}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
}

#[test]
fn detached_session_rejects_connection_epoch_rollback() {
    let host = Host::start();
    let id = Uuid::now_v7();
    let request = Uuid::now_v7();
    let client = Uuid::now_v7().as_bytes().to_vec();
    let claim = vec![12; 32];
    let mut created_link = host.bridge();
    let broker = created_link.hello(&client);
    created_link.send(create_message(request, id, &claim, &broker));
    let created = created_link.recv();
    let body = get(&created, "body").unwrap();
    let token = binary(body, "resume_token").unwrap();
    let attachment = binary(body, "attachment_id").unwrap();
    created_link.send(message(
        "Detach",
        map(vec![
            ("session_id", s(&id.to_string())),
            ("attachment_id", Value::Binary(attachment)),
            ("last_output_ack", 0.into()),
            ("last_input_ack", 0.into()),
            ("keep_running", true.into()),
        ]),
    ));
    drop(created_link);
    let mut newer = host.bridge();
    assert_eq!(newer.hello(&client), broker);
    newer.send(attach_message(id, &token, &client, 2));
    let attached = newer.recv();
    assert_eq!(text(&attached, "type").unwrap(), "SessionAttached");
    let attachment = binary(get(&attached, "body").unwrap(), "attachment_id").unwrap();
    newer.send(message(
        "Detach",
        map(vec![
            ("session_id", s(&id.to_string())),
            ("attachment_id", Value::Binary(attachment)),
            ("last_output_ack", 0.into()),
            ("last_input_ack", 0.into()),
            ("keep_running", true.into()),
        ]),
    ));
    drop(newer);
    let mut stale = host.bridge();
    assert_eq!(stale.hello(&client), broker);
    stale.send(attach_message(id, &token, &client, 1));
    assert_eq!(text(&stale.recv(), "type").unwrap(), "ConnectionSuperseded");
    let cleanup = host.command(&["terminate", &id.to_string(), "--yes"]);
    assert!(
        cleanup.status.success(),
        "terminate failed: {}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
}
