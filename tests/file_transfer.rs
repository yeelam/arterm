#[path = "../src/file_transfer.rs"]
mod file_transfer;

use file_transfer::*;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("arterm-file-transfer-test-{}", Uuid::now_v7()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn manager(&self, limits: Limits) -> TransferManager {
        TransferManager::new(
            AuthorizedSession::after_authorization(Uuid::now_v7()),
            &self.0,
            limits,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove isolated transfer test fixture");
    }
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn code(error: anyhow::Error) -> ErrorCode {
    error
        .downcast_ref::<TransferError>()
        .expect("typed transfer failure")
        .code
}

#[test]
fn empty_binary_and_multichunk_roundtrip_keep_receipts_and_files() {
    let fixture = Fixture::new();
    let mut manager = fixture.manager(Limits::default());
    for bytes in [
        vec![],
        vec![0, 255, 13, 10, 27, 128],
        (0..MAX_CHUNK_BYTES * 3 + 7).map(|i| i as u8).collect(),
    ] {
        let upload = manager
            .begin_upload("evidence.bin", bytes.len() as u64)
            .unwrap();
        let mut offset = 0;
        for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
            offset = manager
                .write_chunk(upload.transfer_id, offset, chunk)
                .unwrap();
        }
        let receipt = manager.finish(upload.transfer_id, hash(&bytes)).unwrap();
        assert_eq!(receipt.actual_path, upload.actual_path);
        assert!(receipt.actual_path.is_absolute());
        assert_eq!(receipt.sha256, hash(&bytes));
        assert_eq!(receipt.bytes, bytes.len() as u64);
        assert_eq!(fs::read(&receipt.actual_path).unwrap(), bytes);
        assert_eq!(
            manager.finish(upload.transfer_id, hash(&bytes)).unwrap(),
            receipt
        );
        assert_eq!(
            code(manager.close(upload.transfer_id).unwrap_err()),
            ErrorCode::InvalidState
        );
        assert_eq!(
            manager.cancel(upload.transfer_id).unwrap().state,
            TransferState::Completed
        );
        assert_eq!(
            manager.status(upload.transfer_id).unwrap().receipt.unwrap(),
            receipt
        );
        let download = manager.begin_download(&receipt.actual_path).unwrap();
        let mut output = Vec::new();
        loop {
            let chunk = manager
                .read_chunk(download.transfer_id, output.len() as u64, 8191)
                .unwrap();
            assert_eq!(chunk.offset, output.len() as u64);
            output.extend(chunk.bytes);
            if chunk.eof {
                break;
            }
        }
        let source_receipt = manager.close(download.transfer_id).unwrap();
        assert_eq!(
            code(
                manager
                    .finish(download.transfer_id, hash(&bytes))
                    .unwrap_err()
            ),
            ErrorCode::InvalidState
        );
        assert_eq!(source_receipt.sha256, receipt.sha256);
        assert_eq!(source_receipt.bytes, receipt.bytes);
        assert_eq!(output, bytes);
    }
    let path = manager.begin_upload("retained.bin", 0).unwrap();
    manager.finish(path.transfer_id, hash(&[])).unwrap();
    manager.shutdown().unwrap();
    assert_eq!(
        code(manager.begin_upload("after-shutdown", 0).unwrap_err()),
        ErrorCode::InvalidState
    );
    assert_eq!(
        manager.status(path.transfer_id).unwrap().state,
        TransferState::Completed
    );
    drop(manager);
    assert!(path.actual_path.exists());
}

#[test]
fn offsets_size_and_checksums_fail_without_publishing() {
    let fixture = Fixture::new();
    let mut manager = fixture.manager(Limits::default());
    let upload = manager.begin_upload("size.bin", 3).unwrap();
    assert_eq!(
        code(
            manager
                .write_chunk(upload.transfer_id, 1, b"a")
                .unwrap_err()
        ),
        ErrorCode::InvalidOffset
    );
    assert_eq!(
        code(
            manager
                .write_chunk(upload.transfer_id, 0, b"abcd")
                .unwrap_err()
        ),
        ErrorCode::SizeMismatch
    );
    assert_eq!(manager.status(upload.transfer_id).unwrap().bytes, 0);
    manager.write_chunk(upload.transfer_id, 0, b"a").unwrap();
    assert_eq!(
        code(
            manager
                .finish(upload.transfer_id, hash(b"abc"))
                .unwrap_err()
        ),
        ErrorCode::SizeMismatch
    );
    assert!(!upload.actual_path.exists());
    assert!(!upload.actual_path.parent().unwrap().exists());
    let upload = manager.begin_upload("hash.bin", 3).unwrap();
    manager.write_chunk(upload.transfer_id, 0, b"abd").unwrap();
    assert_eq!(
        code(
            manager
                .finish(upload.transfer_id, hash(b"abc"))
                .unwrap_err()
        ),
        ErrorCode::IntegrityMismatch
    );
    assert_eq!(
        manager.status(upload.transfer_id).unwrap().state,
        TransferState::Failed
    );
    assert_eq!(manager.charged_bytes(), 0);
}

#[test]
fn destination_collision_never_overwrites_or_deletes_existing_file() {
    let fixture = Fixture::new();
    let mut manager = fixture.manager(Limits::default());
    let upload = manager.begin_upload("collision.bin", 3).unwrap();
    fs::write(&upload.actual_path, b"original").unwrap();
    manager.write_chunk(upload.transfer_id, 0, b"new").unwrap();
    let error = manager
        .finish(upload.transfer_id, hash(b"new"))
        .unwrap_err();
    // Cleanup also reports that the directory is nonempty; original file survives.
    assert!(format!("{error:#}").contains("DestinationExists"));
    assert_eq!(fs::read(&upload.actual_path).unwrap(), b"original");
    assert!(!upload
        .actual_path
        .parent()
        .unwrap()
        .join(".arterm-partial")
        .exists());
}

#[test]
fn cancellation_and_drop_remove_only_owned_partials() {
    let fixture = Fixture::new();
    let mut manager = fixture.manager(Limits::default());
    let first = manager.begin_upload("first.bin", 10).unwrap();
    let second = manager.begin_upload("second.bin", 0).unwrap();
    manager.finish(second.transfer_id, hash(&[])).unwrap();
    manager.write_chunk(first.transfer_id, 0, b"012").unwrap();
    assert_eq!(
        manager.cancel(first.transfer_id).unwrap().state,
        TransferState::Cancelled
    );
    assert!(!first.actual_path.parent().unwrap().exists());
    let third = manager.begin_upload("third.bin", 1).unwrap();
    drop(manager);
    assert!(!third.actual_path.parent().unwrap().exists());
    assert!(second.actual_path.exists());
}

#[test]
fn revoked_commit_after_integrity_check_does_not_publish() {
    let fixture = Fixture::new();
    let mut manager = fixture.manager(Limits::default());
    let upload = manager.begin_upload("revoked.bin", 3).unwrap();
    manager.write_chunk(upload.transfer_id, 0, b"abc").unwrap();
    let result = manager.finish_guarded(upload.transfer_id, hash(b"abc"), || -> anyhow::Result<()> {
        anyhow::bail!("caller exited or lease revoked")
    });
    assert!(format!("{:#}", result.unwrap_err()).contains("caller exited"));
    assert!(!upload.actual_path.exists());
    assert!(!upload.actual_path.parent().unwrap().exists());
    assert_eq!(manager.status(upload.transfer_id).unwrap().state, TransferState::Failed);
}

#[test]
fn shared_source_and_directory_helpers_pin_objects_and_reject_existing_destinations() {
    let fixture = Fixture::new();
    let source = fixture.0.join("ordinary.zip");
    fs::write(&source, b"file bytes, not an extraction request").unwrap();
    let file = pin_source(&source).unwrap();
    assert!(!file.is_directory());
    assert_eq!(file.path(), source);
    assert!(file.file().metadata().unwrap().is_file());
    file.verify_unchanged().unwrap();
    assert!(fs::write(&source, b"cannot replace pinned source").is_err());

    let parents = pin_directories(&fixture.0).unwrap();
    let directory = fixture.0.join("private");
    create_private_directory(&directory).unwrap();
    assert!(create_private_directory(&directory).is_err());
    let pinned = pin_source(&directory).unwrap();
    assert!(pinned.is_directory());
    assert_eq!(actual_path(pinned.file()).unwrap(), directory);
    pinned.verify_unchanged().unwrap();
    assert!(fs::rename(&directory, fixture.0.join("renamed")).is_err());
    assert!(extended_path(&directory).unwrap().to_str().unwrap().starts_with(r"\\?\"));
    drop(pinned);
    drop(parents);
}
#[test]
fn quotas_include_completed_uploads_and_receipts_are_bounded() {
    let fixture = Fixture::new();
    let mut manager = fixture.manager(Limits {
        max_file_bytes: 4,
        max_stored_bytes: 4,
        max_active: 1,
        max_records: 2,
    });
    assert_eq!(
        code(manager.begin_upload("big", 5).unwrap_err()),
        ErrorCode::LimitExceeded
    );
    let upload = manager.begin_upload("one", 4).unwrap();
    assert_eq!(
        code(manager.begin_upload("busy", 0).unwrap_err()),
        ErrorCode::LimitExceeded
    );
    assert_eq!(
        code(
            manager
                .write_chunk(upload.transfer_id, 0, &vec![0; MAX_CHUNK_BYTES + 1])
                .unwrap_err()
        ),
        ErrorCode::LimitExceeded
    );
    manager.write_chunk(upload.transfer_id, 0, b"1234").unwrap();
    manager.finish(upload.transfer_id, hash(b"1234")).unwrap();
    assert_eq!(manager.charged_bytes(), 4);
    assert_eq!(
        code(manager.begin_upload("quota", 1).unwrap_err()),
        ErrorCode::LimitExceeded
    );
    let empty = manager.begin_upload("empty", 0).unwrap();
    manager.cancel(empty.transfer_id).unwrap();
    assert_eq!(manager.record_count(), 2);
    assert_eq!(
        code(manager.begin_upload("records", 0).unwrap_err()),
        ErrorCode::LimitExceeded
    );
    assert!(upload.actual_path.exists());
}

#[test]
fn owned_directory_handle_cleanup_refuses_nonempty_stage_and_preserves_neighbors() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
    };
    let fixture = Fixture::new();
    let _parents = pin_directories(&fixture.0).unwrap();
    let stage = fixture.0.join("owned-stage");
    create_private_directory(&stage).unwrap();
    let owned = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | 0x0001_0000)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(extended_path(&stage).unwrap()).unwrap();
    let child = stage.join("owned-child");
    let neighbor = fixture.0.join("keep-neighbor");
    fs::write(&child, b"partial").unwrap();
    fs::write(&neighbor, b"retained").unwrap();
    assert!(delete_open_file(&owned).is_err());
    assert_eq!(fs::read(&child).unwrap(), b"partial");
    assert_eq!(fs::read(&neighbor).unwrap(), b"retained");
    fs::remove_file(&child).unwrap();
    delete_open_file(&owned).unwrap();
    drop(owned);
    assert!(!stage.exists());
    assert_eq!(fs::read(&neighbor).unwrap(), b"retained");
}

#[test]
fn opaque_file_fingerprints_support_archive_source_comparison() {
    let fixture = Fixture::new();
    let source = fixture.0.join("fingerprint.bin");
    fs::write(&source, b"before").unwrap();
    let handle = fs::File::open(&source).unwrap();
    let before = fingerprint(&handle).unwrap();
    assert_eq!(before, fingerprint(&handle).unwrap());
    fs::write(&source, b"after-size-changed").unwrap();
    assert_ne!(before, fingerprint(&handle).unwrap());
}
#[test]
fn lexical_path_policy_rejects_relative_traversal_devices_unc_and_ads() {
    for name in [
        "",
        ".",
        "..",
        "a\\b",
        "a/b",
        "a:b",
        "NUL",
        "con.txt",
        "COM1.exe",
        "LPT9",
        "COM\u{b9}",
        "CONOUT$",
        "foo.",
        "foo ",
        ".arterm-partial",
        ".ARTERM-PARTIAL",
        "x\0y",
    ] {
        assert!(validate_basename(name).is_err(), "{name:?}");
    }
    for path in [
        r"relative.bin",
        r".\relative.bin",
        r"C:relative.bin",
        r"\rooted",
        r"\\server\share\a",
        r"\\?\C:\a",
        r"\\.\NUL",
        r"C:\a\..\b",
        r"C:\a\.\b",
        r"C:\a:stream",
        r"C:\NUL.txt",
        r"C:/file",
        r"C:\a\\b",
    ] {
        assert!(validate_absolute_path(Path::new(path)).is_err(), "{path}");
    }
    assert!(validate_absolute_path(Path::new(r"C:\logs\result.log")).is_ok());
    assert!(validate_basename("result.log").is_ok());
}

#[test]
fn source_sharing_prevents_mutation_and_early_close_is_failure() {
    use std::fs::OpenOptions;
    let fixture = Fixture::new();
    let source = fixture.0.join("source.bin");
    fs::write(&source, b"abcdef").unwrap();
    let mut manager = fixture.manager(Limits::default());
    let writer = OpenOptions::new().write(true).open(&source).unwrap();
    assert_eq!(
        code(manager.begin_download(&source).unwrap_err()),
        ErrorCode::SourceBusy
    );
    drop(writer);
    let download = manager.begin_download(&source).unwrap();
    assert!(OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&source)
        .is_err());
    assert!(fs::remove_file(&source).is_err());
    assert_eq!(
        code(manager.read_chunk(download.transfer_id, 1, 2).unwrap_err()),
        ErrorCode::InvalidOffset
    );
    assert_eq!(
        code(
            manager
                .read_chunk(download.transfer_id, 0, MAX_CHUNK_BYTES + 1)
                .unwrap_err()
        ),
        ErrorCode::LimitExceeded
    );
    assert_eq!(
        manager
            .read_chunk(download.transfer_id, 0, 2)
            .unwrap()
            .bytes,
        b"ab"
    );
    assert_eq!(
        code(manager.close(download.transfer_id).unwrap_err()),
        ErrorCode::SizeMismatch
    );
    assert_eq!(
        manager.status(download.transfer_id).unwrap().state,
        TransferState::Failed
    );
    assert_eq!(fs::read(&source).unwrap(), b"abcdef");
    assert!(manager.begin_download(&fixture.0).is_err());
}

#[test]
fn junction_source_and_managed_root_are_rejected() {
    use std::process::Command;
    let fixture = Fixture::new();
    let real = fixture.0.join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("data.bin"), b"data").unwrap();
    let junction = fixture.0.join("junction");
    let output = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&real)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    {
        let mut manager = fixture.manager(Limits::default());
        assert!(manager.begin_download(&junction.join("data.bin")).is_err());
        assert!(TransferManager::new(
            AuthorizedSession::after_authorization(Uuid::now_v7()),
            &junction,
            Limits::default()
        )
        .is_err());
        assert!(manager.directory().is_absolute());
    }
    fs::remove_dir(&junction).unwrap();
    assert_eq!(fs::read(real.join("data.bin")).unwrap(), b"data");
}

#[test]
fn publication_preserves_exact_names_across_alignment_boundaries() {
    let fixture = Fixture::new();
    let mut manager = fixture.manager(Limits::default());
    for padding in 0..16 {
        let name = format!("{}file.bin", "x".repeat(padding));
        let upload = manager.begin_upload(&name, 4).unwrap();
        manager.write_chunk(upload.transfer_id, 0, b"data").unwrap();
        let receipt = manager.finish(upload.transfer_id, hash(b"data")).unwrap();
        assert_eq!(receipt.actual_path, upload.actual_path);
        assert_eq!(
            receipt.actual_path.file_name().unwrap().to_str().unwrap(),
            name
        );
        assert_eq!(fs::read(&receipt.actual_path).unwrap(), b"data");
        let children = fs::read_dir(receipt.actual_path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(children, vec![std::ffi::OsString::from(&name)]);
    }
}

#[test]
fn long_roots_and_destinations_publish_and_download_without_truncation() {
    let fixture = Fixture::new();
    let mut long_root = fixture.0.clone();
    for component in ["r".repeat(100), "s".repeat(100), "t".repeat(100)] {
        long_root.push(component);
    }
    assert!(long_root.as_os_str().encode_wide().count() > 277);
    fs::create_dir_all(&long_root).unwrap();
    let mut long_manager = TransferManager::new(
        AuthorizedSession::after_authorization(Uuid::now_v7()),
        &long_root,
        Limits::default(),
    )
    .unwrap();
    let upload = long_manager.begin_upload("long-root.bin", 3).unwrap();
    long_manager
        .write_chunk(upload.transfer_id, 0, b"abc")
        .unwrap();
    let receipt = long_manager
        .finish(upload.transfer_id, hash(b"abc"))
        .unwrap();
    assert_eq!(receipt.actual_path, upload.actual_path);
    assert_eq!(fs::read(&receipt.actual_path).unwrap(), b"abc");
    let download = long_manager.begin_download(&receipt.actual_path).unwrap();
    assert_eq!(
        long_manager
            .read_chunk(download.transfer_id, 0, 64)
            .unwrap()
            .bytes,
        b"abc"
    );
    assert_eq!(
        long_manager.close(download.transfer_id).unwrap().sha256,
        hash(b"abc")
    );

    let mut manager = fixture.manager(Limits::default());
    let prefix_units = manager.directory().as_os_str().encode_wide().count() + 1 + 36 + 1;
    for desired in [260usize, 261, 300] {
        let length = desired
            .checked_sub(prefix_units)
            .expect("test root must permit target length");
        assert!((1..=255).contains(&length));
        let name = "n".repeat(length);
        let upload = manager.begin_upload(&name, 1).unwrap();
        assert_eq!(
            upload.actual_path.as_os_str().encode_wide().count(),
            desired
        );
        manager.write_chunk(upload.transfer_id, 0, b"x").unwrap();
        let receipt = manager.finish(upload.transfer_id, hash(b"x")).unwrap();
        assert_eq!(receipt.actual_path, upload.actual_path);
        assert_eq!(fs::read(&receipt.actual_path).unwrap(), b"x");
        assert_eq!(
            manager.cancel(upload.transfer_id).unwrap().state,
            TransferState::Completed
        );
    }
}

#[test]
fn unicode_case_variants_resolve_by_filesystem_identity() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("\u{00c4}ncestor");
    fs::create_dir(&directory).unwrap();
    let source = directory.join("\u{00c4}file.bin");
    fs::write(&source, b"unicode").unwrap();
    let alternate_directory = fixture.0.join("\u{00e4}ncestor");
    let alternate_source = alternate_directory.join("\u{00e4}file.bin");
    assert_eq!(fs::read(&alternate_source).unwrap(), b"unicode");
    let mut manager = TransferManager::new(
        AuthorizedSession::after_authorization(Uuid::now_v7()),
        &alternate_directory,
        Limits::default(),
    )
    .unwrap();
    let download = manager.begin_download(&alternate_source).unwrap();
    assert_eq!(download.actual_path, source);
    assert_eq!(
        manager
            .read_chunk(download.transfer_id, 0, 64)
            .unwrap()
            .bytes,
        b"unicode"
    );
    assert_eq!(
        manager.close(download.transfer_id).unwrap().sha256,
        hash(b"unicode")
    );
    let upload = manager.begin_upload("\u{00d6}utput.bin", 1).unwrap();
    manager.write_chunk(upload.transfer_id, 0, b"x").unwrap();
    let receipt = manager.finish(upload.transfer_id, hash(b"x")).unwrap();
    assert_eq!(fs::read(&receipt.actual_path).unwrap(), b"x");
    assert_eq!(
        manager.status(upload.transfer_id).unwrap().actual_path,
        receipt.actual_path
    );
}
