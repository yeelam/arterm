use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    },
    Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetirementReason {
    Completed,
    Missing,
    BrokerChanged,
}

fn validate_lifecycle_knowledge(
    ended: bool, exit_confirmed: Option<bool>, reason: Option<RetirementReason>,
) -> Result<()> {
    ensure!(exit_confirmed != Some(true) || ended,
        "confirmed exit knowledge requires ended state");
    if let Some(reason) = reason {
        ensure!(ended, "retirement reason requires ended state");
        match reason {
            RetirementReason::Completed => ensure!(exit_confirmed == Some(true),
                "completed retirement requires confirmed exit knowledge"),
            RetirementReason::Missing | RetirementReason::BrokerChanged =>
                ensure!(exit_confirmed != Some(true),
                    "noncompletion retirement contradicts confirmed exit knowledge"),
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct RetiredSession {
    pub id: Uuid,
    pub reason: RetirementReason,
}

impl std::fmt::Display for RetiredSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session {} is already retired ({:?}); its GUID cannot be reused", self.id, self.reason)
    }
}
impl std::error::Error for RetiredSession {}

#[derive(Debug)]
pub struct RecoveryBlocked;
impl std::fmt::Display for RecoveryBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session recovery is blocked; exit/nonexistence is not confirmed")
    }
}
impl std::error::Error for RecoveryBlocked {}

#[derive(Serialize, Deserialize, Clone)]
pub struct PendingInput {
    pub seq: u64,
    pub bytes: Vec<u8>,
}
#[derive(Serialize, Deserialize, Clone)]
pub struct State {
    pub schema: u32,
    pub id: Uuid,
    pub box_name: String,
    pub request_id: Uuid,
    pub claim: Vec<u8>,
    pub client_id: Uuid,
    pub origin: Option<Vec<u8>>,
    pub token: Option<Vec<u8>>,
    pub epoch: u64,
    pub create_epoch: u64,
    pub shell: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub create_cols: u16,
    pub create_rows: u16,
    pub create_deadline_ms: Option<u64>,
    pub input_ack: u64,
    pub pending: Option<PendingInput>,
    #[serde(default)]
    pub ended: bool,
    /// Separate exit knowledge from the fail-closed recovery guard above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_confirmed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement_reason: Option<RetirementReason>,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default = "legacy_command_execution")]
    pub command_execution: Option<bool>,
}
fn legacy_command_execution() -> Option<bool> { Some(false) }
impl State {
    pub fn new(box_name: String, shell: String, args: Vec<String>, cwd: Option<String>) -> Self {
        Self {
            schema: 1,
            id: Uuid::now_v7(),
            box_name,
            request_id: Uuid::now_v7(),
            // Resume authorization is server-issued; the create claim uses OS RNG below.
            claim: Vec::new(),
            client_id: Uuid::now_v7(),
            origin: None,
            token: None,
            epoch: 0,
            create_epoch: 1,
            shell,
            args,
            cwd,
            create_cols: 80,
            create_rows: 24,
            create_deadline_ms: None,
            input_ack: 0,
            pending: None,
            ended: false,
            exit_confirmed: None,
            retirement_reason: None,
            reference: None,
            command_execution: None,
        }
    }

    pub fn mark_ended(&mut self, confirmed: bool) -> Result<()> {
        validate_lifecycle_knowledge(self.ended, self.exit_confirmed, self.retirement_reason)?;
        if confirmed { return self.mark_retired(RetirementReason::Completed); }
        self.ended = true;
        self.exit_confirmed = Some(self.exit_confirmed == Some(true));
        Ok(())
    }

    pub fn mark_retired(&mut self, reason: RetirementReason) -> Result<()> {
        validate_lifecycle_knowledge(self.ended, self.exit_confirmed, self.retirement_reason)?;
        let reason = if self.exit_confirmed == Some(true) { RetirementReason::Completed } else { reason };
        self.ended = true;
        self.exit_confirmed = Some(reason == RetirementReason::Completed);
        self.retirement_reason = Some(reason);
        self.claim.clear();
        self.token = None;
        self.pending = None;
        self.args.clear();
        self.cwd = None;
        Ok(())
    }

    pub fn merge_ended(&mut self, other: &State) -> Result<()> {
        validate_lifecycle_knowledge(self.ended, self.exit_confirmed, self.retirement_reason)?;
        validate_lifecycle_knowledge(other.ended, other.exit_confirmed, other.retirement_reason)?;
        if other.ended {
            if let Some(reason) = other.retirement_reason {
                self.mark_retired(reason)?;
            } else {
                self.mark_ended(other.exit_confirmed == Some(true))?;
            }
        }
        Ok(())
    }

    pub fn broker_binding_is_safe(&self) -> bool {
        match &self.origin {
            Some(origin) => origin.len() == 16,
            None => self.token.is_none() && self.pending.is_none() && self.epoch == 0
                && self.input_ack == 0 && self.create_deadline_ms.is_none(),
        }
    }
}

pub struct Store {
    path: PathBuf,
    _lock: File,
    _reference_lock: Option<File>,
    known: bool,
    writer: std::sync::Mutex<()>,
}

/// Public, non-secret reference. Names and canonical GUIDs are case-insensitive.
#[derive(Clone, Debug)]
pub struct SessionReference(String);

impl SessionReference {
    pub fn parse(value: &str) -> Result<Self> {
        ensure!(
            (1..=64).contains(&value.len())
                && value.as_bytes()[0].is_ascii_alphanumeric()
                && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "session reference must be 1..64 ASCII letters/digits/hyphens/underscores, starting with a letter or digit"
        );
        let canonical = value.to_ascii_lowercase();
        if let Ok(id) = Uuid::parse_str(value) {
            ensure!(id.to_string() == canonical, "use a canonical hyphenated session GUID");
        }
        Ok(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Public local inventory only. Presence is not proof that a remote session is
/// alive or that a protected recovery record can still be used.
#[derive(Clone, Debug, Serialize)]
pub struct SavedSession {
    pub session_id: Uuid,
    pub session_name: Option<String>,
    pub recovery_record_present: bool,
}

pub fn saved_sessions(dir: &Path) -> Result<Vec<SavedSession>> {
    use std::{collections::BTreeMap, io::Read, os::windows::fs::MetadataExt};
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT};
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("read local session inventory"),
    };
    let mut sessions = BTreeMap::<Uuid, SavedSession>::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(reference) = name.strip_prefix("ref-").and_then(|n| n.strip_suffix(".json")) {
            let reference = SessionReference::parse(reference).context("invalid local session name")?;
            let file = OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(entry.path()).context("open local session name mapping")?;
            let metadata = file.metadata()?;
            ensure!(metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                "local session mapping is not a regular file");
            ensure!(metadata.len() <= 1024, "oversized local session mapping");
            let mut bytes = Vec::new();
            file.take(1025).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 1024, "oversized local session mapping");
            let id: Uuid = serde_json::from_slice(&bytes).context("invalid local session mapping")?;
            let row = sessions.entry(id).or_insert(SavedSession {
                session_id: id, session_name: None, recovery_record_present: false,
            });
            ensure!(row.session_name.is_none(), "multiple local names refer to session {id}");
            row.session_name = Some(reference.as_str().to_owned());
        } else if let Some(id) = name.strip_suffix(".dpapi").and_then(|n| Uuid::parse_str(n).ok()) {
            let metadata = entry.metadata()?;
            ensure!(metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                "local recovery record is not a regular file");
            sessions.entry(id).or_insert(SavedSession {
                session_id: id, session_name: None, recovery_record_present: false,
            }).recovery_record_present = true;
        }
    }
    let mut result: Vec<_> = sessions.into_values().collect();
    result.sort_by(|a, b| a.session_name.cmp(&b.session_name).then(a.session_id.cmp(&b.session_id)));
    Ok(result)
}

#[derive(Deserialize)]
struct Lifecycle {
    schema: u32,
    id: Uuid,
    box_name: String,
    #[serde(default)]
    ended: bool,
    #[serde(default)]
    exit_confirmed: Option<bool>,
    #[serde(default)]
    reference: Option<String>,
    #[serde(default)]
    retirement_reason: Option<RetirementReason>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    reason: Option<RetirementReason>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetiredGuard {
    schema: u32,
    kind: String,
    id: Uuid,
    box_name: String,
    #[serde(deserialize_with = "required_guard_reference")]
    reference: Option<String>,
    reason: RetirementReason,
}

fn required_guard_reference<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

impl Lifecycle {
    fn validate(&self, box_name: &str, id: Uuid) -> Result<()> {
        ensure!(self.schema == 1 && self.id == id && self.box_name == box_name,
            "session identity/box mismatch");
        if let Some(reference) = &self.reference {
            ensure!(SessionReference::parse(reference)?.as_str() == reference,
                "invalid protected reference");
        }
        ensure!(self.kind.as_deref().map_or(true, |kind| kind == "retired"),
            "unknown recovery record kind");
        ensure!(self.kind.is_none() || self.reason.is_some(), "invalid retired guard");
        if self.kind.is_none() {
            validate_lifecycle_knowledge(self.ended, self.exit_confirmed, self.retirement_reason)?;
        }
        Ok(())
    }

    fn retirement(&self) -> Option<RetirementReason> {
        if self.kind.as_deref() == Some("retired") { return self.reason; }
        if !self.ended { return None; }
        self.retirement_reason.or((self.exit_confirmed == Some(true)).then_some(RetirementReason::Completed))
    }
}

fn read_protected_record(path: &Path) -> Result<Vec<u8>> {
    use std::{io::Read, os::windows::fs::MetadataExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    const MAX_RECORD: u64 = 1024 * 1024;
    let file = OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path).context("open protected recovery record")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "protected recovery record is not a regular file");
    ensure!(metadata.len() <= MAX_RECORD, "oversized protected recovery record");
    let mut bytes = Vec::new();
    file.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_RECORD, "oversized protected recovery record");
    let plain = protect(&bytes, false)?;
    ensure!(plain.len() as u64 <= MAX_RECORD, "oversized protected recovery state");
    Ok(plain)
}

fn read_lifecycle(path: &Path, box_name: &str, id: Uuid) -> Result<Lifecycle> {
    Ok(validated_record(&read_protected_record(path)?, box_name, id)?.0)
}

fn validated_record(plain: &[u8], box_name: &str, id: Uuid) -> Result<(Lifecycle, Option<State>)> {
    let lifecycle: Lifecycle = serde_json::from_slice(plain)
        .map_err(|_| anyhow::anyhow!("invalid protected lifecycle"))?;
    lifecycle.validate(box_name, id)?;
    if lifecycle.kind.as_deref() == Some("retired") {
        let guard: RetiredGuard = serde_json::from_slice(plain)
            .map_err(|_| anyhow::anyhow!("invalid retired guard"))?;
        ensure!(guard.schema == 1 && guard.kind == "retired" && guard.id == id
            && guard.box_name == box_name, "retired guard identity/box mismatch");
        return Ok((lifecycle, None));
    }
    let state: State = serde_json::from_slice(plain)
        .map_err(|_| anyhow::anyhow!("invalid protected state"))?;
    validate_state(&state, box_name, id)?;
    Ok((lifecycle, Some(state)))
}

fn validate_state(state: &State, box_name: &str, id: Uuid) -> Result<()> {
    ensure!(state.schema == 1 && state.id == id && state.box_name == box_name,
        "session identity/box mismatch");
    if let Some(reference) = &state.reference {
        ensure!(SessionReference::parse(reference)?.as_str() == reference, "invalid protected reference");
    }
    validate_lifecycle_knowledge(state.ended, state.exit_confirmed, state.retirement_reason)?;
    ensure!(state.claim.len() == 32, "invalid create claim");
    ensure!(state.token.as_ref().map_or(true, |token| token.len() == 32), "invalid resume token");
    ensure!(state.origin.as_ref().map_or(true, |origin| origin.len() == 16), "invalid broker identity");
    ensure!(state.create_epoch > 0 && state.create_epoch < u64::MAX && state.epoch < u64::MAX
        && state.input_ack < u64::MAX, "invalid recovery counters");
    if let Some(pending) = &state.pending {
        ensure!(state.input_ack.checked_add(1) == Some(pending.seq)
            && !pending.bytes.is_empty() && pending.bytes.len() <= 4096, "invalid outstanding input");
    }
    Ok(())
}

pub fn active_sessions(dir: &Path, box_name: &str) -> Result<Vec<SavedSession>> {
    let mut active = Vec::new();
    for session in saved_sessions(dir)? {
        if !session.recovery_record_present {
            active.push(session);
            continue;
        }
        let result = (|| -> Result<bool> {
            let lifecycle = read_lifecycle(&dir.join(format!("{}.dpapi", session.session_id)), box_name, session.session_id)?;
            if lifecycle.kind.as_deref() == Some("retired") { return Ok(false); }
            if let Some(reason) = lifecycle.retirement() {
                let store = Store::open(dir, session.session_id)?;
                store.retire(box_name, session.session_id, reason)?;
                return Ok(false);
            }
            if lifecycle.ended && lifecycle.exit_confirmed.is_none() {
                crate::statusln!("[list] {}: legacy end decision has no exit confirmation; reference retained until explicit reconciliation.", session.session_id);
            }
            Ok(true)
        })();
        match result {
            Ok(false) => {}
            Ok(true) => active.push(session),
            Err(_) => {
                crate::statusln!("[list] {}: recovery lifecycle unavailable; retaining reference with remote state unknown.", session.session_id);
                active.push(session);
            }
        }
    }
    Ok(active)
}

fn lock_record(path: &Path) -> Result<(File, bool)> {
    match OpenOptions::new().write(true).create_new(true).share_mode(0).open(path) {
        Ok(file) => {
            file.sync_all()?;
            Ok((file, false))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok((OpenOptions::new().write(true).share_mode(0).open(path)
                .context("session already open locally, or state directory inaccessible")?, true))
        }
        Err(error) => Err(error).context("reserve session identity"),
    }
}

impl Store {
    pub fn open(dir: &Path, id: Uuid) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let (lock, known) = lock_record(&dir.join(format!("{id}.lock")))?;
        Ok(Self {
            path: dir.join(format!("{id}.dpapi")),
            _lock: lock,
            _reference_lock: None,
            known,
            writer: std::sync::Mutex::new(()),
        })
    }

    pub fn resolve(
        dir: &Path,
        box_name: &str,
        reference: &SessionReference,
        create: bool,
        shell: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<(Self, State)> {
        Self::resolve_inner(dir, box_name, reference, create, shell, cwd, false)
    }

    pub fn resolve_for_management(
        dir: &Path, box_name: &str, reference: &SessionReference,
    ) -> Result<(Self, State)> {
        Self::resolve_inner(dir, box_name, reference, false, None, None, true)
    }

    fn resolve_inner(
        dir: &Path,
        box_name: &str,
        reference: &SessionReference,
        create: bool,
        shell: Option<&str>,
        cwd: Option<&str>,
        management: bool,
    ) -> Result<(Self, State)> {
        fs::create_dir_all(dir)?;
        let mut reference_lock = None;
        let mut new_mapping = false;
        let id = if let Ok(id) = Uuid::parse_str(reference.as_str()) {
            id
        } else {
            let path = dir.join(format!("ref-{}.json", reference.as_str()));
            let marker = path.with_extension("lock");
            // An interrupted reservation is evidence of a known reference, not permission
            // to replace it. Prefixing filenames also avoids Windows device names.
            ensure!(create || path.try_exists()?, "no saved reference; resume never creates");
            let (lock, known) = lock_record(&marker)?;
            reference_lock = Some(lock);
            if known || path.try_exists()? {
                serde_json::from_slice::<Uuid>(&fs::read(path)
                    .context("missing reference mapping; refusing replacement")?)
                    .context("corrupt reference mapping; refusing replacement")?
            } else {
                let id = Uuid::now_v7();
                atomic_write(&path, &serde_json::to_vec(&id)?)?;
                new_mapping = true;
                id
            }
        };
        ensure!(
            create || dir.join(format!("{id}.dpapi")).try_exists()?
                || dir.join(format!("{id}.lock")).try_exists()?,
            "no saved session; existing-only lookup never reserves a new identity"
        );
        let mut store = Self::open(dir, id)?;
        store._reference_lock = reference_lock;
        let state = if store.known || store.path.try_exists()? || (!new_mapping && Uuid::parse_str(reference.as_str()).is_err()) {
            match store.load(box_name, id) {
                Ok(state) => state,
                Err(error) => {
                    if let Some(retired) = error.downcast_ref::<RetiredSession>() {
                        if Uuid::parse_str(reference.as_str()).is_err() {
                            let lifecycle = read_lifecycle(&store.path, box_name, id)?;
                            ensure!(lifecycle.reference.as_deref() == Some(reference.as_str()),
                                "reference/state identity mismatch");
                            if create {
                                let mut fresh = State::new(box_name.to_owned(),
                                    shell.unwrap_or("powershell.exe").to_owned(), Vec::new(), cwd.map(str::to_owned));
                                fresh.reference = Some(reference.as_str().to_owned());
                                fresh.claim = random_claim()?;
                                let mut replacement = Store::open(dir, fresh.id)?;
                                ensure!(!replacement.known && !replacement.path.try_exists()?,
                                    "fresh session identity already reserved");
                                replacement._reference_lock = store._reference_lock.take();
                                replacement.save(&fresh)?;
                                // A crash before this publication leaves an unusable orphan;
                                // the old mapping and its retired GUID remain intact.
                                atomic_write(&dir.join(format!("ref-{}.json", reference.as_str())),
                                    &serde_json::to_vec(&fresh.id)?)?;
                                return Ok((replacement, fresh));
                            }
                        }
                        return Err(RetiredSession { id: retired.id, reason: retired.reason }.into());
                    }
                    return Err(error);
                }
            }
        } else {
            ensure!(create, "no saved session; resume never creates");
            let mut state = State::new(
                box_name.to_owned(),
                shell.unwrap_or("powershell.exe").to_owned(),
                Vec::new(),
                cwd.map(str::to_owned),
            );
            state.id = id;
            if new_mapping {
                state.reference = Some(reference.as_str().to_owned());
            }
            state.claim = random_claim()?;
            store.save(&state)?;
            state
        };
        if Uuid::parse_str(reference.as_str()).is_err() {
            ensure!(state.reference.as_deref() == Some(reference.as_str()), "reference/state identity mismatch");
        }
        if let Some(name) = &state.reference {
            let mapped: Uuid = serde_json::from_slice(&fs::read(dir.join(format!("ref-{name}.json")))?)
                .context("invalid reference publication")?;
            ensure!(mapped == state.id, "reference publication incomplete or superseded; refusing session use");
        }
        if state.ended && !management { return Err(RecoveryBlocked.into()); }
        ensure!(shell.map_or(true, |s| s == state.shell), "incompatible --shell for saved session");
        ensure!(cwd.map_or(true, |c| state.cwd.as_deref() == Some(c)), "incompatible --cwd for saved session");
        Ok((store, state))
    }
    pub fn load(&self, box_name: &str, id: Uuid) -> Result<State> {
        let plain = read_protected_record(&self.path)
            .context("no saved session; refusing to create a replacement")?;
        let (lifecycle, state) = validated_record(&plain, box_name, id)?;
        if let Some(reason) = lifecycle.retirement() {
            if lifecycle.kind.is_none() { self.retire(box_name, id, reason)?; }
            return Err(RetiredSession { id, reason }.into());
        }
        state.context("invalid protected state")
    }
    pub fn save(&self, state: &State) -> Result<()> {
        validate_lifecycle_knowledge(state.ended, state.exit_confirmed, state.retirement_reason)?;
        let _writer = self.writer.lock().unwrap();
        ensure!(self.path.file_stem().and_then(|name| name.to_str())
            .and_then(|name| Uuid::parse_str(name).ok()) == Some(state.id), "session record ID mismatch");
        let reason = state.retirement_reason.or(
            (state.ended && state.exit_confirmed == Some(true)).then_some(RetirementReason::Completed));
        if self.path.try_exists()? {
            let lifecycle = read_lifecycle(&self.path, &state.box_name, state.id)?;
            if let Some(retired) = lifecycle.retirement() {
                if lifecycle.kind.is_none() { self.write_guard(&lifecycle, retired)?; }
                if reason.is_some() { return Ok(()); }
                return Err(RetiredSession { id: state.id, reason: retired }.into());
            }
            if let Some(reason) = reason { return self.write_guard(&lifecycle, reason); }
        } else if reason.is_some() {
            anyhow::bail!("no validated recovery record; refusing retirement");
        }
        validate_state(state, &state.box_name, state.id)?;
        let bytes = protect(&serde_json::to_vec(state)?, true)?;
        atomic_write(&self.path, &bytes)
    }

    pub fn retire(&self, box_name: &str, id: Uuid, reason: RetirementReason) -> Result<()> {
        let _writer = self.writer.lock().unwrap();
        let lifecycle = read_lifecycle(&self.path, box_name, id)?;
        if lifecycle.kind.as_deref() == Some("retired") { return Ok(()); }
        self.write_guard(&lifecycle, reason)
    }

    fn write_guard(&self, lifecycle: &Lifecycle, reason: RetirementReason) -> Result<()> {
        let guard = RetiredGuard { schema: 1, kind: "retired".into(), id: lifecycle.id,
            box_name: lifecycle.box_name.clone(), reference: lifecycle.reference.clone(), reason };
        atomic_write(&self.path, &protect(&serde_json::to_vec(&guard)?, true)?)
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
        let temp = path.with_extension(format!("{}.tmp", Uuid::now_v7()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            let wide = |p: &Path| -> Vec<u16> {
                use std::os::windows::ffi::OsStrExt;
                p.as_os_str().encode_wide().chain(Some(0)).collect()
            };
            let from = wide(&temp);
            let to = wide(path);
            ensure!(
                unsafe {
                    MoveFileExW(
                        from.as_ptr(),
                        to.as_ptr(),
                        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                    )
                } != 0,
                "atomic state replacement failed: {}",
                std::io::Error::last_os_error()
            );
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
}

pub fn protect(bytes: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len().try_into()?,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let entropy_bytes = b"vsterm-personal-v1";
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_bytes.len() as u32,
        pbData: entropy_bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    ensure!(ok != 0, "DPAPI failed: {}", std::io::Error::last_os_error());
    let result =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut _);
    }
    Ok(result)
}

pub fn random_claim() -> Result<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut bytes = vec![0u8; 32];
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    ensure!(status >= 0, "OS random generator failed");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_records_do_not_opt_into_command_integration() {
        let state = super::State::new("target".into(), "powershell.exe".into(), vec![], None);
        assert_eq!(state.command_execution, None);
        let mut value = serde_json::to_value(&state).unwrap();
        value.as_object_mut().unwrap().remove("command_execution");
        let old: super::State = serde_json::from_value(value).unwrap();
        assert_eq!(old.command_execution, Some(false));
    }
    use super::*;
    #[test]
    fn existing_only_guid_lookup_preserves_legacy_state_and_interrupted_reservations() {
        let dir = std::env::temp_dir().join(format!("arterm-guid-lookup-{}", Uuid::now_v7()));
        let mut state = State::new("box".into(), "powershell.exe".into(), vec![], None);
        state.claim = random_claim().unwrap();
        let store = Store::open(&dir, state.id).unwrap();
        store.save(&state).unwrap();
        drop(store);
        fs::remove_file(dir.join(format!("{}.lock", state.id))).unwrap();
        let reference = SessionReference::parse(&state.id.to_string()).unwrap();
        let (store, loaded) = Store::resolve(&dir, "box", &reference, false, None, None).unwrap();
        assert_eq!(loaded.id, state.id);
        assert_eq!(loaded.claim, state.claim);
        assert!(Store::resolve(&dir, "box", &reference, false, None, None).is_err());
        drop(store);

        let interrupted = Uuid::now_v7();
        drop(Store::open(&dir, interrupted).unwrap());
        let reference = SessionReference::parse(&interrupted.to_string()).unwrap();
        for create in [false, true] {
            assert!(Store::resolve(&dir, "box", &reference, create, None, None).is_err());
        }
        assert!(dir.join(format!("{interrupted}.lock")).exists());
        assert!(!dir.join(format!("{interrupted}.dpapi")).exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn references_are_durable_exclusive_and_fail_closed() {
        let dir = std::env::temp_dir().join(format!("arterm-refs-{}", Uuid::now_v7()));
        let reference = SessionReference::parse("MyWork").unwrap();
        let (store, mut state) = Store::resolve(&dir, "box", &reference, true, None, None).unwrap();
        let id = state.id;
        let claim = state.claim.clone();
        let request = state.request_id;
        assert!(Store::resolve(&dir, "box", &reference, true, None, None).is_err());
        assert!(Store::open(&dir, id).is_err());
        drop(store);
        let lower = SessionReference::parse("mywork").unwrap();
        let (store, loaded) = Store::resolve(&dir, "box", &lower, true, None, None).unwrap();
        assert_eq!((loaded.id, loaded.request_id, loaded.claim), (id, request, claim));
        drop(store);
        assert!(Store::resolve(&dir, "box", &lower, true, Some("other.exe"), None).is_err());
        assert!(Store::resolve(&dir, "box", &lower, true, None, Some(r"C:\other")).is_err());
        let legacy = SessionReference::parse(&id.to_string()).unwrap();
        let (store, _) = Store::resolve(&dir, "box", &legacy, false, None, None).unwrap();
        state.ended = true;
        store.save(&state).unwrap();
        drop(store);
        assert!(Store::resolve(&dir, "box", &lower, true, None, None).is_err());
        fs::write(dir.join(format!("{id}.dpapi")), b"corrupt").unwrap();
        assert!(Store::resolve(&dir, "box", &lower, true, None, None).is_err());
        fs::remove_file(dir.join(format!("{id}.dpapi"))).unwrap();
        assert!(Store::resolve(&dir, "box", &lower, true, None, None).is_err());
        assert!(Store::resolve(&dir, "box", &legacy, true, None, None).is_err());
        fs::write(dir.join("ref-mywork.json"), b"corrupt").unwrap();
        assert!(Store::resolve(&dir, "box", &lower, true, None, None).is_err());
        fs::remove_file(dir.join("ref-mywork.json")).unwrap();
        assert!(Store::resolve(&dir, "box", &lower, true, None, None).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reference_grammar_and_legacy_defaults() {
        for bad in ["", "../x", r"..\x", "a.b", "a:b", "-x", "x ", "x\n", "日本語"] {
            assert!(SessionReference::parse(bad).is_err(), "{bad}");
        }
        assert!(SessionReference::parse(&"a".repeat(65)).is_err());
        for good in ["MyWork", "MyOwnResumeToken", "CON", "NUL", "COM1", "a_b-9"] {
            assert!(SessionReference::parse(good).is_ok());
        }
        let mut value = serde_json::to_value(State::new("box".into(), "pwsh".into(), vec![], None)).unwrap();
        value.as_object_mut().unwrap().remove("ended");
        value.as_object_mut().unwrap().remove("reference");
        let old: State = serde_json::from_value(value).unwrap();
        assert!(!old.ended);
        assert!(old.reference.is_none());
    }

    #[test]
    fn concurrent_reference_reservation_has_one_winner() {
        let dir = std::env::temp_dir().join(format!("arterm-ref-race-{}", Uuid::now_v7()));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2).map(|_| {
            let dir = dir.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let result = Store::resolve(&dir, "box", &SessionReference::parse("Race").unwrap(), true, None, None);
                barrier.wait();
                result
            })
        }).collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        drop(results);
        let (store, _) = Store::resolve(&dir, "box", &SessionReference::parse("race").unwrap(), false, None, None).unwrap();
        drop(store);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn protected_roundtrip_and_local_lock() {
        let dir = std::env::temp_dir().join(format!("devbox-state-test-{}", Uuid::now_v7()));
        let mut state = State::new("box".into(), "pwsh".into(), vec![], None);
        state.claim = random_claim().unwrap();
        state.token = Some(vec![42; 32]);
        {
            let store = Store::open(&dir, state.id).unwrap();
            store.save(&state).unwrap();
            assert_eq!(store.load("box", state.id).unwrap().token, state.token);
            assert!(store.load("other-box", state.id).is_err());
            assert!(Store::open(&dir, state.id).is_err());
            let bytes = fs::read(&store.path).unwrap();
            assert!(!bytes.windows(32).any(|b| b == [42; 32]));
            state.epoch = 2;
            store.save(&state).unwrap();
            assert_eq!(store.load("box", state.id).unwrap().epoch, 2);
        }
        fs::remove_file(dir.join(format!("{}.lock", state.id))).unwrap();
        fs::remove_file(dir.join(format!("{}.dpapi", state.id))).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
