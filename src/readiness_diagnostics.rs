//! Typed readiness diagnostics. No terminal content is accepted.
//!
//! `record` only attempts a bounded queue send. Call `open`, `flush`, and final
//! drop outside terminal/protocol locks. Flush acknowledges prior writes, not
//! durable storage. A final drop waits at most 250 ms. Full queues count losses
//! in the next written envelope; I/O failure makes flush return false and emits
//! one fixed diagnostic. Each role retains at most eight process-instance pairs
//! of 1 MiB files. Live instances are never evicted; exhaustion returns an error.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const FILE_LIMIT: u64 = 1024 * 1024;
const INSTANCE_LIMIT: usize = 8;
const QUEUE_LIMIT: usize = 256;
const SHUTDOWN_WAIT: Duration = Duration::from_millis(250);
const PREFIX: &str = "readiness-v1-";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Host,
    Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    Input,
    InputRejected,
    InputBackpressure,
    ShellMarker,
    HostState,
    CommandWait,
    CommandAdmitted,
    CommandTimeout,
    CommandRejected,
    Connection,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellStatus {
    Ready,
    NotReady,
    Busy,
    Unsupported,
    #[default]
    Unknown,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadinessReason {
    Ready,
    Initializing,
    ManagedCommand,
    CommandOutcomeUnknown,
    PartialInputSequence,
    UnclassifiedTerminalInput,
    PartialHumanInput,
    HumanCommandPending,
    IntegrationDisabled,
    #[default]
    Unknown,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub status: ShellStatus,
    pub reason: ReadinessReason,
    pub initialized: Option<bool>,
    pub human_dirty: Option<bool>,
    pub unclassified_input: Option<bool>,
    pub pending_sequence: Option<bool>,
    pub human_pending: Option<u64>,
    pub revision: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSummary {
    pub text: bool,
    pub modifier: bool,
    pub nontext_key: bool,
    pub key_release: bool,
    pub editing: bool,
    pub submit_or_interrupt: bool,
    pub focus: bool,
    pub unclassified: bool,
    pub incomplete: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Details {
    pub state: Snapshot,
    pub input: InputSummary,
}

impl Snapshot {
    pub fn same_state(self, other: Self) -> bool {
        Self { revision: None, ..self } == Self { revision: None, ..other }
    }
}

impl ShellStatus {
    pub fn from_wire(value: &str) -> Self {
        match value {
            "ready" => Self::Ready,
            "not_ready" => Self::NotReady,
            "busy" => Self::Busy,
            "unsupported" => Self::Unsupported,
            _ => Self::Unknown,
        }
    }
}

impl ReadinessReason {
    pub fn from_wire(value: &str) -> Self {
        match value {
            "ready" => Self::Ready,
            "initializing" => Self::Initializing,
            "managed_command" => Self::ManagedCommand,
            "command_outcome_unknown" => Self::CommandOutcomeUnknown,
            "partial_input_sequence" => Self::PartialInputSequence,
            "unclassified_terminal_input" => Self::UnclassifiedTerminalInput,
            "partial_human_input" => Self::PartialHumanInput,
            "human_command_pending" => Self::HumanCommandPending,
            "integration_disabled" => Self::IntegrationDisabled,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub kind: EventKind,
    pub session_id: Uuid,
    pub command_id: Option<Uuid>,
    pub before: Option<Snapshot>,
    pub after: Option<Snapshot>,
    pub input: Option<InputSummary>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u32,
    unix_ms: u64,
    process_id: u32,
    role: Role,
    version: String,
    dropped_events: u64,
    event: Event,
}

enum Message {
    Event(Event),
    Flush(SyncSender<bool>),
}

struct Inner {
    path: PathBuf,
    sender: Option<SyncSender<Message>>,
    done: Mutex<Receiver<()>>,
    drops: Arc<AtomicU64>,
    unavailable: Arc<AtomicBool>,
}

/// Independent, cloneable sink; no environment variables or global root cache.
#[derive(Clone)]
pub struct ReadinessLog(Arc<Inner>);

impl ReadinessLog {
    pub fn open(root: &Path, role: Role) -> Result<Self> {
        let directory = root
            .join(match role {
                Role::Host => "host",
                Role::Client => "client",
            })
            .join("diagnostics");
        // Reject existing reparse ancestors before creating any directories.
        check_ancestors(&directory)?;
        fs::create_dir_all(&directory).context("creating readiness diagnostic directory")?;
        check_ancestors(&directory)?;
        let _directory_guard = lock_file(&directory.join("readiness-v1-directory.lock"))?;
        retain(&directory)?;
        let stem = format!("{PREFIX}{}-{}", std::process::id(), Uuid::now_v7());
        let lease_path = directory.join(format!("{stem}.lock"));
        let lease = new_file(&lease_path)?;
        let path = directory.join(format!("{stem}.jsonl"));
        let file = match data_file(&path) {
            Ok(file) => file,
            Err(error) => {
                drop(lease);
                fs::remove_file(&lease_path).context("removing unused readiness lease")?;
                return Err(error);
            }
        };
        let writer = Writer {
            path: path.clone(),
            file: Some(file),
            bytes: 0,
            _lease: lease,
        };
        let (sender, receiver) = mpsc::sync_channel(QUEUE_LIMIT);
        let (finished, done) = mpsc::sync_channel(1);
        let drops = Arc::new(AtomicU64::new(0));
        let unavailable = Arc::new(AtomicBool::new(false));
        let worker_drops = drops.clone();
        let worker_unavailable = unavailable.clone();
        std::thread::Builder::new()
            .name("readiness-diagnostics".into())
            .spawn(move || {
                let result = worker(writer, receiver, role, &worker_drops);
                // worker has released all file handles before either notification.
                if result.is_err() {
                    worker_unavailable.store(true, Ordering::Release);
                }
                let _ = finished.try_send(());
                if result.is_err() {
                    crate::diagnostics::line(format_args!(
                        "[readiness] diagnostic sink unavailable; events may have been lost"
                    ));
                }
            })
            .context("starting readiness diagnostic worker")?;
        Ok(Self(Arc::new(Inner {
            path,
            sender: Some(sender),
            done: Mutex::new(done),
            drops,
            unavailable,
        })))
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// Never waits for filesystem I/O, queue capacity, or a mutex.
    pub fn record(&self, event: Event) {
        if self
            .0
            .sender
            .as_ref()
            .unwrap()
            .try_send(Message::Event(event))
            .is_err()
        {
            let _ = self
                .0
                .drops
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    Some(n.saturating_add(1))
                });
        }
    }

    /// False means timeout, unavailable storage, or disconnected worker.
    /// Concurrent producers are not covered beyond this call's queue barrier.
    pub fn flush(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut message = Message::Flush(sender);
        loop {
            if self.0.unavailable.load(Ordering::Acquire) {
                return false;
            }
            match self.0.sender.as_ref().unwrap().try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return false,
                Err(TrySendError::Full(value)) => message = value,
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return false;
            }
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
        }
        receiver
            .recv_timeout(timeout.saturating_sub(started.elapsed()))
            .unwrap_or(false)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.sender.take();
        if let Ok(done) = self.done.get_mut() {
            let _ = done.recv_timeout(SHUTDOWN_WAIT);
        }
    }
}

fn worker(
    mut writer: Writer,
    receiver: Receiver<Message>,
    role: Role,
    drops: &AtomicU64,
) -> Result<()> {
    while let Ok(message) = receiver.recv() {
        match message {
            Message::Event(event) => {
                let envelope = Envelope {
                    schema_version: 1,
                    unix_ms: SystemTime::now()
                        .duration_since(UNIX_EPOCH)?
                        .as_millis()
                        .min(u64::MAX as u128) as u64,
                    process_id: std::process::id(),
                    role,
                    version: env!("CARGO_PKG_VERSION").into(),
                    dropped_events: drops.swap(0, Ordering::Relaxed),
                    event,
                };
                let mut bytes = serde_json::to_vec(&envelope)?;
                bytes.push(b'\n');
                writer.write(&bytes)?;
            }
            Message::Flush(ack) => {
                let result = writer.file.as_mut().unwrap().flush();
                let _ = ack.try_send(result.is_ok());
                result?;
            }
        }
    }
    writer.file.as_mut().unwrap().flush()?;
    Ok(())
}

struct Writer {
    path: PathBuf,
    file: Option<File>,
    bytes: u64,
    _lease: File,
}

impl Writer {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() as u64 > FILE_LIMIT {
            bail!("readiness envelope exceeds file limit");
        }
        if self.bytes + bytes.len() as u64 > FILE_LIMIT {
            self.file.take();
            let rotated = self.path.with_extension("jsonl.1");
            if regular_if_exists(&rotated)? {
                fs::remove_file(&rotated)?;
            }
            fs::rename(&self.path, &rotated)?;
            self.file = Some(data_file(&self.path)?);
            self.bytes = 0;
        }
        self.file.as_mut().unwrap().write_all(bytes)?;
        self.bytes += bytes.len() as u64;
        Ok(())
    }
}

fn check_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() && !is_reparse(&metadata) => {}
            Ok(_) => bail!("readiness directory is not a plain directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn regular_if_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_reparse(&metadata) => Ok(true),
        Ok(_) => bail!("readiness owned path is not a plain file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Exclusive lease prevents retention from deleting a live instance.
        // OPEN_REPARSE_POINT also prevents following a substituted leaf link.
        options.share_mode(0).custom_flags(0x00200000);
    }
    options
}

fn new_file(path: &Path) -> Result<File> {
    Ok(options().create_new(true).open(path)?)
}

fn data_file(path: &Path) -> Result<File> {
    let mut options = options();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // Allow diagnostic readers, but not writers/deletion.
    }
    Ok(options.create_new(true).open(path)?)
}

fn lock_file(path: &Path) -> Result<File> {
    regular_if_exists(path)?;
    let file = options().create(true).truncate(false).open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_reparse(&metadata) || metadata.len() != 0 {
        bail!("readiness lock is not an empty plain file");
    }
    Ok(file)
}

fn owned_stem(name: &str) -> Option<&str> {
    let stem = name
        .strip_suffix(".jsonl.1")
        .or_else(|| name.strip_suffix(".jsonl"))
        .or_else(|| name.strip_suffix(".lock"))?;
    let (pid, id) = stem.strip_prefix(PREFIX)?.split_once('-')?;
    if pid.parse::<u32>().ok()?.to_string() != pid {
        return None;
    }
    let uuid = Uuid::parse_str(id).ok()?;
    (uuid.to_string() == id && uuid.get_version_num() == 7).then_some(stem)
}

fn retain(directory: &Path) -> Result<()> {
    let mut stems = BTreeSet::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if let Some(stem) = entry.file_name().to_str().and_then(owned_stem) {
            stems.insert(stem.to_owned());
        }
    }
    // UUID v7 orders instances by creation time, independently of process ID.
    let mut ordered: Vec<_> = stems.iter().collect();
    ordered.sort_by_key(|stem| stem[PREFIX.len()..].split_once('-').unwrap().1);
    let mut remaining = stems.len();
    for stem in ordered {
        if remaining < INSTANCE_LIMIT {
            break;
        }
        let lease_path = directory.join(format!("{stem}.lock"));
        let current = directory.join(format!("{stem}.jsonl"));
        let rotated = directory.join(format!("{stem}.jsonl.1"));
        let lease = match lock_file(&lease_path) {
            Ok(lease) => lease,
            // Sharing violations mean active. Other errors must be visible.
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.raw_os_error() == Some(32)) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        for path in [&current, &rotated] {
            if regular_if_exists(path)? {
                fs::remove_file(path)?;
            }
        }
        drop(lease);
        fs::remove_file(lease_path)?;
        remaining -= 1;
    }
    if remaining >= INSTANCE_LIMIT {
        bail!("readiness retention capacity occupied by active instances");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("arterm-readiness-{}", Uuid::now_v7()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("healthy sink released fixture handles");
        }
    }
    fn event() -> Event {
        Event {
            kind: EventKind::Input,
            session_id: Uuid::now_v7(),
            command_id: None,
            before: Some(Snapshot::default()),
            after: Some(Snapshot {
                reason: ReadinessReason::PartialHumanInput,
                human_dirty: Some(true),
                ..Snapshot::default()
            }),
            input: Some(InputSummary {
                text: true,
                ..InputSummary::default()
            }),
            elapsed_ms: Some(0),
        }
    }
    fn entries(path: &Path) -> Vec<Envelope> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn schema_defaults_categories_and_privacy_whitelist() {
        assert_eq!(Snapshot::default().status, ShellStatus::Unknown);
        assert_eq!(Snapshot::default().reason, ReadinessReason::Unknown);
        assert_eq!(
            serde_json::to_value(InputSummary::default())
                .unwrap()
                .as_object()
                .unwrap()
                .values()
                .filter(|v| **v != false)
                .count(),
            0
        );
        let mut value = serde_json::to_value(event()).unwrap();
        let keys: BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "kind",
                "session_id",
                "command_id",
                "before",
                "after",
                "input",
                "elapsed_ms"
            ])
        );
        value["command_content"] = "forbidden".into();
        assert!(serde_json::from_value::<Event>(value).is_err());
        for kind in [
            EventKind::Input,
            EventKind::ShellMarker,
            EventKind::CommandWait,
            EventKind::CommandAdmitted,
            EventKind::CommandTimeout,
            EventKind::CommandRejected,
            EventKind::Connection,
        ] {
            assert_eq!(
                serde_json::from_value::<EventKind>(serde_json::to_value(kind).unwrap()).unwrap(),
                kind
            );
        }
        let mut value = serde_json::to_value(InputSummary::default()).unwrap();
        value["text"] = "forbidden".into();
        assert!(serde_json::from_value::<InputSummary>(value).is_err());
        let mut value = serde_json::to_value(Snapshot::default()).unwrap();
        value["output"] = "forbidden".into();
        assert!(serde_json::from_value::<Snapshot>(value).is_err());
    }

    #[test]
    fn jsonl_envelope_root_role_separation_and_shutdown() {
        let a = Temp::new();
        let b = Temp::new();
        let host = ReadinessLog::open(&a.0, Role::Host).unwrap();
        let client = ReadinessLog::open(&a.0, Role::Client).unwrap();
        let other = ReadinessLog::open(&b.0, Role::Host).unwrap();
        let path = host.path().to_owned();
        assert!(path.starts_with(a.0.join("host").join("diagnostics")));
        assert!(client
            .path()
            .starts_with(a.0.join("client").join("diagnostics")));
        assert_ne!(path, other.path());
        let clone = host.clone();
        clone.record(event());
        assert!(host.flush(Duration::from_secs(2)));
        drop(clone);
        drop(host);
        let rows = entries(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].schema_version, 1);
        assert_eq!(rows[0].process_id, std::process::id());
        assert_eq!(rows[0].version, env!("CARGO_PKG_VERSION"));
        assert_eq!(rows[0].role, Role::Host);
        assert!(
            rows[0].unix_ms.abs_diff(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64
            ) < 5000
        );
        drop(client);
        drop(other);
    }

    #[test]
    fn storm_rotation_is_bounded_and_jsonl_remains_valid() {
        let temp = Temp::new();
        let log = ReadinessLog::open(&temp.0, Role::Host).unwrap();
        let path = log.path().to_owned();
        let started = Instant::now();
        for _ in 0..30 {
            for _ in 0..QUEUE_LIMIT {
                log.record(event());
            }
            assert!(log.flush(Duration::from_secs(3)));
        }
        for _ in 0..50_000 {
            log.record(event());
        }
        assert!(started.elapsed() < Duration::from_secs(20));
        assert!(log.flush(Duration::from_secs(3)));
        drop(log);
        let rotated = path.with_extension("jsonl.1");
        assert!(rotated.exists());
        for path in [&path, &rotated] {
            assert!(fs::metadata(path).unwrap().len() <= FILE_LIMIT);
            assert!(!entries(path).is_empty());
        }
    }

    #[test]
    fn retention_preserves_foreign_files_and_active_instances() {
        let temp = Temp::new();
        let active = ReadinessLog::open(&temp.0, Role::Host).unwrap();
        let directory = active.path().parent().unwrap().to_owned();
        let foreign = directory.join("readiness-v1-foreign.jsonl");
        fs::write(&foreign, b"untouched").unwrap();
        for _ in 0..INSTANCE_LIMIT * 2 {
            let log = ReadinessLog::open(&temp.0, Role::Host).unwrap();
            log.record(event());
            assert!(log.flush(Duration::from_secs(2)));
        }
        assert!(active.path().exists());
        assert_eq!(fs::read(foreign).unwrap(), b"untouched");
        let count = fs::read_dir(directory)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_str()
                    .and_then(owned_stem)
                    .is_some()
            })
            .count();
        assert!(count <= INSTANCE_LIMIT * 3);
        assert!(active.flush(Duration::from_secs(2)));
    }

    #[test]
    fn failure_is_returned_and_active_capacity_is_not_evicted() {
        let temp = Temp::new();
        fs::write(temp.0.join("host"), b"not a directory").unwrap();
        assert!(ReadinessLog::open(&temp.0, Role::Host).is_err());
        let mut logs = Vec::new();
        for _ in 0..INSTANCE_LIMIT {
            logs.push(ReadinessLog::open(&temp.0, Role::Client).unwrap());
        }
        assert!(ReadinessLog::open(&temp.0, Role::Client).is_err());
        assert!(logs.iter().all(|log| log.path().exists()));
    }

    #[test]
    fn full_queue_count_and_flush_timeout_are_bounded() {
        let temp = Temp::new();
        let (sender, receiver) = mpsc::sync_channel(1);
        let (_finished, done) = mpsc::sync_channel(1);
        let log = ReadinessLog(Arc::new(Inner {
            path: temp.0.join("unused"),
            sender: Some(sender),
            done: Mutex::new(done),
            drops: Arc::new(AtomicU64::new(0)),
            unavailable: Arc::new(AtomicBool::new(false)),
        }));
        log.record(event());
        let start = Instant::now();
        for _ in 0..100_000 {
            log.record(event());
        }
        assert_eq!(log.0.drops.load(Ordering::Relaxed), 100_000);
        assert!(!log.flush(Duration::from_millis(10)));
        assert!(start.elapsed() < Duration::from_secs(3));
        drop(receiver);
        assert!(!log.flush(Duration::from_secs(1)));
        let start = Instant::now();
        drop(log);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn counted_losses_appear_once_on_next_written_entry() {
        let temp = Temp::new();
        let log = ReadinessLog::open(&temp.0, Role::Client).unwrap();
        log.0.drops.store(42, Ordering::Relaxed);
        log.record(event());
        log.record(event());
        assert!(log.flush(Duration::from_secs(2)));
        let rows = entries(log.path());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].dropped_events, 42);
        assert_eq!(rows[1].dropped_events, 0);
        assert_eq!(log.0.drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn worker_file_failure_is_visible_and_releases_handles() {
        let temp = Temp::new();
        let log = ReadinessLog::open(&temp.0, Role::Host).unwrap();
        // An owned leaf replaced by a directory must never be deleted/traversed.
        let obstruction = log.path().with_extension("jsonl.1");
        fs::create_dir(&obstruction).unwrap();
        let mut failed = false;
        for _ in 0..32 {
            for _ in 0..QUEUE_LIMIT {
                log.record(event());
            }
            if !log.flush(Duration::from_secs(2)) {
                failed = true;
                break;
            }
        }
        assert!(failed, "file failure must not be acknowledged as success");
        assert!(obstruction.is_dir());
        let start = Instant::now();
        log.record(event());
        assert!(!log.flush(Duration::from_millis(10)));
        drop(log);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
