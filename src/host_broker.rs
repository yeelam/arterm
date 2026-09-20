use anyhow::{bail, ensure, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use rmpv::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, TerminateJobObject, QueryInformationJobObject,
            JobObjectBasicAccountingInformation, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        },
        Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
    },
};

use crate::host_pipe;
use arterm::{
    deployment, store,
    file_transfer::{AuthorizedSession, Limits, TransferManager, TransferState, MAX_CHUNK_BYTES},
    transfer_payload::{self, PayloadReceipt, PayloadStatus, PreparedSource, SourceMetadata, UnavailableArchive},
    shell_integration::{Commands, CAPABILITY as COMMAND_CAPABILITY},
    wire::{self, bin16, binary, get, map, message, num, s, text, Frames},
};

const CAPS: &[&str] = &[
    "session-create",
    "create-reservation",
    "session-resume",
    "output-sequence",
    "input-ack",
    "writer-lease",
    "connection-epoch",
    "resize-generation",
    "client-session-id",
    arterm::engine::ENDED_SESSION_CAPABILITY,
    COMMAND_CAPABILITY,
    "host-owner-management-v1",
    "session-termination-confirmed",
    arterm::transfer_admission::CAPABILITY,
    transfer_payload::METADATA_CAPABILITY,
];
const MAX_SESSIONS: usize = 16;
const MAX_REPLAY_BYTES: usize = 8 * 1024 * 1024;
const MAX_REPLAY_AGE: Duration = Duration::from_secs(10 * 60);
const MAX_INPUT_QUEUE: usize = 16;
const PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const EXITED_HISTORY_RETENTION: Duration = Duration::from_secs(15 * 60);
const MAX_EXITED_HISTORY: usize = 64;
struct Job(HANDLE);
unsafe impl Send for Job {}
unsafe impl Sync for Job {}
impl Job {
    fn new() -> Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(
            !handle.is_null(),
            "cannot create host job object: {}",
            std::io::Error::last_os_error()
        );
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        };
        if ok == 0 {
            unsafe {
                CloseHandle(handle);
            }
            bail!(
                "cannot configure host job object: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(Self(handle))
    }
    fn assign(&self, pid: u32) -> Result<()> {
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        ensure!(
            !process.is_null(),
            "cannot open ConPTY child process: {}",
            std::io::Error::last_os_error()
        );
        let ok = unsafe { AssignProcessToJobObject(self.0, process) };
        unsafe {
            CloseHandle(process);
        }
        ensure!(
            ok != 0,
            "cannot assign ConPTY child to host job: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
}
impl Job {
    fn terminate(&self) -> Result<()> {
        ensure!(unsafe { TerminateJobObject(self.0, 1) } != 0,
            "cannot terminate session job: {}", std::io::Error::last_os_error());
        Ok(())
    }
    fn empty(&self) -> Result<bool> {
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        ensure!(unsafe { QueryInformationJobObject(self.0, JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            std::mem::size_of_val(&info) as u32, std::ptr::null_mut()) } != 0,
            "cannot inspect session job: {}", std::io::Error::last_os_error());
        Ok(info.ActiveProcesses == 0)
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn exited_history_removals(
    mut exited: Vec<(Uuid, Instant)>,
    now: Instant,
    retention: Duration,
    cap: usize,
) -> Vec<Uuid> {
    let mut remove = exited
        .iter()
        .filter_map(|(id, at)| {
            now.checked_duration_since(*at)
                .filter(|age| *age > retention)
                .map(|_| *id)
        })
        .collect::<Vec<_>>();
    exited.retain(|(id, _)| !remove.contains(id));
    exited.sort_by_key(|(_, at)| *at);
    let excess = exited.len().saturating_sub(cap);
    remove.extend(exited.into_iter().take(excess).map(|(id, _)| id));
    remove
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn random16() -> Result<Vec<u8>> {
    Ok(store::random_claim()?[..16].to_vec())
}

#[derive(Clone)]
struct OutputChunk {
    seq: u64,
    bytes: Vec<u8>,
    at: Instant,
}
#[derive(Clone)]
struct Attachment {
    id: Vec<u8>,
    lease: Vec<u8>,
    client: Vec<u8>,
    epoch: u64,
}
struct SessionState {
    output: VecDeque<OutputChunk>,
    output_bytes: usize,
    next_output: u64,
    attachment: Option<Attachment>,
    attachment_connection: Option<Arc<AtomicBool>>,
    highest_epochs: HashMap<Vec<u8>, u64>,
    input_committed: u64,
    resize_generation: u64,
    exit: Option<u32>,
    exited_at: Option<Instant>,
    pid: u32,
    commands: Option<Commands>,
}
struct Session {
    id: Uuid,
    token: Vec<u8>,
    state: Mutex<SessionState>,
    input: Mutex<Option<SyncSender<Vec<u8>>>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    job: Job,
    transfers: Mutex<FileState>,
}

#[derive(Default)]
struct FileState {
    manager: Option<TransferManager>,
    operations: HashMap<Uuid, Uuid>,
    sources: HashMap<Uuid, SourceMetadata>,
    preparations: HashMap<Uuid, PreparedSource<UnavailableArchive>>,
}

struct FileBridge {
    session: Arc<Session>,
    attachment: Attachment,
    connected: Arc<AtomicBool>,
    directory_capable: bool,
    deadline: Instant,
    ids: Vec<Uuid>,
}

impl FileBridge {
    fn authorize(&self) -> Result<std::sync::MutexGuard<'_, SessionState>> {
        ensure!(Instant::now() < self.deadline, "FileDeadlineExpired");
        ensure!(self.connected.load(Ordering::Acquire), "FileDisconnected");
        let state = self.session.state.lock().unwrap();
        ensure!(state.exit.is_none() && state.attachment.as_ref().is_some_and(|a|
            a.id == self.attachment.id && a.lease == self.attachment.lease &&
            a.client == self.attachment.client && a.epoch == self.attachment.epoch),
            "FileLeaseRevoked");
        ensure!(state.attachment_connection.as_ref().is_some_and(|live| live.load(Ordering::Acquire)),
            "FileWriterDisconnected");
        Ok(state)
    }

    fn request(&mut self, kind: &str, body: &Value, progress: &dyn Fn() -> Result<()>)
        -> Result<(serde_json::Value, Option<Vec<u8>>)> {
        drop(self.authorize()?);
        let session = self.session.clone();
        let mut files = session.transfers.try_lock().map_err(|_| anyhow::anyhow!("FileBusy"))?;
        // Revalidate after taking the independent file lock, never a terminal/global lock during I/O.
        drop(self.authorize()?);
        if matches!(kind, "FileBeginUpload" | "FileBeginDownload") {
            let operation = Uuid::parse_str(text(body, "operation_id")?)?;
            ensure!(!files.operations.contains_key(&operation), "FileDuplicateOperation");
            ensure!(files.operations.len() < 256, "FileOperationCapacity");
            let check = || { drop(self.authorize()?); progress() };
            let prepared = if kind == "FileBeginDownload" {
                Some(transfer_payload::prepare_source(
                    arterm::file_transfer::pin_source(Path::new(text(body, "path")?))?,
                    self.directory_capable, &check, transfer_payload::archive_unavailable)?)
            } else { None };
            let source = if let Some(prepared) = &prepared { prepared.metadata().clone() }
                else { SourceMetadata::from_wire(body)? };
            source.require_directory_support(self.directory_capable)?;
            if kind == "FileBeginUpload" && source.kind == transfer_payload::SourceKind::File {
                ensure!(text(body, "path")? == source.original_basename, "FileSourceMetadataMismatch");
            }
            if files.manager.is_none() {
                files.manager = Some(TransferManager::new(AuthorizedSession::after_authorization(session.id),
                    &std::env::temp_dir().components().collect::<PathBuf>(), Limits::default())?);
            }
            let manager = files.manager.as_mut().context("FileManagerMissing")?;
            let status = if kind == "FileBeginUpload" {
                manager.begin_upload(text(body, "path")?, num(body, "size")?)?
            } else { manager.begin_download(prepared.as_ref().context("FilePreparationMissing")?.payload_path())? };
            self.ids.push(status.transfer_id);
            files.operations.insert(operation, status.transfer_id);
            files.sources.insert(status.transfer_id, source.clone());
            if let Some(prepared) = prepared { files.preparations.insert(status.transfer_id, prepared); }
            drop(self.authorize()?);
            return Ok((serde_json::to_value(PayloadStatus { payload: status, source })?, None));
        }
        let id = Uuid::parse_str(text(body, "transfer_id")?)?;
        ensure!(self.ids.contains(&id), "FileUnauthorizedTransfer");
        let source = files.sources.get(&id).context("FileSourceMetadataMissing")?.clone();
        if kind == "FileClose" {
            if let Some(prepared) = files.preparations.get(&id) {
                prepared.verify_sources(&|| { drop(self.authorize()?); progress() })?;
            }
        }
        let manager = files.manager.as_mut().context("FileManagerMissing")?;
        let result = match kind {
            "FileWrite" => {
                let bytes = binary(body, "bytes")?;
                ensure!(bytes.len() <= MAX_CHUNK_BYTES, "FileChunkTooLarge");
                serde_json::json!({"offset":manager.write_chunk(id, num(body, "offset")?, &bytes)?})
            }
            "FileRead" => {
                let chunk = manager.read_chunk(id, num(body, "offset")?, MAX_CHUNK_BYTES)?;
                drop(self.authorize()?);
                return Ok((serde_json::json!({"offset":chunk.offset, "eof":chunk.eof}), Some(chunk.bytes)));
            }
            "FileFinish" => {
                let hash: [u8; 32] = binary(body, "sha256")?.try_into()
                    .map_err(|_| anyhow::anyhow!("FileInvalidDigest"))?;
                let receipt = manager.finish_guarded(id, hash, || self.authorize())?;
                serde_json::to_value(transfer_payload::complete_payload(manager, source, receipt,
                    &|| { drop(self.authorize()?); progress() }, transfer_payload::extraction_unavailable)?)?
            }
            "FileClose" => serde_json::to_value(PayloadReceipt {
                payload: manager.close(id)?, source, extracted_bytes: None,
            })?,
            "FileCancel" => serde_json::to_value(manager.cancel(id)?)?,
            _ => bail!("FileUnsupportedOperation"),
        };
        if matches!(kind, "FileClose" | "FileCancel") { files.preparations.remove(&id); }
        drop(self.authorize()?);
        Ok((result, None))
    }
}

impl Drop for FileBridge {
    fn drop(&mut self) {
        if self.ids.is_empty() { return; }
        let mut files = self.session.transfers.lock().unwrap();
        if let Some(manager) = &mut files.manager {
            for id in &self.ids {
                if manager.status(*id).is_ok_and(|s| matches!(s.state, TransferState::Uploading | TransferState::Downloading)) {
                    if let Err(error) = manager.cancel(*id) {
                        arterm::statusln!("[file] Partial cleanup failed: {error:#}");
                    }
                }
            }
        }
        for id in &self.ids { files.preparations.remove(id); }
    }
}

struct FileConnection(Arc<AtomicBool>);
impl Drop for FileConnection {
    fn drop(&mut self) { self.0.store(false, Ordering::Release); }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct Fingerprint {
    session_id: Uuid,
    claim: Vec<u8>,
    shell: String,
    args: Vec<String>,
    cwd: Option<String>,
    cols: u16,
    rows: u16,
    #[serde(default)]
    command_execution: bool,
}
#[derive(Serialize, Deserialize, Clone)]
struct Reservation {
    schema: u32,
    request_id: Uuid,
    broker: Vec<u8>,
    fingerprint: Fingerprint,
    token: Vec<u8>,
    created_ms: u64,
    active: bool,
}

struct Broker {
    root: PathBuf,
    pipe: String,
    instance: Vec<u8>,
    sessions: Mutex<HashMap<Uuid, Arc<Session>>>,
    reservations: Mutex<HashMap<Uuid, Reservation>>,
    create_lock: Mutex<()>,
    stopping: AtomicBool,
}

pub struct RunGuard {
    _lock: File,
}

pub fn pipe_name(root: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().to_ascii_lowercase().as_bytes());
    hasher.update(host_pipe::current_user_sid()?.as_bytes());
    let mut session = 0;
    ensure!(
        unsafe {
            windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(
                windows_sys::Win32::System::Threading::GetCurrentProcessId(),
                &mut session,
            )
        } != 0,
        "cannot resolve logon session: {}",
        std::io::Error::last_os_error()
    );
    hasher.update(session.to_le_bytes());
    let digest = format!("{:x}", hasher.finalize());
    Ok(format!("vsterm-{}-{session}", &digest[..24]))
}

fn acquire(root: &Path, pipe: &str) -> Result<RunGuard> {
    fs::create_dir_all(root.join("host"))?;
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .share_mode(0)
        .open(root.join("host").join(format!("{pipe}.lock")))
        .context("host is already running for this user, logon session, and data root")?;
    Ok(RunGuard { _lock: lock })
}

fn reservation_path(root: &Path, request: Uuid) -> PathBuf {
    root.join("host")
        .join("requests")
        .join(format!("{request}.dpapi"))
}
fn save_reservation(root: &Path, reservation: &Reservation) -> Result<()> {
    let dir = root.join("host").join("requests");
    fs::create_dir_all(&dir)?;
    let bytes = store::protect(&serde_json::to_vec(reservation)?, true)?;
    let path = reservation_path(root, reservation.request_id);
    let temp = dir.join(format!("{}.tmp", Uuid::now_v7()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, &path).or_else(|_| {
        let _ = fs::remove_file(&path);
        fs::rename(&temp, &path)
    })?;
    Ok(())
}
fn load_reservation(root: &Path, request: Uuid) -> Result<Option<Reservation>> {
    let path = reservation_path(root, request);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = store::protect(&fs::read(path)?, false)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

impl Session {
    fn spawn(id: Uuid, token: Vec<u8>, fp: &Fingerprint) -> Result<Arc<Self>> {
        let job = Job::new()?;
        let pair = native_pty_system().openpty(PtySize {
            rows: fp.rows,
            cols: fp.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut command = CommandBuilder::new(&fp.shell);
        command.args(&fp.args);
        let commands = if fp.command_execution {
            ensure!(Commands::supported(&fp.shell, &fp.args), "CommandShellUnsupported");
            let commands = Commands::new();
            command.args(["-NoExit", "-EncodedCommand", &commands.bootstrap_encoded()]);
            Some(commands)
        } else { None };
        if let Some(cwd) = &fp.cwd {
            command.cwd(cwd);
        }
        let mut child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to start {}", fp.shell))?;
        drop(pair.slave);
        let pid = child
            .process_id()
            .context("ConPTY child has no process id")?;
        if let Err(error) = job.assign(pid) { let _ = child.kill(); return Err(error); }
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let (input_tx, input_rx) = mpsc::sync_channel::<Vec<u8>>(MAX_INPUT_QUEUE);
        let session = Arc::new(Self {
            id,
            token,
            transfers: Mutex::new(FileState::default()),
            state: Mutex::new(SessionState {
                output: VecDeque::new(),
                output_bytes: 0,
                next_output: 1,
                attachment: None,
                attachment_connection: None,
                highest_epochs: HashMap::new(),
                input_committed: 0,
                resize_generation: 0,
                exit: None,
                exited_at: None,
                pid,
                commands,
            }),
            input: Mutex::new(Some(input_tx)),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            job,
        });
        let weak = Arc::downgrade(&session);
        thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        let weak_output = weak.clone();
        let (reader_done_tx, reader_done_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut bytes = [0u8; 4096];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Some(session) = weak_output.upgrade() {
                            session.push_output(bytes[..n].to_vec());
                        } else {
                            break;
                        }
                    }
                }
            }
            let _ = reader_done_tx.send(());
        });
        let weak_child = weak;
        thread::spawn(move || loop {
            let Some(session) = weak_child.upgrade() else {
                return;
            };
            let status = session.child.lock().unwrap().try_wait();
            match status {
                Ok(Some(status)) => {
                    session.input.lock().unwrap().take();
                    let _ = reader_done_rx.recv_timeout(Duration::from_secs(2));
                    let mut state = session.state.lock().unwrap();
                    state.exit = Some(status.exit_code());
                    if let Some(commands) = &mut state.commands { commands.exited(); }
                    state.exited_at = Some(Instant::now());
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => {
                    let mut state = session.state.lock().unwrap();
                    state.exit = Some(1);
                    if let Some(commands) = &mut state.commands { commands.exited(); }
                    state.exited_at = Some(Instant::now());
                    return;
                }
            }
        });
        Ok(session)
    }
    fn push_output(&self, bytes: Vec<u8>) {
        let mut state = self.state.lock().unwrap();
        let bytes = if let Some(commands) = &mut state.commands {
            commands.output(&bytes)
        } else { bytes };
        if bytes.is_empty() { return; }
        let seq = state.next_output;
        state.next_output = state.next_output.saturating_add(1);
        state.output_bytes += bytes.len();
        state.output.push_back(OutputChunk {
            seq,
            bytes,
            at: Instant::now(),
        });
        while state.output_bytes > MAX_REPLAY_BYTES
            || state
                .output
                .front()
                .is_some_and(|v| v.at.elapsed() > MAX_REPLAY_AGE)
        {
            if let Some(old) = state.output.pop_front() {
                state.output_bytes -= old.bytes.len();
            } else {
                break;
            }
        }
    }
    fn terminate(&self) -> Result<()> {
        self.input.lock().unwrap().take();
        self.job.terminate()
    }
}

fn command_snapshot(id: Uuid, state: &SessionState, requested: Option<Uuid>) -> Value {
    let commands = state.commands.as_ref();
    let records = commands.map(|commands| {
        if let Some(id) = requested { commands.record(id).into_iter().collect() }
        else { commands.records() }
    }).unwrap_or_default();
    map(vec![
        ("session_id", s(&id.to_string())),
        ("shell_status", s(commands.map_or("unsupported", Commands::shell_status))),
        ("readiness_reason", s(commands.map_or("integration_disabled", Commands::readiness_reason))),
        ("command_execution", commands.is_some().into()),
        ("input_ready", commands.map_or(true, Commands::input_ready).into()),
        ("after_output_seq", (state.next_output - 1).into()),
        ("command_id", requested.map(|id| s(&id.to_string())).unwrap_or(Value::Nil)),
        ("lookup", s(if requested.is_some() && records.is_empty() { "unknown" } else { "known" })),
        ("records", Value::Array(records.into_iter().map(|record| map(vec![
            ("command_id", s(&record.command_id.to_string())),
            ("request_hash", s(&record.request_hash)),
            ("state", s(&record.state)),
            ("succeeded", record.succeeded.map(Value::from).unwrap_or(Value::Nil)),
            ("exit_code", record.exit_code.map(Value::from).unwrap_or(Value::Nil)),
            ("interrupt_requested", record.interrupt_requested.into()),
        ])).collect())),
    ])
}

fn error(code: &str, detail: &str) -> Value {
    message(
        "Error",
        map(vec![("code", s(code)), ("message", s(detail))]),
    )
}
fn send(mut file: impl Write, value: Value) -> Result<()> {
    file.write_all(&wire::encode(&value)?)?;
    file.flush()?;
    Ok(())
}
fn parse_uuid16(value: &Value, key: &str) -> Result<Uuid> {
    Ok(Uuid::from_slice(&bin16(value, key)?)?)
}

impl Broker {
    fn inventory(&self, include_exited: bool) -> serde_json::Value {
        let sessions = self.sessions.lock().unwrap().values().cloned().collect::<Vec<_>>();
        serde_json::Value::Array(sessions.into_iter().filter_map(|session| {
            let state = session.state.lock().unwrap();
            if !include_exited && state.exit.is_some() { return None; }
            Some(serde_json::json!({"id":session.id,"pid":state.pid,"exited":state.exit.is_some(),
                "attached":state.attachment.is_some(),"state":if state.exit.is_some() {"exited"} else {"running"}}))
        }).collect())
    }
    fn new(root: PathBuf, pipe: String) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            root,
            pipe,
            instance: random16()?,
            sessions: Mutex::new(HashMap::new()),
            reservations: Mutex::new(HashMap::new()),
            create_lock: Mutex::new(()),
            stopping: AtomicBool::new(false),
        }))
    }
    fn prune_exited(&self, now: Instant, retention: Duration, cap: usize) {
        let mut sessions = self.sessions.lock().unwrap();
        let exited = sessions
            .iter()
            .filter_map(|(id, session)| session.state.lock().unwrap().exited_at.map(|at| (*id, at)))
            .collect::<Vec<_>>();
        for id in exited_history_removals(exited, now, retention, cap) {
            sessions.remove(&id);
        }
    }
    fn create(
        self: &Arc<Self>,
        body: &Value,
        client: &[u8],
    ) -> Result<(Arc<Session>, bool, Attachment)> {
        let _create = self.create_lock.lock().unwrap();
        self.prune_exited(Instant::now(), EXITED_HISTORY_RETENTION, MAX_EXITED_HISTORY);
        let request_id = parse_uuid16(body, "request_id")?;
        let session_id = Uuid::parse_str(text(body, "requested_session_id")?)?;
        let claim = binary(body, "create_claim")?;
        ensure!(claim.len() == 32, "invalid create claim");
        ensure!(
            bin16(body, "origin_broker_instance_id")? == self.instance,
            "origin broker mismatch"
        );
        let args = get(body, "args")?
            .as_array()
            .context("invalid args")?
            .iter()
            .map(|v| v.as_str().context("invalid arg").map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        let cwd = if get(body, "cwd")?.is_nil() {
            None
        } else {
            Some(text(body, "cwd")?.to_owned())
        };
        let fp = Fingerprint {
            session_id,
            claim,
            shell: text(body, "shell")?.to_owned(),
            args,
            cwd,
            cols: num(body, "cols")?.try_into()?,
            rows: num(body, "rows")?.try_into()?,
            command_execution: get(body, "command_execution").ok()
                .map(|v| v.as_bool().context("invalid command_execution")).transpose()?.unwrap_or(false),
        };
        if let Some(existing) = self.reservations.lock().unwrap().get(&request_id).cloned() {
            ensure!(existing.fingerprint == fp, "CreateRequestConflict");
            ensure!(
                now_ms().saturating_sub(existing.created_ms) <= 600_000,
                "CreateRecoveryUnavailable"
            );
            let session = self
                .sessions
                .lock()
                .unwrap()
                .get(&session_id)
                .cloned()
                .context("CreateRecoveryUnavailable")?;
            let attachment = Attachment {
                id: random16()?,
                lease: random16()?,
                client: client.to_vec(),
                epoch: num(body, "connection_epoch")?,
            };
            return Ok((session, true, attachment));
        }
        if let Some(existing) = load_reservation(&self.root, request_id)? {
            ensure!(
                existing.schema == 1 && existing.broker == self.instance,
                "CreateRecoveryUnavailable"
            );
            ensure!(existing.fingerprint == fp, "CreateRequestConflict");
            bail!("CreateRecoveryUnavailable");
        }
        ensure!(
            self.sessions
                .lock()
                .unwrap()
                .values()
                .filter(|session| session.state.lock().unwrap().exit.is_none())
                .count()
                < MAX_SESSIONS,
            "SessionLimit"
        );
        ensure!(
            !self.sessions.lock().unwrap().contains_key(&session_id),
            "SessionIdConflict"
        );
        let token = store::random_claim()?;
        let mut reservation = Reservation {
            schema: 1,
            request_id,
            broker: self.instance.clone(),
            fingerprint: fp.clone(),
            token: token.clone(),
            created_ms: now_ms(),
            active: false,
        };
        save_reservation(&self.root, &reservation)?;
        let session = Session::spawn(session_id, token, &fp)?;
        reservation.active = true;
        save_reservation(&self.root, &reservation)?;
        self.reservations
            .lock()
            .unwrap()
            .insert(request_id, reservation);
        self.sessions
            .lock()
            .unwrap()
            .insert(session_id, session.clone());
        let attachment = Attachment {
            id: random16()?,
            lease: random16()?,
            client: client.to_vec(),
            epoch: num(body, "connection_epoch")?,
        };
        {
            let mut state = session.state.lock().unwrap();
            state
                .highest_epochs
                .insert(client.to_vec(), attachment.epoch);
            state.attachment = Some(attachment.clone());
        }
        Ok((session, false, attachment))
    }
    fn handle_control(self: &Arc<Self>, mut input: File, mut output: File) -> Result<()> {
        let mut len = [0u8; 4];
        input.read_exact(&mut len)?;
        let mut bytes = vec![0u8; u32::from_le_bytes(len).min(64 * 1024) as usize];
        input.read_exact(&mut bytes)?;
        let request: serde_json::Value = serde_json::from_slice(&bytes)?;
        let command = request
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mut response = serde_json::json!({"ok": true});
        match command {
            "status" => response["sessions"] = (self.sessions.lock().unwrap().len() as u64).into(),
            "sessions" => {
                response["sessions"] = self.inventory(true);
            }
            "terminate" => {
                let id = Uuid::parse_str(
                    request
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .context("missing session_id")?,
                )?;
                self.sessions
                    .lock()
                    .unwrap()
                    .get(&id)
                    .context("session not found")?
                    .terminate()?;
            }
            "stop" => {
                let terminate = request
                    .get("terminate_sessions")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let live = self
                    .sessions
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|s| s.state.lock().unwrap().exit.is_none())
                    .cloned()
                    .collect::<Vec<_>>();
                if !live.is_empty() && !terminate {
                    response = serde_json::json!({"ok":false,"error":"live sessions exist; use --terminate-sessions"});
                } else {
                    for session in live {
                        session.terminate()?;
                    }
                    self.stopping.store(true, Ordering::SeqCst);
                }
            }
            _ => response = serde_json::json!({"ok":false,"error":"unknown control command"}),
        }
        let bytes = serde_json::to_vec(&response)?;
        output.write_all(&(bytes.len() as u32).to_le_bytes())?;
        output.write_all(&bytes)?;
        output.flush()?;
        if self.stopping.load(Ordering::SeqCst) {
            let _ = host_pipe::connect(&self.pipe, Duration::from_secs(1));
        }
        Ok(())
    }
    fn handle_protocol(
        self: &Arc<Self>,
        mut input: File,
        mut output: File,
        first: [u8; 4],
    ) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel::<Result<Value>>(64);
        let connected = Arc::new(AtomicBool::new(true));
        let reader_connected = connected.clone();
        thread::spawn(move || {
            let _connection = FileConnection(reader_connected);
            let mut frames = Frames::default();
            if frames.push(&first).is_err() {
                return;
            }
            let mut bytes = [0u8; 8192];
            loop {
                while let Ok(Some(value)) = frames.next() {
                    if tx.send(Ok(value)).is_err() {
                        return;
                    }
                }
                match input.read(&mut bytes) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if let Err(e) = frames.push(&bytes[..n]) {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    }
                }
            }
        });
        let hello = rx
            .recv_timeout(Duration::from_secs(10))
            .context("Hello timeout")??;
        ensure!(
            text(&hello, "type")? == "Hello",
            "first frame must be Hello"
        );
        let hello_body = get(&hello, "body")?;
        ensure!(
            num(hello_body, "min_version")? <= 1 && num(hello_body, "max_version")? >= 1,
            "protocol v1 is not supported by peer"
        );
        let client = bin16(hello_body, "client_instance_id")?;
        let command_capable = get(hello_body, "capabilities")?.as_array()
            .context("invalid capabilities")?.iter().any(|v| v.as_str() == Some(COMMAND_CAPABILITY));
        let management_capable = get(hello_body, "capabilities")?.as_array().context("invalid capabilities")?
            .iter().any(|v| v.as_str() == Some("host-owner-management-v1"));
        send(
            &mut output,
            message(
                "HelloOk",
                map(vec![
                    ("version", 1.into()),
                    ("host_version", s(env!("CARGO_PKG_VERSION"))),
                    ("broker_instance_id", Value::Binary(self.instance.clone())),
                    (
                        "capabilities",
                        Value::Array(CAPS.iter().copied()
                            .chain(transfer_payload::DIRECTORY_ADAPTER_INSTALLED.then_some(transfer_payload::DIRECTORY_CAPABILITY))
                            .map(s).collect()),
                    ),
                    ("max_frame", (wire::MAX_FRAME as u64).into()),
                    ("max_input_window_bytes", 65536.into()),
                ]),
            ),
        )?;
        let mut attached: Option<(Arc<Session>, Vec<u8>, u64)> = None;
        let result = (|| -> Result<()> {
        let file_caps = get(hello_body, "capabilities")?.as_array().context("invalid capabilities")?;
        let file_capable = [arterm::transfer_admission::CAPABILITY, transfer_payload::METADATA_CAPABILITY]
            .iter().all(|cap| file_caps.iter().any(|v| v.as_str() == Some(cap)));
        let mut file_bridge: Option<FileBridge> = None;
        let mut cursor = 0u64;
        let mut command_revision = None;
        let mut last_peer = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Ok(value)) => {
                    last_peer = Instant::now();
                    let kind = text(&value, "type")?;
                    let body = get(&value, "body")?;
                    match kind {
                        "FileAuthorize" => {
                            let authorized = (|| -> Result<FileBridge> {
                                ensure!(file_capable && file_bridge.is_none(), "FileUnauthorized");
                                ensure!(bin16(body, "broker_instance_id")? == self.instance, "FileUnauthorized");
                                let id = Uuid::parse_str(text(body, "session_id")?)?;
                                let session = self.sessions.lock().unwrap().get(&id).cloned().context("FileUnauthorized")?;
                                ensure!(binary(body, "resume_token")? == session.token &&
                                    bin16(body, "client_instance_id")? == client, "FileUnauthorized");
                                let bridge = FileBridge {
                                    session, attachment: Attachment {
                                        id: bin16(body, "attachment_id")?, lease: bin16(body, "lease_id")?,
                                        client: client.clone(), epoch: num(body, "connection_epoch")?,
                                    }, connected: connected.clone(), ids: Vec::new(),
                                    directory_capable: transfer_payload::DIRECTORY_ADAPTER_INSTALLED &&
                                        file_caps.iter().any(|v| v.as_str() == Some(transfer_payload::DIRECTORY_CAPABILITY)),
                                    deadline: Instant::now() + transfer_payload::OPERATION_TIMEOUT,
                                };
                                drop(bridge.authorize()?);
                                Ok(bridge)
                            })();
                            match authorized {
                                Ok(bridge) => {
                                    file_bridge = Some(bridge);
                                    send(&mut output, message("FileAuthorized", map(vec![])))?;
                                }
                                Err(_) => send(&mut output, error("FileUnauthorized", "file authorization rejected"))?,
                            }
                        }
                        "FileBeginUpload" | "FileBeginDownload" | "FileWrite" | "FileRead" |
                        "FileFinish" | "FileClose" | "FileCancel" => {
                            let request_id = s(text(body, "request_id")?);
                            let result = {
                                let last_progress = std::cell::Cell::new(Instant::now());
                                let progress = || {
                                    if last_progress.get().elapsed() >= transfer_payload::PROGRESS_INTERVAL {
                                        send(&output, message("FileProgress", map(vec![
                                            ("request_id", request_id.clone()),
                                            ("phase", s(match kind {
                                                "FileBeginDownload" => "preparing",
                                                "FileFinish" => "publishing",
                                                "FileClose" => "verifying",
                                                _ => "transferring",
                                            })),
                                        ])))?;
                                        last_progress.set(Instant::now());
                                    }
                                    Ok(())
                                };
                                file_bridge.as_mut().context("FileUnauthorized")
                                    .and_then(|bridge| bridge.request(kind, body, &progress))
                            };
                            last_peer = Instant::now();
                            match result {
                                Ok((result, bytes)) => send(&mut output, message("FileResult", map(vec![
                                    ("request_id", request_id), ("json", s(&serde_json::to_string(&result)?)),
                                    ("bytes", bytes.map(Value::Binary).unwrap_or(Value::Nil)),
                                ])))?,
                                Err(error) => send(&mut output, message("FileError", map(vec![
                                    ("request_id", request_id), ("detail", s(&error.to_string())),
                                ])))?,
                            }
                        }
                        "ListSessions" => {
                            // The accepted host pipe authenticates the Windows owner/logon context.
                            // The remote transport must launch the native helper as that owner.
                            // A session token is deliberately not used as inventory authority.
                            if !management_capable {
                                send(&mut output, error("ManagementCapabilityRequired", "owner inventory was not negotiated"))?;
                            } else {
                                let inventory = serde_json::json!({
                                    "authorization_scope":"host-windows-owner",
                                    "broker_instance_id":self.instance.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                                    "sessions":self.inventory(false),
                                });
                                send(&mut output, message("SessionInventory", map(vec![
                                    ("json", s(&serde_json::to_string(&inventory)?)),
                                ])))?;
                            }
                        }
                        "CreateSession" => match (|| {
                            ensure!(command_capable || get(body, "command_execution").ok().and_then(Value::as_bool) != Some(true),
                                "CommandCapabilityRequired");
                            self.create(body, &client)
                        })() {
                            Ok((session, recovered, attachment)) => {
                                if recovered {
                                    send(
                                        &mut output,
                                        message(
                                            "SessionCreated",
                                            map(vec![
                                                ("session_id", s(&session.id.to_string())),
                                                (
                                                    "resume_token",
                                                    Value::Binary(session.token.clone()),
                                                ),
                                                ("requires_attach", true.into()),
                                                ("recovered", true.into()),
                                            ]),
                                        ),
                                    )?;
                                } else {
                                    session.state.lock().unwrap().attachment_connection = Some(connected.clone());
                                    attached = Some((session.clone(), attachment.id.clone(), attachment.epoch));
                                    let committed = session.state.lock().unwrap().input_committed;
                                    cursor = num(body, "after_output_seq")?;
                                    send(
                                        &mut output,
                                        message(
                                            "SessionCreated",
                                            map(vec![
                                                ("session_id", s(&session.id.to_string())),
                                                (
                                                    "resume_token",
                                                    Value::Binary(session.token.clone()),
                                                ),
                                                ("requires_attach", false.into()),
                                                ("recovered", false.into()),
                                                (
                                                    "attachment_id",
                                                    Value::Binary(attachment.id.clone()),
                                                ),
                                                (
                                                    "lease_id",
                                                    Value::Binary(attachment.lease.clone()),
                                                ),
                                                ("input_committed_through", committed.into()),
                                            ]),
                                        ),
                                    )?;
                                    command_revision = None;
                                }
                            }
                            Err(e) => {
                                let msg = format!("{e:#}");
                                let code = if msg.contains("CreateRecoveryUnavailable") {
                                    "CreateRecoveryUnavailable"
                                } else if msg.contains("CreateRequestConflict") {
                                    "CreateRequestConflict"
                                } else if msg.contains("SessionIdConflict") {
                                    "SessionIdConflict"
                                } else {
                                    "CreateFailed"
                                };
                                send(&mut output, error(code, "session creation rejected"))?;
                            }
                        },
                        "TerminateSession" => {
                            let id = Uuid::parse_str(text(body, "session_id")?)?;
                            let token = binary(body, "resume_token")?;
                            let Some(session) = self.sessions.lock().unwrap().get(&id).cloned()
                            else {
                                send(
                                    &mut output,
                                    message(
                                        "Unauthorized",
                                        map(vec![("session_id", s(&id.to_string()))]),
                                    ),
                                )?;
                                continue;
                            };
                            if session.token != token {
                                send(
                                    &mut output,
                                    message(
                                        "Unauthorized",
                                        map(vec![("session_id", s(&id.to_string()))]),
                                    ),
                                )?;
                                continue;
                            }
                            session.terminate()?;
                            send(
                                &mut output,
                                message(
                                    "TerminateAccepted",
                                    map(vec![("session_id", s(&id.to_string()))]),
                                ),
                            )?;
                            let deadline = Instant::now() + Duration::from_secs(10);
                            loop {
                                if session.state.lock().unwrap().exit.is_some() && session.job.empty()? {
                                    send(&mut output, message("SessionTerminated", map(vec![
                                        ("session_id", s(&id.to_string())),
                                    ])))?;
                                    break;
                                }
                                if Instant::now() >= deadline {
                                    send(&mut output, error("TerminationUnconfirmed", "termination requested; process exit not confirmed"))?;
                                    break;
                                }
                                thread::sleep(Duration::from_millis(20));
                            }
                        }
                        "AttachSession" => {
                            let id = Uuid::parse_str(text(body, "session_id")?)?;
                            let token = binary(body, "resume_token")?;
                            let epoch = num(body, "connection_epoch")?;
                            let Some(session) = self.sessions.lock().unwrap().get(&id).cloned()
                            else {
                                send(
                                    &mut output,
                                    message(
                                        "Unauthorized",
                                        map(vec![("session_id", s(&id.to_string()))]),
                                    ),
                                )?;
                                continue;
                            };
                            if session.token != token {
                                send(
                                    &mut output,
                                    message(
                                        "Unauthorized",
                                        map(vec![("session_id", s(&id.to_string()))]),
                                    ),
                                )?;
                                continue;
                            }
                            let attachment = Attachment {
                                id: random16()?,
                                lease: random16()?,
                                client: client.clone(),
                                epoch,
                            };
                            let mut st = session.state.lock().unwrap();
                            if st.exit.is_some() {
                                drop(st);
                                send(
                                    &mut output,
                                    message("SessionEnded", map(vec![("session_id", s(&id.to_string()))])),
                                )?;
                                continue;
                            }
                            if st
                                .attachment
                                .as_ref()
                                .is_some_and(|old| old.client != client)
                            {
                                drop(st);
                                send(
                                    &mut output,
                                    message(
                                        "WriterBusy",
                                        map(vec![("session_id", s(&id.to_string()))]),
                                    ),
                                )?;
                                continue;
                            }
                            let highest = st.highest_epochs.get(&client).copied().unwrap_or(0);
                            if epoch <= highest {
                                drop(st);
                                send(
                                    &mut output,
                                    message(
                                        "ConnectionSuperseded",
                                        map(vec![("session_id", s(&id.to_string()))]),
                                    ),
                                )?;
                                continue;
                            }
                            st.highest_epochs.insert(client.clone(), epoch);
                            st.attachment = Some(attachment.clone());
                            st.attachment_connection = Some(connected.clone());
                            let committed = st.input_committed;
                            let generation = st.resize_generation;
                            drop(st);
                            attached = Some((session.clone(), attachment.id.clone(), epoch));
                            cursor = num(body, "after_output_seq")?;
                            send(
                                &mut output,
                                message(
                                    "SessionAttached",
                                    map(vec![
                                        ("session_id", s(&id.to_string())),
                                        ("mode", s("writer")),
                                        ("accepted_connection_epoch", epoch.into()),
                                        ("attachment_id", Value::Binary(attachment.id.clone())),
                                        ("lease_id", Value::Binary(attachment.lease.clone())),
                                        ("input_committed_through", committed.into()),
                                        ("resize_generation", generation.into()),
                                    ]),
                                ),
                            )?;
                            command_revision = None;
                        }
                        "CommandSubmit" | "CommandStatus" | "CommandInterrupt" | "SessionInterrupt" => {
                            let result = (|| -> Result<Value> {
                                ensure!(command_capable, "CommandCapabilityRequired");
                                let (session, attachment_id, epoch) = attached.as_ref().context("CommandNotAttached")?;
                                ensure!(text(body, "session_id")? == session.id.to_string(), "CommandSessionMismatch");
                                let sender = session.input.lock().unwrap().as_ref().cloned().context("CommandInputClosed")?;
                                let mut state = session.state.lock().unwrap();
                                let attachment = state.attachment.as_ref().context("CommandLeaseRevoked")?;
                                ensure!(attachment.id == *attachment_id
                                    && bin16(body, "attachment_id")? == *attachment_id
                                    && binary(body, "lease_id")? == attachment.lease
                                    && bin16(body, "client_instance_id")? == client
                                    && attachment.client == client
                                    && num(body, "connection_epoch")? == *epoch
                                    && attachment.epoch == *epoch, "CommandLeaseRevoked");
                                ensure!(state.exit.is_none(), "CommandSessionEnded");
                                if kind == "SessionInterrupt" {
                                    ensure!(state.commands.as_ref().and_then(Commands::active).is_none(),
                                        "CommandInterruptTargetChanged");
                                    sender.try_send(vec![3]).context("SessionInterruptDeliveryFailed")?;
                                    return Ok(map(vec![
                                        ("session_id", s(&session.id.to_string())),
                                        ("command_id", get(body, "command_id")?.clone()),
                                        ("operation_id", get(body, "operation_id")?.clone()),
                                    ]));
                                }
                                let commands = state.commands.as_mut().context("CommandShellUnsupported")?;
                                let id = Uuid::parse_str(text(body, "command_id")?)?;
                                if kind == "CommandSubmit" {
                                    let (_, bytes) = commands.submit(id, text(body, "command")?)?;
                                    if let Some(bytes) = bytes {
                                        if let Err(error) = sender.try_send(bytes) {
                                            commands.submission_failed(id);
                                            bail!("CommandDeliveryUnknown: {error}");
                                        }
                                    }
                                } else if kind == "CommandInterrupt" {
                                    commands.interrupt(id)?;
                                    sender.try_send(vec![3]).context("CommandInterruptDeliveryFailed")?;
                                }
                                let mut snapshot = command_snapshot(session.id, &state, Some(id));
                                if let (Value::Map(fields), Ok(operation)) = (&mut snapshot, get(body, "operation_id")) {
                                    fields.push((s("operation_id"), operation.clone()));
                                }
                                Ok(snapshot)
                            })();
                            match result {
                                Ok(body) => send(&mut output, message(
                                    if kind == "CommandSubmit" { "CommandAccepted" }
                                    else if kind == "CommandInterrupt" { "CommandInterruptAccepted" }
                                    else if kind == "SessionInterrupt" { "SessionInterruptAccepted" }
                                    else { "CommandStatus" }, body))?,
                                Err(error) => send(&mut output, message("CommandRejected", map(vec![
                                    ("code", s(&error.to_string())),
                                    ("command_id", get(body, "command_id").cloned().unwrap_or(Value::Nil)),
                                    ("operation_id", get(body, "operation_id").cloned().unwrap_or(Value::Nil)),
                                ])))?,
                            }
                        }
                        "Input" => {
                            if let Some((session, attachment_id, _)) = &attached {
                                let seq = num(body, "input_seq")?;
                                let bytes = binary(body, "bytes")?;
                                ensure!(!bytes.is_empty() && bytes.len() <= 4096, "invalid input");
                                let lease = binary(body, "lease_id")?;
                                let input_client = bin16(body, "client_instance_id")?;
                                let input_sender = session.input.lock().unwrap().as_ref().cloned();
                                let mut st = session.state.lock().unwrap();
                                let valid = st.attachment.as_ref().is_some_and(|current| {
                                    current.id == *attachment_id
                                        && current.lease == lease
                                        && current.client == input_client
                                });
                                if !valid {
                                    drop(st);
                                    send(
                                        &mut output,
                                        message(
                                            "LeaseRevoked",
                                            map(vec![("session_id", s(&session.id.to_string()))]),
                                        ),
                                    )?;
                                    continue;
                                }
                                if seq == st.input_committed + 1 {
                                    if let Some(commands) = &mut st.commands {
                                        if let Err(error) = commands.input(&bytes) {
                                            let detail = if commands.input_ready() {
                                                "human input rejected while managed command owns input"
                                            } else {
                                                "shell integration has not emitted its first supported ready marker; input was not injected"
                                            };
                                            drop(st);
                                            send(&mut output, message(
                                                if command_capable { "InputRejected" } else { "Error" }, map(vec![
                                                ("session_id", s(&session.id.to_string())),
                                                ("input_seq", seq.into()),
                                                ("code", s(&error.to_string())),
                                                ("detail", s(detail)),
                                            ])))?;
                                            continue;
                                        }
                                    }
                                    match input_sender
                                        .context("PTY input closed")?
                                        .try_send(bytes.clone())
                                    {
                                        Ok(()) => st.input_committed = seq,
                                        Err(TrySendError::Full(_)) => {
                                            drop(st);
                                            send(
                                                &mut output,
                                                message(
                                                    "InputBackpressure",
                                                    map(vec![
                                                        ("session_id", s(&session.id.to_string())),
                                                        ("expected", seq.into()),
                                                        ("retry_after_ms", 25.into()),
                                                        ("available_window_bytes", 0.into()),
                                                    ]),
                                                ),
                                            )?;
                                            continue;
                                        }
                                        Err(TrySendError::Disconnected(_)) => {
                                            bail!("PTY input closed")
                                        }
                                    }
                                } else if seq > st.input_committed {
                                    let expected = st.input_committed + 1;
                                    drop(st);
                                    send(
                                        &mut output,
                                        message(
                                            "InputSequenceGap",
                                            map(vec![
                                                ("session_id", s(&session.id.to_string())),
                                                ("expected", expected.into()),
                                            ]),
                                        ),
                                    )?;
                                    continue;
                                }
                                let committed = st.input_committed;
                                drop(st);
                                send(
                                    &mut output,
                                    message(
                                        "InputAck",
                                        map(vec![
                                            ("session_id", s(&session.id.to_string())),
                                            ("client_instance_id", Value::Binary(client.clone())),
                                            ("committed_through", committed.into()),
                                            ("available_window_bytes", 65536.into()),
                                        ]),
                                    ),
                                )?;
                            }
                        }
                        "Resize" => {
                            if let Some((session, attachment_id, _)) = &attached {
                                let generation = num(body, "resize_generation")?;
                                let lease = binary(body, "lease_id")?;
                                let mut st = session.state.lock().unwrap();
                                let valid = st
                                    .attachment
                                    .as_ref()
                                    .is_some_and(|a| a.id == *attachment_id && a.lease == lease);
                                if !valid {
                                    drop(st);
                                    send(
                                        &mut output,
                                        message(
                                            "LeaseRevoked",
                                            map(vec![("session_id", s(&session.id.to_string()))]),
                                        ),
                                    )?;
                                    continue;
                                }
                                if generation > st.resize_generation {
                                    st.resize_generation = generation;
                                    drop(st);
                                    session.master.lock().unwrap().resize(PtySize {
                                        rows: num(body, "rows")?.try_into()?,
                                        cols: num(body, "cols")?.try_into()?,
                                        pixel_width: 0,
                                        pixel_height: 0,
                                    })?;
                                } else {
                                    drop(st);
                                }
                                send(
                                    &mut output,
                                    message(
                                        "ResizeAck",
                                        map(vec![
                                            ("session_id", s(&session.id.to_string())),
                                            ("resize_generation", generation.into()),
                                        ]),
                                    ),
                                )?;
                            }
                        }
                        "Detach" => {
                            if let Some((session, attachment_id, _)) = attached.take() {
                                let mut st = session.state.lock().unwrap();
                                if st
                                    .attachment
                                    .as_ref()
                                    .is_some_and(|a| a.id == attachment_id)
                                {
                                    st.attachment = None;
                                }
                            }
                            return Ok(());
                        }
                        "OutputAck" => {}
                        "Ping" => send(
                            &mut output,
                            message(
                                "Pong",
                                map(vec![
                                    ("nonce", get(body, "nonce")?.clone()),
                                    ("broker_time_ms", now_ms().into()),
                                ]),
                            ),
                        )?,
                        "Pong" => {}
                        _ => send(
                            &mut output,
                            error("UnsupportedMessage", "unsupported protocol message"),
                        )?,
                    }
                }
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if file_bridge.as_ref().is_some_and(|bridge| bridge.authorize().is_err()) {
                file_bridge = None;
            }
            if last_peer.elapsed() > PEER_IDLE_TIMEOUT {
                eprintln!("host connection closed: peer idle timeout after 20 seconds without a client protocol message");
                break;
            }
            if let Some((session, attachment_id, _)) = &attached {
                let st = session.state.lock().unwrap();
                if !st
                    .attachment
                    .as_ref()
                    .is_some_and(|a| a.id == *attachment_id)
                {
                    drop(st);
                    send(
                        &mut output,
                        error(
                            "ConnectionSuperseded",
                            "a newer connection epoch owns the writer lease",
                        ),
                    )?;
                    break;
                }
                let earliest = st.output.front().map(|v| v.seq).unwrap_or(st.next_output);
                if cursor + 1 < earliest {
                    send(
                        &mut output,
                        message(
                            "ReplayGap",
                            map(vec![
                                ("session_id", s(&session.id.to_string())),
                                ("requested_after", cursor.into()),
                                ("earliest_available", earliest.into()),
                            ]),
                        ),
                    )?;
                    cursor = earliest - 1;
                }
                let chunks = st
                    .output
                    .iter()
                    .filter(|v| v.seq > cursor)
                    .cloned()
                    .collect::<Vec<_>>();
                let exit = st.exit;
                let latest = st.next_output - 1;
                let revision = st.commands.as_ref().map(|commands| commands.revision);
                let command_update = if command_capable && revision != command_revision {
                    Some(command_snapshot(session.id, &st, None))
                } else { None };
                drop(st);
                for chunk in chunks {
                    send(
                        &mut output,
                        message(
                            "Output",
                            map(vec![
                                ("session_id", s(&session.id.to_string())),
                                ("output_seq", chunk.seq.into()),
                                ("bytes", Value::Binary(chunk.bytes)),
                            ]),
                        ),
                    )?;
                    cursor = chunk.seq;
                }
                if let Some(update) = command_update {
                    send(&mut output, message("CommandState", update))?;
                    command_revision = revision;
                }
                if let Some(code) = exit {
                    if cursor >= latest {
                        send(
                            &mut output,
                            message(
                                "SessionExited",
                                map(vec![
                                    ("session_id", s(&session.id.to_string())),
                                    ("exit_code", code.into()),
                                    ("reason", s("shell-exited")),
                                ]),
                            ),
                        )?;
                        break;
                    }
                }
            }
        }
        Ok(())
        })();
        if let Some((session, attachment_id, _)) = attached {
            let mut st = session.state.lock().unwrap();
            if st
                .attachment
                .as_ref()
                .is_some_and(|a| a.id == attachment_id)
            {
                st.attachment = None;
            }
        }
        result
    }
}

pub fn run_with_transport<T>(start: impl FnOnce() -> Result<T>) -> Result<()> {
    run_at_with_transport(deployment::data_root()?, start)
}

#[cfg(test)]
fn run_at(root: PathBuf) -> Result<()> {
    run_at_with_transport(root, || Ok(()))
}

fn run_at_with_transport<T>(root: PathBuf, start: impl FnOnce() -> Result<T>) -> Result<()> {
    let pipe = pipe_name(&root)?;
    let _guard = acquire(&root, &pipe)?;
    let broker = Broker::new(root.clone(), pipe.clone())?;
    // Start the product-owned tunnel only after winning the broker singleton.
    let _transport = start()?;
    fs::write(
        root.join("host").join("broker.json"),
        serde_json::to_vec(
            &serde_json::json!({"pid":std::process::id(),"pipe":pipe,"started_ms":now_ms()}),
        )?,
    )?;
    while !broker.stopping.load(Ordering::SeqCst) {
        let pair = host_pipe::accept(&broker.pipe);
        // The shutdown wakeup can close before accept completes.
        if broker.stopping.load(Ordering::SeqCst) {
            break;
        }
        let pair = pair?;
        let broker = broker.clone();
        thread::spawn(move || {
            let mut input = pair.input;
            let mut first = [0u8; 4];
            let result = input
                .read_exact(&mut first)
                .map_err(anyhow::Error::from)
                .and_then(|_| {
                    if &first == b"DBHC" {
                        broker.handle_control(input, pair.output)
                    } else {
                        broker.handle_protocol(input, pair.output, first)
                    }
                });
            if let Err(error) = result {
                eprintln!("host connection closed: {error:#}");
            }
        });
    }
    let sessions = broker
        .sessions
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for session in sessions {
        session.terminate()?;
    }
    let _ = fs::remove_file(root.join("host").join("broker.json"));
    Ok(())
}

pub fn stop(terminate_sessions: bool) -> Result<()> {
    stop_at(&deployment::data_root()?, terminate_sessions)
}
fn stop_at(root: &Path, terminate_sessions: bool) -> Result<()> {
    let pipe = pipe_name(root)?;
    match host_pipe::send_control(
        &pipe,
        &serde_json::json!({
            "command": "stop",
            "session_id": serde_json::Value::Null,
            "terminate_sessions": terminate_sessions
        }),
    ) {
        Ok(value) => {
            ensure!(
                value.get("ok").and_then(|v| v.as_bool()) == Some(true),
                "{}",
                value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("host command failed")
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(_stopped) = acquire(root, &pipe) {
                    return Ok(());
                }
                ensure!(Instant::now() < deadline, "host accepted stop but did not exit; update cancelled");
                thread::sleep(Duration::from_millis(50));
            }
        }
        Err(control_error) => match acquire(root, &pipe) {
            Ok(_not_running) => Ok(()),
            Err(_) => Err(control_error
                .context("host appears to be running but its control pipe is unavailable")),
        },
    }
}
pub fn control(
    command: &str,
    session_id: Option<&str>,
    terminate_sessions: bool,
) -> Result<serde_json::Value> {
    control_at(
        &deployment::data_root()?,
        command,
        session_id,
        terminate_sessions,
    )
}
fn control_at(
    root: &Path,
    command: &str,
    session_id: Option<&str>,
    terminate_sessions: bool,
) -> Result<serde_json::Value> {
    let pipe = pipe_name(root)?;
    let value = host_pipe::send_control(
        &pipe,
        &serde_json::json!({"command":command,"session_id":session_id,"terminate_sessions":terminate_sessions}),
    )?;
    ensure!(
        value.get("ok").and_then(|v| v.as_bool()) == Some(true),
        "{}",
        value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("host command failed")
    );
    Ok(value)
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
