//! Authenticated source metadata and adapter boundaries. ZIP parsing/creation
//! belongs to the archive adapter, not the byte-transfer or terminal protocols.
use anyhow::{bail, ensure, Context, Result};
use rmpv::Value;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::{Path, PathBuf}, time::Duration};

use crate::{
    file_transfer::{self, PinnedSource, Receipt, Status},
    wire::{get, map, s, text},
};

pub const METADATA_CAPABILITY: &str = "transfer-source-metadata-v1";
pub const DIRECTORY_CAPABILITY: &str = "directory-transfer-zip-v1";
// Enable only together with the concrete preparation/extraction adapters.
pub const DIRECTORY_ADAPTER_INSTALLED: bool = false;
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);
pub const RESPONSE_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMetadata {
    pub kind: SourceKind,
    pub original_basename: String,
}

impl SourceMetadata {
    pub fn from_source(source: &PinnedSource) -> Result<Self> {
        let metadata = Self {
            kind: if source.is_directory() { SourceKind::Directory } else { SourceKind::File },
            original_basename: source.path().file_name().and_then(|s| s.to_str())
                .context("source has no supported basename")?.into(),
        };
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn validate(&self) -> Result<()> {
        file_transfer::validate_basename(&self.original_basename)
    }

    pub fn to_wire(&self) -> Value {
        map(vec![
            ("kind", s(match self.kind { SourceKind::File => "file", SourceKind::Directory => "directory" })),
            ("original_basename", s(&self.original_basename)),
        ])
    }

    pub fn from_wire(body: &Value) -> Result<Self> {
        let source = get(body, "source")?;
        let fields = source.as_map().context("invalid source metadata")?;
        ensure!(fields.len() == 2 &&
            fields.iter().filter(|(k, _)| k.as_str() == Some("kind")).count() == 1 &&
            fields.iter().filter(|(k, _)| k.as_str() == Some("original_basename")).count() == 1,
            "invalid or duplicate source metadata fields");
        let metadata = Self {
            kind: match text(source, "kind")? {
                "file" => SourceKind::File,
                "directory" => SourceKind::Directory,
                _ => bail!("unsupported source kind"),
            },
            original_basename: text(source, "original_basename")?.into(),
        };
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn require_directory_support(&self, supported: bool) -> Result<()> {
        self.validate()?;
        ensure!(self.kind == SourceKind::File || supported,
            "directory transfer unsupported: {DIRECTORY_CAPABILITY} is not available");
        Ok(())
    }

    pub fn payload_basename(&self) -> String {
        match self.kind {
            SourceKind::File => self.original_basename.clone(),
            SourceKind::Directory => format!("{}.zip", uuid::Uuid::now_v7()),
        }
    }

    pub fn completion_path(&self, payload_path: &Path) -> Result<PathBuf> {
        self.validate()?;
        file_transfer::validate_absolute_path(payload_path)?;
        Ok(match self.kind {
            SourceKind::File => payload_path.to_owned(),
            SourceKind::Directory => payload_path.parent().context("payload has no parent")?
                .join(&self.original_basename),
        })
    }
}

#[derive(Serialize, Deserialize)]
pub struct PayloadStatus {
    #[serde(flatten)]
    pub payload: Status,
    pub source: SourceMetadata,
}

/// FileClose is a source-stream receipt, not proof of destination publication.
#[derive(Serialize, Deserialize)]
pub struct PayloadReceipt {
    #[serde(flatten)]
    pub payload: Receipt,
    pub source: SourceMetadata,
}

/// Own the archive and its cleanup/pins until the transfer ends. The adapter must
/// verify the entire enumerated tree, not merely the top directory timestamp.
pub trait PreparedArchive {
    fn payload_path(&self) -> &Path;
    fn verify_sources(&self, check: &dyn Fn() -> Result<()>) -> Result<()>;
}

pub struct PreparedSource<A> {
    source: PinnedSource,
    archive: Option<A>,
    metadata: SourceMetadata,
}

impl<A: PreparedArchive> PreparedSource<A> {
    pub fn metadata(&self) -> &SourceMetadata { &self.metadata }
    pub fn payload_path(&self) -> &Path {
        self.archive.as_ref().map_or(self.source.path(), PreparedArchive::payload_path)
    }
    pub fn verify_sources(&self, check: &dyn Fn() -> Result<()>) -> Result<()> {
        check()?;
        self.source.verify_unchanged()?;
        if let Some(archive) = &self.archive { archive.verify_sources(check)?; }
        check()
    }
}

pub fn prepare_source<A: PreparedArchive>(
    source: PinnedSource,
    directory_supported: bool,
    check: &dyn Fn() -> Result<()>,
    prepare_directory: impl FnOnce(&PinnedSource, &dyn Fn() -> Result<()>) -> Result<A>,
) -> Result<PreparedSource<A>> {
    check()?;
    let metadata = SourceMetadata::from_source(&source)?;
    metadata.require_directory_support(directory_supported)?;
    let archive = match metadata.kind {
        SourceKind::File => None,
        SourceKind::Directory => Some(prepare_directory(&source, check)?),
    };
    let prepared = PreparedSource { source, archive, metadata };
    prepared.verify_sources(check)?;
    Ok(prepared)
}

// An uninhabited adapter cannot accidentally advertise or complete directories.
pub enum UnavailableArchive {}
impl PreparedArchive for UnavailableArchive {
    fn payload_path(&self) -> &Path { match *self {} }
    fn verify_sources(&self, _: &dyn Fn() -> Result<()>) -> Result<()> { match *self {} }
}
pub fn archive_unavailable(_: &PinnedSource, _: &dyn Fn() -> Result<()>) -> Result<UnavailableArchive> {
    bail!("directory archive adapter is not installed")
}
pub fn extraction_unavailable(_: &SourceMetadata, _: &Receipt, _: &dyn Fn() -> Result<()>) -> Result<PathBuf> {
    bail!("directory extraction adapter is not installed")
}

/// Receiver-only completion gate. Directory publication MUST do bounded safe
/// extraction into an owned private staging directory, check cancellation, and
/// atomically publish without replacement under the caller's final lease guard.
/// It must preserve empty/nested directories and never infer kind from `.zip`.
/// The callback runs without a global/session lock; acquire that guard only for
/// its final rename. Until it succeeds, the payload receipt is not completion.
pub fn complete_payload(
    source: SourceMetadata,
    mut payload: Receipt,
    check: &dyn Fn() -> Result<()>,
    publish_directory: impl FnOnce(&SourceMetadata, &Receipt, &dyn Fn() -> Result<()>) -> Result<PathBuf>,
) -> Result<PayloadReceipt> {
    source.validate()?;
    check()?;
    if source.kind == SourceKind::Directory {
        let expected = source.completion_path(&payload.actual_path)?;
        // Retain byte integrity and path custody from verification through
        // extraction. A finished payload path alone is not a trusted ZIP.
        let archive = file_transfer::pin_source(&payload.actual_path)?;
        ensure!(!archive.is_directory() && archive.file().metadata()?.len() == payload.bytes,
            "directory payload size/type changed before extraction");
        let mut reader = archive.file();
        let mut hash = Sha256::new();
        let mut buffer = [0; file_transfer::MAX_CHUNK_BYTES];
        let mut remaining = payload.bytes;
        while remaining > 0 {
            check()?;
            let count = remaining.min(buffer.len() as u64) as usize;
            reader.read_exact(&mut buffer[..count])?;
            hash.update(&buffer[..count]);
            remaining -= count as u64;
        }
        archive.verify_unchanged()?;
        let actual: [u8; 32] = hash.finalize().into();
        ensure!(actual == payload.sha256, "directory payload SHA-256 changed before extraction");
        check()?;
        let published = publish_directory(&source, &payload, check)?;
        let directory = file_transfer::pin_source(&published)
            .context("directory publication returned an invalid path; outcome may be unknown")?;
        ensure!(directory.is_directory() && directory.path() == expected,
            "directory publication returned the wrong destination; outcome may be unknown");
        payload.actual_path = directory.path().to_owned();
        drop(archive);
    }
    Ok(PayloadReceipt { payload, source })
}
