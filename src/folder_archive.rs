//! Explicit directory wrappers for ordinary file transfer. No authorization or RPC.
//! Callers run this blocking work outside terminal/global locks and supply a
//! cancellation callback covering session, lease, and deadline validity.
//! Extraction additionally requires a final authorization-guard factory; the
//! returned guard is held only across the atomic rename, never extraction/cleanup.
//!
//! Contents (including hidden/system files and empty directories) are preserved,
//! not ACLs, timestamps, attributes, hard-link relationships, or alternate streams.
//! Packing is not an atomic filesystem snapshot: source pins and a final identity/
//! membership audit reject observable changes while preparing the ZIP. Successful
//! preparation releases the entire original tree; the immutable ZIP becomes the
//! transfer snapshot. Later edits to the original tree do not invalidate it.
//! `verify_payload` checks the pinned ZIP and caller authorization/cancellation,
//! never rereads the original tree. Single-file streaming policy is unchanged.
//! Only explicit directory metadata should invoke extraction; a regular .zip file
//! remains an ordinary file. Guarded production entry points preserve the exact
//! basename and fail on collision. The convenience `extract_archive` may append a
//! UUID on collision. Completed folders outlive the archive/session.
//!
//! Limits: 8GiB ZIP and actual uncompressed bytes, 10,000 entries including implied
//! parents, 1,024 UTF-16 relative-path units, depth 64, and 16MiB central directory.
//! Data buffers are 64KiB; metadata is bounded by those limits. Deflate is pure Rust.
//! Raw central records are bounded and checked before the ZIP library allocates
//! its index. Non-ASCII legacy names and Unicode-path overrides are unsupported.
//! Cleanup only deletes recorded object identities by handle; unexpected children
//! or substitutions are retained and reported, never recursively swept away.
//! ZIP CRC plus the ordinary transfer's integrity check protect against corruption,
//! not an untrusted sender. No shell tools, authority tokens, or RPC live here.
use crate::file_transfer::{
    create_private_directory, delete_open_file, extended_path, handle_identity, pin_directories,
    pin_directory, pin_source, rename_no_replace, validate_basename, verify_path_identity,
    PinnedSource,
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

pub const MAX_ENTRIES: usize = 10_000;
pub const MAX_PATH_UNITS: usize = 1024;
pub const MAX_DEPTH: usize = 64;
pub const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_UNCOMPRESSED_BYTES: u64 = MAX_ARCHIVE_BYTES;
pub const MAX_CENTRAL_BYTES: u64 = 16 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const DELETE_ACCESS: u32 = 0x0001_0000;

/// Owns a private temporary ZIP. Keep alive until ordinary file transfer finishes.
pub struct PreparedArchive {
    // Drop the ZIP pin before Stage's owned-object cleanup.
    payload: Option<PinnedSource>,
    stage: Stage,
    path: PathBuf,
    basename: String,
    entries: usize,
    bytes: u64,
}
impl PreparedArchive {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn original_basename(&self) -> &str {
        &self.basename
    }
    pub fn entry_count(&self) -> usize {
        self.entries
    }
    pub fn uncompressed_bytes(&self) -> u64 {
        self.bytes
    }
    /// Compatibility with the core adapter trait: the source being transferred
    /// is now the prepared ZIP, not the original directory.
    pub fn verify_sources(&self, mut check: impl FnMut() -> Result<()>) -> Result<()> {
        check()?;
        self.payload
            .as_ref()
            .context("archive payload already released")?
            .verify_unchanged()?;
        check()
    }
    /// Invoke through transfer completion, alongside normal authorization checks.
    pub fn verify_payload(&self, check: &dyn Fn() -> Result<()>) -> Result<()> {
        self.verify_sources(check)
    }
    /// Explicit cleanup reports failures; Drop is a best-effort logged backstop.
    pub fn cleanup(mut self) -> Result<()> {
        self.payload.take();
        self.stage.cleanup()
    }
}

struct SourceSnapshot {
    source: PinnedSource,
    children: Option<Vec<String>>,
}

fn directory_children(path: &Path, check: &mut impl FnMut() -> Result<()>) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(extended_path(path)?)? {
        check()?;
        ensure!(names.len() < MAX_ENTRIES, "archive directory entry limit");
        names.push(
            entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-Unicode filename"))?,
        );
    }
    names.sort();
    Ok(names)
}

fn verify_sources(
    sources: &[SourceSnapshot],
    check: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    for snapshot in sources {
        check()?;
        snapshot.source.verify_unchanged()?;
        if let Some(children) = &snapshot.children {
            ensure!(
                *children == directory_children(snapshot.source.path(), check)?,
                "SourceChanged: directory membership changed"
            );
            snapshot.source.verify_unchanged()?;
        }
    }
    check()
}

#[derive(Debug)]
pub struct PublishedFolder {
    pub path: PathBuf,
    pub entry_count: usize,
    pub uncompressed_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ArchiveSummary {
    pub entry_count: usize,
    pub uncompressed_bytes: u64,
}

struct Stage {
    path: PathBuf,
    handle: Option<File>,
    _parents: Vec<File>,
    owned: Vec<OwnedObject>,
}
struct OwnedObject {
    path: PathBuf,
    identity: (u32, u64),
    pin: Option<File>,
}
impl Stage {
    fn new(parent: &Path) -> Result<Self> {
        let parents = pin_directories(parent)?;
        let path = parent.join(format!("arterm-archive-{}", Uuid::now_v7()));
        create_private_directory(&path)?;
        let handle = match directory_for_rename(&path) {
            Ok(handle) => handle,
            Err(error) => {
                return Err(error.context(
                    "new stage retained: ownership handle unavailable; unsafe path cleanup refused",
                ));
            }
        };
        Ok(Self {
            path,
            handle: Some(handle),
            _parents: parents,
            owned: Vec::new(),
        })
    }
    fn track(&mut self, path: &Path, created: &File) -> Result<()> {
        let identity = OpenOptions::new()
            .read(true)
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(extended_path(path)?)?;
        verify_path_identity(created, path)?;
        verify_path_identity(&identity, path)?;
        self.owned.push(OwnedObject {
            path: path.to_owned(),
            identity: handle_identity(&identity)?,
            pin: Some(identity),
        });
        Ok(())
    }
    fn create_directory(&mut self, path: &Path) -> Result<()> {
        match fs::create_dir(extended_path(path)?) {
            Ok(()) => {
                let handle = directory_for_rename(path)?;
                self.track(path, &handle)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let owned = self
                    .owned
                    .iter()
                    .find(|owned| owned.path == path)
                    .context("unexpected unowned directory in archive stage")?;
                ensure!(
                    handle_identity(&pin_directory(path)?)? == owned.identity,
                    "owned directory identity changed"
                );
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
    fn forget_published(&mut self, payload: &Path) {
        self.owned.retain(|owned| !owned.path.starts_with(payload));
    }
    fn release_publication_handles(&mut self, payload: &Path) {
        for owned in &mut self.owned {
            if owned.path.starts_with(payload) {
                owned.pin.take();
            }
        }
    }
    fn cleanup(&mut self) -> Result<()> {
        if let Some(handle) = &self.handle {
            verify_path_identity(handle, &self.path)?;
            while let Some(owned) = self.owned.last() {
                // Deny concurrent replacement before comparing the recorded object.
                // Deletion is by this handle, never a recursive path operation.
                let deletion = OpenOptions::new()
                    .read(true)
                    .access_mode(DELETE_ACCESS | FILE_READ_ATTRIBUTES)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(extended_path(&owned.path)?)?;
                ensure!(
                    handle_identity(&deletion)? == owned.identity,
                    "owned cleanup object was substituted"
                );
                delete_open_file(&deletion)?;
                self.owned.pop();
                drop(deletion);
            }
            // Unexpected children deliberately cause a nonempty-stage error.
            delete_open_file(handle)?;
            self.handle.take();
        }
        Ok(())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            crate::statusln!("archive stage cleanup failed: {error:#}");
        }
    }
}

fn directory_for_rename(path: &Path) -> Result<File> {
    let handle = OpenOptions::new()
        .read(true)
        .access_mode(DELETE_ACCESS | 0x80)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(extended_path(path)?)?;
    ensure!(handle.metadata()?.is_dir(), "not a directory");
    verify_path_identity(&handle, path)?;
    Ok(handle)
}
// Includes implicit parents: prevents a/A, file/child, and duplicate entries
// independently of the destination volume's case-sensitivity setting.
#[derive(Default)]
struct Names(HashMap<String, (String, bool, bool)>);
impl Names {
    fn insert(&mut self, name: &str, directory: bool) -> Result<()> {
        ensure!(!name.contains('\\'), "backslash ZIP entry");
        let parts: Vec<_> = name.split('/').collect();
        ensure!(
            parts.len() <= MAX_DEPTH && name.encode_utf16().count() <= MAX_PATH_UNITS,
            "archive path limit"
        );
        let mut path = String::new();
        for (i, part) in parts.iter().enumerate() {
            validate_basename(part)?;
            if i != 0 {
                path.push('/');
            }
            path.push_str(part);
            let explicit = i + 1 == parts.len();
            let is_dir = !explicit || directory;
            let key = path.to_uppercase();
            if let Some((spelling, prior_explicit, prior_dir)) = self.0.get_mut(&key) {
                ensure!(
                    *spelling == path && *prior_dir == is_dir,
                    "archive path collision"
                );
                ensure!(!explicit || !*prior_explicit, "duplicate archive entry");
                *prior_explicit |= explicit;
            } else {
                ensure!(self.0.len() < MAX_ENTRIES, "archive expanded entry limit");
                self.0.insert(key, (path.clone(), explicit, is_dir));
            }
        }
        Ok(())
    }
}

pub fn pack_directory(
    source: &Path,
    temp_parent: &Path,
    mut check_cancel: impl FnMut() -> Result<()>,
) -> Result<PreparedArchive> {
    check_cancel()?;
    let source = pin_source(source)?;
    ensure!(source.is_directory(), "archive source is not a directory");
    let temp = pin_source(temp_parent)?;
    ensure!(
        temp.is_directory(),
        "archive temporary parent is not a directory"
    );
    let basename = source
        .path()
        .file_name()
        .and_then(|s| s.to_str())
        .context("folder basename")?
        .to_owned();
    validate_basename(&basename)?;
    // A staging tree inside the source would archive itself.
    let source_text = source
        .path()
        .to_str()
        .context("source Unicode")?
        .to_uppercase();
    let temp_text = temp
        .path()
        .to_str()
        .context("temporary parent Unicode")?
        .to_uppercase();
    ensure!(
        temp_text != source_text && !temp_text.starts_with(&(source_text + "\\")),
        "temporary parent must be outside source"
    );
    let mut stage = Stage::new(temp_parent)?;
    let path = stage.path.join("contents.zip");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ)
        .open(extended_path(&path)?)?;
    stage.track(&path, &file)?;
    let mut writer = ZipWriter::new(file);
    let mut state = PackState {
        entries: 0,
        bytes: 0,
        names: Names::default(),
        observed: Vec::new(),
        check: &mut check_cancel,
    };
    pack_tree(source, "", &mut writer, &mut state)?;
    verify_sources(&state.observed, state.check)?;
    (state.check)()?;
    let mut file = writer.finish()?;
    file.sync_all()?;
    ensure!(
        file.metadata()?.len() <= MAX_ARCHIVE_BYTES,
        "compressed archive exceeds transfer limit"
    );
    (state.check)()?;
    preflight(&mut file, state.check)?;
    drop(file);
    verify_sources(&state.observed, state.check)?;
    let payload = pin_source(&path)?;
    state.observed.clear();
    Ok(PreparedArchive {
        payload: Some(payload),
        stage,
        path,
        basename,
        entries: state.entries,
        bytes: state.bytes,
    })
}

struct PackState<'a, F> {
    entries: usize,
    bytes: u64,
    names: Names,
    observed: Vec<SourceSnapshot>,
    check: &'a mut F,
}
fn pack_tree<F: FnMut() -> Result<()>>(
    source: PinnedSource,
    prefix: &str,
    writer: &mut ZipWriter<File>,
    state: &mut PackState<'_, F>,
) -> Result<()> {
    let directory = source.path();
    // Retain only bounded names, not file contents.
    let mut children = Vec::new();
    for entry in fs::read_dir(extended_path(directory)?)? {
        (state.check)()?;
        ensure!(
            children.len() + state.entries < MAX_ENTRIES,
            "archive entry limit"
        );
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-Unicode filename"))?;
        children.push(name);
    }
    children.sort();
    for name in &children {
        (state.check)()?;
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = directory.join(name);
        let child = pin_source(&path)?;
        state.entries += 1;
        ensure!(state.entries <= MAX_ENTRIES, "archive entry limit");
        state.names.insert(&relative, child.is_directory())?;
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .large_file(true);
        if child.is_directory() {
            writer.add_directory(format!("{relative}/"), options)?;
            pack_tree(child, &relative, writer, state)?;
        } else {
            let mut file = child.file();
            writer.start_file(&relative, options)?;
            let mut buffer = [0u8; CHUNK];
            loop {
                (state.check)()?;
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                state.bytes += count as u64;
                ensure!(
                    state.bytes <= MAX_UNCOMPRESSED_BYTES,
                    "uncompressed archive limit"
                );
                writer.write_all(&buffer[..count])?;
            }
            child.verify_unchanged()?;
            state.observed.push(SourceSnapshot {
                source: child,
                children: None,
            });
        }
    }
    let after = directory_children(directory, state.check)?;
    source.verify_unchanged()?;
    ensure!(
        children == after,
        "SourceChanged: directory changed while packing"
    );
    state.observed.push(SourceSnapshot {
        source,
        children: Some(children),
    });
    Ok(())
}

// Bound central-directory allocation before ZipArchive parses untrusted input.
// ZIP64 is needed for the 8GiB transfer limit; split archives are not supported.
fn preflight(file: &mut File, check: &mut impl FnMut() -> Result<()>) -> Result<()> {
    let length = file.metadata()?.len();
    ensure!(length <= MAX_ARCHIVE_BYTES, "compressed archive limit");
    let tail_len = length.min(65_557) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail)?;
    let offset = (0..tail_len.saturating_sub(21))
        .rev()
        .find(|&i| {
            tail[i..].starts_with(b"PK\x05\x06")
                && i + 22 + u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize <= tail_len
        })
        .context("missing ZIP end record")?;
    // The decoder accepts trailing bytes. Select its newest plausible end
    // record first, then reject trailing/embedded records instead of validating
    // an older directory that the decoder would never use.
    ensure!(
        offset + 22 + u16::from_le_bytes([tail[offset + 20], tail[offset + 21]]) as usize == tail_len,
        "ambiguous ZIP end record or trailing bytes"
    );
    let end = &tail[offset..];
    let u16at = |i| u16::from_le_bytes([end[i], end[i + 1]]);
    let u32at = |i| u32::from_le_bytes(end[i..i + 4].try_into().unwrap());
    ensure!(
        u16at(4) == 0 && u16at(6) == 0 && u16at(8) == u16at(10),
        "split ZIP unsupported"
    );
    let mut entries = u16at(10) as u64;
    let mut central_bytes = u32at(12) as u64;
    let mut central_offset = u32at(16) as u64;
    let mut central_end = length - tail_len as u64 + offset as u64;
    if entries == 0xffff || central_bytes == 0xffff_ffff || central_offset == 0xffff_ffff {
        let end_offset = length - tail_len as u64 + offset as u64;
        ensure!(end_offset >= 20, "missing ZIP64 locator");
        file.seek(SeekFrom::Start(end_offset - 20))?;
        let mut locator = [0; 20];
        file.read_exact(&mut locator)?;
        ensure!(
            &locator[..4] == b"PK\x06\x07"
                && locator[4..8] == [0; 4]
                && locator[16..20] == [1, 0, 0, 0],
            "invalid ZIP64 locator"
        );
        let record_offset = u64::from_le_bytes(locator[8..16].try_into().unwrap());
        ensure!(
            record_offset <= end_offset.saturating_sub(76),
            "invalid ZIP64 offset"
        );
        file.seek(SeekFrom::Start(record_offset))?;
        let mut record = [0; 56];
        file.read_exact(&mut record)?;
        ensure!(
            &record[..4] == b"PK\x06\x06" && record[16..24] == [0; 8],
            "invalid ZIP64 record"
        );
        let at = |i| u64::from_le_bytes(record[i..i + 8].try_into().unwrap());
        ensure!(
            at(4) == 44 && record_offset + 56 == end_offset - 20,
            "ZIP64 extensible end records unsupported"
        );
        ensure!(at(24) == at(32), "split ZIP64 unsupported");
        entries = at(32);
        central_bytes = at(40);
        central_offset = at(48);
        central_end = record_offset;
    }
    ensure!(
        entries <= MAX_ENTRIES as u64
            && central_bytes <= MAX_CENTRAL_BYTES
            && central_offset
                .checked_add(central_bytes)
                .is_some_and(|n| n <= length),
        "archive central directory limit"
    );
    ensure!(
        central_offset <= central_end,
        "invalid central directory offset"
    );
    file.seek(SeekFrom::Start(central_offset))?;
    let mut names = Names::default();
    let mut consumed = 0u64;
    let mut count = 0u64;
    while file.stream_position()? < central_end {
        check()?;
        ensure!(count < MAX_ENTRIES as u64, "actual central entry limit");
        let mut header = [0u8; 46];
        ensure!(
            central_end - file.stream_position()? >= 46,
            "truncated central record"
        );
        file.read_exact(&mut header)?;
        ensure!(&header[..4] == b"PK\x01\x02", "invalid central record");
        let h16 = |i| u16::from_le_bytes(header[i..i + 2].try_into().unwrap());
        let h32 = |i| u32::from_le_bytes(header[i..i + 4].try_into().unwrap());
        let name_len = h16(28) as usize;
        let extra_len = h16(30) as usize;
        let comment_len = h16(32) as u64;
        let record_bytes = 46 + name_len as u64 + extra_len as u64 + comment_len;
        consumed = consumed
            .checked_add(record_bytes)
            .context("central size overflow")?;
        ensure!(
            consumed <= MAX_CENTRAL_BYTES && central_offset + consumed <= central_end,
            "actual central directory byte limit"
        );
        ensure!(
            name_len <= MAX_PATH_UNITS * 4 && h16(34) == 0,
            "central name/disk limit"
        );
        ensure!(h16(8) & 1 == 0, "encrypted archive unsupported");
        let mut name = vec![0; name_len];
        file.read_exact(&mut name)?;
        ensure!(
            h16(8) & 0x800 != 0 || name.is_ascii(),
            "legacy non-ASCII ZIP names unsupported"
        );
        let name_text = std::str::from_utf8(&name).context("non-UTF8 ZIP path")?;
        let directory = name_text.ends_with('/');
        names.insert(
            if directory {
                &name_text[..name_text.len() - 1]
            } else {
                name_text
            },
            directory,
        )?;
        let mode = (h32(38) >> 16) & 0o170000;
        ensure!(
            mode == 0 || mode == if directory { 0o040000 } else { 0o100000 },
            "archive link/special entry"
        );
        let mut extra = vec![0; extra_len];
        file.read_exact(&mut extra)?;
        let zip64 = checked_extra(&extra)?;
        let mut local_offset = h32(42) as u64;
        if local_offset == u32::MAX as u64 {
            let data = zip64.context("missing ZIP64 local offset")?;
            let skip = usize::from(h32(24) == u32::MAX) * 8 + usize::from(h32(20) == u32::MAX) * 8;
            ensure!(data.len() >= skip + 8, "truncated ZIP64 local offset");
            local_offset = u64::from_le_bytes(data[skip..skip + 8].try_into().unwrap());
        }
        let next = central_offset + consumed;
        // The decoder may consult local extra fields. Reject name overrides there too.
        ensure!(
            local_offset
                .checked_add(30)
                .is_some_and(|n| n <= central_offset),
            "invalid local header offset"
        );
        file.seek(SeekFrom::Start(local_offset))?;
        let mut local = [0; 30];
        file.read_exact(&mut local)?;
        ensure!(&local[..4] == b"PK\x03\x04", "invalid local header");
        let l16 = |i| u16::from_le_bytes(local[i..i + 2].try_into().unwrap());
        ensure!(
            l16(6) == h16(8) && l16(8) == h16(10),
            "local/central flags mismatch"
        );
        let local_name_len = l16(26) as usize;
        let local_extra_len = l16(28) as usize;
        ensure!(
            local_name_len == name_len
                && local_offset + 30 + local_name_len as u64 + local_extra_len as u64
                    <= central_offset,
            "local header length limit"
        );
        let mut local_name = vec![0; local_name_len];
        file.read_exact(&mut local_name)?;
        ensure!(local_name == name, "local/central name mismatch");
        let mut local_extra = vec![0; local_extra_len];
        file.read_exact(&mut local_extra)?;
        checked_extra(&local_extra)?;
        file.seek(SeekFrom::Start(next))?;
        count += 1;
    }
    ensure!(
        count == entries && consumed == central_bytes && central_offset + consumed == central_end,
        "central directory size/count mismatch"
    );
    file.rewind()?;
    Ok(())
}

fn checked_extra(mut extra: &[u8]) -> Result<Option<&[u8]>> {
    let mut zip64 = None;
    while !extra.is_empty() {
        ensure!(extra.len() >= 4, "truncated ZIP extra field");
        let kind = u16::from_le_bytes(extra[..2].try_into().unwrap());
        let length = u16::from_le_bytes(extra[2..4].try_into().unwrap()) as usize;
        ensure!(extra.len() >= 4 + length, "truncated ZIP extra field data");
        ensure!(kind != 0x7075, "Unicode path overrides unsupported");
        if kind == 1 {
            ensure!(zip64.is_none(), "duplicate ZIP64 extra field");
            zip64 = Some(&extra[4..4 + length]);
        }
        extra = &extra[4 + length..];
    }
    Ok(zip64)
}

pub fn extract_archive<G>(
    zip: &Path,
    parent: &Path,
    original_basename: &str,
    mut check_cancel: impl FnMut() -> Result<()>,
    authorize_publish: impl FnOnce() -> Result<G>,
) -> Result<PublishedFolder> {
    extract_with_limit(
        zip,
        parent,
        original_basename,
        &mut check_cancel,
        MAX_UNCOMPRESSED_BYTES,
        false,
        |_| authorize_publish(),
    )
}
/// Strict production publication: preserve the exact basename under the caller's
/// private parent. Existing destinations fail; no alternate completion is chosen.
pub fn extract_archive_guarded<G>(
    zip: &Path,
    parent: &Path,
    original_basename: &str,
    mut check_cancel: impl FnMut() -> Result<()>,
    authorize_publish: impl FnOnce() -> Result<G>,
) -> Result<PublishedFolder> {
    extract_with_limit(
        zip,
        parent,
        original_basename,
        &mut check_cancel,
        MAX_UNCOMPRESSED_BYTES,
        true,
        |_| authorize_publish(),
    )
}
/// Enforces the caller's expansion budget while streaming. The final factory
/// receives actual totals and must atomically admit/reserve retained quota under
/// its returned publication guard; rejection leaves no published directory.
/// Like `extract_archive_guarded`, this requires the exact basename to be free.
pub fn extract_archive_with_limit<G>(
    zip: &Path,
    parent: &Path,
    original_basename: &str,
    max_uncompressed_bytes: u64,
    mut check_cancel: impl FnMut() -> Result<()>,
    authorize_publish: impl FnOnce(&ArchiveSummary) -> Result<G>,
) -> Result<PublishedFolder> {
    extract_with_limit(
        zip,
        parent,
        original_basename,
        &mut check_cancel,
        max_uncompressed_bytes.min(MAX_UNCOMPRESSED_BYTES),
        true,
        authorize_publish,
    )
}
fn extract_with_limit<G>(
    zip: &Path,
    parent: &Path,
    original_basename: &str,
    check_cancel: &mut impl FnMut() -> Result<()>,
    byte_limit: u64,
    exact_basename: bool,
    authorize_publish: impl FnOnce(&ArchiveSummary) -> Result<G>,
) -> Result<PublishedFolder> {
    extract_with_metadata_impl(zip, parent, original_basename, check_cancel,
        byte_limit, exact_basename, None, authorize_publish)
}

/// Applies recipient metadata only to newly created regular staging files while
/// their identity handles remain pinned, before the final publication guard.
pub fn extract_archive_with_metadata<G>(
    zip: &Path,
    parent: &Path,
    original_basename: &str,
    max_uncompressed_bytes: u64,
    mut check_cancel: impl FnMut() -> Result<()>,
    metadata: &mut dyn FnMut(&File) -> Result<()>,
    authorize_publish: impl FnOnce(&ArchiveSummary) -> Result<G>,
) -> Result<PublishedFolder> {
    extract_with_metadata_impl(zip, parent, original_basename, &mut check_cancel,
        max_uncompressed_bytes.min(MAX_UNCOMPRESSED_BYTES), true, Some(metadata), authorize_publish)
}

fn extract_with_metadata_impl<G>(
    zip: &Path,
    parent: &Path,
    original_basename: &str,
    check_cancel: &mut impl FnMut() -> Result<()>,
    byte_limit: u64,
    exact_basename: bool,
    mut metadata: Option<&mut dyn FnMut(&File) -> Result<()>>,
    authorize_publish: impl FnOnce(&ArchiveSummary) -> Result<G>,
) -> Result<PublishedFolder> {
    check_cancel()?;
    validate_basename(original_basename)?;
    let source = pin_source(zip)?;
    ensure!(!source.is_directory(), "ZIP payload is not a regular file");
    let mut file = source.file().try_clone()?;
    preflight(&mut file, check_cancel)?;
    let mut archive = ZipArchive::new(file)?;
    ensure!(archive.len() <= MAX_ENTRIES, "archive entry limit");
    let mut stage = Stage::new(parent)?;
    let payload = stage.path.join("folder");
    create_private_directory(&payload)?;
    let payload_handle = directory_for_rename(&payload)?;
    stage.track(&payload, &payload_handle)?;
    let mut names = Names::default();
    let mut bytes = 0u64;
    for index in 0..archive.len() {
        check_cancel()?;
        let mut entry = archive.by_index(index)?;
        ensure!(!entry.encrypted(), "encrypted archive unsupported");
        let directory = entry.is_dir();
        let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
        ensure!(
            mode == 0 || mode == if directory { 0o040000 } else { 0o100000 },
            "archive link/special entry"
        );
        let raw = std::str::from_utf8(entry.name_raw()).context("non-UTF8 ZIP path")?;
        let name = if directory {
            raw.strip_suffix('/').context("invalid ZIP directory")?
        } else {
            raw
        };
        names.insert(name, directory)?;
        let target = payload.join(name.replace('/', "\\"));
        extended_path(&target)?;
        let parts: Vec<_> = name.split('/').collect();
        let mut current = payload.clone();
        let mut directory_pins = Vec::new();
        for part in &parts[..parts.len() - 1] {
            current.push(part);
            stage.create_directory(&current)?;
            directory_pins.push(pin_directory(&current)?);
        }
        if directory {
            stage.create_directory(&target)?;
        }
        let mut output = if directory {
            None
        } else {
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(FILE_SHARE_READ)
                .open(extended_path(&target)?)?;
            stage.track(&target, &file)?;
            Some(file)
        };
        let mut buffer = [0; CHUNK];
        loop {
            check_cancel()?;
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes += count as u64;
            ensure!(bytes <= byte_limit, "actual uncompressed byte limit");
            ensure!(!directory, "directory entry contains data");
            output.as_mut().unwrap().write_all(&buffer[..count])?;
        }
        if let Some(output) = output {
            output.sync_all()?;
            if let Some(metadata) = metadata.as_mut() {
                metadata(&output).context("recipient metadata failed before folder publication")?;
            }
        }
    }
    let entry_count = names.0.len();
    drop(archive);
    source.verify_unchanged()?;
    check_cancel()?;
    let mut target = parent.join(original_basename);
    match fs::symlink_metadata(extended_path(&target)?) {
        Ok(_) => {
            ensure!(!exact_basename, "destination already exists");
            target = parent.join(unique_basename(original_basename));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect publication destination"),
    }
    check_cancel()?;
    let summary = ArchiveSummary {
        entry_count,
        uncompressed_bytes: bytes,
    };
    // Windows refuses directory rename while descendant identity handles remain
    // open. Keep their volume/file IDs for exact-object cleanup if admission fails.
    stage.release_publication_handles(&payload);
    {
        let _publication_guard = authorize_publish(&summary)?;
        rename_no_replace(&payload_handle, &target)?;
    }
    stage.forget_published(&payload);
    drop(payload_handle);
    // Publication has succeeded: cleanup errors must not pretend it did not.
    if let Err(error) = stage.cleanup() {
        crate::statusln!("archive published; stage cleanup failed: {error:#}");
    }
    Ok(PublishedFolder {
        path: target,
        entry_count,
        uncompressed_bytes: bytes,
    })
}
fn unique_basename(original: &str) -> String {
    let suffix = format!("-{}", Uuid::now_v7());
    let mut prefix = String::new();
    for c in original.chars() {
        if prefix.encode_utf16().count() + c.len_utf16() + suffix.len() > 255 {
            break;
        }
        prefix.push(c);
    }
    format!("{prefix}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn byte_limits_match_core_transfer_admission() {
        assert_eq!(
            MAX_ARCHIVE_BYTES,
            crate::file_transfer::Limits::default().max_file_bytes
        );
        assert_eq!(MAX_UNCOMPRESSED_BYTES, MAX_ARCHIVE_BYTES);
    }
    #[test]
    fn fa01_cleanup_preserves_unowned_intruder_and_reports_nonempty_stage() {
        let s = Sandbox::new();
        let prepared = pack_directory(&s.p("source"), &s.p("temp"), || Ok(())).unwrap();
        let intruder = prepared.path().parent().unwrap().join("unrelated.txt");
        fs::write(&intruder, b"not owned by archive").unwrap();
        assert!(prepared.cleanup().is_err());
        assert_eq!(fs::read(intruder).unwrap(), b"not owned by archive");
    }
    #[test]
    fn cleanup_refuses_substituted_owned_filename() {
        let s = Sandbox::new();
        let mut prepared = pack_directory(&s.p("source"), &s.p("temp"), || Ok(())).unwrap();
        let path = prepared.path().to_owned();
        let moved = path.with_file_name("moved-original.zip");
        // Force substitution past the stage's normal rename-sharing protection
        // to exercise the independent object-identity cleanup defense.
        prepared.payload.take();
        prepared.stage.handle.take();
        let replacement_handle = OpenOptions::new()
            .read(true)
            .access_mode(DELETE_ACCESS | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
            .unwrap();
        rename_no_replace(&replacement_handle, &moved).unwrap();
        drop(replacement_handle);
        fs::write(&path, b"unowned replacement").unwrap();
        prepared.stage.handle = Some(directory_for_rename(&prepared.stage.path).unwrap());
        assert!(prepared.cleanup().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"unowned replacement");
        assert!(moved.is_file());
    }

    fn oversized_central_zip(s: &Sandbox, hide_size: bool) -> PathBuf {
        let path = s.p("oversized-central.zip");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        for n in 0..300 {
            writer
                .start_file(
                    format!("n{n:03}"),
                    SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
                )
                .unwrap();
        }
        writer.finish().unwrap();
        let original = fs::read(&path).unwrap();
        let end = original.len() - 22;
        let start = u32::from_le_bytes(original[end + 16..end + 20].try_into().unwrap()) as usize;
        let mut result = original[..start].to_vec();
        let mut position = start;
        for _ in 0..300 {
            let size = 46
                + u16::from_le_bytes(original[position + 28..position + 30].try_into().unwrap())
                    as usize;
            let mut record = original[position..position + size].to_vec();
            record[32..34].copy_from_slice(&60_000u16.to_le_bytes());
            result.extend(record);
            result.extend(vec![b'x'; 60_000]);
            position += size;
        }
        assert_eq!(result.len() - start, 18_015_000);
        let mut ending = original[end..].to_vec();
        let declared = if hide_size { 1 } else { 18_015_000u32 };
        ending[12..16].copy_from_slice(&declared.to_le_bytes());
        result.extend(ending);
        fs::write(&path, result).unwrap();
        path
    }

    #[test]
    fn fa02_actual_central_bytes_cannot_be_hidden_by_declared_size() {
        let s = Sandbox::new();
        let path = oversized_central_zip(&s, true);
        assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());
        s.assert_clean();
    }

    fn wrap_end_record_in_outer_comment(path: &Path) {
        let mut bytes = fs::read(path).unwrap();
        let inner = bytes[bytes.len() - 22..].to_vec();
        assert_eq!(&inner[..4], b"PK\x05\x06");
        let mut outer = [0u8; 22];
        outer[..4].copy_from_slice(b"PK\x05\x06");
        outer[16..20].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        outer[20..22].copy_from_slice(&23u16.to_le_bytes());
        bytes.extend(outer);
        bytes.extend(inner);
        bytes.push(0);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn fa05_embedded_end_record_cannot_hide_oversized_metadata() {
        let s = Sandbox::new();
        let path = oversized_central_zip(&s, false);
        wrap_end_record_in_outer_comment(&path);
        let result = super::extract_archive_guarded(&path, &s.p("out"), "folder",
            || Ok(()), || Ok(()));
        assert!(result.is_err(), "unvalidated central directory was decoded");
        s.assert_clean();
    }

    #[test]
    fn fa05_embedded_end_record_cannot_hide_duplicate_names() {
        let s = Sandbox::new();
        let path = s.zip(&[("same", b"one"), ("dame", b"two")]);
        let mut bytes = fs::read(&path).unwrap();
        let positions: Vec<_> = bytes.windows(4).enumerate()
            .filter_map(|(i, b)| (b == b"dame").then_some(i)).collect();
        assert_eq!(positions.len(), 2);
        for position in positions {
            bytes[position..position + 4].copy_from_slice(b"same");
        }
        fs::write(&path, bytes).unwrap();
        wrap_end_record_in_outer_comment(&path);
        let result = super::extract_archive_guarded(&path, &s.p("out"), "folder",
            || Ok(()), || Ok(()));
        assert!(result.is_err(), "unvalidated duplicate entries were decoded");
        s.assert_clean();
    }

    #[test]
    fn ordinary_archive_comment_keeps_one_validated_end_record() {
        let s = Sandbox::new();
        let path = s.zip(&[("file", b"payload")]);
        let mut bytes = fs::read(&path).unwrap();
        let end = bytes.len() - 22;
        let comment = b"ordinary folder archive comment";
        bytes[end + 20..end + 22].copy_from_slice(&(comment.len() as u16).to_le_bytes());
        bytes.extend(comment);
        fs::write(&path, bytes).unwrap();
        let folder = super::extract_archive_guarded(&path, &s.p("out"), "folder",
            || Ok(()), || Ok(())).unwrap();
        assert_eq!(fs::read(folder.path.join("file")).unwrap(), b"payload");
    }

    #[test]
    fn fa03_duplicate_central_names_rejected_before_library_deduplication() {
        let s = Sandbox::new();
        let path = s.zip(&[("same", b"one"), ("dame", b"two")]);
        let mut bytes = fs::read(&path).unwrap();
        let positions: Vec<_> = bytes
            .windows(4)
            .enumerate()
            .filter_map(|(i, b)| (b == b"dame").then_some(i))
            .collect();
        assert_eq!(positions.len(), 2);
        for position in positions {
            bytes[position..position + 4].copy_from_slice(b"same");
        }
        fs::write(&path, bytes).unwrap();
        assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());
        s.assert_clean();
    }
    #[test]
    fn central_unicode_path_override_and_legacy_names_rejected() {
        let s = Sandbox::new();
        let path = s.zip(&[("safe", b"data")]);
        let mut bytes = fs::read(&path).unwrap();
        let end = bytes.len() - 22;
        let start = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let old_size = u32::from_le_bytes(bytes[end + 12..end + 16].try_into().unwrap());
        assert_eq!(&bytes[start + 30..start + 32], &[0, 0]);
        bytes[start + 30..start + 32].copy_from_slice(&9u16.to_le_bytes());
        bytes.splice(start + 50..start + 50, [0x75, 0x70, 5, 0, 1, 0, 0, 0, 0]);
        bytes[end + 9 + 12..end + 9 + 16].copy_from_slice(&(old_size + 9).to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());

        let path = s.zip(&[("\u{e9}", b"data")]);
        let mut bytes = fs::read(&path).unwrap();
        let end = bytes.len() - 22;
        let start = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        bytes[7] &= !8;
        bytes[start + 9] &= !8;
        fs::write(&path, bytes).unwrap();
        assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());
        s.assert_clean();
    }

    #[test]
    fn small_zip64_end_records_pass_bounded_raw_scan() {
        let s = Sandbox::new();
        let path = s.zip(&[("file", b"zip64")]);
        let mut bytes = fs::read(&path).unwrap();
        let end = bytes.len() - 22;
        let mut eocd = bytes.split_off(end);
        let size = u32::from_le_bytes(eocd[12..16].try_into().unwrap()) as u64;
        let offset = u32::from_le_bytes(eocd[16..20].try_into().unwrap()) as u64;
        let mut record = [0u8; 56];
        record[..4].copy_from_slice(b"PK\x06\x06");
        record[4..12].copy_from_slice(&44u64.to_le_bytes());
        record[12..14].copy_from_slice(&45u16.to_le_bytes());
        record[14..16].copy_from_slice(&45u16.to_le_bytes());
        record[24..32].copy_from_slice(&1u64.to_le_bytes());
        record[32..40].copy_from_slice(&1u64.to_le_bytes());
        record[40..48].copy_from_slice(&size.to_le_bytes());
        record[48..56].copy_from_slice(&offset.to_le_bytes());
        bytes.extend(record);
        bytes.extend(b"PK\x06\x07");
        bytes.extend(0u32.to_le_bytes());
        bytes.extend((end as u64).to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        eocd[8..12].fill(0xff);
        eocd[12..20].fill(0xff);
        bytes.extend(eocd);
        fs::write(&path, bytes).unwrap();
        let result = extract_archive(&path, &s.p("out"), "folder", || Ok(())).unwrap();
        assert_eq!(fs::read(result.path.join("file")).unwrap(), b"zip64");
    }

    #[test]
    fn extracted_quota_checked_before_publication_with_actual_totals() {
        let s = Sandbox::new();
        let path = s.zip(&[("nested/file", b"12345")]);
        let admitted = std::cell::Cell::new(false);
        assert!(extract_archive_with_limit(
            &path,
            &s.p("out"),
            "folder",
            4,
            || Ok(()),
            |_| {
                admitted.set(true);
                Ok(())
            }
        )
        .is_err());
        assert!(!admitted.get());
        s.assert_clean();
        assert!(extract_archive_with_limit(
            &path,
            &s.p("out"),
            "folder",
            5,
            || Ok(()),
            |summary| -> Result<()> {
                admitted.set(true);
                assert_eq!(summary.uncompressed_bytes, 5);
                assert_eq!(summary.entry_count, 2);
                anyhow::bail!("retained quota changed concurrently")
            }
        )
        .is_err());
        assert!(admitted.get());
        s.assert_clean();
    }

    #[test]
    fn fa04_final_audit_rechecks_previously_enumerated_subdirectory() {
        let s = Sandbox::new();
        let a = s.p("source").join("a");
        fs::create_dir(&a).unwrap();
        fs::write(a.join("old"), []).unwrap();
        fs::write(s.p("source").join("z"), []).unwrap();
        let mut calls = 0;
        let result = pack_directory(&s.p("source"), &s.p("temp"), || {
            calls += 1;
            if calls == 10 {
                fs::write(a.join("new"), b"added after a")?;
            }
            Ok(())
        });
        assert!(calls >= 10);
        assert!(result.is_err());
        s.assert_clean();
    }
    fn extract_archive(
        zip: &Path,
        parent: &Path,
        basename: &str,
        check: impl FnMut() -> Result<()>,
    ) -> Result<PublishedFolder> {
        super::extract_archive(zip, parent, basename, check, || Ok(()))
    }
    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("arterm-archive-test-{}", Uuid::now_v7()));
            fs::create_dir(&root).unwrap();
            for name in ["source", "temp", "out", "outside"] {
                fs::create_dir(root.join(name)).unwrap();
            }
            fs::write(root.join("outside").join("sentinel"), b"untouched").unwrap();
            Self(root)
        }
        fn p(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn assert_clean(&self) {
            assert_eq!(fs::read_dir(self.p("temp")).unwrap().count(), 0);
            assert_eq!(fs::read_dir(self.p("out")).unwrap().count(), 0);
            assert_eq!(
                fs::read(self.p("outside").join("sentinel")).unwrap(),
                b"untouched"
            );
        }
        fn zip(&self, entries: &[(&str, &[u8])]) -> PathBuf {
            let path = self.p("test.zip");
            let mut writer = ZipWriter::new(File::create(&path).unwrap());
            for (name, bytes) in entries {
                writer
                    .start_file(
                        *name,
                        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
                    )
                    .unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
            path
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn roundtrip_hidden_system_nested_unicode_empty_binary_and_zero_length() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
        };
        let s = Sandbox::new();
        let nested = s.p("source").join("\u{6587}\u{4ef6}");
        fs::create_dir(&nested).unwrap();
        fs::create_dir(nested.join("empty")).unwrap();
        fs::write(nested.join("zero"), []).unwrap();
        let binary: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        let hidden = s.p("source").join("hidden.bin");
        fs::write(&hidden, &binary).unwrap();
        let wide: Vec<u16> = hidden.as_os_str().encode_wide().chain(Some(0)).collect();
        assert_ne!(
            unsafe {
                SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM)
            },
            0
        );
        let prepared = pack_directory(&s.p("source"), &s.p("temp"), || Ok(())).unwrap();
        assert_eq!(prepared.entry_count(), 4);
        assert_eq!(prepared.uncompressed_bytes(), 200_000);
        let published = extract_archive(
            prepared.path(),
            &s.p("out"),
            prepared.original_basename(),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(published.entry_count, 4);
        assert_eq!(published.uncompressed_bytes, 200_000);
        assert_eq!(published.path.file_name().unwrap(), "source");
        assert_eq!(fs::read(published.path.join("hidden.bin")).unwrap(), binary);
        assert!(published
            .path
            .join("\u{6587}\u{4ef6}")
            .join("empty")
            .is_dir());
        assert_eq!(
            fs::metadata(published.path.join("\u{6587}\u{4ef6}").join("zero"))
                .unwrap()
                .len(),
            0
        );
        prepared.cleanup().unwrap();
        assert!(published.path.exists());
        assert_eq!(fs::read_dir(s.p("temp")).unwrap().count(), 0);
    }
    #[test]
    fn empty_folder_and_collision_preserve_existing_contents() {
        let s = Sandbox::new();
        fs::create_dir(s.p("out").join("source")).unwrap();
        fs::write(s.p("out").join("source").join("keep"), b"original").unwrap();
        let prepared = pack_directory(&s.p("source"), &s.p("temp"), || Ok(())).unwrap();
        let result = extract_archive(prepared.path(), &s.p("out"), "source", || Ok(())).unwrap();
        assert_ne!(result.path, s.p("out").join("source"));
        assert!(result
            .path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("source-"));
        assert_eq!(fs::read_dir(result.path).unwrap().count(), 0);
        assert_eq!(
            fs::read(s.p("out").join("source").join("keep")).unwrap(),
            b"original"
        );
    }
    #[test]
    fn cancellation_during_pack_cleans_only_owned_stage() {
        let s = Sandbox::new();
        fs::write(s.p("source").join("large"), vec![9; CHUNK * 4]).unwrap();
        let mut calls = 0;
        assert!(pack_directory(&s.p("source"), &s.p("temp"), || {
            calls += 1;
            ensure!(calls < 6, "cancelled lease");
            Ok(())
        })
        .is_err());
        assert!(calls >= 6);
        s.assert_clean();
    }
    #[test]
    fn cancellation_during_unpack_never_publishes() {
        let s = Sandbox::new();
        let zip = s.zip(&[("large", &vec![8; CHUNK * 4])]);
        let mut calls = 0;
        assert!(extract_archive(&zip, &s.p("out"), "folder", || {
            calls += 1;
            ensure!(calls < 4, "deadline expired");
            Ok(())
        })
        .is_err());
        s.assert_clean();
    }
    #[test]
    fn cancellation_immediately_before_publication_never_publishes() {
        let s = Sandbox::new();
        let zip = s.zip(&[]);
        let mut calls = 0;
        assert!(extract_archive(&zip, &s.p("out"), "folder", || {
            calls += 1;
            ensure!(calls < 3, "session ended");
            Ok(())
        })
        .is_err());
        assert_eq!(calls, 3);
        s.assert_clean();
    }
    #[test]
    fn actual_uncompressed_limit_rejects_compression_bomb() {
        let s = Sandbox::new();
        fs::write(s.p("source").join("bomb"), vec![0; CHUNK * 8]).unwrap();
        let prepared = pack_directory(&s.p("source"), &s.p("temp"), || Ok(())).unwrap();
        assert!(fs::metadata(prepared.path()).unwrap().len() < CHUNK as u64);
        let error = extract_with_limit(
            prepared.path(),
            &s.p("out"),
            "folder",
            &mut || Ok(()),
            100,
            true,
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("actual uncompressed"));
        prepared.cleanup().unwrap();
        s.assert_clean();
    }
    #[test]
    fn corrupt_payload_crc_and_truncation_rejected() {
        let s = Sandbox::new();
        let path = s.zip(&[("data", b"unique-payload-for-crc")]);
        let mut bytes = fs::read(&path).unwrap();
        let position = bytes
            .windows(b"unique-payload-for-crc".len())
            .position(|w| w == b"unique-payload-for-crc")
            .unwrap();
        bytes[position] ^= 0xff;
        fs::write(&path, &bytes).unwrap();
        assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());
        bytes.truncate(20);
        fs::write(&path, bytes).unwrap();
        assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());
        s.assert_clean();
    }
    #[test]
    fn unsafe_names_and_links_rejected() {
        let s = Sandbox::new();
        for name in [
            "/absolute",
            "C:/drive",
            "//server/share",
            "../escape",
            "a/../../escape",
            "a\\escape",
            "a//b",
            "NUL.txt",
            "stream:ads",
            "trailing.",
            "./x",
        ] {
            let path = s.zip(&[(name, b"bad")]);
            assert!(
                extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err(),
                "{name}"
            );
            s.assert_clean();
        }
        let path = s.p("link.zip");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        writer
            .add_symlink("link", "../outside", SimpleFileOptions::default())
            .unwrap();
        writer.finish().unwrap();
        assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());
        s.assert_clean();
    }
    #[test]
    fn duplicate_case_and_file_directory_conflicts_rejected() {
        let s = Sandbox::new();
        for names in [["a", "A"], ["a", "a/b"], ["A/x", "a/y"]] {
            let path = s.zip(&[(names[0], b"x"), (names[1], b"y")]);
            assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());
            s.assert_clean();
        }
        let mut names = Names::default();
        names.insert("same", false).unwrap();
        assert!(names.insert("same", false).is_err());
    }
    #[test]
    fn entry_depth_and_path_limits_are_enforced() {
        let mut names = Names::default();
        assert!(names
            .insert(&vec!["a"; MAX_DEPTH + 1].join("/"), false)
            .is_err());
        assert!(names
            .insert(&vec!["a".repeat(255); 5].join("/"), false)
            .is_err());
        for n in 0..MAX_ENTRIES {
            names.insert(&format!("n{n}"), false).unwrap();
        }
        assert!(names.insert("overflow", false).is_err());
        let s = Sandbox::new();
        let path = s.zip(&[]);
        let mut bytes = fs::read(&path).unwrap();
        bytes[8..10].copy_from_slice(&((MAX_ENTRIES + 1) as u16).to_le_bytes());
        bytes[10..12].copy_from_slice(&((MAX_ENTRIES + 1) as u16).to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(extract_archive(&path, &s.p("out"), "folder", || Ok(())).is_err());
        s.assert_clean();
    }
    #[test]
    fn regular_zip_source_is_not_implicitly_a_directory() {
        let s = Sandbox::new();
        let path = s.zip(&[("file", b"bytes")]);
        assert!(pack_directory(&path, &s.p("temp"), || Ok(())).is_err());
        assert!(fs::metadata(path).unwrap().is_file());
        s.assert_clean();
    }
    #[test]
    fn nested_temp_parent_and_unsafe_basename_rejected() {
        let s = Sandbox::new();
        assert!(pack_directory(&s.p("source"), &s.p("source"), || Ok(())).is_err());
        let zip = s.zip(&[]);
        for name in ["..", "C:", "NUL", "a/b"] {
            assert!(extract_archive(&zip, &s.p("out"), name, || Ok(())).is_err());
        }
        s.assert_clean();
    }

    #[test]
    fn observed_source_change_is_rejected() {
        let s = Sandbox::new();
        let file = s.p("source").join("first");
        fs::write(&file, b"initial").unwrap();
        let mut calls = 0;
        let result = pack_directory(&s.p("source"), &s.p("temp"), || {
            calls += 1;
            // After this small file's read handle closes, before the final recheck.
            if calls == 6 {
                fs::write(&file, b"changed-size")?;
            }
            Ok(())
        });
        assert!(result.is_err());
        assert!(calls >= 6);
        s.assert_clean();
    }

    #[test]
    fn source_and_destination_junctions_are_rejected() {
        let s = Sandbox::new();
        let junction = s.p("source").join("junction");
        // Junction creation needs no symlink privilege and is confined to this nonce tree.
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(s.p("outside"))
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert!(pack_directory(&s.p("source"), &s.p("temp"), || Ok(())).is_err());
        let zip = s.zip(&[]);
        assert!(extract_archive(&zip, &junction, "folder", || Ok(())).is_err());
        fs::remove_dir(junction).unwrap();
        s.assert_clean();
    }

    #[test]
    fn prepared_zip_snapshot_releases_original_tree_and_preserves_packed_contents() {
        let s = Sandbox::new();
        let nested = s.p("source").join("nested");
        fs::create_dir(&nested).unwrap();
        let file = nested.join("data");
        fs::write(&file, b"original").unwrap();
        let prepared = pack_directory(&s.p("source"), &s.p("temp"), || Ok(())).unwrap();
        prepared.verify_sources(&|| Ok(())).unwrap();
        fs::write(&file, b"changed").unwrap();
        fs::write(nested.join("new-after-pack"), b"new").unwrap();
        fs::remove_file(&file).unwrap();
        fs::rename(&nested, s.p("source").join("renamed")).unwrap();
        fs::rename(s.p("source"), s.p("renamed-source")).unwrap();
        prepared.verify_sources(&|| Ok(())).unwrap();
        assert!(fs::write(prepared.path(), b"corrupt snapshot").is_err());
        assert!(fs::remove_file(prepared.path()).is_err());
        let transferred = s.p("transferred.zip");
        assert!(fs::copy(prepared.path(), &transferred).unwrap() > 0);
        prepared.verify_payload(&|| Ok(())).unwrap();
        let published = extract_archive(
            &transferred,
            &s.p("out"),
            prepared.original_basename(),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            fs::read(published.path.join("nested").join("data")).unwrap(),
            b"original"
        );
        assert!(!published
            .path
            .join("nested")
            .join("new-after-pack")
            .exists());
        assert!(!published.path.join("renamed").exists());
        let zip_path = prepared.path().to_owned();
        prepared.cleanup().unwrap();
        assert!(!zip_path.exists());
        assert!(published.path.exists());
        assert_eq!(fs::read_dir(s.p("temp")).unwrap().count(), 0);
    }

    #[test]
    fn verify_payload_checks_cancellation_but_accepts_later_original_membership_changes() {
        let s = Sandbox::new();
        let nested = s.p("source").join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("data"), b"original").unwrap();
        let prepared = pack_directory(&s.p("source"), &s.p("temp"), || Ok(())).unwrap();
        let calls = std::cell::Cell::new(0);
        assert!(prepared
            .verify_sources(&|| {
                calls.set(calls.get() + 1);
                ensure!(calls.get() < 2, "verification cancelled");
                Ok(())
            })
            .is_err());
        assert_eq!(calls.get(), 2);
        fs::write(nested.join("new-after-pack"), b"new").unwrap();
        prepared.verify_payload(&|| Ok(())).unwrap();
        prepared.verify_sources(&|| Ok(())).unwrap();
        let mut mutable_checks = 0;
        prepared
            .verify_sources(|| {
                mutable_checks += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(mutable_checks, 2);
        prepared.cleanup().unwrap();
        s.assert_clean();
    }

    #[test]
    fn authorization_guard_spans_only_publication_and_drops_before_cleanup() {
        use std::cell::Cell;
        struct Guard<'a> {
            dropped: &'a Cell<bool>,
            stage: PathBuf,
            published: PathBuf,
        }
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                assert!(self.published.join("data").is_file());
                assert!(self.stage.is_dir(), "cleanup must run after guard drops");
                self.dropped.set(true);
            }
        }
        let s = Sandbox::new();
        let zip = s.zip(&[("data", b"flushed payload")]);
        let authorized = Cell::new(false);
        let dropped = Cell::new(false);
        let result = super::extract_archive_guarded(
            &zip,
            &s.p("out"),
            "folder",
            || {
                assert!(
                    !authorized.get(),
                    "no progress or cancellation callbacks under guard"
                );
                Ok(())
            },
            || {
                assert!(!authorized.replace(true));
                let stage = fs::read_dir(s.p("out"))?.next().unwrap()?.path();
                assert_eq!(
                    fs::read(stage.join("folder").join("data"))?,
                    b"flushed payload"
                );
                assert!(!s.p("out").join("folder").exists());
                Ok(Guard {
                    dropped: &dropped,
                    stage,
                    published: s.p("out").join("folder"),
                })
            },
        )
        .unwrap();
        assert!(authorized.get() && dropped.get());
        assert_eq!(fs::read_dir(s.p("out")).unwrap().count(), 1);
        assert_eq!(
            fs::read(result.path.join("data")).unwrap(),
            b"flushed payload"
        );
    }

    #[test]
    fn failed_final_authorization_or_racing_collision_never_publishes_partial() {
        let s = Sandbox::new();
        let zip = s.zip(&[("data", b"payload")]);
        assert!(super::extract_archive_guarded(
            &zip,
            &s.p("out"),
            "folder",
            || Ok(()),
            || -> Result<()> { anyhow::bail!("lease revoked") }
        )
        .is_err());
        s.assert_clean();
        let result = super::extract_archive_guarded(
            &zip,
            &s.p("out"),
            "folder",
            || Ok(()),
            || {
                fs::create_dir(s.p("out").join("folder"))?;
                fs::write(s.p("out").join("folder").join("sentinel"), b"competitor")?;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read_dir(s.p("out")).unwrap().count(), 1);
        assert_eq!(
            fs::read(s.p("out").join("folder").join("sentinel")).unwrap(),
            b"competitor"
        );
        assert!(!s.p("out").join("folder").join("data").exists());
    }

    #[test]
    fn production_collision_preserves_exact_destination_without_renamed_completion() {
        let s = Sandbox::new();
        let zip = s.zip(&[("data", b"new payload")]);
        let existing = s.p("out").join("folder");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("sentinel"), b"existing contents").unwrap();
        let authorized = std::cell::Cell::new(false);
        assert!(super::extract_archive_guarded(
            &zip,
            &s.p("out"),
            "folder",
            || Ok(()),
            || {
                authorized.set(true);
                Ok(())
            }
        )
        .is_err());
        assert!(extract_archive_with_limit(
            &zip,
            &s.p("out"),
            "folder",
            MAX_UNCOMPRESSED_BYTES,
            || Ok(()),
            |_| {
                authorized.set(true);
                Ok(())
            }
        )
        .is_err());
        assert!(!authorized.get());
        assert_eq!(fs::read_dir(s.p("out")).unwrap().count(), 1);
        assert_eq!(
            fs::read(existing.join("sentinel")).unwrap(),
            b"existing contents"
        );
        assert!(!existing.join("data").exists());
    }
}
