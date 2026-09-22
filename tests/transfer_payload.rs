use arterm::{
    file_transfer::{self, AuthorizedSession, Limits, TransferManager},
    transfer_payload::{self, SourceKind, SourceMetadata},
    wire::{map, s},
};
use sha2::{Digest, Sha256};
use std::{cell::Cell, fs, path::PathBuf};
use uuid::Uuid;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("arterm-payload-test-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn payload(&self) -> (TransferManager, file_transfer::Receipt) {
        let mut manager = TransferManager::new(
            AuthorizedSession::after_authorization(Uuid::now_v7()), &self.0, Limits::default()).unwrap();
        let upload = manager.begin_upload("opaque.zip", 0).unwrap();
        let receipt = manager.finish(upload.transfer_id, Sha256::digest([]).into()).unwrap();
        (manager, receipt)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { fs::remove_dir_all(&self.0).expect("remove exact payload fixture"); }
}

#[test]
fn regular_zip_never_enters_preparation_or_extraction_adapter() {
    let fixture = Fixture::new();
    let source = fixture.0.join("ordinary.zip");
    fs::write(&source, b"PK\x05\x06").unwrap();
    let prepared = transfer_payload::prepare_source(file_transfer::pin_source(&source).unwrap(),
        false, &|| Ok(()), transfer_payload::archive_unavailable).unwrap();
    assert_eq!(prepared.metadata().kind, SourceKind::File);
    assert_eq!(prepared.metadata().original_basename, "ordinary.zip");
    let payload_pin = file_transfer::pin_source(prepared.payload_path()).unwrap();
    file_transfer::verify_path_identity(payload_pin.file(), &source).unwrap();
    prepared.verify_sources(&|| Ok(())).unwrap();
    let (mut manager, payload) = fixture.payload();
    let original = payload.actual_path.clone();
    let complete = transfer_payload::complete_payload(&mut manager, prepared.metadata().clone(), payload,
        &|| Ok(()), |_, _, _, _, _| anyhow::bail!("must never extract a file")).unwrap();
    assert_eq!(complete.payload.actual_path, original);
    assert_eq!(complete.source.kind, SourceKind::File);
    assert_eq!(complete.extracted_bytes, None);
    complete.validate_completion().unwrap();
}

#[test]
fn directory_requires_capability_before_preparation_and_publication_before_completion() {
    let fixture = Fixture::new();
    let folder = fixture.0.join("reports");
    fs::create_dir(&folder).unwrap();
    let called = Cell::new(false);
    let result = transfer_payload::prepare_source(file_transfer::pin_source(&folder).unwrap(),
        false, &|| Ok(()), |source, check| {
            called.set(true);
            transfer_payload::archive_unavailable(source, check)
        });
    assert!(result.is_err());
    assert!(!called.get());
    let source = SourceMetadata::from_source(&file_transfer::pin_source(&folder).unwrap()).unwrap();
    assert_eq!(source.kind, SourceKind::Directory);
    assert_eq!(source.original_basename, "reports");
    let (mut manager, payload) = fixture.payload();
    assert!(transfer_payload::complete_payload(&mut manager, source, payload, &|| Ok(()),
        transfer_payload::extraction_unavailable).is_err());
}

#[test]
fn cancellation_prevents_extraction_callback_and_no_false_directory_receipt_is_possible() {
    let fixture = Fixture::new();
    let source = SourceMetadata { kind: SourceKind::Directory, original_basename: "reports".into() };
    let called = Cell::new(false);
    let (mut manager, payload) = fixture.payload();
    let result = transfer_payload::complete_payload(&mut manager, source.clone(), payload,
        &|| anyhow::bail!("caller cancelled"), |_, _, _, _, _| {
            called.set(true);
            Ok(fixture.0.join("reports"))
        });
    assert!(result.is_err() && !called.get());
    let (mut other_manager, receipt) = fixture.payload();
    let not_a_directory = receipt.actual_path.clone();
    let result = transfer_payload::complete_payload(&mut other_manager, source, receipt,
        &|| Ok(()), |_, _, _, _, admit| { admit(0, 0)?; Ok(not_a_directory) });
    assert!(result.is_err(), "a transferred ZIP is not a completed directory");
}

#[test]
fn metadata_is_exact_validated_and_roundtrips_without_extension_inference() {
    for kind in [SourceKind::File, SourceKind::Directory] {
        let source = SourceMetadata { kind, original_basename: "same.zip".into() };
        let body = map(vec![("source", source.to_wire())]);
        assert_eq!(SourceMetadata::from_wire(&body).unwrap(), source);
    }

    for basename in ["..", "../escape", r"..\escape", "x:ads", "CON", "name."] {
        let source = SourceMetadata { kind: SourceKind::Directory, original_basename: basename.into() };
        assert!(SourceMetadata::from_wire(&map(vec![("source", source.to_wire())])).is_err());
    }
    for fields in [
        vec![("kind", s("file")), ("kind", s("directory")), ("original_basename", s("a"))],
        vec![("kind", s("unknown")), ("original_basename", s("a"))],
        vec![("kind", s("directory"))],
    ] {
        assert!(SourceMetadata::from_wire(&map(vec![("source", map(fields))])).is_err());
    }
}

#[test]
fn folder_metadata_hook_failure_cancellation_and_collision_preserve_unrelated_marks() {
    use arterm::{folder_archive, recipient_metadata};
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    for mode in ["success", "metadata_failure", "cancel", "collision"] {
        let fixture = Fixture::new();
        let source = fixture.0.join("source");
        let out = fixture.0.join("out");
        let temp = fixture.0.join("temp");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir(&out).unwrap();
        fs::create_dir(&temp).unwrap();
        fs::write(source.join("nested").join("owned.txt"), b"fixture").unwrap();
        let zip = folder_archive::pack_directory(&source, &temp, || Ok(())).unwrap();
        let unrelated = out.join("keep.txt");
        fs::write(&unrelated, b"untouched").unwrap();
        fs::write(format!("{}:Zone.Identifier", unrelated.display()), b"keep mark").unwrap();
        let destination = out.join("source");
        if mode == "collision" {
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("existing.txt"), b"existing").unwrap();
            fs::write(format!("{}:Zone.Identifier", destination.join("existing.txt").display()), b"existing mark").unwrap();
        }
        let called = Cell::new(false);
        let mut hook = |file: &fs::File| {
            called.set(true);
            let path = file_transfer::actual_path(file)?;
            let ads = format!("{}:Zone.Identifier", path.display());
            fs::write(&ads, b"[ZoneTransfer]\r\nZoneId=4\r\n")?;
            fs::write(format!("{}:unrelated", path.display()), b"keep stream")?;
            if mode == "metadata_failure" {
                let lock = fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&ads)?;
                let result = recipient_metadata::ensure_unblocked(file);
                drop(lock);
                result?;
            } else {
                recipient_metadata::ensure_unblocked(file)?;
            }
            assert_eq!(fs::read(&ads).unwrap_err().raw_os_error(), Some(2));
            assert_eq!(fs::read(format!("{}:unrelated", path.display()))?, b"keep stream");
            Ok(())
        };
        let result = folder_archive::extract_archive_with_metadata(zip.path(), &out, "source",
            1024, || {
                anyhow::ensure!(mode != "cancel" || !called.get(), "cancel after owned metadata");
                Ok(())
            }, &mut hook, |_| Ok(()));
        assert_eq!(result.is_ok(), mode == "success");
        assert!(called.get());
        if mode == "success" {
            let file = destination.join("nested").join("owned.txt");
            assert_eq!(fs::read(&file).unwrap(), b"fixture");
            assert_eq!(fs::read(format!("{}:Zone.Identifier", file.display())).unwrap_err().raw_os_error(), Some(2));
            assert_eq!(fs::read(format!("{}:unrelated", file.display())).unwrap(), b"keep stream");
        } else {
            assert!(!destination.join("nested").exists());
        }
        assert_eq!(fs::read(format!("{}:Zone.Identifier", unrelated.display())).unwrap(), b"keep mark");
        if mode == "collision" {
            assert_eq!(fs::read(format!("{}:Zone.Identifier", destination.join("existing.txt").display())).unwrap(), b"existing mark");
        } else if mode != "success" {
            assert!(!destination.exists());
        }
        assert!(fs::read_dir(&out).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            name == "keep.txt" || name == "source"
        }), "owned extraction stage was not cleaned");
    }
}

#[test]
fn changed_directory_payload_never_reaches_the_extraction_callback() {
    let fixture = Fixture::new();
    let mut manager = TransferManager::new(
        AuthorizedSession::after_authorization(Uuid::now_v7()), &fixture.0, Limits::default()).unwrap();
    let upload = manager.begin_upload("opaque.zip", 3).unwrap();
    manager.write_chunk(upload.transfer_id, 0, b"abc").unwrap();
    let receipt = manager.finish(upload.transfer_id, Sha256::digest(b"abc").into()).unwrap();
    fs::write(&receipt.actual_path, b"xyz").unwrap();
    let called = Cell::new(false);
    let result = transfer_payload::complete_payload(
        &mut manager,
        SourceMetadata { kind: SourceKind::Directory, original_basename: "reports".into() },
        receipt, &|| Ok(()), |_, _, _, _, _| {
            called.set(true);
            Ok(fixture.0.join("reports"))
        });
    assert!(format!("{:#}", result.err().expect("changed payload must fail")).contains("SHA-256"));
    assert!(!called.get());
    assert!(!fixture.0.join("reports").exists());
}

#[test]
fn expansion_budget_is_passed_to_extractor_and_admission_precedes_publication() {
    let fixture = Fixture::new();
    let limits = Limits { max_file_bytes: 8, max_stored_bytes: 10, max_active: 2, max_records: 8 };
    let mut manager = TransferManager::new(
        AuthorizedSession::after_authorization(Uuid::now_v7()), &fixture.0, limits).unwrap();
    let first = manager.begin_upload("first.zip", 1).unwrap();
    manager.write_chunk(first.transfer_id, 0, b"x").unwrap();
    manager.finish(first.transfer_id, Sha256::digest(b"x").into()).unwrap();
    manager.reserve_extracted_bytes(first.transfer_id, 7).unwrap();
    let second = manager.begin_upload("second.zip", 1).unwrap();
    manager.write_chunk(second.transfer_id, 0, b"x").unwrap();
    let receipt = manager.finish(second.transfer_id, Sha256::digest(b"x").into()).unwrap();
    let expected = receipt.actual_path.parent().unwrap().join("reports");
    let published = Cell::new(false);
    let result = transfer_payload::complete_payload(&mut manager,
        SourceMetadata { kind: SourceKind::Directory, original_basename: "reports".into() },
        receipt, &|| Ok(()), |_, _, budget, check, admit| {
            assert_eq!(budget, 1, "compressed and prior extracted bytes must both count");
            check()?;
            admit(2, 1)?;
            published.set(true);
            fs::create_dir(&expected)?;
            Ok(expected.clone())
        });
    assert!(result.is_err());
    assert!(!published.get() && !expected.exists());
    assert_eq!(manager.charged_bytes(), 9);
}

#[test]
fn uncertain_publication_keeps_quota_and_rejects_duplicate_extraction_before_callback() {
    let fixture = Fixture::new();
    let (mut manager, receipt) = fixture.payload();
    let metadata = SourceMetadata { kind: SourceKind::Directory, original_basename: "reports".into() };
    let result = transfer_payload::complete_payload(&mut manager, metadata.clone(), receipt.clone(),
        &|| Ok(()), |_, _, _, _, admit| {
            admit(7, 1)?;
            anyhow::bail!("publication outcome unknown")
        });
    assert!(format!("{:#}", result.err().unwrap()).contains("charge retained"));
    assert_eq!(manager.charged_bytes(), 7);
    let called = Cell::new(false);
    let result = transfer_payload::complete_payload(&mut manager, metadata, receipt,
        &|| Ok(()), |_, _, _, _, _| {
            called.set(true);
            anyhow::bail!("duplicate must never reach extraction")
        });
    assert!(result.is_err() && !called.get());
    assert_eq!(manager.charged_bytes(), 7);
}

#[test]
fn directory_completion_without_quota_admission_is_rejected() {
    let fixture = Fixture::new();
    let (mut manager, receipt) = fixture.payload();
    let result = transfer_payload::complete_payload(&mut manager,
        SourceMetadata { kind: SourceKind::Directory, original_basename: "reports".into() },
        receipt, &|| Ok(()), |_, _, _, _, _| Ok(fixture.0.join("reports")));
    assert!(format!("{:#}", result.err().unwrap()).contains("omitted retained-quota admission"));
    assert_eq!(manager.charged_bytes(), 0);
}

#[test]
fn successful_folder_publication_charges_actual_expansion_and_returns_only_folder_path() {
    use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
    };
    let fixture = Fixture::new();
    let mut manager = TransferManager::new(
        AuthorizedSession::after_authorization(Uuid::now_v7()), &fixture.0,
        Limits { max_file_bytes: 8, max_stored_bytes: 10, max_active: 2, max_records: 8 }).unwrap();
    let upload = manager.begin_upload("opaque.zip", 2).unwrap();
    manager.write_chunk(upload.transfer_id, 0, b"zz").unwrap();
    let payload = manager.finish(upload.transfer_id, Sha256::digest(b"zz").into()).unwrap();
    let original_zip = payload.actual_path.clone();
    let result = transfer_payload::complete_payload(&mut manager,
        SourceMetadata { kind: SourceKind::Directory, original_basename: "reports".into() },
        payload, &|| Ok(()), |metadata, payload, budget, check, admit| {
            assert_eq!(budget, 8);
            let parent = payload.actual_path.parent().unwrap();
            let stage = parent.join("owned-unit-stage");
            file_transfer::create_private_directory(&stage)?;
            let handle = OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES | 0x0001_0000)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(file_transfer::extended_path(&stage)?)?;
            fs::write(stage.join("data.bin"), b"12345678")?;
            let actual = fs::metadata(stage.join("data.bin"))?.len();
            check()?;
            admit(actual, 1)?;
            let target = parent.join(&metadata.original_basename);
            file_transfer::rename_no_replace(&handle, &target)?;
            drop(handle);
            Ok(target)
        }).unwrap();
    result.validate_completion().unwrap();
    assert_eq!(result.extracted_bytes, Some(8));
    assert_eq!(manager.charged_bytes(), 10);
    assert!(result.payload.actual_path.is_dir());
    assert_ne!(result.payload.actual_path, original_zip);
    assert_eq!(result.payload.actual_path.file_name().unwrap(), "reports");
    assert_eq!(fs::read(result.payload.actual_path.join("data.bin")).unwrap(), b"12345678");
    assert!(manager.begin_upload("excess.bin", 1).is_err());
}

#[test]
fn concrete_folder_adapter_enforces_budget_guard_and_no_replace() {
    for mode in ["success", "quota", "revoked", "collision"] {
        let fixture = Fixture::new();
        let source = fixture.0.join("reports");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data.txt"), vec![42; 4096]).unwrap();
        let prepared = transfer_payload::prepare_source(file_transfer::pin_source(&source).unwrap(),
            true, &|| Ok(()), transfer_payload::prepare_directory).unwrap();
        fs::write(source.join("data.txt"), b"new source").unwrap();
        prepared.verify_sources(&|| Ok(())).unwrap();
        let bytes = fs::read(prepared.payload_path()).unwrap();
        let mut manager = TransferManager::new(
            AuthorizedSession::after_authorization(Uuid::now_v7()), &fixture.0,
            Limits { max_file_bytes: 8192, max_stored_bytes: bytes.len() as u64 + if mode == "quota" { 4095 } else { 4096 },
                max_active: 2, max_records: 8 }).unwrap();
        let upload = manager.begin_upload("payload.zip", bytes.len() as u64).unwrap();
        manager.write_chunk(upload.transfer_id, 0, &bytes).unwrap();
        let payload = manager.finish(upload.transfer_id, Sha256::digest(&bytes).into()).unwrap();
        let destination = payload.actual_path.parent().unwrap().join("reports");
        if mode == "collision" {
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("keep.txt"), b"keep").unwrap();
        }
        let guard_held = Cell::new(false);
        struct Guard<'a>(&'a Cell<bool>);
        impl Drop for Guard<'_> { fn drop(&mut self) { self.0.set(false); } }
        let result = transfer_payload::complete_payload(&mut manager, prepared.metadata().clone(), payload,
            &|| { assert!(!guard_held.get(), "progress must not run under publication guard"); Ok(()) },
            |metadata, payload, budget, check, admit| transfer_payload::publish_directory(
                metadata, payload, budget, check, admit, || {
                    if mode == "revoked" { anyhow::bail!("lease revoked"); }
                    guard_held.set(true);
                    Ok(Guard(&guard_held))
                }));
        assert!(!guard_held.get());
        if mode == "success" {
            let receipt = result.unwrap();
            assert_eq!(receipt.extracted_bytes, Some(4096));
            assert_eq!(receipt.payload.actual_path, destination);
            assert_eq!(fs::read(destination.join("data.txt")).unwrap(), vec![42; 4096]);
            assert_eq!(manager.charged_bytes(), bytes.len() as u64 + 4096);
        } else {
            assert!(result.is_err(), "{mode}");
            assert!(!destination.join("data.txt").exists());
            if mode == "collision" {
                assert_eq!(fs::read(destination.join("keep.txt")).unwrap(), b"keep");
            } else {
                assert!(!destination.exists());
            }
            if mode == "quota" || mode == "revoked" {
                assert_eq!(manager.charged_bytes(), bytes.len() as u64);
            }
        }
    }
}
