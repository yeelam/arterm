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
    fn payload(&self) -> file_transfer::Receipt {
        let mut manager = TransferManager::new(
            AuthorizedSession::after_authorization(Uuid::now_v7()), &self.0, Limits::default()).unwrap();
        let upload = manager.begin_upload("opaque.zip", 0).unwrap();
        manager.finish(upload.transfer_id, Sha256::digest([]).into()).unwrap()
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
    let payload = fixture.payload();
    let original = payload.actual_path.clone();
    let complete = transfer_payload::complete_payload(prepared.metadata().clone(), payload,
        &|| Ok(()), |_, _, _| anyhow::bail!("must never extract a file")).unwrap();
    assert_eq!(complete.payload.actual_path, original);
    assert_eq!(complete.source.kind, SourceKind::File);
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
    assert!(transfer_payload::complete_payload(source, fixture.payload(), &|| Ok(()),
        transfer_payload::extraction_unavailable).is_err());
}

#[test]
fn cancellation_prevents_extraction_callback_and_no_false_directory_receipt_is_possible() {
    let fixture = Fixture::new();
    let source = SourceMetadata { kind: SourceKind::Directory, original_basename: "reports".into() };
    let called = Cell::new(false);
    let result = transfer_payload::complete_payload(source.clone(), fixture.payload(),
        &|| anyhow::bail!("caller cancelled"), |_, _, _| {
            called.set(true);
            Ok(fixture.0.join("reports"))
        });
    assert!(result.is_err() && !called.get());
    let receipt = fixture.payload();
    let not_a_directory = receipt.actual_path.clone();
    let result = transfer_payload::complete_payload(source, receipt,
        &|| Ok(()), |_, _, _| Ok(not_a_directory));
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
        SourceMetadata { kind: SourceKind::Directory, original_basename: "reports".into() },
        receipt, &|| Ok(()), |_, _, _| {
            called.set(true);
            Ok(fixture.0.join("reports"))
        });
    assert!(format!("{:#}", result.err().expect("changed payload must fail")).contains("SHA-256"));
    assert!(!called.get());
    assert!(!fixture.0.join("reports").exists());
}
