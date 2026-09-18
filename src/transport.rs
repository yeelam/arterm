use crate::wire::{self, binary, get, map, num, s, text, Frames};
use crate::statusln as eprintln;
use anyhow::{bail, ensure, Context, Result};
use rmpv::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpStream},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

pub const BRIDGE_ARGS: &[&str] = &["bridge", "--protocol", "vsterm-session-v1"];
const SPAWN: u64 = 10;
pub enum Poll {
    Message(Value),
    Idle,
    Closed,
}

pub trait Link {
    fn send(&mut self, value: &Value) -> Result<()>;
    fn poll(&mut self, timeout: Duration) -> Result<Poll>;
}

pub fn probe(addr: &str) -> Result<()> {
    probe_with_timeout(addr, Duration::from_secs(3))
}

fn probe_with_timeout(addr: &str, timeout: Duration) -> Result<()> {
    ensure!(!timeout.is_zero(), "control probe timed out");
    let addr: std::net::SocketAddr = addr.parse()?;
    ensure!(addr.ip().is_loopback(), "probe endpoint must be loopback");
    let mut socket = TcpStream::connect_timeout(&addr, timeout)?;
    socket.set_read_timeout(Some(timeout))?;
    socket.set_write_timeout(Some(timeout))?;
    rmpv::encode::write_value(&mut socket, &rpc(Some(1), "version", map(vec![])))?;
    let deadline = Instant::now() + timeout;
    let mut reader = BufReader::new(socket);
    for _ in 0..4 {
        reader
            .get_ref()
            .set_read_timeout(Some(remaining(deadline)?))?;
        let response =
            rmpv::decode::read_value_with_max_depth(&mut reader.by_ref().take(1024 * 1024), 32)?;
        if version_greeting(&response)? {
            continue;
        }
        return validate_version_reply(&response);
    }
    bail!("VS control version reply did not arrive after its greetings")
}

fn version_greeting(value: &Value) -> Result<bool> {
    if text(value, "method").ok() != Some("version")
        || get(value, "id").is_ok_and(|id| !id.is_nil())
    {
        return Ok(false);
    }
    let params = get(value, "params")?;
    text(params, "version")?;
    num(params, "protocol_version")?;
    Ok(true)
}

fn validate_version_reply(response: &Value) -> Result<()> {
    if response.as_map().is_none() {
        if response.as_u64() == Some(b'H' as u64) {
            bail!("endpoint appears to speak HTTP, not VS control MessagePack RPC");
        }
        bail!("endpoint returned a non-map message, not a VS control RPC reply");
    }
    ensure!(
        num(response, "id")? == 1
            && get(response, "result").is_ok()
            && get(response, "error").map_or(true, Value::is_nil),
        "incompatible VS control version response"
    );
    Ok(())
}
struct ForwardLock {
    file: Option<File>,
    path: PathBuf,
}
impl ForwardLock {
    fn acquire() -> Result<Self> {
        let dir = crate::deployment::data_root()?
            .join("client")
            .join("forwards");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.lock", uuid::Uuid::now_v7()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&path)?;
        Ok(Self {
            file: Some(file),
            path,
        })
    }
}
impl Drop for ForwardLock {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

pub struct Forward {
    child: Child,
    _lock: ForwardLock,
}

fn note_forward(candidates: &mut BTreeMap<u16, Instant>, port: u16, now: Instant) {
    // The CLI prints forwarding announcements before its SSH forwarding table is ready.
    // Probing synchronously on that announcement can break the listener with "not being forwarded".
    candidates
        .entry(port)
        .or_insert(now + Duration::from_secs(1));
}

#[derive(Debug)]
pub struct LoginRequired;
impl std::fmt::Display for LoginRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("devtunnel login is missing or expired; run the new client's `login` command using the host's GitHub account")
    }
}
impl std::error::Error for LoginRequired {}

impl Forward {
    pub fn start(devtunnel: &Path, tunnel_id: &str) -> Result<(Self, String)> {
        let deadline = Instant::now() + Duration::from_secs(40);
        if !authenticated_until(devtunnel, deadline)? {
            return Err(LoginRequired.into());
        }
        let resolved = resolve_tunnel(devtunnel, tunnel_id, deadline)?;
        let published = published_ports(devtunnel, &resolved, deadline)?;
        let lock = ForwardLock::acquire()?;
        let mut child = Command::new(devtunnel)
            .args(["connect", &resolved])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start devtunnel forward")?;
        let stdout = child.stdout.take().context("devtunnel stdout")?;
        let stderr = child.stderr.take().context("devtunnel stderr")?;
        let (tx, rx) = mpsc::sync_channel(64);
        drain_forward_output(stdout, tx.clone());
        drain_forward_output(stderr, tx);
        let mut forward = Self { child, _lock: lock };
        let mut candidates = BTreeMap::new();
        let mut probe_errors = BTreeMap::new();
        while Instant::now() < deadline {
            let mut lines = Vec::new();
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => lines.push(line),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("devtunnel output closed before a compatible control port was ready")
                }
            }
            // Drain startup output before probing: a stale first port must not hide later ports.
            lines.extend(rx.try_iter().take(63));
            for line in lines {
                let line = line?;
                if authentication_failure_text(&line) {
                    return Err(LoginRequired.into());
                }
                if let Some((local, remote)) = forwarded_port(&line) {
                    if published.is_empty() || published.contains(&remote) {
                        note_forward(&mut candidates, local, Instant::now());
                    }
                } else if !line.trim().is_empty() {
                    eprintln!("[devtunnel] {}", line.trim_end());
                }
            }
            if let Some(status) = forward.child.try_wait()? {
                bail!(
                    "devtunnel connect exited before a compatible control port was ready: {status}"
                );
            }
            let due: Vec<u16> = candidates
                .iter()
                .filter(|(_, when)| **when <= Instant::now())
                .map(|(port, _)| *port)
                .collect();
            for port in due {
                if Instant::now() >= deadline {
                    break;
                }
                let addr = format!("127.0.0.1:{port}");
                let timeout = remaining(deadline)?.min(Duration::from_secs(3));
                match probe_with_timeout(&addr, timeout) {
                    Ok(()) => {
                        eprintln!("[client] VS control server selected at {addr}");
                        continue_forward_drain(rx);
                        return Ok((forward, addr));
                    }
                    Err(error) => {
                        let diagnostic = format!("{error:#}");
                        if probe_errors.get(&port) != Some(&diagnostic) {
                            eprintln!("[client] Control probe {addr}: {diagnostic}");
                            probe_errors.insert(port, diagnostic);
                        }
                        candidates.insert(port, Instant::now() + Duration::from_secs(5));
                    }
                }
            }
        }
        bail!("devtunnel did not expose a compatible control port within 40 seconds; {} candidate(s) examined", probe_errors.len())
    }
}
impl Drop for Forward {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        kill_owned_process_tree(&mut self.child);
    }
}

fn collect_port_numbers(value: &serde_json::Value, ports: &mut BTreeSet<u16>) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if key.eq_ignore_ascii_case("portNumber") {
                    if let Some(port) = value.as_u64().and_then(|v| u16::try_from(v).ok()) {
                        if port > 0 {
                            ports.insert(port);
                        }
                    }
                }
                collect_port_numbers(value, ports);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_port_numbers(value, ports);
            }
        }
        _ => {}
    }
}

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .context("devtunnel metadata/startup timed out")
}

fn read_bounded(mut reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok((kept, truncated));
        }
        let available = limit.saturating_sub(kept.len());
        let take = available.min(count);
        kept.extend_from_slice(&buffer[..take]);
        truncated |= take < count;
    }
}

fn kill_owned_process_tree(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = Command::new("taskkill.exe")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

fn capture_bounded(mut command: Command, timeout: Duration, limit: usize) -> Result<Captured> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("start devtunnel metadata command")?;
    let stdout = child.stdout.take().context("metadata stdout")?;
    let stderr = child.stderr.take().context("metadata stderr")?;
    let (out_tx, out_rx) = mpsc::sync_channel(1);
    let (err_tx, err_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = out_tx.send(read_bounded(stdout, limit));
    });
    thread::spawn(move || {
        let _ = err_tx.send(read_bounded(stderr, limit));
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }

        if Instant::now() >= deadline {
            kill_owned_process_tree(&mut child);
            bail!(
                "devtunnel metadata command timed out after {} seconds",
                timeout.as_secs_f32()
            );
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = out_rx
        .recv_timeout(remaining(deadline)?)
        .context("devtunnel metadata stdout did not close before timeout")??;
    let stderr = err_rx
        .recv_timeout(remaining(deadline)?)
        .context("devtunnel metadata stderr did not close before timeout")??;
    Ok(Captured {
        status,
        stdout: stdout.0,
        stderr: stderr.0,
        truncated: stdout.1 || stderr.1,
    })
}

pub fn output_bounded(command: Command, timeout: Duration) -> Result<std::process::Output> {
    let output = capture_bounded(command, timeout, 4 * 1024 * 1024)?;
    ensure!(
        !output.truncated,
        "dependency metadata output exceeded 4 MiB"
    );
    Ok(std::process::Output {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn find_tunnel(value: &serde_json::Value, requested: &str) -> Result<Option<String>> {
    let tunnels = value
        .get("tunnels")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
        .context("unrecognized tunnel discovery response")?;
    let mut matches = BTreeSet::new();
    for tunnel in tunnels {
        let Some(id) = tunnel.get("tunnelId").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let named = ["name", "tunnelId"].iter().any(|key| {
            tunnel
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case(requested))
        });
        let labelled = tunnel
            .get("labels")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|labels| {
                labels.iter().any(|v| {
                    v.as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(requested))
                })
            });
        if named || labelled {
            let cluster = tunnel.get("clusterId").and_then(serde_json::Value::as_str)
                .filter(|cluster| !cluster.is_empty());
            let scoped = match cluster {
                Some(cluster) if !id.contains('.') => format!("{id}.{cluster}"),
                _ => id.to_owned(),
            };
            matches.insert(scoped);
        }
    }
    ensure!(
        matches.len() <= 1,
        "tunnel name is ambiguous; give hosts unique names or explicitly register an exact tunnel ID"
    );
    Ok(matches.into_iter().next())
}

fn resolve_tunnel(devtunnel: &Path, requested: &str, deadline: Instant) -> Result<String> {
    let result = metadata(devtunnel, &["list", "--json"], deadline)?;
    reject_expired_auth(&result)?;
    if !result.status.success() {
        eprintln!(
            "[devtunnel] Discovery unavailable; trying the supplied value as an exact tunnel ID."
        );
        return Ok(requested.to_owned());
    }
    ensure!(!result.truncated, "tunnel discovery metadata too large");
    let value = serde_json::from_slice(&result.stdout).context("invalid tunnel discovery JSON")?;
    Ok(find_tunnel(&value, requested)?.unwrap_or_else(|| requested.to_owned()))
}

#[cfg(test)]
mod discovery_tests {
    use super::*;
    #[test]
    fn friendly_name_rediscovers_recreated_vscode_tunnel_without_pinning_an_id() {
        for (id, cluster, expected) in [
            ("generated-old", "usw2", "generated-old.usw2"),
            ("generated-new", "use2", "generated-new.use2"),
            ("already-scoped.usw2", "usw2", "already-scoped.usw2"),
        ] {
            let data = serde_json::json!({"tunnels":[{
                "tunnelId": id, "clusterId": cluster, "name": "",
                "labels": ["dev01", "vscode-server-launcher", "_flag2"]
            }]});
            assert_eq!(find_tunnel(&data, "dev01").unwrap().as_deref(), Some(expected));
            assert_eq!(find_tunnel(&data, "DEV01").unwrap().as_deref(), Some(expected));
        }
        let ambiguous = serde_json::json!({"tunnels":[
            {"tunnelId":"same-id","clusterId":"usw2","labels":["dev01"]},
            {"tunnelId":"same-id","clusterId":"use2","labels":["dev01"]}
        ]});
        assert!(find_tunnel(&ambiguous, "dev01").is_err());
    }

    #[test]
    fn resolves_name_and_label_but_rejects_ambiguity() {
        let data = serde_json::json!({"tunnels":[
            {"tunnelId":"actual-a","labels":["my-box"]},
            {"tunnelId":"actual-b","name":"other-box"}
        ]});
        assert_eq!(
            find_tunnel(&data, "my-box").unwrap().as_deref(),
            Some("actual-a")
        );
        assert_eq!(
            find_tunnel(&data, "actual-b").unwrap().as_deref(),
            Some("actual-b")
        );
        assert_eq!(find_tunnel(&data, "missing").unwrap(), None);
        let ambiguous = serde_json::json!({"tunnels":[
            {"tunnelId":"a","name":"box"},{"tunnelId":"b","labels":["box"]}
        ]});
        assert!(find_tunnel(&ambiguous, "box").is_err());
    }
}

fn metadata(devtunnel: &Path, args: &[&str], deadline: Instant) -> Result<Captured> {
    let mut command = Command::new(devtunnel);
    command.args(args);
    capture_bounded(command, remaining(deadline)?, 4 * 1024 * 1024)
}

fn authentication_failure_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "login token expired",
        "login required",
        "not logged in",
        "not authenticated",
        "authentication failed",
        "unauthorized",
    ]
    .iter()
    .any(|s| lower.contains(s))
}

fn reject_expired_auth(output: &Captured) -> Result<()> {
    if authentication_failure_text(&String::from_utf8_lossy(&output.stdout))
        || authentication_failure_text(&String::from_utf8_lossy(&output.stderr))
    {
        return Err(LoginRequired.into());
    }
    Ok(())
}

fn auth_response_ready(output: &Captured) -> bool {
    if !output.status.success() || output.truncated || reject_expired_auth(output).is_err() {
        return false;
    }
    // `user show --json` can exit zero with {"status":"Login token expired"}.
    // Require a positive status rather than trusting its process exit code.
    let text = String::from_utf8_lossy(&output.stdout);
    let parsed = serde_json::from_str::<serde_json::Value>(&text).ok();
    let status = parsed
        .as_ref()
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or(&text)
        .trim()
        .to_ascii_lowercase();
    status == "logged in" || status.starts_with("logged in as ")
}

fn authenticated_until(devtunnel: &Path, deadline: Instant) -> Result<bool> {
    Ok(auth_response_ready(&metadata(
        devtunnel,
        &["user", "show", "--json"],
        deadline,
    )?))
}

pub fn authenticated(devtunnel: &Path) -> Result<bool> {
    authenticated_until(devtunnel, Instant::now() + Duration::from_secs(10))
}

fn published_ports(devtunnel: &Path, tunnel_id: &str, deadline: Instant) -> Result<BTreeSet<u16>> {
    for args in [
        vec!["show", tunnel_id, "--json"],
        vec!["port", "list", tunnel_id, "--json"],
    ] {
        let output = metadata(devtunnel, &args, deadline)?;
        reject_expired_auth(&output)?;
        if output.status.success() && !output.truncated {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                let mut ports = BTreeSet::new();
                collect_port_numbers(&value, &mut ports);
                if !ports.is_empty() {
                    return Ok(ports);
                }
            }
        } else if !output.status.success() && !output.stderr.is_empty() {
            eprintln!(
                "[devtunnel] {}",
                String::from_utf8_lossy(&output.stderr).trim_end()
            );
        }
    }
    Ok(BTreeSet::new())
}

fn forwarded_port(line: &str) -> Option<(u16, u16)> {
    let start = ["Forwarding from 127.0.0.1:", "Forwarding from [::1]:"]
        .iter()
        .find_map(|prefix| line.find(prefix).map(|index| index + prefix.len()))?;
    let local_end = start + line[start..].find(|c: char| !c.is_ascii_digit())?;
    let local = line[start..local_end].parse().ok()?;
    let remote_prefix = "to host port";
    let remote_start = local_end + line[local_end..].find(remote_prefix)? + remote_prefix.len();
    let remote_text = line[remote_start..].trim_start();
    let remote_end = remote_text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(remote_text.len());
    let remote = remote_text[..remote_end].parse().ok()?;
    Some((local, remote))
}

fn continue_forward_drain(rx: Receiver<std::io::Result<String>>) {
    thread::spawn(move || {
        while let Ok(result) = rx.recv() {
            match result {
                Ok(line) if !line.trim().is_empty() => eprintln!("[devtunnel] {}", line.trim_end()),
                Ok(_) => {}
                Err(error) => {
                    eprintln!("[devtunnel] output error: {error}");
                    break;
                }
            }
        }
    });
}

fn drain_forward_output(
    reader: impl Read + Send + 'static,
    tx: mpsc::SyncSender<std::io::Result<String>>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::with_capacity(8192, reader);
        loop {
            let mut bytes = Vec::with_capacity(1024);
            match reader
                .by_ref()
                .take(16 * 1024)
                .read_until(b'\n', &mut bytes)
            {
                Ok(0) => break,
                Ok(_) => {
                    if !bytes.ends_with(b"\n") && bytes.len() == 16 * 1024 {
                        let _ = tx.send(Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "devtunnel output line exceeded 16 KiB",
                        )));
                        break;
                    }
                    if tx
                        .send(Ok(String::from_utf8_lossy(&bytes).into_owned()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error));
                    break;
                }
            }
        }
    });
}

fn rpc(id: Option<u64>, method: &str, params: Value) -> Value {
    map(vec![
        ("id", id.map(Value::from).unwrap_or(Value::Nil)),
        ("method", s(method)),
        ("params", params),
    ])
}

pub struct TunnelLink {
    socket: TcpStream,
    events: Receiver<Result<Value>>,
    stdin_id: u32,
    stdout_id: u32,
    stderr_id: u32,
    frames: Frames,
    _forward: Option<Forward>,
}
impl TunnelLink {
    pub fn connect(addr: &str, host_exe: &str, forward: Option<Forward>) -> Result<Self> {
        let endpoint = addr
            .parse()
            .context("expected a numeric loopback address:port")?;
        let socket = TcpStream::connect_timeout(&endpoint, Duration::from_secs(10))?;
        socket.set_nodelay(true)?;
        socket.set_write_timeout(Some(Duration::from_secs(10)))?;
        let reader = socket.try_clone()?;
        let (tx, events) = mpsc::sync_channel(32);
        thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let result = rmpv::decode::read_value_with_max_depth(
                    &mut reader.by_ref().take(2 * 1024 * 1024),
                    32,
                )
                .map_err(anyhow::Error::from);
                let failed = result.is_err();
                if tx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let mut link = Self {
            socket,
            events,
            stdin_id: 0,
            stdout_id: 0,
            stderr_id: 0,
            frames: Frames::default(),
            _forward: forward,
        };
        link.raw(&rpc(Some(1), "version", map(vec![])))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let response = link
                .events
                .recv_timeout(remaining(deadline)?)
                .context("VS control version probe timed out")??;
            if version_greeting(&response)? {
                continue;
            }
            validate_version_reply(&response)?;
            break;
        }
        link.raw(&rpc(
            Some(SPAWN),
            "spawn",
            map(vec![
                ("command", s(host_exe)),
                (
                    "args",
                    Value::Array(BRIDGE_ARGS.iter().map(|v| s(v)).collect()),
                ),
                ("env", map(vec![])),
            ]),
        ))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        let started = loop {
            let event = link
                .events
                .recv_timeout(remaining(deadline)?)
                .context("arterm host bridge did not start")??;
            if version_greeting(&event)? {
                continue;
            }
            break event;
        };
        ensure!(
            text(&started, "method").ok() == Some("streams_started"),
            "arterm host bridge unsupported or failed to spawn (requires vsterm-session-v1)"
        );
        let ids = get(get(&started, "params")?, "stream_ids")?
            .as_array()
            .context("invalid VS stream IDs")?;
        ensure!(ids.len() == 3, "expected exactly three VS spawn streams");
        let id =
            |v: &Value| -> Result<u32> { Ok(v.as_u64().context("invalid stream ID")?.try_into()?) };
        link.stdin_id = id(&ids[0])?;
        link.stdout_id = id(&ids[1])?;
        link.stderr_id = id(&ids[2])?;
        ensure!(
            link.stdin_id != link.stdout_id
                && link.stdin_id != link.stderr_id
                && link.stdout_id != link.stderr_id,
            "duplicate VS stream IDs"
        );
        Ok(link)
    }
    fn raw(&mut self, value: &Value) -> Result<()> {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, value)?;
        self.socket.write_all(&bytes)?;
        self.socket.flush()?;
        Ok(())
    }
}
impl Link for TunnelLink {
    fn send(&mut self, value: &Value) -> Result<()> {
        self.raw(&rpc(
            None,
            "stream_data",
            map(vec![
                ("stream", self.stdin_id.into()),
                ("segment", Value::Binary(wire::encode(value)?)),
            ]),
        ))
    }
    fn poll(&mut self, timeout: Duration) -> Result<Poll> {
        if let Some(value) = self.frames.next()? {
            return Ok(Poll::Message(value));
        }
        match self.events.recv_timeout(timeout) {
            Ok(Ok(value)) => {
                if num(&value, "id").ok() == Some(SPAWN) {
                    // The process result can overtake stdout's final SessionExited frame.
                    // Only stdout EOF (or a lost transport) ends the protocol stream.
                    return Ok(Poll::Idle);
                }
                match text(&value, "method").ok() {
                    Some("stream_data") => {
                        let params = get(&value, "params")?;
                        let stream = num(params, "stream")?;
                        let segment = binary(params, "segment")?;
                        if stream == self.stdout_id as u64 {
                            self.frames.push(&segment)?;
                            if let Some(value) = self.frames.next()? {
                                return Ok(Poll::Message(value));
                            }
                        } else if stream == self.stderr_id as u64 {
                            std::io::stderr().write_all(&segment)?;
                        } else {
                            bail!("unexpected VS output stream");
                        }
                        Ok(Poll::Idle)
                    }
                    Some("stream_closed") | Some("stream_ended") => {
                        let stream = num(get(&value, "params")?, "stream")?;
                        if stream == self.stdout_id as u64 {
                            Ok(Poll::Closed)
                        } else if stream == self.stderr_id as u64
                            || stream == self.stdin_id as u64
                        {
                            Ok(Poll::Idle)
                        } else {
                            bail!("unexpected ended stream");
                        }
                    }
                    _ => bail!("unexpected VS control message"),
                }
            }
            Ok(Err(error)) => Err(error.context("VS stream disconnected")),
            Err(RecvTimeoutError::Timeout) => Ok(Poll::Idle),
            Err(RecvTimeoutError::Disconnected) => Ok(Poll::Closed),
        }
    }
}
impl Drop for TunnelLink {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarding_announcement_does_not_trigger_an_immediate_probe() {
        let now = Instant::now();
        let mut candidates = BTreeMap::new();
        note_forward(&mut candidates, 31545, now);
        assert!(candidates[&31545] > now);
        note_forward(&mut candidates, 31545, now + Duration::from_millis(500));
        assert_eq!(candidates[&31545], now + Duration::from_secs(1));
        note_forward(&mut candidates, 31546, now);
        assert_eq!(candidates[&31546], now + Duration::from_secs(1));
    }

    #[test]
    fn zero_exit_with_expired_token_is_not_authenticated() {
        use std::os::windows::process::ExitStatusExt;
        let output = |stdout: &str, stderr: &str| Captured {
            status: ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            truncated: false,
        };
        let expired = output(r#"{"status":"Login token expired"}"#, "");
        assert!(!auth_response_ready(&expired));
        assert!(reject_expired_auth(&expired)
            .unwrap_err()
            .is::<LoginRequired>());
        assert!(!auth_response_ready(&output("", "Login token expired.")));
        assert!(!auth_response_ready(&output("{}", "")));
        assert!(!auth_response_ready(&output(
            r#"{"status":"not logged in"}"#,
            ""
        )));
        assert!(auth_response_ready(&output(
            r#"{"status":"Logged in as example (GitHub)"}"#,
            ""
        )));
        assert!(auth_response_ready(&output("Logged in as example", "")));
        assert!(reject_expired_auth(&output(r#"{"tunnels":[]}"#, "")).is_ok());
    }

    #[test]
    fn tunnel_link_spawns_native_host_bridge() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let version = rmpv::decode::read_value(&mut socket).unwrap();
            assert_eq!(text(&version, "method").unwrap(), "version");
            let greeting = map(vec![
                ("method", s("version")),
                (
                    "params",
                    map(vec![
                        ("version", s("fixture")),
                        ("protocol_version", 6.into()),
                    ]),
                ),
            ]);
            rmpv::encode::write_value(&mut socket, &greeting).unwrap();
            rmpv::encode::write_value(
                &mut socket,
                &map(vec![("id", 1.into()), ("result", map(vec![]))]),
            )
            .unwrap();
            let spawn = rmpv::decode::read_value(&mut socket).unwrap();
            assert_eq!(text(&spawn, "method").unwrap(), "spawn");
            let params = get(&spawn, "params").unwrap();
            assert_eq!(
                text(params, "command").unwrap(),
                r"C:\Tools\arterm-host.exe"
            );
            assert_eq!(
                get(params, "args").unwrap().as_array().unwrap(),
                &BRIDGE_ARGS.iter().map(|value| s(value)).collect::<Vec<_>>()
            );
            rmpv::encode::write_value(&mut socket, &greeting).unwrap();
            rmpv::encode::write_value(
                &mut socket,
                &map(vec![
                    ("method", s("streams_started")),
                    (
                        "params",
                        map(vec![(
                            "stream_ids",
                            Value::Array(vec![11.into(), 12.into(), 13.into()]),
                        )]),
                    ),
                ]),
            )
            .unwrap();
        });
        let link = TunnelLink::connect(&address, r"C:\Tools\arterm-host.exe", None).unwrap();
        drop(link);
        server.join().unwrap();
    }

    #[test]
    fn successful_forward_receiver_continues_draining() {
        let (tx, rx) = mpsc::sync_channel(1);
        continue_forward_drain(rx);
        let sender = thread::spawn(move || {
            for _ in 0..128 {
                tx.send(Ok(String::new())).unwrap();
            }
        });
        sender.join().unwrap();
    }

    #[test]
    fn bounded_capture_times_out_and_caps_output() {
        let exe = std::env::current_exe().unwrap();
        let mut output = Command::new(&exe);
        output.args(["--exact", "transport::tests::metadata_child", "--nocapture"]);
        output.env("VSTERM_METADATA_CHILD", "output");
        let captured = capture_bounded(output, Duration::from_secs(5), 1024).unwrap();
        assert!(captured.status.success());
        assert_eq!(captured.stdout.len(), 1024);
        assert!(captured.truncated);

        let mut sleeper = Command::new(exe);
        sleeper.args(["--exact", "transport::tests::metadata_child", "--nocapture"]);
        sleeper.env("VSTERM_METADATA_CHILD", "sleep");
        let started = Instant::now();
        assert!(capture_bounded(sleeper, Duration::from_millis(150), 1024).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn metadata_child() {
        match std::env::var("VSTERM_METADATA_CHILD").as_deref() {
            Ok("output") => print!("{}", "x".repeat(4096)),
            Ok("sleep") => thread::sleep(Duration::from_secs(30)),
            _ => {}
        }
    }

    #[test]
    fn parses_forwarding_lines_and_nested_port_json() {
        assert_eq!(
            forwarded_port("Forwarding from 127.0.0.1:54321 to host port 31545"),
            Some((54321, 31545))
        );
        assert_eq!(forwarded_port("unrelated output"), None);
        assert_eq!(
            forwarded_port("[devtunnel] SSH: Forwarding from [::1]:31548 to host port 31546."),
            Some((31548, 31546))
        );
        let value =
            serde_json::json!({"tunnel":{"ports":[{"portNumber":31545},{"portNumber":3000}]}});
        let mut ports = BTreeSet::new();
        collect_port_numbers(&value, &mut ports);
        assert_eq!(ports.into_iter().collect::<Vec<_>>(), vec![3000, 31545]);
    }

    #[test]
    fn http_is_not_a_control_reply_and_version_greetings_are_recognized() {
        assert!(validate_version_reply(&Value::from(b'H'))
            .unwrap_err()
            .to_string()
            .contains("HTTP"));
        let greeting = map(vec![
            ("id", Value::Nil),
            ("method", s("version")),
            (
                "params",
                map(vec![
                    ("version", s("fixture")),
                    ("protocol_version", 6.into()),
                ]),
            ),
        ]);
        assert!(version_greeting(&greeting).unwrap());
        assert!(!version_greeting(&map(vec![("id", 1.into()), ("result", map(vec![]))])).unwrap());
    }
}
