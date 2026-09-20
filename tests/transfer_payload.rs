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
    assert_eq!(prepared.payload_path(), source);
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
        &|| Ok(()), |_, _, _, _, admit| { admit(0)?; Ok(not_a_directory) });
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
            admit(2)?;
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
            admit(7)?;
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
            admit(actual)?;
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
