//! Regular-file transfer primitives, not a remotely callable filesystem service.
//!
//! The integrator must authenticate before constructing `AuthorizedSession`, enforce
//! connection deadlines/global quotas, and keep one manager for the session lifetime.
//! Limits here cover this manager, including its retained completed uploads. No
//! credentials, terminal input, transport, automatic retry, or byte-resume live here.
//! Receipts are bounded and in-memory; losing the manager loses commit knowledge,
//! NOT completed files. Reconnection must not infer failure or retransmit blindly.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree, GENERIC_READ, GENERIC_WRITE, HANDLE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateDirectoryW, FileDispositionInfo, FileRenameInfo, GetDriveTypeW,
        GetFileInformationByHandle, GetFileType, GetFinalPathNameByHandleW,
        SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_FLAG_SEQUENTIAL_SCAN, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub const MAX_CHUNK_BYTES: usize = 64 * 1024;
const DELETE_ACCESS: u32 = 0x0001_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ErrorCode {
    InvalidPath,
    PathPolicyDenied,
    NotRegularFile,
    SourceBusy,
    SourceChanged,
    InvalidOffset,
    SizeMismatch,
    IntegrityMismatch,
    LimitExceeded,
    NotFound,
    InvalidState,
    DestinationExists,
    Io,
    OutcomeUnknown,
}

#[derive(Debug)]
pub struct TransferError {
    pub code: ErrorCode,
    pub detail: String,
}
impl fmt::Display for TransferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.detail)
    }
}
impl std::error::Error for TransferError {}
fn fail(code: ErrorCode, detail: impl Into<String>) -> anyhow::Error {
    TransferError {
        code,
        detail: detail.into(),
    }
    .into()
}
fn io_error(action: &str, error: std::io::Error) -> anyhow::Error {
    fail(ErrorCode::Io, format!("{action}: {error}"))
}

/// Assertion made by trusted in-process code AFTER session authorization.
/// This is not a credential and must never be deserialized from a wire request.
pub struct AuthorizedSession {
    session_id: Uuid,
}
impl AuthorizedSession {
    pub fn after_authorization(session_id: Uuid) -> Self {
        Self { session_id }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_file_bytes: u64,
    pub max_stored_bytes: u64,
    pub max_active: usize,
    pub max_records: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 8 * 1024 * 1024 * 1024,
            max_stored_bytes: 16 * 1024 * 1024 * 1024,
            max_active: 2,
            max_records: 256,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub session_id: Uuid,
    pub transfer_id: Uuid,
    pub actual_path: PathBuf,
    pub bytes: u64,
    pub sha256: [u8; 32],
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransferState {
    Uploading,
    Downloading,
    Completed,
    Cancelled,
    Failed,
    OutcomeUnknown,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Direction {
    Upload,
    Download,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub session_id: Uuid,
    pub transfer_id: Uuid,
    /// Allocated destination for uploads; validated source for downloads.
    pub actual_path: PathBuf,
    pub direction: Direction,
    pub state: TransferState,
    pub bytes: u64,
    pub expected_bytes: u64,
    pub receipt: Option<Receipt>,
    pub error: Option<String>,
}
#[derive(Debug)]
pub struct DownloadChunk {
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub eof: bool,
}

/// Pins the object and every ancestor against write/delete sharing. This is an
/// object snapshot, not a recursive directory snapshot: archive preparation must
/// pin/check each descendant and detect changes to the enumerated tree itself.
pub struct PinnedSource {
    file: File,
    _pins: Vec<File>,
    path: PathBuf,
    directory: bool,
    initial: Fingerprint,
}

impl PinnedSource {
    pub fn path(&self) -> &Path { &self.path }
    pub fn file(&self) -> &File { &self.file }
    pub fn is_directory(&self) -> bool { self.directory }
    pub fn verify_unchanged(&self) -> Result<()> {
        if object_fingerprint(&self.file)? != self.initial {
            return Err(fail(ErrorCode::SourceChanged, "pinned source metadata changed"));
        }
        verify_path_identity(&self.file, &self.path)
    }
}

/// Call only after authorization. Classification uses the pinned handle, never
/// an extension or an unpinned `is_dir` check.
pub fn pin_source(source: &Path) -> Result<PinnedSource> {
    validate_absolute_path(source)?;
    let pins = pin_directories(source.parent()
        .ok_or_else(|| fail(ErrorCode::InvalidPath, "source has no parent"))?)?;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_BACKUP_SEMANTICS)
        .open(extended_path(source)?)
        .map_err(|e| if e.raw_os_error() == Some(32) {
            fail(ErrorCode::SourceBusy, "source is open for writing/deletion")
        } else { io_error("pin source", e) })?;
    let metadata = file.metadata().map_err(|e| io_error("inspect pinned source type", e))?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(fail(ErrorCode::NotRegularFile, "source is not a regular file or directory"));
    }
    let initial = object_fingerprint(&file)?;
    let path = actual_path(&file)?;
    verify_path_identity(&file, source)?;
    Ok(PinnedSource { file, _pins: pins, path, directory: metadata.is_dir(), initial })
}

struct Upload {
    file: File,
    directory: PathBuf,
    directory_identity: (u32, u64),
    _pin: File,
    hash: Sha256,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Fingerprint {
    volume: u32,
    index: u64,
    size: u64,
    modified: u64,
}
struct Download {
    file: File,
    _pins: Vec<File>,
    hash: Sha256,
    initial: Fingerprint,
}
enum Active {
    Upload(Upload),
    Download(Download),
}
struct Entry {
    status: Status,
    active: Option<Active>,
    extracted_bytes: Option<u64>,
}

pub struct TransferManager {
    session_id: Uuid,
    root: PathBuf,
    _pins: Vec<File>,
    limits: Limits,
    charged_bytes: u64,
    closed: bool,
    entries: HashMap<Uuid, Entry>,
}

impl TransferManager {
    /// `temp_root` must already exist. Ancestors are pinned and reparse points
    /// rejected; a fresh ACL-protected session directory is created beneath it.
    pub fn new(auth: AuthorizedSession, temp_root: &Path, limits: Limits) -> Result<Self> {
        if limits.max_active == 0 || limits.max_records < limits.max_active {
            return Err(fail(ErrorCode::LimitExceeded, "invalid transfer limits"));
        }
        let mut pins = pin_directories(temp_root)?;
        let root = temp_root.join(format!("{}-{}", auth.session_id, Uuid::now_v7()));
        create_private_directory(&root)?;
        let root_pin = pin_directory(&root)
            .context("new transfer root could not be verified; unverified path retained")?;
        let root_identity = handle_identity(&root_pin)
            .context("new transfer root identity unavailable; unverified path retained")?;
        let root = match actual_path(&root_pin) {
            Ok(path) => path,
            Err(error) => {
                drop(root_pin);
                return Err(cleanup_empty_directory(&root, root_identity, error));
            }
        };
        pins.push(root_pin);
        Ok(Self {
            session_id: auth.session_id,
            root,
            _pins: pins,
            limits,
            charged_bytes: 0,
            closed: false,
            entries: HashMap::new(),
        })
    }
    pub fn directory(&self) -> &Path {
        &self.root
    }
    pub fn charged_bytes(&self) -> u64 {
        self.charged_bytes
    }
    pub fn record_count(&self) -> usize {
        self.entries.len()
    }

    /// Remaining receiver expansion budget, including already retained payloads.
    /// An admitted publication is never implicitly retried or refunded: its
    /// outcome may be unknown even if the response/cleanup later fails.
    pub fn extraction_budget(&self, id: Uuid) -> Result<u64> {
        if self.closed {
            return Err(fail(ErrorCode::InvalidState, "transfer manager has shut down"));
        }
        let entry = self.entries.get(&id).ok_or_else(|| fail(ErrorCode::NotFound, "transfer"))?;
        if entry.status.direction != Direction::Upload || entry.status.state != TransferState::Completed
            || entry.status.receipt.is_none() {
            return Err(fail(ErrorCode::InvalidState, "extraction requires a verified receiver payload"));
        }
        if entry.extracted_bytes.is_some() {
            return Err(fail(ErrorCode::OutcomeUnknown,
                "directory publication was already admitted; do not blindly retry"));
        }
        let remaining = self.limits.max_stored_bytes.checked_sub(self.charged_bytes)
            .ok_or_else(|| fail(ErrorCode::InvalidState, "retained-byte accounting is inconsistent"))?;
        Ok(remaining.min(self.limits.max_file_bytes))
    }

    /// Call with ACTUAL streamed expansion bytes under the final publication
    /// guard, before rename. The caller already holds this manager exclusively;
    /// no terminal/session lock is needed during decompression.
    pub fn reserve_extracted_bytes(&mut self, id: Uuid, bytes: u64) -> Result<()> {
        if bytes > self.extraction_budget(id)? {
            return Err(fail(ErrorCode::LimitExceeded, "extracted data exceeds retained storage quota"));
        }
        let charged = self.charged_bytes.checked_add(bytes)
            .ok_or_else(|| fail(ErrorCode::LimitExceeded, "retained storage quota overflow"))?;
        self.entries.get_mut(&id).ok_or_else(|| fail(ErrorCode::NotFound, "transfer"))?
            .extracted_bytes = Some(bytes);
        self.charged_bytes = charged;
        Ok(())
    }

    fn admission(&self, size: u64) -> Result<()> {
        if self.closed {
            return Err(fail(
                ErrorCode::InvalidState,
                "transfer manager has shut down",
            ));
        }
        if size > self.limits.max_file_bytes
            || self.entries.len() >= self.limits.max_records
            || self.entries.values().filter(|e| e.active.is_some()).count()
                >= self.limits.max_active
        {
            return Err(fail(
                ErrorCode::LimitExceeded,
                "file, active transfer, or receipt limit",
            ));
        }
        Ok(())
    }
    fn initial_status(&self, id: Uuid, path: PathBuf, size: u64, state: TransferState) -> Status {
        Status {
            session_id: self.session_id,
            transfer_id: id,
            actual_path: path,
            direction: if state == TransferState::Uploading {
                Direction::Upload
            } else {
                Direction::Download
            },
            state,
            bytes: 0,
            expected_bytes: size,
            receipt: None,
            error: None,
        }
    }

    /// Reserve a create-new destination. The sender streams its SHA-256 alongside
    /// the bytes and supplies the final digest to `finish`; no preliminary pass.
    pub fn begin_upload(&mut self, basename: &str, size: u64) -> Result<Status> {
        validate_basename(basename)?;
        self.admission(size)?;
        let charged = self
            .charged_bytes
            .checked_add(size)
            .filter(|n| *n <= self.limits.max_stored_bytes)
            .ok_or_else(|| fail(ErrorCode::LimitExceeded, "destination storage quota"))?;
        let id = Uuid::now_v7();
        let directory = self.root.join(id.to_string());
        let path = directory.join(basename);
        validate_absolute_path(&path)?;
        create_private_directory(&directory)?;
        let pin = pin_directory(&directory)
            .context("new upload directory could not be verified; unverified path retained")?;
        let directory_identity = handle_identity(&pin)
            .context("new upload directory identity unavailable; unverified path retained")?;
        // A fixed staging name cannot collide with a permitted destination name.
        let staging = extended_path(&directory)?.join(".arterm-partial");
        let opened = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE_ACCESS)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&staging);
        let file = match opened {
            Ok(file) => file,
            Err(error) => {
                drop(pin);
                return Err(cleanup_empty_directory(
                    &directory,
                    directory_identity,
                    io_error("create upload staging file", error),
                ));
            }
        };
        let status = self.initial_status(id, path, size, TransferState::Uploading);
        self.entries.insert(
            id,
            Entry {
                status: status.clone(),
                extracted_bytes: None,
                active: Some(Active::Upload(Upload {
                    file,
                    directory,
                    directory_identity,
                    _pin: pin,
                    hash: Sha256::new(),
                })),
            },
        );
        self.charged_bytes = charged;
        Ok(status)
    }

    /// Invalid offsets/lengths reject the message without changing transfer state.
    /// I/O failure aborts the transfer and preserves an explicit failure status.
    pub fn write_chunk(&mut self, id: Uuid, offset: u64, bytes: &[u8]) -> Result<u64> {
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or_else(|| fail(ErrorCode::NotFound, "transfer"))?;
        let Some(Active::Upload(upload)) = &mut entry.active else {
            return Err(fail(ErrorCode::InvalidState, "not an active upload"));
        };
        if offset != entry.status.bytes {
            return Err(fail(ErrorCode::InvalidOffset, "nonsequential upload"));
        }
        if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
            return Err(fail(
                ErrorCode::LimitExceeded,
                "chunk must contain 1..65536 bytes",
            ));
        }
        let next = offset
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= entry.status.expected_bytes)
            .ok_or_else(|| fail(ErrorCode::SizeMismatch, "upload exceeds declared length"))?;
        if let Err(e) = upload.file.write_all(bytes) {
            let error = io_error("write upload", e);
            return Err(self.abort_failure(id, error));
        }
        upload.hash.update(bytes);
        entry.status.bytes = next;
        Ok(next)
    }

    pub fn finish(&mut self, id: Uuid, expected_sha256: [u8; 32]) -> Result<Receipt> {
        self.finish_guarded(id, expected_sha256, || Ok(()))
    }

    /// Validate/flush without the authorization lock, then hold its guard only
    /// across the atomic rename. A rejected guard removes only our partial.
    pub fn finish_guarded<G>(
        &mut self,
        id: Uuid,
        expected_sha256: [u8; 32],
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<Receipt> {
        let entry = self
            .entries
            .get(&id)
            .ok_or_else(|| fail(ErrorCode::NotFound, "transfer"))?;
        if entry.status.direction != Direction::Upload {
            return Err(fail(ErrorCode::InvalidState, "finish requires an upload"));
        }
        if let Some(receipt) = &entry.status.receipt {
            if receipt.sha256 != expected_sha256 {
                return Err(fail(
                    ErrorCode::IntegrityMismatch,
                    "conflicting finish digest",
                ));
            }
            return Ok(receipt.clone());
        }
        let Some(Active::Upload(upload)) = &entry.active else {
            return Err(fail(ErrorCode::InvalidState, "not an active upload"));
        };
        let digest: [u8; 32] = upload.hash.clone().finalize().into();
        let validation = if entry.status.bytes != entry.status.expected_bytes {
            Err(fail(
                ErrorCode::SizeMismatch,
                "upload ended before declared length",
            ))
        } else if digest != expected_sha256 {
            Err(fail(ErrorCode::IntegrityMismatch, "SHA-256 mismatch"))
        } else {
            fingerprint(&upload.file).and_then(|fp| {
                if fp.size != entry.status.expected_bytes {
                    return Err(fail(ErrorCode::SizeMismatch, "staging file length differs"));
                }
                upload
                    .file
                    .sync_all()
                    .map_err(|e| io_error("flush upload", e))
            })
        };
        if let Err(error) = validation {
            return Err(self.abort_failure(id, error));
        }
        let guard = match authorize() {
            Ok(guard) => guard,
            Err(error) => return Err(self.abort_failure(id, error)),
        };
        let entry = self.entries.get_mut(&id).unwrap();
        let Some(Active::Upload(upload)) = entry.active.take() else {
            unreachable!()
        };
        // Rename the open file by handle, never close then rename a replaceable path.
        let published = rename_no_replace(&upload.file, &entry.status.actual_path);
        drop(guard);
        if let Err(error) = published {
            entry.active = Some(Active::Upload(upload));
            return Err(self.abort_failure(id, error));
        }
        let confirmed = actual_path(&upload.file).and_then(|actual| {
            verify_path_identity(&upload.file, &entry.status.actual_path)?;
            if fingerprint(&upload.file)?.size != entry.status.expected_bytes {
                return Err(fail(ErrorCode::OutcomeUnknown, "final length differs"));
            }
            upload
                .file
                .sync_all()
                .map_err(|e| io_error("flush finalized file", e))?;
            Ok(actual)
        });
        match confirmed {
            Ok(path) => {
                let receipt = Receipt {
                    session_id: self.session_id,
                    transfer_id: id,
                    actual_path: path,
                    bytes: entry.status.bytes,
                    sha256: digest,
                };
                entry.status.actual_path = receipt.actual_path.clone();
                entry.status.state = TransferState::Completed;
                entry.status.receipt = Some(receipt.clone());
                Ok(receipt)
            }
            Err(error) => {
                // Publication has happened: never delete or claim definite failure.
                entry.status.state = TransferState::OutcomeUnknown;
                entry.status.error = Some(format!("{error:#}"));
                Err(fail(
                    ErrorCode::OutcomeUnknown,
                    format!("publication occurred; inspect status: {error:#}"),
                ))
            }
        }
    }

    pub fn begin_download(&mut self, source: &Path) -> Result<Status> {
        self.admission(0)?;
        let PinnedSource { file, _pins: pins, path: actual, .. } = pin_source(source)?;
        let initial = fingerprint(&file)?;
        self.admission(initial.size)?;
        let id = Uuid::now_v7();
        let status = self.initial_status(id, actual, initial.size, TransferState::Downloading);
        self.entries.insert(
            id,
            Entry {
                status: status.clone(),
                extracted_bytes: None,
                active: Some(Active::Download(Download {
                    file,
                    _pins: pins,
                    hash: Sha256::new(),
                    initial,
                })),
            },
        );
        Ok(status)
    }

    pub fn read_chunk(&mut self, id: Uuid, offset: u64, maximum: usize) -> Result<DownloadChunk> {
        if maximum == 0 || maximum > MAX_CHUNK_BYTES {
            return Err(fail(ErrorCode::LimitExceeded, "invalid read chunk limit"));
        }
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or_else(|| fail(ErrorCode::NotFound, "transfer"))?;
        let Some(Active::Download(download)) = &mut entry.active else {
            return Err(fail(ErrorCode::InvalidState, "not an active download"));
        };
        if offset != entry.status.bytes {
            return Err(fail(ErrorCode::InvalidOffset, "nonsequential download"));
        }
        let result = (|| -> Result<DownloadChunk> {
            if fingerprint(&download.file)? != download.initial {
                return Err(fail(ErrorCode::SourceChanged, "source metadata changed"));
            }
            let count = (entry.status.expected_bytes - offset).min(maximum as u64) as usize;
            let mut bytes = vec![0; count];
            download.file.read_exact(&mut bytes).map_err(|e| {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    fail(ErrorCode::SourceChanged, "source truncated during download")
                } else {
                    io_error("read download source", e)
                }
            })?;
            if fingerprint(&download.file)? != download.initial {
                return Err(fail(ErrorCode::SourceChanged, "source changed during read"));
            }
            download.hash.update(&bytes);
            entry.status.bytes += count as u64;
            Ok(DownloadChunk {
                offset,
                bytes,
                eof: entry.status.bytes == entry.status.expected_bytes,
            })
        })();
        match result {
            Ok(chunk) => Ok(chunk),
            Err(e) => Err(self.abort_failure(id, e)),
        }
    }

    /// Source-stream receipt only: the receiving endpoint must independently
    /// verify its bytes/digest and finalize its own destination before success.
    pub fn close(&mut self, id: Uuid) -> Result<Receipt> {
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or_else(|| fail(ErrorCode::NotFound, "transfer"))?;
        if entry.status.direction != Direction::Download {
            return Err(fail(ErrorCode::InvalidState, "close requires a download"));
        }
        if let Some(receipt) = &entry.status.receipt {
            return Ok(receipt.clone());
        }
        let Some(Active::Download(download)) = &mut entry.active else {
            return Err(fail(ErrorCode::InvalidState, "not an active download"));
        };
        let result = (|| -> Result<Receipt> {
            if entry.status.bytes != entry.status.expected_bytes {
                return Err(fail(ErrorCode::SizeMismatch, "download closed before EOF"));
            }
            if fingerprint(&download.file)? != download.initial {
                return Err(fail(
                    ErrorCode::SourceChanged,
                    "source changed before close",
                ));
            }
            let mut extra = [0u8; 1];
            if download
                .file
                .read(&mut extra)
                .map_err(|e| io_error("check source EOF", e))?
                != 0
            {
                return Err(fail(ErrorCode::SourceChanged, "source grew"));
            }
            Ok(Receipt {
                session_id: self.session_id,
                transfer_id: id,
                actual_path: entry.status.actual_path.clone(),
                bytes: entry.status.bytes,
                sha256: download.hash.clone().finalize().into(),
            })
        })();
        match result {
            Ok(receipt) => {
                entry.active.take();
                entry.status.state = TransferState::Completed;
                entry.status.receipt = Some(receipt.clone());
                Ok(receipt)
            }
            Err(e) => Err(self.abort_failure(id, e)),
        }
    }

    pub fn status(&self, id: Uuid) -> Result<Status> {
        self.entries
            .get(&id)
            .map(|e| e.status.clone())
            .ok_or_else(|| {
                fail(
                    ErrorCode::NotFound,
                    "transfer status unavailable; do not assume uncommitted",
                )
            })
    }

    /// Idempotent for terminal records; never removes a completed artifact.
    pub fn cancel(&mut self, id: Uuid) -> Result<Status> {
        let entry = self
            .entries
            .get_mut(&id)
            .ok_or_else(|| fail(ErrorCode::NotFound, "transfer"))?;
        let Some(active) = entry.active.take() else {
            return Ok(entry.status.clone());
        };
        if let Active::Upload(upload) = active {
            if let Err(error) = delete_open_file(&upload.file) {
                entry.active = Some(Active::Upload(upload));
                entry.status.error = Some(format!("partial cleanup failed: {error:#}"));
                return Err(error);
            }
            let Upload { file, directory, directory_identity, _pin: pin, .. } = upload;
            drop(file);
            drop(pin);
            self.charged_bytes -= entry.status.expected_bytes;
            entry.status.state = TransferState::Cancelled;
            if let Err(error) = delete_owned_directory(&directory, directory_identity) {
                entry.status.error =
                    Some(format!("empty transfer-directory cleanup failed: {error}"));
                return Err(error).context("remove owned empty transfer directory");
            }
        }
        entry.status.state = TransferState::Cancelled;
        Ok(entry.status.clone())
    }
    fn abort_failure(&mut self, id: Uuid, error: anyhow::Error) -> anyhow::Error {
        let cleanup = self.cancel(id);
        let entry = self.entries.get_mut(&id).unwrap();
        if entry.active.is_none() {
            entry.status.state = TransferState::Failed;
        }
        entry.status.error = Some(format!("{error:#}"));
        match cleanup {
            Ok(_) => error,
            Err(cleanup) => {
                let detail = format!("{error:#}; cleanup also failed: {cleanup:#}");
                entry.status.error = Some(detail.clone());
                error.context(detail)
            }
        }
    }
    /// Revoke new work and cancel unfinished transfers; receipts remain queryable.
    /// Call explicitly on teardown so cleanup errors can be returned to the owner.
    pub fn shutdown(&mut self) -> Result<()> {
        self.closed = true;
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| e.active.is_some())
            .map(|(id, _)| *id)
            .collect();
        let mut errors = Vec::new();
        for id in ids {
            if let Err(e) = self.cancel(id) {
                errors.push(format!("{id}: {e:#}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(fail(ErrorCode::Io, errors.join("; ")))
        }
    }
}
impl Drop for TransferManager {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("file transfer cleanup failed: {error:#}");
        }
    }
}

pub fn validate_basename(name: &str) -> Result<()> {
    if name.is_empty()
        || name.encode_utf16().count() > 255
        || name == "."
        || name == ".."
        || name.eq_ignore_ascii_case(".arterm-partial")
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c <= '\u{1f}' || "<>:\"/\\|?*".contains(c))
    {
        return Err(fail(ErrorCode::InvalidPath, "unsafe filename component"));
    }
    let stem = name
        .split('.')
        .next()
        .unwrap()
        .trim_end_matches(' ')
        .to_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(*prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2"
                    | "3"
                    | "4"
                    | "5"
                    | "6"
                    | "7"
                    | "8"
                    | "9"
                    | "\u{b9}"
                    | "\u{b2}"
                    | "\u{b3}"
            )
        })
    });
    if device {
        return Err(fail(ErrorCode::PathPolicyDenied, "Windows device filename"));
    }
    Ok(())
}

/// Remote sources and managed roots must be local, drive-qualified absolute paths.
/// Caller-relative source resolution belongs to the CLI, not this module.
pub fn validate_absolute_path(path: &Path) -> Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| fail(ErrorCode::InvalidPath, "path is not Unicode"))?;
    let bytes = text.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || text.contains('/')
        || text.encode_utf16().count() > 32_000
    {
        return Err(fail(
            ErrorCode::InvalidPath,
            "require a drive-qualified absolute Windows path",
        ));
    }
    if text.len() > 3 {
        for part in text[3..].split('\\') {
            validate_basename(part)?;
        }
    }
    Ok(())
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
/// Adds the Win32 long-path prefix only after applying the shared path policy.
pub fn extended_path(path: &Path) -> Result<PathBuf> {
    validate_absolute_path(path)?;
    let mut extended = std::ffi::OsString::from(r"\\?\");
    extended.push(path.as_os_str());
    Ok(PathBuf::from(extended))
}
/// Identity of an open non-reparse object, not an ownership proof by itself.
/// Cleanup must also stay within a pinned, caller-owned staging scope.
pub(crate) fn handle_identity(file: &File) -> Result<(u32, u64)> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(io_error(
            "inspect path identity",
            std::io::Error::last_os_error(),
        ));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(fail(ErrorCode::PathPolicyDenied, "reparse path identity"));
    }
    Ok((
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}
pub fn verify_path_identity(file: &File, requested: &Path) -> Result<()> {
    // Ask the filesystem which object the spelling denotes, including Unicode
    // casing and case-sensitive directories, rather than approximating its rules.
    let resolved = OpenOptions::new()
        .read(true)
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(extended_path(requested)?)
        .map_err(|e| io_error("verify requested path identity", e))?;
    if handle_identity(file)? != handle_identity(&resolved)? {
        return Err(fail(
            ErrorCode::PathPolicyDenied,
            "requested path denotes a different object",
        ));
    }
    Ok(())
}
/// Pins this component only. Use `pin_directories` for an untrusted full path.
pub fn pin_directory(path: &Path) -> Result<File> {
    pin_directory_access(path, FILE_READ_ATTRIBUTES)
}
fn pin_directory_access(path: &Path, access: u32) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .access_mode(access)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(extended_path(path)?)
        .map_err(|e| io_error("pin directory (write/delete sharing denied)", e))?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(io_error(
            "inspect directory",
            std::io::Error::last_os_error(),
        ));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !file
            .metadata()
            .map_err(|e| io_error("inspect directory type", e))?
            .is_dir()
    {
        return Err(fail(
            ErrorCode::PathPolicyDenied,
            "directory is a reparse point or not a directory",
        ));
    }
    verify_path_identity(&file, path)?;
    Ok(file)
}
/// Retain every returned handle for the entire protected operation.
pub fn pin_directories(path: &Path) -> Result<Vec<File>> {
    validate_absolute_path(path)?;
    let text = path.to_str().unwrap();
    let drive = Path::new(&text[..3]);
    // Fixed/removable/RAM disks only. Unknown, remote and optical roots fail closed.
    if !matches!(unsafe { GetDriveTypeW(wide(drive).as_ptr()) }, 2 | 3 | 6) {
        return Err(fail(
            ErrorCode::PathPolicyDenied,
            "not a supported local disk",
        ));
    }
    let mut current = drive.to_path_buf();
    let mut pins = vec![pin_directory(&current)?];
    if text.len() > 3 {
        for component in text[3..].split('\\') {
            current.push(component);
            pins.push(pin_directory(&current)?);
        }
    }
    Ok(pins)
}
/// Regular-file identity/size/mtime snapshot; fields intentionally stay opaque.
pub(crate) fn fingerprint(file: &File) -> Result<Fingerprint> {
    let handle = file.as_raw_handle() as HANDLE;
    if unsafe { GetFileType(handle) } != FILE_TYPE_DISK
        || !file
            .metadata()
            .map_err(|e| io_error("inspect file type", e))?
            .is_file()
    {
        return Err(fail(
            ErrorCode::NotRegularFile,
            "only regular disk files are supported",
        ));
    }
    object_fingerprint(file)
}
fn object_fingerprint(file: &File) -> Result<Fingerprint> {
    let handle = file.as_raw_handle() as HANDLE;
    if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
        return Err(fail(ErrorCode::NotRegularFile, "source is not a disk object"));
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io_error(
            "inspect file identity",
            std::io::Error::last_os_error(),
        ));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(fail(ErrorCode::PathPolicyDenied, "reparse source"));
    }
    Ok(Fingerprint {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        modified: (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
            | u64::from(info.ftLastWriteTime.dwLowDateTime),
    })
}
pub fn actual_path(file: &File) -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32_768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle() as HANDLE,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(io_error(
            "resolve actual file path",
            std::io::Error::last_os_error(),
        ));
    }
    let name = String::from_utf16(&buffer[..length]).context("invalid actual Windows path")?;
    let path = PathBuf::from(name.strip_prefix(r"\\?\").unwrap_or(&name));
    validate_absolute_path(&path)?;
    Ok(path)
}
fn rename_information(target: &Path) -> Result<(Vec<usize>, u32)> {
    // Include an explicit NUL even when the filename exactly fills the aligned
    // allocation. FileNameLength excludes it; the API buffer length includes it.
    let name = wide(&extended_path(target)?);
    let bytes = std::mem::offset_of!(FILE_RENAME_INFO, FileName) + name.len() * 2;
    let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).Anonymous.ReplaceIfExists = 0;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = ((name.len() - 1) * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
    }
    Ok((storage, bytes as u32))
}
/// The caller must own a DELETE-capable handle, pin the destination ancestors,
/// and hold its final authorization guard. Does not flush or perform extraction.
pub fn rename_no_replace(file: &File, target: &Path) -> Result<()> {
    let (storage, bytes) = rename_information(target)?;
    unsafe {
        if SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileRenameInfo,
            storage.as_ptr().cast(),
            bytes,
        ) == 0
        {
            let error = std::io::Error::last_os_error();
            return Err(if matches!(error.raw_os_error(), Some(80 | 183)) {
                fail(ErrorCode::DestinationExists, "destination already exists")
            } else {
                io_error("atomic no-replace rename", error)
            });
        }
    }
    Ok(())
}
/// Marks only the caller-owned DELETE-capable object for deletion on close.
/// Nonempty directories fail; this never recursively deletes or unpins a path.
pub(crate) fn delete_open_file(file: &File) -> Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    } == 0
    {
        return Err(io_error(
            "delete owned partial by handle",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

fn delete_owned_directory(path: &Path, identity: (u32, u64)) -> Result<()> {
    let directory = pin_directory_access(path, FILE_READ_ATTRIBUTES | DELETE_ACCESS)?;
    if handle_identity(&directory)? != identity {
        return Err(fail(ErrorCode::PathPolicyDenied, "owned directory was substituted; retained"));
    }
    delete_open_file(&directory)
}
fn cleanup_empty_directory(path: &Path, identity: (u32, u64), error: anyhow::Error) -> anyhow::Error {
    match delete_owned_directory(path, identity) {
        Ok(()) => error,
        Err(cleanup) => error.context(format!("empty directory cleanup also failed: {cleanup}")),
    }
}

/// Creates exactly one new ACL-protected directory; never opens an existing one.
/// Pin the parent chain first and retain it until all writes/publication finish.
pub fn create_private_directory(path: &Path) -> Result<()> {
    let path = extended_path(path)?;
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io_error(
            "open current user token",
            std::io::Error::last_os_error(),
        ));
    }
    let result = (|| -> Result<String> {
        let mut needed = 0;
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(io_error(
                "size current user token",
                std::io::Error::last_os_error(),
            ));
        }
        let mut data = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                data.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(io_error(
                "read current user token",
                std::io::Error::last_os_error(),
            ));
        }
        let user = unsafe { &*data.as_ptr().cast::<TOKEN_USER>() };
        let mut sid = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
            return Err(io_error(
                "format current user SID",
                std::io::Error::last_os_error(),
            ));
        }
        let mut length = 0;
        unsafe {
            while *sid.add(length) != 0 {
                length += 1;
            }
        }
        let value = unsafe { String::from_utf16(std::slice::from_raw_parts(sid, length)) };
        unsafe {
            LocalFree(sid.cast());
        }
        Ok(value?)
    })();
    unsafe {
        CloseHandle(token);
    }
    let sid = result?;
    let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io_error(
            "create private directory ACL",
            std::io::Error::last_os_error(),
        ));
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let ok = unsafe { CreateDirectoryW(wide(&path).as_ptr(), &attributes) };
    let error = std::io::Error::last_os_error();
    unsafe {
        LocalFree(descriptor);
    }
    if ok == 0 {
        return Err(io_error("create private transfer directory", error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Seek, SeekFrom};

    #[test]
    fn directory_cleanup_rejects_a_substituted_empty_object() {
        let root = std::env::temp_dir().join(format!("arterm-owned-cleanup-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        {
            let _parents = pin_directories(&root).unwrap();
            let original = root.join("stage");
            let moved = root.join("moved");
            create_private_directory(&original).unwrap();
            let pin = pin_directory(&original).unwrap();
            let identity = handle_identity(&pin).unwrap();
            drop(pin);
            fs::rename(&original, &moved).unwrap();
            fs::create_dir(&original).unwrap();
            let error = delete_owned_directory(&original, identity).unwrap_err();
            assert_eq!(error.downcast_ref::<TransferError>().unwrap().code, ErrorCode::PathPolicyDenied);
            assert!(original.is_dir() && moved.is_dir());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rename_information_includes_nul_when_filename_consumes_alignment_padding() {
        let mut exercised_no_padding = false;
        for length in 1..=16 {
            let target = PathBuf::from(format!(r"C:\transfer\{}.bin", "x".repeat(length)));
            let expected = wide(&extended_path(&target).unwrap());
            let (storage, bytes) = rename_information(&target).unwrap();
            let info = unsafe { &*storage.as_ptr().cast::<FILE_RENAME_INFO>() };
            let filename_units = info.FileNameLength as usize / 2;
            let end = std::mem::offset_of!(FILE_RENAME_INFO, FileName) + filename_units * 2;
            exercised_no_padding |= end % std::mem::size_of::<usize>() == 0;
            assert_eq!(filename_units + 1, expected.len());
            assert_eq!(
                bytes as usize,
                end + 2,
                "NUL must be inside the declared API buffer"
            );
            assert!(storage.len() * std::mem::size_of::<usize>() >= end + 2);
            let actual =
                unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), filename_units + 1) };
            assert_eq!(actual, expected);
            assert_eq!(actual[filename_units], 0);
        }
        assert!(
            exercised_no_padding,
            "must cover the original missing-terminator boundary"
        );
    }

    #[test]
    fn path_identity_rejects_different_files_and_directories() {
        let root =
            std::env::temp_dir().join(format!("arterm-transfer-identity-test-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("file.bin"), b"same bytes").unwrap();
        fs::write(second.join("file.bin"), b"same bytes").unwrap();
        {
            let directory = pin_directory(&first).unwrap();
            let file = File::open(first.join("file.bin")).unwrap();
            for error in [
                verify_path_identity(&directory, &second).unwrap_err(),
                verify_path_identity(&file, &second.join("file.bin")).unwrap_err(),
            ] {
                assert_eq!(
                    error.downcast_ref::<TransferError>().unwrap().code,
                    ErrorCode::PathPolicyDenied
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_source_and_unexpected_eof_fail_even_if_sharing_is_bypassed() {
        let root =
            std::env::temp_dir().join(format!("arterm-transfer-fault-test-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.bin");
        {
            let mut manager = TransferManager::new(
                AuthorizedSession::after_authorization(Uuid::now_v7()),
                &root,
                Limits::default(),
            )
            .unwrap();
            for truncate in [true, false] {
                fs::write(&source, b"abcdef").unwrap();
                let status = manager.begin_download(&source).unwrap();
                let Some(Active::Download(download)) =
                    &mut manager.entries.get_mut(&status.transfer_id).unwrap().active
                else {
                    unreachable!()
                };
                if truncate {
                    // Fault injection: deliberately relax only this test handle's
                    // sharing to exercise defense beyond the production write lock.
                    download.file = File::open(&source).unwrap();
                    fs::write(&source, b"a").unwrap();
                } else {
                    download.file.seek(SeekFrom::End(0)).unwrap();
                }
                let error = manager.read_chunk(status.transfer_id, 0, 6).unwrap_err();
                assert_eq!(
                    error.downcast_ref::<TransferError>().unwrap().code,
                    ErrorCode::SourceChanged
                );
                assert_eq!(
                    manager.status(status.transfer_id).unwrap().state,
                    TransferState::Failed
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
}
