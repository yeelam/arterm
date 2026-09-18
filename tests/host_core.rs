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
        self.hello_capabilities(client, false)
    }
    fn hello_capabilities(&mut self, client: &[u8], commands: bool) -> Vec<u8> {
        self.send(message(
            "Hello",
            map(vec![
                ("min_version", 1.into()),
                ("max_version", 1.into()),
                ("client_version", s("test")),
                ("client_instance_id", Value::Binary(client.to_vec())),
                ("correlation_id", Value::Binary(vec![8; 16])),
                ("capabilities", Value::Array(if commands {
                    vec![s("client-session-id"), s(arterm::shell_integration::CAPABILITY)]
                } else { vec![s("client-session-id")] })),
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
fn receive_command_state(bridge: &mut Bridge, id: Option<Uuid>, expected: &str) -> (Value, String) {
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < deadline, "command event deadline: {}", String::from_utf8_lossy(&output));
        let response = bridge.recv();
        let kind = text(&response, "type").unwrap();
        let body = get(&response, "body").unwrap();
        if kind == "Output" { output.extend(binary(body, "bytes").unwrap()); }
        if matches!(kind, "CommandState" | "CommandStatus" | "CommandAccepted") {
            if let Some(id) = id {
                if let Some(record) = get(body, "records").unwrap().as_array().unwrap().iter()
                    .find(|record| text(record, "command_id").unwrap() == id.to_string()
                        && text(record, "state").unwrap() == expected) {
                    return (record.clone(), String::from_utf8_lossy(&output).into_owned());
                }
            } else if text(body, "shell_status").unwrap() == expected {
                return (body.clone(), String::from_utf8_lossy(&output).into_owned());
            }
        }
        assert_ne!(kind, "CommandRejected", "{response:?}");
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
fn command_protocol_rejects_unintegrated_shell_before_input() {
    let host = Host::start();
    let mut bridge = host.bridge();
    let client = vec![21; 16];
    let broker = bridge.hello_capabilities(&client, true);
    let session = Uuid::now_v7();
    bridge.send(create_message(Uuid::now_v7(), session, &[3; 32], &broker));
    let created = bridge.recv();
    let body = get(&created, "body").unwrap();
    let attachment = binary(body, "attachment_id").unwrap();
    let lease = binary(body, "lease_id").unwrap();
    bridge.send(writer_message(session, &attachment, &lease, &client, "CommandSubmit",
        vec![("connection_epoch", 1.into()), ("command_id", s(&Uuid::now_v7().to_string())),
            ("command", s("$ShouldNotExist=1"))]));
    loop {
        let response = bridge.recv();
        if text(&response, "type").unwrap() == "CommandRejected" {
            assert!(text(get(&response, "body").unwrap(), "code").unwrap().contains("CommandShellUnsupported"));
            break;
        }
    }
    bridge.send(writer_message(session, &attachment, &lease, &client, "Input",
        vec![("input_seq", 1.into()), ("bytes", Value::Binary(
            b"Write-Output ('UNCHANGED=' + ($null -eq $ShouldNotExist))\r".to_vec()))]));
    let output = collect_marker(&mut bridge, session, &mut 0, "UNCHANGED=True");
    assert!(output.contains("UNCHANGED=True"));
}

#[test]
fn managed_commands_preserve_runspace_and_report_real_completion() {
    let host = Host::start();
    let mut bridge = host.bridge();
    let client = vec![17; 16];
    let broker = bridge.hello_capabilities(&client, true);
    let session = Uuid::now_v7();
    let mut create = create_message(Uuid::now_v7(), session, &[9; 32], &broker);
    if let Value::Map(fields) = &mut create {
        let body = &mut fields.iter_mut().find(|(key, _)| key.as_str() == Some("body")).unwrap().1;
        if let Value::Map(fields) = body { fields.push((s("command_execution"), true.into())); }
    }
    bridge.send(create);
    let created = bridge.recv();
    assert_eq!(text(&created, "type").unwrap(), "SessionCreated", "{created:?}");
    let created = get(&created, "body").unwrap();
    let attachment = binary(created, "attachment_id").unwrap();
    let lease = binary(created, "lease_id").unwrap();
    let token = binary(created, "resume_token").unwrap();
    receive_command_state(&mut bridge, None, "ready");
    let submit = |id: Uuid, command: &str| writer_message(session, &attachment, &lease, &client,
        "CommandSubmit", vec![("connection_epoch", 1.into()), ("command_id", s(&id.to_string())), ("command", s(command))]);
    let first = Uuid::now_v7();
    let path = host.home.to_string_lossy().replace('\'', "''");
    let code = format!("$Keep=41; Set-Location -LiteralPath '{path}'; Write-Output ('FIRST='+$PID+':resumable')");
    let started = Instant::now();
    bridge.send(submit(first, &code));
    let (record, output) = receive_command_state(&mut bridge, Some(first), "completed");
    assert!(started.elapsed() < Duration::from_secs(5), "completion waited instead of observing shell event");
    assert_eq!(get(&record, "succeeded").unwrap().as_bool(), Some(true), "{output}");
    let pid = marker_pid(&output, "FIRST=");
    bridge.send(submit(first, &code));
    receive_command_state(&mut bridge, Some(first), "completed");
    let second = Uuid::now_v7();
    bridge.send(submit(second, "$Keep++; Write-Output ('SECOND='+$PID+':resumable'); Write-Output ('VALUE='+$Keep); Write-Output ('DIR='+$PWD.Path)"));
    let (_, output) = receive_command_state(&mut bridge, Some(second), "completed");
    assert_eq!(marker_pid(&output, "SECOND="), pid);
    assert!(output.contains("VALUE=42"), "{output}");
    assert!(output.contains(host.home.to_str().unwrap()), "{output}");
    // Duplicate IDs must not increment again.
    bridge.send(submit(second, "$Keep++; Write-Output ('SECOND='+$PID+':resumable'); Write-Output ('VALUE='+$Keep); Write-Output ('DIR='+$PWD.Path)"));
    receive_command_state(&mut bridge, Some(second), "completed");
    let formatted = Uuid::now_v7();
    bridge.send(submit(formatted, "$Text=@'\nquotes ' \" and \u{20ac}\n'@\nWrite-Output ('UTF8='+$Text); [pscustomobject]@{Answer=42}"));
    let (record, output) = receive_command_state(&mut bridge, Some(formatted), "completed");
    assert_eq!(get(&record, "succeeded").unwrap().as_bool(), Some(true), "{output}");
    assert!(output.contains("quotes ' \" and \u{20ac}"), "{output}");
    assert!(output.contains("Answer") && output.contains("42"), "completion preceded formatted output: {output}");
    for (code, success, exit) in [
        ("cmd.exe /c exit 0", true, Some(0)),
        ("cmd.exe /c exit 7", false, Some(7)),
        ("Write-Error 'nonterminating'; Write-Output 'AFTER_ERROR'", false, None),
        ("throw 'terminating'", false, None),
    ] {
        let id = Uuid::now_v7();
        bridge.send(submit(id, code));
        let (record, _) = receive_command_state(&mut bridge, Some(id), "completed");
        assert_eq!(get(&record, "succeeded").unwrap().as_bool(), Some(success), "{code}: {record:?}");
        assert_eq!(get(&record, "exit_code").unwrap().as_i64(), exit, "{code}: {record:?}");
    }
    let slow = Uuid::now_v7();
    bridge.send(submit(slow, "Start-Sleep -Seconds 30"));
    receive_command_state(&mut bridge, Some(slow), "running");
    bridge.send(writer_message(session, &attachment, &lease, &client, "CommandStatus",
        vec![("connection_epoch", 1.into()), ("command_id", s(&slow.to_string()))]));
    receive_command_state(&mut bridge, Some(slow), "running");
    bridge.send(writer_message(session, &attachment, &lease, &client, "Input",
        vec![("input_seq", 1.into()), ("bytes", Value::Binary(b"$Keep=999\r".to_vec()))]));
    loop {
        let response = bridge.recv();
        if text(&response, "type").unwrap() == "InputRejected" { break; }
    }
    bridge.send(submit(Uuid::now_v7(), "$Keep=999"));
    loop {
        let response = bridge.recv();
        if text(&response, "type").unwrap() == "CommandRejected" {
            assert!(text(get(&response, "body").unwrap(), "code").unwrap().contains("CommandBusy"));
            break;
        }
    }
    bridge.send(writer_message(session, &attachment, &lease, &client, "CommandInterrupt",
        vec![("connection_epoch", 1.into()), ("command_id", s(&slow.to_string()))]));
    let (record, _) = receive_command_state(&mut bridge, Some(slow), "completed");
    assert_eq!(get(&record, "interrupt_requested").unwrap().as_bool(), Some(true));
    assert_eq!(get(&record, "succeeded").unwrap().as_bool(), Some(false));
    let after = Uuid::now_v7();
    bridge.send(submit(after, "Write-Output ('AFTER='+$PID+':resumable'); Write-Output ('VALUE='+$Keep)"));
    let (_, output) = receive_command_state(&mut bridge, Some(after), "completed");
    assert_eq!(marker_pid(&output, "AFTER="), pid);
    assert!(output.contains("VALUE=42"), "{output}");
    let detached = Uuid::now_v7();
    bridge.send(submit(detached, "Start-Sleep -Seconds 1; $Keep=77"));
    receive_command_state(&mut bridge, Some(detached), "running");
    bridge.send(writer_message(session, &attachment, &lease, &client, "Detach", vec![]));
    drop(bridge);
    thread::sleep(Duration::from_millis(1500));
    let mut bridge = host.bridge();
    bridge.hello_capabilities(&client, true);
    bridge.send(attach_message(session, &token, &client, 2));
    let attachment_reply = bridge.recv();
    assert_eq!(text(&attachment_reply, "type").unwrap(), "SessionAttached");
    let body = get(&attachment_reply, "body").unwrap();
    let new_attachment = binary(body, "attachment_id").unwrap();
    let new_lease = binary(body, "lease_id").unwrap();
    receive_command_state(&mut bridge, Some(detached), "completed");
    // Old leases/epochs never authorize a command, even for the same client.
    bridge.send(submit(Uuid::now_v7(), "$Keep=999"));
    loop {
        let response = bridge.recv();
        if text(&response, "type").unwrap() == "CommandRejected" {
            assert!(text(get(&response, "body").unwrap(), "code").unwrap().contains("CommandLeaseRevoked"));
            break;
        }
    }
    bridge.send(writer_message(session, &new_attachment, &new_lease, &client, "Input",
        vec![("input_seq", 1.into()), ("bytes", Value::Binary(b"$partial".to_vec()))]));
    loop { if text(&bridge.recv(), "type").unwrap() == "InputAck" { break; } }
    bridge.send(writer_message(session, &new_attachment, &new_lease, &client, "CommandSubmit",
        vec![("connection_epoch", 2.into()), ("command_id", s(&Uuid::now_v7().to_string())), ("command", s("$Keep=999"))]));
    loop {
        let response = bridge.recv();
        if text(&response, "type").unwrap() == "CommandRejected" {
            assert!(text(get(&response, "body").unwrap(), "code").unwrap().contains("CommandBusy"));
            break;
        }
    }
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
