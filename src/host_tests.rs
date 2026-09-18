use super::*;
use std::io::{Read, Write};

struct Link {
    pair: host_pipe::PipePair,
}
impl Link {
    fn connect(root: &Path) -> Self {
        Self {
            pair: host_pipe::connect(&pipe_name(root).unwrap(), Duration::from_secs(3)).unwrap(),
        }
    }
    fn send(&mut self, value: Value) {
        self.pair
            .input
            .write_all(&wire::encode(&value).unwrap())
            .unwrap();
        self.pair.input.flush().unwrap();
    }
    fn recv(&mut self) -> Value {
        let mut len = [0; 4];
        self.pair.output.read_exact(&mut len).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(len) as usize];
        self.pair.output.read_exact(&mut bytes).unwrap();
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
    loop {
        let v = link.recv();
        if text(&v, "type").unwrap() == "Output" {
            let b = get(&v, "body").unwrap();
            assert_eq!(text(b, "session_id").unwrap(), id.to_string());
            let seq = num(b, "output_seq").unwrap();
            if seq > *cursor {
                *cursor = seq;
                all.extend(binary(b, "bytes").unwrap());
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

#[test]
fn real_conpty_pid_and_state_survive_pipe_disconnect() {
    let root = std::env::temp_dir().join(format!("devbox-host-unit-{}", Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    let server_root = root.clone();
    let server = thread::spawn(move || run_at(server_root));
    let deadline = Instant::now() + Duration::from_secs(10);
    while control_at(&root, "status", None, false).is_err() {
        assert!(Instant::now() < deadline, "broker did not start");
        thread::sleep(Duration::from_millis(25));
    }
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
    one.send(input(
        id,
        &attach,
        &lease,
        &client,
        1,
        b"$global:KeepMe='resumable'; Write-Output ('FIRST=' + $PID + ':' + $global:KeepMe)\r\n",
    ));
    let mut cursor = 0;
    let first = marker(&mut one, id, &mut cursor, ":resumable");
    let shell_pid = pid(&first, "FIRST=");
    assert!(first.contains(&format!("FIRST={shell_pid}:resumable")));
    drop(one);
    thread::sleep(Duration::from_millis(100));
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
    control_at(&root, "stop", None, true).unwrap();
    drop(two);
    server.join().unwrap().unwrap();
    let _ = fs::remove_dir_all(&root);
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
