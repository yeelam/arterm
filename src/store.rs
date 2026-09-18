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
    #[serde(default)]
    pub reference: Option<String>,
}
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
            reference: None,
        }
    }
}

pub struct Store {
    path: PathBuf,
    _lock: File,
    _reference_lock: Option<File>,
    known: bool,
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
            store.load(box_name, id)?
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
        ensure!(!state.ended, "session has ended; choose a NEW reference");
        ensure!(shell.map_or(true, |s| s == state.shell), "incompatible --shell for saved session");
        ensure!(cwd.map_or(true, |c| state.cwd.as_deref() == Some(c)), "incompatible --cwd for saved session");
        Ok((store, state))
    }
    pub fn load(&self, box_name: &str, id: Uuid) -> Result<State> {
        let bytes =
            fs::read(&self.path).context("no saved session; refusing to create a replacement")?;
        let plain = protect(&bytes, false)?;
        let state: State = serde_json::from_slice(&plain).context("invalid protected state")?;
        ensure!(
            state.schema == 1 && state.id == id && state.box_name == box_name,
            "session identity/box mismatch"
        );
        ensure!(state.claim.len() == 32, "invalid create claim");
        ensure!(
            state.token.as_ref().map_or(true, |t| t.len() == 32),
            "invalid resume token"
        );
        ensure!(
            state.origin.as_ref().map_or(true, |b| b.len() == 16),
            "invalid broker identity"
        );
        if let Some(p) = &state.pending {
            ensure!(
                p.seq == state.input_ack + 1 && !p.bytes.is_empty() && p.bytes.len() <= 4096,
                "invalid outstanding input"
            );
        }
        Ok(state)
    }
    pub fn save(&self, state: &State) -> Result<()> {
        let bytes = protect(&serde_json::to_vec(state)?, true)?;
        atomic_write(&self.path, &bytes)
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
