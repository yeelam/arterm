//! Per-connection local control. Discovery contains identities, never credentials.
use crate::console::{Input, Terminal};
#[cfg(any(test, all(feature = "test-unsigned-ipc", debug_assertions)))]
use crate::peer_auth::test_fixture::FixtureIdentity as IpcIdentity;
#[cfg(not(any(test, all(feature = "test-unsigned-ipc", debug_assertions))))]
use crate::peer_auth::ClientIdentity as IpcIdentity;
use crate::peer_auth::PeerEnd;
use crate::readiness_diagnostics::{Details, Event, EventKind, ReadinessLog, ReadinessReason, Role, ShellStatus, Snapshot};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File},
    io::{Read, Write},
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
};

const VERSION: u32 = 1;
const MAX_FRAME: usize = 8 * 1024 * 1024;
const HISTORY: usize = 2000;
const RPC_TIMEOUT: Duration = Duration::from_secs(3);
pub const READINESS_TIMEOUT_MS: u64 = 30_000;
pub const FILE_TIMEOUT: Duration = crate::transfer_payload::OPERATION_TIMEOUT;

type Service = dyn Fn(Uuid, Operation, &dyn Fn() -> Result<()>) -> Result<Value> + Send + Sync;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub version: u32,
    pub instance_id: Uuid,
    pub scope_id: String,
    pub target_id: String,
    pub machine: String,
    pub session_id: Uuid,
    pub reference: Option<String>,
    pub pipe: String,
    pub pid: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Inspect,
    Read {
        lines: usize,
    },
    Detach,
    Send {
        command: String,
        timeout_ms: Option<u64>,
    },
    Interrupt,
    CommandStatus {
        command_id: Uuid,
    },
    Wait {
        command_id: Uuid,
        timeout_ms: u64,
    },
    Terminate,
    FileSend { path: String },
    FileReceive { path: String },
}

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub operation_id: Uuid,
    pub instance_id: Uuid,
    pub scope_id: String,
    pub target_id: String,
    pub session_id: Uuid,
    pub action: Operation,
}

pub struct Buffer {
    parser: vt100::Parser,
    bytes: u64,
    gap: bool,
    controls: ControlFilter,
}

#[derive(Default)]
struct ControlFilter {
    pending: Vec<u8>,
    string: bool,
    discarded: bool,
    escape: bool,
    utf8_remaining: u8,
    lost: u64,
}
impl ControlFilter {
    const LIMIT: usize = 4096;
    fn filter(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            let continuation = self.utf8_remaining > 0 && (byte & 0xc0) == 0x80;
            if continuation {
                self.utf8_remaining -= 1;
            } else {
                self.utf8_remaining = match byte {
                    0xc2..=0xdf => 1,
                    0xe0..=0xef => 2,
                    0xf0..=0xf4 => 3,
                    _ => 0,
                };
            }
            if self.string {
                let ended =
                    byte == 7 || (self.escape && byte == b'\\') || (!continuation && byte == 0x9c);
                self.escape = byte == 27;
                if !self.discarded {
                    if self.pending.len() < Self::LIMIT {
                        self.pending.push(byte);
                    } else {
                        self.pending.clear();
                        self.discarded = true;
                        self.lost += 1;
                    }
                }
                if ended {
                    if !self.discarded {
                        output.append(&mut self.pending);
                    }
                    self.string = false;
                    self.discarded = false;
                    self.escape = false;
                }
                continue;
            }
            if !self.pending.is_empty() {
                self.pending.push(byte);
                if matches!(byte, b']' | b'P' | b'X' | b'^' | b'_') {
                    self.string = true;
                } else {
                    output.append(&mut self.pending);
                }
            } else if byte == 27 {
                self.pending.push(byte);
            } else if !continuation && matches!(byte, 0x90 | 0x98 | 0x9d | 0x9e | 0x9f) {
                self.pending.push(byte);
                self.string = true;
            } else {
                output.push(byte);
            }
        }
        output
    }
}
impl Buffer {
    pub fn new() -> Self {
        Self {
            parser: vt100::Parser::new(24, 80, HISTORY),
            bytes: 0,
            gap: false,
            controls: ControlFilter::default(),
        }
    }
    pub fn push(&mut self, bytes: &[u8]) {
        self.parser.process(&self.controls.filter(bytes));
        self.bytes = self.bytes.saturating_add(bytes.len() as u64);
    }
    pub fn snapshot(&mut self, count: usize) -> Result<Value> {
        ensure!(
            (1..=HISTORY).contains(&count),
            "lines must be 1..={HISTORY}"
        );
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let retained = self.parser.screen().scrollback();
        let mut lines = Vec::<String>::new();
        let mut current = String::new();
        for offset in (0..=retained).rev() {
            self.parser.screen_mut().set_scrollback(offset);
            let screen = self.parser.screen();
            let rows: Vec<String> = screen.rows(0, screen.size().1).collect();
            let end = if offset == 0 { rows.len() } else { 1 };
            for (row, text) in rows.iter().enumerate().take(end) {
                current.extend(text.chars().filter(|c| !c.is_control()));
                if !screen.row_wrapped(row as u16) {
                    lines.push(std::mem::take(&mut current));
                }
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let omitted = lines.len().saturating_sub(count);
        let lines = lines.into_iter().skip(omitted).collect::<Vec<_>>();
        Ok(json!({
            "lines": lines, "requested_lines": count,
            "truncated": omitted > 0 || retained == HISTORY || self.controls.lost > 0,
            "discarded_control_sequences": self.controls.lost,
            "retained_control_bytes": self.controls.pending.len(),
            "history_may_be_incomplete": retained == HISTORY,
            "replay_gap": self.gap,
            "output_bytes": self.bytes,
            "alternate_screen": self.parser.screen().alternate_screen()
        }))
    }
}

struct Shared {
    session_id: Uuid,
    diagnostic_log: Option<ReadinessLog>,
    buffer: Mutex<Buffer>,
    state: Mutex<String>,
    detach: AtomicBool,
    stop: Arc<AtomicBool>,
    control: Mutex<ControlState>,
    changed: Condvar,
    waiters: AtomicUsize,
    service: Mutex<Option<Arc<Service>>>,
}

#[derive(Clone)]
pub struct ControlMessage {
    pub operation_id: Uuid,
    pub action: Operation,
    pub submission: Option<Arc<Submission>>,
}
pub struct Submission {
    state: AtomicU8,
    deadline: Instant,
    caller_pipe: Option<File>,
}
impl Submission {
    pub fn dispatch(&self) -> bool {
        if Instant::now() >= self.deadline {
            self.cancel_pending();
            return false;
        }
        if let Some(pipe) = &self.caller_pipe {
            let mut available = 0;
            if unsafe {
                PeekNamedPipe(
                    pipe.as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                self.cancel_pending();
                return false;
            }
        }
        self.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn cancel_pending(&self) -> bool {
        self.state
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            || self.state.load(Ordering::Acquire) == 2
    }
}
struct PendingSubmission(Option<Arc<Submission>>);
impl Drop for PendingSubmission {
    fn drop(&mut self) {
        if let Some(submission) = &self.0 {
            submission.cancel_pending();
        }
    }
}
struct WaitSlot<'a>(&'a AtomicUsize);
impl Drop for WaitSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Default)]
struct ControlState {
    queue: VecDeque<ControlMessage>,
    submitted: BTreeMap<Uuid, String>,
    records: BTreeMap<Uuid, Value>,
    replies: BTreeMap<Uuid, Value>,
    supported: bool,
    shell_status: String,
    creation_enabled: Option<bool>,
    host_version: Option<String>,
    readiness_reason: Option<String>,
    delivered: u64,
    submitting: Option<Uuid>,
    capability_known: bool,
    readiness_details: Option<Details>,
    bad_diagnostic_reported: bool,
}
impl ControlState {
    fn diagnostic_snapshot(&self) -> Snapshot {
        Snapshot {
            status: ShellStatus::from_wire(&self.shell_status),
            reason: ReadinessReason::from_wire(self.readiness_reason.as_deref().unwrap_or("")),
            ..self.readiness_details.map(|d| d.state).unwrap_or_default()
        }
    }
}

fn record_readiness(shared: &Shared, kind: EventKind, command_id: Option<Uuid>,
    before: Option<Snapshot>, control: &ControlState, elapsed_ms: Option<u64>) {
    if let Some(log) = &shared.diagnostic_log {
        log.record(Event {
            kind, session_id: shared.session_id, command_id, before,
            after: Some(control.diagnostic_snapshot()),
            input: control.readiness_details.map(|details| details.input),
            elapsed_ms,
        });
    }
}
pub struct Owner {
    pub identity: Identity,
    shared: Arc<Shared>,
    registry: PathBuf,
    worker: Option<thread::JoinHandle<()>>,
}
impl Owner {
    pub fn start(
        root: &Path,
        target: &str,
        machine: &str,
        session: Uuid,
        reference: Option<String>,
    ) -> Result<Self> {
        let authentication =
            Arc::new(IpcIdentity::current().context("authenticate local client program")?);
        ensure!(
            root.is_absolute(),
            "local control requires an absolute data root"
        );
        fs::create_dir_all(root)?;
        let instance = Uuid::now_v7();
        let identity = Identity {
            version: VERSION,
            instance_id: instance,
            scope_id: scope_id(root)?,
            target_id: target.into(),
            machine: machine.into(),
            session_id: session,
            reference,
            pipe: format!(r"\\.\pipe\arterm-client-{instance}"),
            pid: std::process::id(),
        };
        let diagnostic_log = match ReadinessLog::open(root, Role::Client) {
            Ok(log) => {
                crate::statusln!("[readiness] client diagnostic log: {}", log.path().display());
                Some(log)
            }
            Err(error) => {
                crate::statusln!("[readiness] client diagnostic log unavailable: {error:#}");
                None
            }
        };
        let shared = Arc::new(Shared {
            session_id: session,
            diagnostic_log,
            buffer: Mutex::new(Buffer::new()),
            state: Mutex::new("connecting".into()),
            detach: AtomicBool::new(false),
            stop: Arc::new(AtomicBool::new(false)),
            control: Mutex::new(ControlState {
                shell_status: "not_ready".into(),
                readiness_reason: Some("initializing".into()),
                ..ControlState::default()
            }),
            changed: Condvar::new(),
            waiters: AtomicUsize::new(0),
            service: Mutex::new(None),
        });
        let worker_shared = shared.clone();
        let worker_identity = identity.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let pipes = match (0..8)
                .map(|index| server_pipe_instance(&worker_identity.pipe, index == 0))
                .collect::<Result<Vec<_>>>()
            {
                Ok(pipes) => pipes,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let mut workers = Vec::new();
            for pipe in pipes {
                let worker_shared = worker_shared.clone();
                let worker_identity = worker_identity.clone();
                let authentication = authentication.clone();
                workers.push(thread::spawn(move || {
                    while !worker_shared.stop.load(Ordering::Acquire) {
                        let connected =
                            unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) };
                        let error = std::io::Error::last_os_error().raw_os_error();
                        if connected != 0 || error == Some(ERROR_PIPE_CONNECTED as i32) {
                            // One bounded request per connection; malformed peers cannot consume workers.
                            let result = (|| -> Result<()> {
                                let peer = authentication
                                    .authenticate(pipe.as_handle(), PeerEnd::Client)
                                    .context("authenticate local pipe caller")?;
                                serve(&pipe, &worker_identity, &worker_shared, &|| {
                                    peer.revalidate().map_err(anyhow::Error::from)
                                })
                            })();
                            if let Err(error) = result {
                                crate::statusln!("[local-control] Request failed: {error:#}");
                            }
                            unsafe {
                                DisconnectNamedPipe(pipe.as_raw_handle());
                            }
                        } else {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                }));
            }
            let _ = ready_tx.send(Ok(()));
            for worker in workers {
                let _ = worker.join();
            }
        });
        ready_rx
            .recv_timeout(RPC_TIMEOUT)
            .context("local IPC startup timed out")??;
        let directory = root.join("client").join("active");
        let registry = directory.join(format!("{instance}.json"));
        let mut owner = Self {
            identity,
            shared,
            registry,
            worker: Some(worker),
        };
        fs::create_dir_all(&directory)?;
        let temp = directory.join(format!("{instance}.tmp"));
        let publication = (|| -> Result<()> {
            let mut file = File::create(&temp)?;
            file.write_all(&serde_json::to_vec(&owner.identity)?)?;
            file.sync_all()?;
            fs::rename(&temp, &owner.registry)?;
            Ok(())
        })();
        if publication.is_err() {
            let _ = fs::remove_file(&temp);
        }
        publication?;
        owner.set_state("connecting");
        Ok(owner)
    }
    pub fn set_state(&mut self, state: &str) {
        *self.shared.state.lock().unwrap() = state.into();
        let control = self.shared.control.lock().unwrap();
        record_readiness(&self.shared, EventKind::Connection, None, None, &control, None);
    }
    pub fn set_service(
        &mut self,
        service: Arc<Service>,
    ) {
        *self.shared.service.lock().unwrap() = Some(service);
    }
    pub fn detached(&self) -> bool {
        self.shared.detach.load(Ordering::Acquire)
    }
    pub fn terminal<T: Terminal>(&self, terminal: T) -> ManagedTerminal<T> {
        ManagedTerminal {
            inner: terminal,
            shared: self.shared.clone(),
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.registry);
    }
}

pub struct ManagedTerminal<T> {
    inner: T,
    shared: Arc<Shared>,
}
impl<T: Terminal> Terminal for ManagedTerminal<T> {
    fn output(&mut self, bytes: &[u8]) -> Result<()> {
        {
            let mut buffer = self.shared.buffer.lock().unwrap();
            let (cols, rows) = self.inner.size();
            buffer
                .parser
                .screen_mut()
                .set_size(rows.clamp(1, 200), cols.clamp(1, 500));
            buffer.push(bytes);
        }
        self.inner.output(bytes)
    }
    fn input(&mut self, accept: bool) -> Result<Input> {
        if self.detach_requested() {
            return Ok(Input::Detach);
        }
        self.inner.input(accept)
    }
    fn size(&self) -> (u16, u16) {
        self.inner.size()
    }
    fn reading(&self, enabled: bool) {
        self.inner.reading(enabled);
    }
    fn connection_state(&mut self, state: &str) {
        *self.shared.state.lock().unwrap() = state.into();
        if state != "connected" {
            let mut control = self.shared.control.lock().unwrap();
            control.supported = false;
            control.capability_known = false;
            control.submitting = None;
            control.queue.clear();
        }
        self.shared.changed.notify_all();
    }
    fn detach_requested(&self) -> bool {
        self.shared.detach.load(Ordering::Acquire)
    }
    fn output_gap(&mut self) {
        self.shared.buffer.lock().unwrap().gap = true;
    }
    fn control(&mut self) -> Option<ControlMessage> {
        self.shared.control.lock().unwrap().queue.pop_front()
    }
    fn command_capability(&mut self, supported: bool) {
        let mut control = self.shared.control.lock().unwrap();
        control.supported = supported;
        control.capability_known = true;
        self.shared.changed.notify_all();
    }
    fn command_context(&mut self, enabled: bool, host_version: Option<&str>) {
        let mut control = self.shared.control.lock().unwrap();
        control.creation_enabled = Some(enabled);
        control.host_version = host_version.map(str::to_owned);
        control.readiness_details = None;
        control.shell_status = if enabled { "not_ready" } else { "unsupported" }.into();
        control.readiness_reason = Some(
            if enabled {
                "initializing"
            } else {
                "integration_disabled"
            }
            .into(),
        );
        self.shared.changed.notify_all();
    }
    fn command_output_progress(&mut self, seq: u64) {
        self.shared.control.lock().unwrap().delivered = seq;
        self.shared.changed.notify_all();
    }
    fn command_event(&mut self, kind: &str, body: &rmpv::Value) {
        let value = json_wire(body);
        let mut control = self.shared.control.lock().unwrap();
        let before = control.diagnostic_snapshot();
        let before_input = control.readiness_details.map(|details| details.input);
        if let Some(status) = value["shell_status"].as_str() {
            control.shell_status = status.into();
        }
        if let Some(reason) = value["readiness_reason"].as_str() {
            control.readiness_reason = Some(reason.into());
        } else if value.get("shell_status").is_some() {
            control.readiness_reason = None;
        }
        if let Some(enabled) = value["command_execution"].as_bool() {
            control.creation_enabled = Some(enabled);
        }
        if let Some(raw) = value.get("readiness_diagnostics").filter(|v| !v.is_null()) {
            let parsed = raw.as_str().filter(|text| text.len() <= 4096)
                .and_then(|text| serde_json::from_str::<Details>(text).ok());
            if parsed.is_none() && !control.bad_diagnostic_reported {
                crate::statusln!("[readiness] invalid host diagnostic details omitted");
                control.bad_diagnostic_reported = true;
            }
            control.readiness_details = parsed;
        }
        if !before.same_state(control.diagnostic_snapshot())
            || before_input != control.readiness_details.map(|details| details.input) {
            record_readiness(&self.shared, EventKind::HostState, None, Some(before), &control, None);
        }
        let barrier = value["after_output_seq"].as_u64().unwrap_or(0);
        if let Some(records) = value["records"].as_array() {
            for record in records {
                if let Some(id) = record["command_id"]
                    .as_str()
                    .and_then(|id| Uuid::parse_str(id).ok())
                {
                    let mut record = record.clone();
                    record["after_output_seq"] = barrier.into();
                    control.records.insert(id, record);
                }
            }
        }
        if let Some(id) = value["operation_id"]
            .as_str()
            .or_else(|| value["command_id"].as_str())
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            if control.submitting == Some(id)
                && matches!(
                    kind,
                    "CommandAccepted" | "CommandRejected" | "CommandNotSubmitted"
                )
            {
                control.submitting = None;
            }
            if kind == "CommandNotSubmitted" {
                control.submitted.remove(&id);
            }
            control
                .replies
                .insert(id, json!({"kind":kind, "body":value}));
        }
        self.shared.changed.notify_all();
    }
}

fn json_wire(value: &rmpv::Value) -> Value {
    match value {
        rmpv::Value::Nil => Value::Null,
        rmpv::Value::Boolean(value) => (*value).into(),
        rmpv::Value::Integer(value) => value
            .as_u64()
            .map(Value::from)
            .or_else(|| value.as_i64().map(Value::from))
            .unwrap_or(Value::Null),
        rmpv::Value::String(value) => value.as_str().map(Value::from).unwrap_or(Value::Null),
        rmpv::Value::Array(values) => Value::Array(values.iter().map(json_wire).collect()),
        rmpv::Value::Map(values) => Value::Object(
            values
                .iter()
                .filter_map(|(key, value)| key.as_str().map(|key| (key.into(), json_wire(value))))
                .collect(),
        ),
        _ => Value::Null,
    }
}

fn readiness_error(control: &ControlState) -> String {
    let hint = match (control.shell_status.as_str(), control.readiness_reason.as_deref()) {
        ("busy", _) => "a managed command owns input; query its command ID or wait for completion",
        ("unsupported", _) => "the host reports no supported shell integration for this session; use a new compatible PowerShell session",
        (_, Some("partial_human_input")) => "the host input tracker recorded a possible line edit; this is not proof that visible text remains; inspect the readiness diagnostic log",
        (_, Some("partial_input_sequence")) => "an incomplete terminal input sequence is pending; wait for it to finish",
        (_, Some("unclassified_terminal_input")) => "terminal control/editing input could not be classified; inspect the owning terminal and shell integration; this is not proof that a person typed a partial command",
        (_, Some("human_command_pending")) => "an interactive invocation has not returned to an integrated prompt",
        (_, Some("initializing")) => "IntegrationNotEstablished: shell integration has not emitted its first ready event; VT traffic is not evidence of support; inspect shell startup/profile compatibility",
        _ => "the host has not reported a safe prompt; inspect the owning terminal and shell/profile integration",
    };
    format!(
        "shell_status={}, reason={}: {hint}; no command input was sent",
        control.shell_status,
        control.readiness_reason.as_deref().unwrap_or("unreported")
    )
}

fn command_rpc(
    shared: &Shared,
    operation_id: Uuid,
    action: Operation,
    pipe: &File,
    caller_alive: &dyn Fn() -> Result<()>,
) -> Result<Value> {
    let occupied = if matches!(&action, Operation::Send { .. } | Operation::Wait { .. }) {
        Some(shared.waiters.fetch_add(1, Ordering::AcqRel))
    } else {
        None
    };
    let _slot = occupied.map(|_| WaitSlot(&shared.waiters));
    ensure!(
        occupied.is_none_or(|n| n < 4),
        "waiter capacity reached; query or retry explicitly"
    );
    let mut control = shared.control.lock().unwrap();
    if let Operation::Wait {
        command_id,
        timeout_ms,
    } = action
    {
        ensure!(timeout_ms > 0, "wait timeout must be positive");
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(timeout_ms))
            .context("timeout overflow")?;
        loop {
            caller_alive()?;
            if let Some(record) = control.records.get(&command_id) {
                if record["state"] == "unknown" {
                    return Ok(json!({"status":"unknown","command_id":command_id,"record":record}));
                }
                if record["state"] == "completed"
                    && control.delivered >= record["after_output_seq"].as_u64().unwrap_or(u64::MAX)
                {
                    return Ok(
                        json!({"status":"completed", "command_id":command_id, "record":record}),
                    );
                }
            }
            if shared.stop.load(Ordering::Acquire) {
                return Ok(json!({"status":"unknown","command_id":command_id}));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(
                    json!({"status":"timeout","phase":"completion","command_id":command_id,"submitted":true,"cancelled":false,
                    "record":control.records.get(&command_id)}),
                );
            }
            control = shared
                .changed
                .wait_timeout(control, (deadline - now).min(Duration::from_millis(100)))
                .unwrap()
                .0;
        }
    }
    let command_id = match &action {
        Operation::CommandStatus { command_id } => *command_id,
        _ => operation_id,
    };
    let id = operation_id;
    let mut actual = action.clone();
    let mut submission = None;
    let mut send_deadline = None;
    if let Operation::Send {
        command,
        timeout_ms,
    } = &action
    {
        let budget = timeout_ms.unwrap_or(READINESS_TIMEOUT_MS);
        ensure!(budget > 0, "readiness timeout must be positive");
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(budget))
            .context("timeout overflow")?;
        send_deadline = Some(deadline);
        let wait_started = Instant::now();
        let mut last_logged = None;
        let hash = format!("{:x}", Sha256::digest(command.as_bytes()));
        loop {
            let observed = control.diagnostic_snapshot();
            if last_logged.is_none_or(|previous: Snapshot| !previous.same_state(observed)) {
                record_readiness(shared, EventKind::CommandWait, Some(id), last_logged, &control,
                    Some(wait_started.elapsed().as_millis().min(u64::MAX as u128) as u64));
                last_logged = Some(observed);
            }
            caller_alive()?;
            ensure!(
                !shared.stop.load(Ordering::Acquire) && !shared.detach.load(Ordering::Acquire),
                "local session is stopping; command was not submitted"
            );
            let connection = shared.state.lock().unwrap().clone();
            ensure!(
                matches!(connection.as_str(), "connecting" | "connected"),
                "connection changed before submission; command was not submitted"
            );
            if control.capability_known {
                ensure!(control.supported, "remote host does not advertise command-execution-v1; update the remote host (existing sessions are not retrofitted)");
            }
            ensure!(control.creation_enabled != Some(false),
                "session was created without command integration; use a new supported PowerShell session after updating the remote host; saved identity was not changed");
            ensure!(
                !(control.capability_known || control.creation_enabled.is_some())
                    || control.shell_status != "unsupported",
                "{}", readiness_error(&control)
            );
            if let Some(previous) = control.submitted.get(&id) {
                ensure!(*previous == hash, "command ID conflicts with prior text");
                actual = Operation::CommandStatus { command_id: id };
                break;
            }
            if connection == "connected"
                && control.supported
                && control.shell_status == "ready"
                && control.submitting.is_none()
            {
                if Instant::now() >= deadline {
                    return Ok(
                        json!({"status":"timeout","phase":"readiness","command_id":id,"submitted":false,"cancelled":false}),
                    );
                }
                ensure!(
                    control.queue.len() < 16 && control.replies.len() < 512,
                    "local command capacity reached"
                );
                ensure!(
                    control.submitted.len() < 256,
                    "local command identity capacity reached"
                );
                caller_alive()?; // Last check at the admission boundary.
                let ticket = Arc::new(Submission {
                    state: AtomicU8::new(0),
                    deadline,
                    caller_pipe: Some(pipe.try_clone()?),
                });
                submission = Some(ticket);
                control.submitted.insert(id, hash);
                control.submitting = Some(id);
                record_readiness(shared, EventKind::CommandAdmitted, Some(id), last_logged, &control,
                    Some(wait_started.elapsed().as_millis().min(u64::MAX as u128) as u64));
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(
                    json!({"status":"timeout","phase":"readiness","command_id":id,"submitted":false,
                    "cancelled":false,"error":readiness_error(&control)}),
                );
            }
            control = shared
                .changed
                .wait_timeout(control, (deadline - now).min(Duration::from_millis(100)))
                .unwrap()
                .0;
        }
    }
    ensure!(
        *shared.state.lock().unwrap() == "connected" && control.supported,
        "local session is disconnected or host lacks command-execution-v1"
    );
    ensure!(
        control.queue.len() < 16 && control.replies.len() < 512,
        "local command capacity reached"
    );
    let _pending = PendingSubmission(submission.clone());
    control.replies.remove(&id);
    control.queue.push_back(ControlMessage {
        operation_id: id,
        action: actual,
        submission: submission.clone(),
    });
    let deadline = send_deadline.map_or(Instant::now() + Duration::from_secs(2), |deadline| {
        deadline.min(Instant::now() + Duration::from_secs(2))
    });
    loop {
        caller_alive()?;
        if let Some(reply) = control.replies.remove(&id) {
            let kind = reply["kind"].as_str().unwrap_or("");
            let status = match kind {
                "CommandRejected" => "rejected",
                "CommandInterruptAccepted" | "SessionInterruptAccepted" => "interrupt_requested",
                "CommandStatus" => "ok",
                "CommandNotSubmitted" => "timeout",
                _ => "accepted",
            };
            return Ok(
                json!({"status":status, "command_id":command_id, "host":reply["body"],
                "phase":if kind == "CommandNotSubmitted" { Some("readiness") } else { None },
                "submitted":kind != "CommandNotSubmitted",
                "record":control.records.get(&command_id), "error":reply["body"]["code"]}),
            );
        }
        let now = Instant::now();
        if now >= deadline || shared.stop.load(Ordering::Acquire) {
            if submission
                .as_ref()
                .is_some_and(|ticket| ticket.cancel_pending())
            {
                control.queue.retain(|request| request.operation_id != id);
                control.submitted.remove(&id);
                if control.submitting == Some(id) {
                    control.submitting = None;
                }
                shared.changed.notify_all();
                return Ok(
                    json!({"status":"timeout","phase":"readiness","command_id":id,"submitted":false,"cancelled":false}),
                );
            }
            if send_deadline.is_some_and(|deadline| now >= deadline) {
                return Ok(
                    json!({"status":"timeout","phase":"acceptance","command_id":id,"submitted":true,
                    "cancelled":false,"outcome":"unknown"}),
                );
            }
            return Ok(
                json!({"status":"unknown","command_id":id,"error":"remote outcome not yet known; query this ID; do not resubmit"}),
            );
        }
        control = shared
            .changed
            .wait_timeout(control, (deadline - now).min(Duration::from_millis(100)))
            .unwrap()
            .0;
    }
}

fn serve(
    pipe: &File,
    identity: &Identity,
    shared: &Shared,
    revalidate: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let request: Request = serde_json::from_slice(&read_frame(pipe)?)?;
    revalidate().context("revalidate local pipe caller before dispatch")?;
    ensure!(
        request.version == VERSION
            && request.instance_id == identity.instance_id
            && request.scope_id == identity.scope_id
            && request.target_id == identity.target_id
            && request.session_id == identity.session_id,
        "local IPC routing mismatch"
    );
    let request_started = Instant::now();
    let state = shared.state.lock().unwrap().clone();
    let command_request = matches!(&request.action, Operation::Send { .. });
    let mut result = json!({
        "schema_version": VERSION, "operation_id": request.operation_id,
        "identity": identity, "connection_state": state, "status": "ok"
    });
    match request.action {
        Operation::Inspect => {
            let control = shared.control.lock().unwrap();
            result["command_capability"] = control.supported.into();
            result["shell_status"] = control.shell_status.clone().into();
        }
        Operation::Read { lines } => match shared.buffer.lock().unwrap().snapshot(lines) {
            Ok(snapshot) => result["output"] = snapshot,
            Err(error) => {
                result["status"] = "invalid_request".into();
                result["error"] = error.to_string().into();
            }
        },
        Operation::Detach => {
            shared.detach.store(true, Ordering::Release);
            result["status"] = "detach_requested".into();
        }
        action @ (Operation::Terminate | Operation::FileSend { .. } | Operation::FileReceive { .. }) => {
            let service = shared.service.lock().unwrap().clone();
            let deadline = Instant::now() + if matches!(action, Operation::Terminate) {
                Duration::from_secs(120)
            } else { FILE_TIMEOUT };
            let response = (|| -> Result<Value> {
                let check = || {
                    ensure!(Instant::now() < deadline && !shared.stop.load(Ordering::Acquire),
                        "local service stopped or deadline expired");
                    check_caller(pipe, revalidate)
                };
                check()?;
                service.context("operation service is unavailable")?(request.operation_id, action, &check)
            })();
            match response {
                Ok(value) => {
                    for (key, value) in value.as_object().context("invalid service response")? {
                        result[key] = value.clone();
                    }
                }
                Err(error) => {
                    result["status"] = "error".into();
                    result["error"] = format!("{error:#}").into();
                }
            }
        }
        action @ (Operation::Send { .. }
        | Operation::Interrupt
        | Operation::CommandStatus { .. }
        | Operation::Wait { .. }) => {
            let response = command_rpc(shared, request.operation_id, action, pipe, &|| {
                revalidate().context("pending control caller changed or exited")?;
                let mut available = 0;
                ensure!(
                    unsafe {
                        PeekNamedPipe(
                            pipe.as_raw_handle(),
                            std::ptr::null_mut(),
                            0,
                            std::ptr::null_mut(),
                            &mut available,
                            std::ptr::null_mut(),
                        )
                    } != 0,
                    "pending control caller disconnected"
                );
                Ok(())
            });
            match response {
                Ok(value) => {
                    for (key, value) in value.as_object().unwrap() {
                        result[key] = value.clone();
                    }
                }
                Err(error) => {
                    result["status"] = "rejected".into();
                    result["error"] = error.to_string().into();
                }
            }
        }
    }
    {
        let control = shared.control.lock().unwrap();
        result["shell_status"] = control.shell_status.clone().into();
        result["command_capability"] = control.supported.into();
        result["command_execution"] = json!(control.creation_enabled);
        result["readiness_reason"] = json!(control.readiness_reason);
        result["host_version"] = json!(control.host_version);
        result["control_waiters"] = shared.waiters.load(Ordering::Acquire).into();
        result["readiness_diagnostics"] = serde_json::to_value(control.readiness_details)?;
        if command_request && result["status"] == "rejected" {
            record_readiness(shared, EventKind::CommandRejected, Some(request.operation_id), None, &control, None);
        }
        if command_request && result["status"] == "timeout" {
            record_readiness(shared, EventKind::CommandTimeout, Some(request.operation_id), None, &control,
                Some(request_started.elapsed().as_millis().min(u64::MAX as u128) as u64));
        }
    }
    if let Some(log) = &shared.diagnostic_log {
        result["diagnostic_log"] = log.path().to_string_lossy().into_owned().into();
    }
    result["connection_state"] = shared.state.lock().unwrap().clone().into();
    write_frame(pipe, &serde_json::to_vec(&result)?)?;
    let mut acknowledgement = [0];
    transfer(pipe, &mut acknowledgement, false)?;
    ensure!(acknowledgement == [1], "invalid response acknowledgement");
    Ok(())
}

fn check_caller(pipe: &File, revalidate: &dyn Fn() -> Result<()>) -> Result<()> {
    revalidate().context("local service caller changed or exited")?;
    let mut available = 0;
    ensure!(unsafe {
        PeekNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut(), 0,
            std::ptr::null_mut(), &mut available, std::ptr::null_mut())
    } != 0, "local service caller disconnected");
    Ok(())
}

pub fn discover(root: &Path) -> Result<Vec<Identity>> {
    let directory = root.join("client").join("active");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let scope = scope_id(root)?;
    let mut result = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        ensure!(
            fs::metadata(&path)?.len() <= 16384,
            "oversized local discovery record"
        );
        let identity: Identity =
            serde_json::from_slice(&fs::read(path)?).context("invalid local discovery record")?;
        ensure!(
            identity.scope_id == scope,
            "local discovery data-root mismatch"
        );
        match request(&identity, Operation::Inspect) {
            Ok(_) => result.push(identity),
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32)
                }) => {}
            Err(error) => return Err(error).context("local owner discovery failed"),
        }
    }
    Ok(result)
}

pub fn request(identity: &Identity, action: Operation) -> Result<Value> {
    request_with_id(identity, action, Uuid::now_v7())
}

pub fn request_with_id(
    identity: &Identity,
    action: Operation,
    operation_id: Uuid,
) -> Result<Value> {
    let authentication = IpcIdentity::current().context("authenticate local client program")?;
    let file_request = matches!(&action, Operation::FileSend { .. } | Operation::FileReceive { .. });
    let response_timeout = if file_request {
        FILE_TIMEOUT + Duration::from_secs(120)
    } else if matches!(&action, Operation::Terminate) {
        Duration::from_secs(120)
    } else if let Operation::Send { timeout_ms, .. } = &action {
        Duration::from_millis(timeout_ms.unwrap_or(READINESS_TIMEOUT_MS))
            .checked_add(RPC_TIMEOUT)
            .context("timeout overflow")?
    } else if let Operation::Wait { timeout_ms, .. } = &action {
        Duration::from_millis(*timeout_ms)
            .checked_add(RPC_TIMEOUT)
            .context("timeout overflow")?
    } else {
        RPC_TIMEOUT
    };
    ensure!(
        identity.version == VERSION
            && identity.pipe == format!(r"\\.\pipe\arterm-client-{}", identity.instance_id),
        "invalid local endpoint identity"
    );
    let name = wide(&identity.pipe);
    let deadline = Instant::now() + RPC_TIMEOUT;
    let file = loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break unsafe { File::from_raw_handle(handle) };
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) || Instant::now() >= deadline {
            return Err(error).context("local connection is not active");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let peer = authentication
        .authenticate(file.as_handle(), PeerEnd::Server)
        .context("authenticate local pipe owner")?;
    let mut pid = 0;
    ensure!(
        unsafe { GetNamedPipeServerProcessId(file.as_raw_handle(), &mut pid) } != 0
            && pid == identity.pid
            && process_sid(pid)? == process_sid(std::process::id())?,
        "local endpoint owner mismatch"
    );
    let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
    ensure!(
        unsafe {
            SetNamedPipeHandleState(
                file.as_raw_handle(),
                &mode,
                std::ptr::null(),
                std::ptr::null(),
            )
        } != 0,
        "cannot configure local IPC"
    );
    peer.revalidate()
        .context("revalidate local owner before request")?;
    write_frame(
        &file,
        &serde_json::to_vec(&Request {
            version: VERSION,
            operation_id,
            instance_id: identity.instance_id,
            scope_id: identity.scope_id.clone(),
            target_id: identity.target_id.clone(),
            session_id: identity.session_id,
            action,
        })?,
    )?;
    let result = (|| -> Result<Value> {
        let response: Value = serde_json::from_slice(&read_frame_timeout_checked(
            &file, response_timeout, &|| peer.revalidate().context("local owner exited or changed while awaiting response"),
        )?)?;
        peer.revalidate().context("revalidate local owner response")?;
        ensure!(
            response["operation_id"] == operation_id.to_string()
                && response["identity"]["instance_id"] == identity.instance_id.to_string(),
            "local response correlation mismatch"
        );
        transfer(&file, &mut [1], true)?;
        Ok(response)
    })();
    match result {
        Err(error) if file_request => Ok(json!({
            "schema_version": VERSION,
            "operation_id": operation_id,
            "identity": identity,
            "status": "unknown",
            "operation_kind": "file_transfer",
            "commit_may_have_started": true,
            "automatic_retry": false,
            "error": format!("Local result channel lost after file-request dispatch: {error:#}. Inspect the destination; do not automatically retry.")
        })),
        other => other,
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn scope_id(root: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(
        fs::canonicalize(root)?
            .to_string_lossy()
            .to_lowercase()
            .as_bytes(),
    );
    hash.update([0]);
    hash.update(process_sid(std::process::id())?.as_bytes());
    Ok(format!("{:x}", hash.finalize()))
}
fn process_sid(pid: u32) -> Result<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(!process.is_null(), "cannot inspect local IPC process");
        let mut token = std::ptr::null_mut();
        let ok = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        CloseHandle(process);
        ensure!(ok != 0, "cannot inspect local IPC token");
        let mut length = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut length);
        let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        );
        CloseHandle(token);
        ensure!(ok != 0, "cannot inspect local IPC SID");
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = std::ptr::null_mut();
        ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut sid) != 0,
            "cannot format SID"
        );
        let mut length = 0;
        while *sid.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(sid, length));
        LocalFree(sid.cast());
        Ok(result?)
    }
}
#[cfg(test)]
fn server_pipe(name: &str) -> Result<File> {
    server_pipe_instance(name, true)
}
fn server_pipe_instance(name: &str, first: bool) -> Result<File> {
    let sid = process_sid(std::process::id())?;
    let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
    let mut descriptor = std::ptr::null_mut();
    ensure!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } != 0,
        "cannot create IPC ACL"
    );
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let name = wide(name);
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX
                | if first {
                    FILE_FLAG_FIRST_PIPE_INSTANCE
                } else {
                    0
                },
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            8,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    unsafe {
        LocalFree(descriptor);
    }
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "cannot create local IPC: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_handle(handle) })
}
fn transfer(file: &File, bytes: &mut [u8], writing: bool) -> Result<()> {
    transfer_timeout(file, bytes, writing, RPC_TIMEOUT)
}
fn transfer_timeout(file: &File, bytes: &mut [u8], writing: bool, timeout: Duration) -> Result<()> {
    transfer_timeout_checked(file, bytes, writing, timeout, &|| Ok(()))
}
fn transfer_timeout_checked(
    file: &File, bytes: &mut [u8], writing: bool, timeout: Duration, check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("timeout overflow")?;
    let mut offset = 0;
    let mut next_check = Instant::now();
    while offset < bytes.len() {
        ensure!(Instant::now() < deadline, "local IPC deadline expired");
        if Instant::now() >= next_check {
            check()?;
            next_check = Instant::now() + Duration::from_millis(100);
        }
        let mut file = file;
        let result = if writing {
            file.write(&bytes[offset..])
        } else {
            file.read(&mut bytes[offset..])
        };
        match result {
            Ok(0) => {
                if !writing { check_read_pipe(file)?; }
                thread::sleep(Duration::from_millis(2));
            }
            Ok(n) => offset += n,
            Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA as i32) => {
                if !writing { check_read_pipe(file)?; }
                thread::sleep(Duration::from_millis(2))
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn check_read_pipe(file: &File) -> Result<()> {
    let mut available = 0;
    if unsafe { PeekNamedPipe(file.as_raw_handle(), std::ptr::null_mut(), 0,
        std::ptr::null_mut(), &mut available, std::ptr::null_mut()) } == 0
    {
        return Err(std::io::Error::last_os_error()).context("local result channel closed");
    }
    Ok(())
}
fn read_frame(file: &File) -> Result<Vec<u8>> {
    read_frame_timeout(file, RPC_TIMEOUT)
}
fn read_frame_timeout(file: &File, timeout: Duration) -> Result<Vec<u8>> {
    read_frame_timeout_checked(file, timeout, &|| Ok(()))
}
fn read_frame_timeout_checked(file: &File, timeout: Duration, check: &dyn Fn() -> Result<()>) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    transfer_timeout_checked(file, &mut length, false, timeout, check)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(
        (1..=MAX_FRAME).contains(&length),
        "invalid local frame length"
    );
    let mut bytes = vec![0; length];
    transfer_timeout_checked(file, &mut bytes, false, RPC_TIMEOUT, check)?;
    Ok(bytes)
}
fn write_frame(file: &File, bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= MAX_FRAME, "local frame too large");
    let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(bytes);
    transfer(file, &mut framed, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admitted_readiness_timeouts_are_logged_once_for_all_terminal_paths() {
        for reply_instead_of_expire in [false, true] {
            let root = std::env::temp_dir().join(format!("arterm-ready-log-{}", Uuid::now_v7()));
            let mut owner = Owner::start(&root, "diagnostic-target", "fixture", Uuid::now_v7(), None).unwrap();
            owner.set_state("connected");
            {
                let mut control = owner.shared.control.lock().unwrap();
                control.supported = true;
                control.capability_known = true;
                control.creation_enabled = Some(true);
                control.shell_status = "ready".into();
                control.readiness_reason = Some("ready".into());
            }
            let responder = if reply_instead_of_expire {
                let shared = owner.shared.clone();
                Some(thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    loop {
                        let mut control = shared.control.lock().unwrap();
                        if let Some(message) = control.queue.pop_front() {
                            message.submission.as_ref().unwrap().cancel_pending();
                            control.submitted.remove(&message.operation_id);
                            control.submitting = None;
                            control.replies.insert(message.operation_id, json!({
                                "kind":"CommandNotSubmitted", "body":{"code":"ReadinessDeadline"}
                            }));
                            shared.changed.notify_all();
                            break;
                        }
                        drop(control);
                        assert!(Instant::now() < deadline, "no admitted command");
                        thread::sleep(Duration::from_millis(1));
                    }
                }))
            } else { None };
            let response = request(&owner.identity, Operation::Send {
                command: "PRIVATE_COMMAND_CANARY".into(),
                timeout_ms: Some(if reply_instead_of_expire { 2000 } else { 30 }),
            }).unwrap();
            if let Some(responder) = responder { responder.join().unwrap(); }
            assert_eq!(response["status"], "timeout");
            assert_eq!(response["phase"], "readiness");
            assert_eq!(response["submitted"], false);
            let log = owner.shared.diagnostic_log.as_ref().unwrap();
            assert!(log.flush(Duration::from_secs(2)));
            let data = fs::read_to_string(log.path()).unwrap();
            assert!(!data.contains("PRIVATE_COMMAND_CANARY"));
            let events: Vec<Value> = data.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
            for kind in ["CommandAdmitted", "CommandTimeout"] {
                assert_eq!(events.iter().filter(|event| event["event"]["kind"] == kind
                    && event["event"]["command_id"] == response["command_id"]).count(), 1);
            }
            drop(owner);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn readiness_admission_expires_cancels_and_dispatches_at_most_once() {
        assert_eq!(READINESS_TIMEOUT_MS, 30_000);
        let cancelled = Arc::new(Submission {
            state: AtomicU8::new(0),
            deadline: Instant::now() + Duration::from_secs(1),
            caller_pipe: None,
        });
        drop(PendingSubmission(Some(cancelled.clone())));
        assert!(!cancelled.dispatch());
        let expired = Submission {
            state: AtomicU8::new(0),
            deadline: Instant::now(),
            caller_pipe: None,
        };
        assert!(!expired.dispatch());
        let sent = Submission {
            state: AtomicU8::new(0),
            deadline: Instant::now() + Duration::from_secs(1),
            caller_pipe: None,
        };
        assert!(sent.dispatch());
        assert!(
            !sent.cancel_pending(),
            "a dispatched command must not be cancelled by waiter teardown"
        );
        assert!(!sent.dispatch(), "the same admission cannot execute twice");
    }
    #[test]
    fn startup_without_markers_is_finite_and_unsupported_is_immediate() {
        let root = std::env::temp_dir().join(format!("arterm-startup-{}", Uuid::now_v7()));
        let mut owner = Owner::start(&root, "target", "machine", Uuid::now_v7(), None).unwrap();
        let start = Instant::now();
        let response = request(&owner.identity, Operation::Send {
            command: "Get-Date".into(), timeout_ms: Some(50),
        }).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(50) && start.elapsed() < Duration::from_secs(2));
        assert_eq!(response["status"], "timeout");
        assert_eq!(response["phase"], "readiness");
        assert_eq!(response["submitted"], false);
        assert_eq!(response["connection_state"], "connecting");
        assert_eq!(response["shell_status"], "not_ready");
        assert_eq!(response["readiness_reason"], "initializing");
        owner.set_state("connected");
        {
            let mut control = owner.shared.control.lock().unwrap();
            control.supported = true;
            control.capability_known = true;
            control.creation_enabled = Some(true);
            control.shell_status = "not_ready".into();
            control.readiness_reason = Some("initializing".into());
        }
        let start = Instant::now();
        let response = request(&owner.identity, Operation::Send {
            command: "Get-Date".into(), timeout_ms: Some(50),
        }).unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(response["status"], "timeout");
        assert_eq!(response["phase"], "readiness");
        assert_eq!(response["submitted"], false);
        assert!(response["error"].as_str().unwrap().contains("IntegrationNotEstablished"));
        {
            let mut control = owner.shared.control.lock().unwrap();
            assert!(control.queue.is_empty() && control.submitted.is_empty());
            control.shell_status = "unsupported".into();
        }
        let start = Instant::now();
        let response = request(&owner.identity, Operation::Send {
            command: "Get-Date".into(), timeout_ms: Some(30_000),
        }).unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(response["status"], "rejected");
        assert!(response["error"].as_str().unwrap().contains("no supported shell integration"));
        drop(owner);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn readiness_diagnostics_distinguish_shell_and_input_states() {
        for (status, reason, expected) in [
            ("unsupported", None, "no supported shell integration"),
            (
                "busy",
                Some("managed_command"),
                "managed command owns input",
            ),
            ("not_ready", Some("initializing"), "first ready event"),
            (
                "not_ready",
                Some("partial_human_input"),
                "host input tracker recorded a possible line edit",
            ),
            (
                "not_ready",
                Some("unclassified_terminal_input"),
                "not proof that a person typed",
            ),
            ("not_ready", None, "host has not reported a safe prompt"),
        ] {
            let control = ControlState {
                shell_status: status.into(),
                readiness_reason: reason.map(str::to_owned),
                ..ControlState::default()
            };
            let error = readiness_error(&control);
            assert!(
                error.contains(expected) && error.contains(status),
                "{error}"
            );
        }
    }
    #[test]
    fn vt_snapshot_overwrites_and_decodes_split_unicode() {
        let mut buffer = Buffer::new();
        buffer.push(b"old\r\x1b[2Knew\r\n\x1b[31mred\x1b[0m ");
        buffer.push(&[0xe2, 0x82]);
        buffer.push(&[0xac]);
        let snapshot = buffer.snapshot(20).unwrap();
        assert_eq!(snapshot["lines"], json!(["new", "red €"]));
        assert!(buffer.snapshot(0).is_err());
        assert_eq!(buffer.snapshot(1).unwrap()["truncated"], true);
    }
    #[test]
    fn oversized_osc_is_discarded_before_vt_parser_and_memory_is_bounded() {
        let mut buffer = Buffer::new();
        buffer.push(b"\x1b]0;");
        let chunk = vec![b'x'; 4096];
        for _ in 0..8192 {
            buffer.push(&chunk);
            assert!(buffer.controls.pending.len() <= ControlFilter::LIMIT);
            assert!(buffer.controls.pending.capacity() <= ControlFilter::LIMIT);
        }
        assert_eq!(
            buffer.snapshot(20).unwrap()["discarded_control_sequences"],
            1
        );
        buffer.push(b"\x07visible");
        let snapshot = buffer.snapshot(20).unwrap();
        assert_eq!(snapshot["lines"], json!(["visible"]));
        assert_eq!(snapshot["truncated"], true);
        let unicode = "\u{41d}\u{41e}\u{41f}".as_bytes(); // UTF-8 continuation bytes overlap C1 codes.
        for byte in unicode {
            buffer.push(&[*byte]);
        }
        assert!(buffer.snapshot(20).unwrap()["lines"][0]
            .as_str()
            .unwrap()
            .ends_with("\u{41d}\u{41e}\u{41f}"));
    }
    #[test]
    fn scrollback_is_bounded_and_deep_snapshots_do_not_panic() {
        let mut buffer = Buffer::new();
        for n in 0..4000 {
            buffer.push(format!("line{n}\r\n").as_bytes());
        }
        let snapshot = buffer.snapshot(20).unwrap();
        assert_eq!(snapshot["lines"].as_array().unwrap().len(), 20);
        assert_eq!(snapshot["lines"][19], "line3999");
        assert_eq!(snapshot["history_may_be_incomplete"], true);
    }
    #[test]
    fn pipe_acl_grants_only_current_user() {
        let pipe = server_pipe(&format!(r"\\.\pipe\arterm-acl-test-{}", Uuid::now_v7())).unwrap();
        unsafe {
            let mut dacl = std::ptr::null_mut();
            let mut descriptor = std::ptr::null_mut();
            assert_eq!(
                GetSecurityInfo(
                    pipe.as_raw_handle(),
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut dacl,
                    std::ptr::null_mut(),
                    &mut descriptor
                ),
                0
            );
            assert!(!dacl.is_null());
            assert_eq!((*dacl).AceCount, 1);
            let mut ace = std::ptr::null_mut();
            assert_ne!(GetAce(dacl, 0, &mut ace), 0);
            let ace = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            assert_eq!(ace.Header.AceType, 0, "ACCESS_ALLOWED_ACE_TYPE");
            let text = wide(&process_sid(std::process::id()).unwrap());
            let mut sid = std::ptr::null_mut();
            assert_ne!(ConvertStringSidToSidW(text.as_ptr(), &mut sid), 0);
            assert_ne!(
                EqualSid((&ace.SidStart as *const u32).cast_mut().cast(), sid),
                0
            );
            LocalFree(sid);
            LocalFree(descriptor);
        }
    }
    #[test]
    fn owner_routes_read_detach_and_rejects_wrong_identity() {
        let root = std::env::temp_dir().join(format!("arterm-ipc-{}", Uuid::now_v7()));
        let owner = Owner::start(
            &root,
            "target",
            "machine",
            Uuid::now_v7(),
            Some("work".into()),
        )
        .unwrap();
        owner.shared.buffer.lock().unwrap().push(b"hello");
        assert_eq!(discover(&root).unwrap().len(), 1);
        let result = request(&owner.identity, Operation::Read { lines: 20 }).unwrap();
        assert_eq!(result["output"]["lines"], json!(["hello"]));
        let mut wrong = owner.identity.clone();
        wrong.target_id = "wrong".into();
        assert!(request(&wrong, Operation::Inspect).is_err());
        request(&owner.identity, Operation::Detach).unwrap();
        assert!(owner.detached());
        let record = owner.registry.clone();
        let identity = owner.identity.clone();
        drop(owner);
        assert!(discover(&root).unwrap().is_empty());
        fs::write(&record, serde_json::to_vec(&identity).unwrap()).unwrap();
        assert!(
            discover(&root).unwrap().is_empty(),
            "stale endpoint is not active"
        );
        assert!(record.exists(), "discovery must not delete state");
        fs::remove_dir_all(root).unwrap();
    }
}
