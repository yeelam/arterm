use arterm::{
    client_config,
    store::{self, SessionReference, State, Store},
};
use serde_json::{json, Value};
use std::{
    fs,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    dir: PathBuf,
    target_id: String,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("client-inventory-{}", Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let vendor = root.join("vendor.exe");
        let build = Command::new("rustc")
            .args(["--edition=2021", "tests\\fixtures\\auth_vendor.rs", "-o"])
            .arg(&vendor)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        fs::write(root.join("mode"), b"expired").unwrap();
        let home = root.join("home");
        client_config::update(&home, |config| {
            config.devtunnel_path = Some(vendor);
            client_config::add(config, "work", "fixture", r"C:\fixture\host.exe")?;
            Ok(())
        })
        .unwrap();
        let config = client_config::load(&home).unwrap();
        let target = &config.targets["work"];
        let dir = client_config::state_dir(&home, target);
        fs::create_dir_all(&dir).unwrap();
        Self {
            root,
            home,
            dir,
            target_id: target.target_id.clone(),
        }
    }

    fn record(&self, reference: &str, ended: bool, confirmed: Option<bool>) -> Uuid {
        let (store, mut state) = Store::resolve(
            &self.dir,
            &self.target_id,
            &SessionReference::parse(reference).unwrap(),
            true,
            None,
            None,
        )
        .unwrap();
        state.origin = Some(vec![7; 16]);
        state.token = Some(vec![9; 32]);
        state.args = vec!["SECRET_COMMAND".into()];
        state.pending = Some(store::PendingInput {
            seq: 1,
            bytes: b"SECRET_PENDING_INPUT".to_vec(),
        });
        state.ended = ended;
        state.exit_confirmed = confirmed;
        store.save(&state).unwrap();
        state.id
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arterm"))
            .env("VSTERM_REMOTE_HOME", &self.home)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn no_vendor_calls(&self) {
        assert!(
            !self.root.join("calls").exists(),
            "offline inventory must not invoke vendor or login"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn no_secrets(output: &Output) {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for secret in [
            "SECRET_COMMAND",
            "SECRET_PENDING_INPUT",
            "SECRET_OAUTH",
            "SECRET_CREATE_CLAIM",
            "SECRET_RESUME_TOKEN",
            "PRIVATE_RECORD",
            "\"claim\"",
            "\"token\"",
            "\"origin\"",
            "\"pending\"",
        ] {
            assert!(!text.contains(secret), "{text}");
        }
    }
}

#[test]
fn offline_list_omits_retired_guards_but_keeps_unknown_and_unconfirmed_records_unchanged() {
    let fixture = Fixture::new();
    let mut ended = Vec::new();
    for name in ["myfirsttry", "mywork01", "mywork02"] {
        ended.push((name.to_owned(), fixture.record(name, true, Some(true))));
    }
    let unnamed = Uuid::now_v7();
    ended.push((
        unnamed.to_string(),
        fixture.record(&unnamed.to_string(), true, Some(true)),
    ));
    let unknown = fixture.record("mywork", false, None);
    let accepted = fixture.record("termination-uncertain", true, Some(false));
    let changed_broker = fixture.record("broker-uncertain", true, Some(false));
    let confirmed = fixture.record("confirmed-exit", true, Some(true));
    ended.push(("confirmed-exit".into(), confirmed));
    let reserved = Uuid::now_v7();
    fs::write(
        fixture.dir.join("ref-reserved.json"),
        serde_json::to_vec(&reserved).unwrap(),
    )
    .unwrap();
    let before = snapshot(&fixture.dir);
    let config_path = fixture.home.join("client").join("config.json");
    let config_before = fs::read(&config_path).unwrap();

    let output = fixture.run(&["list"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let text = String::from_utf8_lossy(&output.stdout);
    for (name, id) in &ended {
        assert!(
            !text.contains(name) && !text.contains(&id.to_string()),
            "{text}"
        );
    }
    for (name, id) in [
        ("mywork", unknown),
        ("termination-uncertain", accepted),
        ("broker-uncertain", changed_broker),
        ("reserved", reserved),
    ] {
        assert!(
            text.contains(name) && text.contains(&id.to_string()),
            "{text}"
        );
    }
    assert!(text.contains("remote state unknown"));
    assert!(text.contains("proven nonresumable") && text.contains("reservation only"));
    no_secrets(&output);
    let output = fixture.run(&["list", "--json"]);
    assert!(output.status.success());
    no_secrets(&output);
    let machines: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(machines[0]["machine"], "work");
    let rows = machines[0]["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    for (name, id) in [
        ("mywork", unknown),
        ("termination-uncertain", accepted),
        ("broker-uncertain", changed_broker),
        ("reserved", reserved),
    ] {
        assert!(rows
            .iter()
            .any(|row| row["session_name"] == name && row["session_id"] == id.to_string()));
    }
    assert!(rows.iter().all(|row| row.as_object().unwrap().len() == 3));
    assert!(rows
        .iter()
        .any(|row| row["session_name"] == "reserved" && row["recovery_record_present"] == false));
    assert_eq!(snapshot(&fixture.dir), before);
    assert_eq!(fs::read(config_path).unwrap(), config_before);
    // Listing must not relax ended/recovery guards or recycle names.
    assert!(Store::resolve(
        &fixture.dir,
        &fixture.target_id,
        &SessionReference::parse("termination-uncertain").unwrap(),
        true,
        None,
        None
    )
    .is_err());
    assert_eq!(snapshot(&fixture.dir), before);
    fixture.no_vendor_calls();
}

#[test]
fn unreadable_corrupt_locked_and_identity_mismatched_lifecycles_remain_visible_with_sanitized_warning(
) {
    let fixture = Fixture::new();
    let corrupt = Uuid::now_v7();
    fs::write(
        fixture.dir.join(format!("{corrupt}.dpapi")),
        b"PRIVATE_RECORD_SECRET_RESUME_TOKEN",
    )
    .unwrap();
    let oversize = Uuid::now_v7();
    fs::write(
        fixture.dir.join(format!("{oversize}.dpapi")),
        vec![0; 1024 * 1024 + 1],
    )
    .unwrap();
    let locked = fixture.record("locked", true, Some(true));
    let mut mismatches = Vec::new();
    for kind in ["box", "id", "schema", "json"] {
        let id = Uuid::now_v7();
        let mut state = State::new(
            fixture.target_id.clone(),
            "powershell.exe".into(),
            vec![],
            None,
        );
        state.id = id;
        state.claim = store::random_claim().unwrap();
        state.ended = true;
        let mut value = serde_json::to_value(state).unwrap();
        match kind {
            "box" => value["box_name"] = "another-target".into(),
            "id" => value["id"] = Uuid::now_v7().to_string().into(),
            "schema" => value["schema"] = 2.into(),
            _ => {}
        }
        let plain = if kind == "json" {
            b"SECRET_CREATE_CLAIM SECRET_RESUME_TOKEN not-json".to_vec()
        } else {
            serde_json::to_vec(&value).unwrap()
        };
        fs::write(
            fixture.dir.join(format!("{id}.dpapi")),
            store::protect(&plain, true).unwrap(),
        )
        .unwrap();
        mismatches.push(id);
    }
    let before = snapshot(&fixture.dir);
    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fixture.dir.join(format!("{locked}.dpapi")))
        .unwrap();
    for args in [&["list"][..], &["list", "--json"]] {
        let output = fixture.run(args);
        assert!(output.status.success());
        let warnings = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            warnings.matches("recovery lifecycle unavailable").count(),
            7,
            "{warnings}"
        );
        assert!(warnings.contains("remote state unknown"));
        for id in [corrupt, oversize, locked]
            .into_iter()
            .chain(mismatches.iter().copied())
        {
            assert!(String::from_utf8_lossy(&output.stdout).contains(&id.to_string()));
        }
        no_secrets(&output);
        if args.len() == 2 {
            let machines: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(machines[0]["sessions"].as_array().unwrap().len(), 7);
        }
    }
    drop(lock);
    assert_eq!(snapshot(&fixture.dir), before);
    let output = fixture.run(&["list", "--json"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&locked.to_string()));
    fixture.no_vendor_calls();
}

#[test]
fn no_targets_default_list_and_configured_print_only_connect_never_start_vendor_or_login() {
    let fixture = Fixture::new();
    let output = fixture.run(&["connect", "work"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("connect"));
    for args in [&["list", "--client"][..], &["list", "--client", "--json"]] {
        assert!(fixture.run(args).status.success());
    }
    client_config::update(&fixture.home, |config| {
        config.targets.clear();
        Ok(())
    })
    .unwrap();
    let output = fixture.run(&["list"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "No registered boxes."
    );
    let output = fixture.run(&["list", "--json"]);
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!([])
    );
    for args in [&["list", "--saved"][..], &["list", "--json", "--saved"]] {
        let output = fixture.run(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
    }
    fixture.no_vendor_calls();
}

#[test]
fn lifecycle_projection_and_merges_separate_confirmed_exit_from_fail_closed_uncertainty() {
    let fixture = Fixture::new();
    let id = fixture.record("uncertain", true, Some(false));
    assert!(store::active_sessions(&fixture.dir, &fixture.target_id)
        .unwrap()
        .iter()
        .any(|row| row.session_id == id));
    let store = Store::open(&fixture.dir, id).unwrap();
    let mut state = store.load(&fixture.target_id, id).unwrap();
    let uncertain = state.clone();
    state.mark_ended(true).unwrap();
    state.merge_ended(&uncertain).unwrap();
    assert!(state.ended && state.exit_confirmed == Some(true));
    store.save(&state).unwrap();
    assert!(!store::active_sessions(&fixture.dir, &fixture.target_id)
        .unwrap()
        .iter()
        .any(|row| row.session_id == id));
    let mut fresh = State::new(
        fixture.target_id.clone(),
        "powershell.exe".into(),
        vec![],
        None,
    );
    fresh.merge_ended(&uncertain).unwrap();
    assert!(fresh.ended && fresh.exit_confirmed == Some(false));
    fresh.merge_ended(&state).unwrap();
    assert!(fresh.ended && fresh.exit_confirmed == Some(true));
    state.mark_ended(false).unwrap();
    assert_eq!(
        state.exit_confirmed,
        Some(true),
        "positive exit knowledge must not be downgraded"
    );
    drop(store);
    fixture.no_vendor_calls();
}

fn guard(fixture: &Fixture, id: Uuid) -> Value {
    let protected = fs::read(fixture.dir.join(format!("{id}.dpapi"))).unwrap();
    serde_json::from_slice(&store::protect(&protected, false).unwrap()).unwrap()
}

#[test]
fn retirement_cleans_session_data_and_name_reuse_is_fresh_while_old_guid_termination_is_idempotent()
{
    let fixture = Fixture::new();
    let old_id = fixture.record("reusable", false, None);
    let record = Store::open(&fixture.dir, old_id).unwrap();
    let old = record.load(&fixture.target_id, old_id).unwrap();
    record
        .retire(
            &fixture.target_id,
            old_id,
            store::RetirementReason::Completed,
        )
        .unwrap();
    let retired_bytes = fs::read(fixture.dir.join(format!("{old_id}.dpapi"))).unwrap();
    assert!(record.save(&old).unwrap_err().is::<store::RetiredSession>());
    record
        .retire(&fixture.target_id, old_id, store::RetirementReason::Missing)
        .unwrap();
    assert_eq!(
        fs::read(fixture.dir.join(format!("{old_id}.dpapi"))).unwrap(),
        retired_bytes
    );
    let retired = guard(&fixture, old_id);
    assert_eq!(retired["kind"], "retired");
    assert_eq!(retired["reason"], "completed");
    assert_eq!(retired.as_object().unwrap().len(), 6);
    for field in [
        "claim",
        "token",
        "origin",
        "args",
        "pending",
        "request_id",
        "client_id",
        "input_ack",
    ] {
        assert!(retired.get(field).is_none());
    }
    drop(record);
    for reference in ["reusable".to_owned(), old_id.to_string()] {
        let output = fixture.run(&["terminate", "work", &reference, "--json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "already_retired");
        assert_eq!(value["session_id"], old_id.to_string());
        no_secrets(&output);
    }
    let (replacement, fresh) = Store::resolve(
        &fixture.dir,
        &fixture.target_id,
        &SessionReference::parse("reusable").unwrap(),
        true,
        Some("fresh-shell.exe"),
        Some(r"C:\fresh"),
    )
    .unwrap();
    assert_ne!(fresh.id, old.id);
    assert_ne!(fresh.client_id, old.client_id);
    assert_ne!(fresh.request_id, old.request_id);
    assert_ne!(fresh.claim, old.claim);
    assert_eq!(fresh.claim.len(), 32);
    assert!(
        fresh.token.is_none()
            && fresh.origin.is_none()
            && fresh.pending.is_none()
            && fresh.args.is_empty()
    );
    assert_eq!(
        (fresh.epoch, fresh.input_ack, fresh.create_epoch),
        (0, 0, 1)
    );
    assert!(fresh.create_deadline_ms.is_none() && fresh.command_execution.is_none());
    assert_eq!(fresh.shell, "fresh-shell.exe");
    assert_eq!(fresh.cwd.as_deref(), Some(r"C:\fresh"));
    let mapping: Uuid =
        serde_json::from_slice(&fs::read(fixture.dir.join("ref-reusable.json")).unwrap()).unwrap();
    assert_eq!(mapping, fresh.id);
    let output = fixture.run(&["terminate", "work", &old_id.to_string(), "--json"]);
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["session_id"], old_id.to_string());
    assert_eq!(value["status"], "already_retired");
    no_secrets(&output);
    let rows = store::active_sessions(&fixture.dir, &fixture.target_id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, fresh.id);
    assert_eq!(rows[0].session_name.as_deref(), Some("reusable"));
    drop(replacement);
    assert!(Store::resolve(
        &fixture.dir,
        &fixture.target_id,
        &SessionReference::parse(&old_id.to_string()).unwrap(),
        true,
        None,
        None
    )
    .err()
    .unwrap()
    .is::<store::RetiredSession>());
    assert_eq!(
        fs::read(fixture.dir.join(format!("{old_id}.dpapi"))).unwrap(),
        retired_bytes
    );
    fixture.no_vendor_calls();
}

#[test]
fn failed_mapping_publication_preserves_old_guard_and_unpublished_guid_cannot_be_used() {
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    let fixture = Fixture::new();
    let old = fixture.record("reusable", true, Some(true));
    let mapping_path = fixture.dir.join("ref-reusable.json");
    let mapping_before = fs::read(&mapping_path).unwrap();
    let guard_before = fs::read(fixture.dir.join(format!("{old}.dpapi"))).unwrap();
    let mapping_lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&mapping_path)
        .unwrap();
    assert!(Store::resolve(
        &fixture.dir,
        &fixture.target_id,
        &SessionReference::parse("reusable").unwrap(),
        true,
        None,
        None
    )
    .is_err());
    assert_eq!(fs::read(&mapping_path).unwrap(), mapping_before);
    let orphan = store::saved_sessions(&fixture.dir)
        .unwrap()
        .into_iter()
        .find(|row| row.session_id != old)
        .unwrap()
        .session_id;
    assert!(Store::resolve(
        &fixture.dir,
        &fixture.target_id,
        &SessionReference::parse(&orphan.to_string()).unwrap(),
        true,
        None,
        None
    )
    .err()
    .unwrap()
    .to_string()
    .contains("publication incomplete"));
    assert_eq!(
        fs::read(fixture.dir.join(format!("{old}.dpapi"))).unwrap(),
        guard_before
    );
    drop(mapping_lock);
    let (record, fresh) = Store::resolve(
        &fixture.dir,
        &fixture.target_id,
        &SessionReference::parse("reusable").unwrap(),
        true,
        None,
        None,
    )
    .unwrap();
    assert_ne!(fresh.id, old);
    assert_ne!(fresh.id, orphan);
    assert_eq!(
        serde_json::from_slice::<Uuid>(&fs::read(mapping_path).unwrap()).unwrap(),
        fresh.id
    );
    drop(record);
    fixture.no_vendor_calls();
}

#[test]
fn concurrent_name_recreation_produces_one_fresh_identity_and_stale_saves_cannot_resurrect() {
    let fixture = Fixture::new();
    let old = fixture.record("reusable", true, Some(true));
    let barrier = std::sync::Barrier::new(8);
    let ids = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let fixture = &fixture;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let resolved = Store::resolve(
                        &fixture.dir,
                        &fixture.target_id,
                        &SessionReference::parse("reusable").unwrap(),
                        true,
                        None,
                        None,
                    );
                    resolved.ok().map(|(record, state)| {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        drop(record);
                        state.id
                    })
                })
            })
            .collect();
        workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
    });
    assert_eq!(ids.len(), 1);
    let fresh = *ids.iter().next().unwrap();
    assert_ne!(fresh, old);
    let record = Store::open(&fixture.dir, fresh).unwrap();
    let state = record.load(&fixture.target_id, fresh).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = record.save(&state);
        });
        scope.spawn(|| {
            record
                .retire(&fixture.target_id, fresh, store::RetirementReason::Missing)
                .unwrap();
        });
    });
    assert_eq!(guard(&fixture, fresh)["reason"], "missing");
    assert!(record
        .save(&state)
        .unwrap_err()
        .is::<store::RetiredSession>());
    drop(record);
    fixture.no_vendor_calls();
}

#[test]
fn ambiguous_legacy_end_decisions_are_visible_and_cannot_be_replaced_without_proof() {
    let fixture = Fixture::new();
    let old = fixture.record("legacy", true, None);
    let before = snapshot(&fixture.dir);
    let output = fixture.run(&["list", "--json"]);
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value[0]["sessions"][0]["session_id"], old.to_string());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no exit confirmation"));
    assert!(Store::resolve(
        &fixture.dir,
        &fixture.target_id,
        &SessionReference::parse("legacy").unwrap(),
        true,
        None,
        None
    )
    .err()
    .unwrap()
    .is::<store::RecoveryBlocked>());
    assert_eq!(snapshot(&fixture.dir), before);
    fixture.no_vendor_calls();
}

#[test]
fn protected_record_validation_errors_do_not_expose_credential_contents_or_rewrite_unknown_data() {
    let fixture = Fixture::new();
    let id = fixture.record("corrupt-credential", false, None);
    let path = fixture.dir.join(format!("{id}.dpapi"));
    let mut value = guard(&fixture, id);
    value["token"] = "SECRET_RESUME_TOKEN".into();
    fs::write(
        &path,
        store::protect(&serde_json::to_vec(&value).unwrap(), true).unwrap(),
    )
    .unwrap();
    let before = fs::read(&path).unwrap();
    let output = fixture.run(&["terminate", "work", "corrupt-credential", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid protected state"));
    no_secrets(&output);
    assert_eq!(fs::read(path).unwrap(), before);
    fixture.no_vendor_calls();
}

#[test]
fn missing_resume_or_broker_binding_stays_unknown_without_network_or_replacement() {
    let fixture = Fixture::new();
    for (name, remove_token) in [("pending-create", true), ("unbound-legacy", false)] {
        let id = fixture.record(name, false, None);
        let record = Store::open(&fixture.dir, id).unwrap();
        let mut state = record.load(&fixture.target_id, id).unwrap();
        if remove_token {
            state.token = None;
        } else {
            state.origin = None;
        }
        record.save(&state).unwrap();
        drop(record);
        let path = fixture.dir.join(format!("{id}.dpapi"));
        let before = fs::read(&path).unwrap();
        let output = fixture.run(&["terminate", "work", name, "--json"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("retained"));
        no_secrets(&output);
        assert_eq!(fs::read(path).unwrap(), before);
        if !remove_token {
            let output = fixture.run(&["connect", "work", name, "--stdio", "--retries", "0"]);
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("without adopting a broker"));
            no_secrets(&output);
        }
        let (record, loaded) = Store::resolve(
            &fixture.dir,
            &fixture.target_id,
            &SessionReference::parse(name).unwrap(),
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(loaded.id, id);
        drop(record);
    }
    assert_eq!(
        store::active_sessions(&fixture.dir, &fixture.target_id)
            .unwrap()
            .len(),
        2
    );
    fixture.no_vendor_calls();
}

#[test]
fn claimed_completion_never_retires_malformed_ordinary_state_or_authorizes_name_recreation() {
    let fixture = Fixture::new();
    let cases = [
        "missing-request",
        "missing-shell",
        "token-type",
        "token-length",
        "claim-length",
        "origin-length",
        "wrong-id",
        "wrong-box",
        "wrong-schema",
        "bad-pending",
        "ack-overflow",
        "zero-create-epoch",
        "epoch-overflow",
        "bad-reference",
    ];
    let mut ids = Vec::new();
    for name in cases {
        let id = fixture.record(name, false, None);
        let mut value = guard(&fixture, id);
        value["ended"] = true.into();
        value["exit_confirmed"] = true.into();
        match name {
            "missing-request" => {
                value.as_object_mut().unwrap().remove("request_id");
            }
            "missing-shell" => {
                value.as_object_mut().unwrap().remove("shell");
            }
            "token-type" => value["token"] = "SECRET_RESUME_TOKEN".into(),
            "token-length" => value["token"] = json!(vec![9; 31]),
            "claim-length" => value["claim"] = json!(vec![7; 31]),
            "origin-length" => value["origin"] = json!(vec![7; 15]),
            "wrong-id" => value["id"] = Uuid::now_v7().to_string().into(),
            "wrong-box" => value["box_name"] = "another-box".into(),
            "wrong-schema" => value["schema"] = 2.into(),
            "bad-pending" => value["pending"]["seq"] = 99.into(),
            "ack-overflow" => value["input_ack"] = u64::MAX.into(),
            "zero-create-epoch" => value["create_epoch"] = 0.into(),
            "epoch-overflow" => value["epoch"] = u64::MAX.into(),
            "bad-reference" => value["reference"] = "UPPERCASE".into(),
            _ => unreachable!(),
        }
        fs::write(
            fixture.dir.join(format!("{id}.dpapi")),
            store::protect(&serde_json::to_vec(&value).unwrap(), true).unwrap(),
        )
        .unwrap();
        ids.push((name, id));
    }
    let before = snapshot(&fixture.dir);
    for args in [&["list"][..], &["list", "--json"]] {
        let output = fixture.run(args);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr)
                .matches("recovery lifecycle unavailable")
                .count(),
            cases.len()
        );
        for (_, id) in &ids {
            assert!(String::from_utf8_lossy(&output.stdout).contains(&id.to_string()));
        }
        no_secrets(&output);
        if args.len() == 2 {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value[0]["sessions"].as_array().unwrap().len(), cases.len());
        }
        assert_eq!(snapshot(&fixture.dir), before);
    }
    for (name, id) in ids {
        let output = fixture.run(&["connect", "work", name, "--stdio", "--retries", "0"]);
        assert_eq!(output.status.code(), Some(1));
        no_secrets(&output);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("already retired"));
        let record = Store::open(&fixture.dir, id).unwrap();
        let loaded = record.load(&fixture.target_id, id).err().unwrap();
        assert!(!loaded.is::<store::RetiredSession>());
        assert!(record
            .retire(&fixture.target_id, id, store::RetirementReason::Completed)
            .is_err());
        let mut replacement = State::new(
            fixture.target_id.clone(),
            "powershell.exe".into(),
            vec![],
            None,
        );
        replacement.id = id;
        replacement.reference = Some(name.into());
        replacement.claim = store::random_claim().unwrap();
        assert!(
            record.save(&replacement).is_err(),
            "invalid on-disk state must not be overwritten"
        );
        replacement.mark_ended(true).unwrap();
        assert!(
            record.save(&replacement).is_err(),
            "invalid on-disk state must not be rewritten to a guard"
        );
        drop(record);
        assert_eq!(snapshot(&fixture.dir), before);
    }
    fixture.no_vendor_calls();
}

#[test]
fn retired_guard_requires_exact_valid_schema_and_cannot_disguise_ordinary_state() {
    let fixture = Fixture::new();
    let cases = [
        "guard-extra-token",
        "guard-extra-ended",
        "guard-missing-reference",
        "guard-missing-reason",
        "guard-wrong-box",
        "guard-wrong-id",
        "guard-bad-reference",
    ];
    let mut ids = Vec::new();
    for name in cases {
        let id = fixture.record(name, true, Some(true));
        let mut value = guard(&fixture, id);
        match name {
            "guard-extra-token" => value["token"] = "SECRET_RESUME_TOKEN".into(),
            "guard-extra-ended" => {
                value["ended"] = true.into();
                value["exit_confirmed"] = true.into();
            }
            "guard-missing-reference" => {
                value.as_object_mut().unwrap().remove("reference");
            }
            "guard-missing-reason" => {
                value.as_object_mut().unwrap().remove("reason");
            }
            "guard-wrong-box" => value["box_name"] = "another-box".into(),
            "guard-wrong-id" => value["id"] = Uuid::now_v7().to_string().into(),
            "guard-bad-reference" => value["reference"] = "UPPERCASE".into(),
            _ => unreachable!(),
        }
        fs::write(
            fixture.dir.join(format!("{id}.dpapi")),
            store::protect(&serde_json::to_vec(&value).unwrap(), true).unwrap(),
        )
        .unwrap();
        ids.push((name, id));
    }
    let before = snapshot(&fixture.dir);
    let output = fixture.run(&["list", "--json"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("recovery lifecycle unavailable")
            .count(),
        cases.len()
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value[0]["sessions"].as_array().unwrap().len(), cases.len());
    no_secrets(&output);
    for (name, _) in ids {
        let output = fixture.run(&["connect", "work", name, "--stdio", "--retries", "0"]);
        assert_eq!(output.status.code(), Some(1));
        no_secrets(&output);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("already retired"));
        assert_eq!(snapshot(&fixture.dir), before);
    }
    fixture.no_vendor_calls();
}

#[test]
fn fully_valid_completed_ordinary_records_migrate_to_minimal_guards_on_load_and_inventory() {
    let fixture = Fixture::new();
    for name in ["completed-load", "completed-inventory"] {
        let id = fixture.record(name, false, None);
        let mut value = guard(&fixture, id);
        value["ended"] = true.into();
        value["exit_confirmed"] = true.into();
        let path = fixture.dir.join(format!("{id}.dpapi"));
        fs::write(
            &path,
            store::protect(&serde_json::to_vec(&value).unwrap(), true).unwrap(),
        )
        .unwrap();
        if name == "completed-load" {
            let record = Store::open(&fixture.dir, id).unwrap();
            assert!(record
                .load(&fixture.target_id, id)
                .err()
                .unwrap()
                .is::<store::RetiredSession>());
            drop(record);
        } else {
            let output = fixture.run(&["list", "--json"]);
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            assert!(!String::from_utf8_lossy(&output.stdout).contains(&id.to_string()));
        }
        let value = guard(&fixture, id);
        assert_eq!(value["kind"], "retired");
        assert_eq!(value["reason"], "completed");
        assert_eq!(value["id"], id.to_string());
        assert_eq!(value.as_object().unwrap().len(), 6);
        for field in ["claim", "token", "pending", "args", "origin"] {
            assert!(value.get(field).is_none());
        }
        assert_eq!(
            serde_json::from_slice::<Uuid>(
                &fs::read(fixture.dir.join(format!("ref-{name}.json"))).unwrap()
            )
            .unwrap(),
            id
        );
    }
    fixture.no_vendor_calls();
}

#[test]
fn contradictory_lifecycle_proofs_are_rejected_without_repair_retirement_or_recreation() {
    let fixture = Fixture::new();
    let cases = [
        ("active-confirmed", false, Some(true), None),
        (
            "active-completed",
            false,
            Some(true),
            Some(store::RetirementReason::Completed),
        ),
        (
            "active-missing",
            false,
            Some(false),
            Some(store::RetirementReason::Missing),
        ),
        (
            "unconfirmed-completed",
            true,
            Some(false),
            Some(store::RetirementReason::Completed),
        ),
        (
            "legacy-completed",
            true,
            None,
            Some(store::RetirementReason::Completed),
        ),
        (
            "confirmed-missing",
            true,
            Some(true),
            Some(store::RetirementReason::Missing),
        ),
        (
            "confirmed-broker-change",
            true,
            Some(true),
            Some(store::RetirementReason::BrokerChanged),
        ),
    ];
    let mut ids = Vec::new();
    for (name, ended, confirmed, reason) in cases {
        let id = fixture.record(name, false, None);
        let mut value = guard(&fixture, id);
        value["ended"] = ended.into();
        value["exit_confirmed"] = json!(confirmed);
        value["retirement_reason"] = json!(reason);
        fs::write(
            fixture.dir.join(format!("{id}.dpapi")),
            store::protect(&serde_json::to_vec(&value).unwrap(), true).unwrap(),
        )
        .unwrap();
        ids.push((name, id));
    }
    let before = snapshot(&fixture.dir);
    for args in [&["list"][..], &["list", "--json"]] {
        let output = fixture.run(args);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr)
                .matches("recovery lifecycle unavailable")
                .count(),
            cases.len()
        );
        for (_, id) in &ids {
            assert!(String::from_utf8_lossy(&output.stdout).contains(&id.to_string()));
        }
        no_secrets(&output);
        assert_eq!(snapshot(&fixture.dir), before);
    }
    let active_confirmed = ids[0].1;
    for (name, id) in ids {
        let output = fixture.run(&["connect", "work", name, "--stdio", "--retries", "0"]);
        assert_eq!(output.status.code(), Some(1));
        no_secrets(&output);
        let record = Store::open(&fixture.dir, id).unwrap();
        assert!(!record
            .load(&fixture.target_id, id)
            .err()
            .unwrap()
            .is::<store::RetiredSession>());
        assert!(record
            .retire(&fixture.target_id, id, store::RetirementReason::Completed)
            .is_err());
        drop(record);
        assert_eq!(snapshot(&fixture.dir), before);
    }
    // An accepted-but-unconfirmed transition cannot normalize invalid caller state
    // into confirmed completion and subsequently create a guard.
    let id = active_confirmed;
    let value = guard(&fixture, id);
    let mut state: State = serde_json::from_value(value).unwrap();
    let state_before = serde_json::to_vec(&state).unwrap();
    assert!(state.mark_ended(false).is_err());
    assert!(serde_json::to_vec(&state).unwrap() == state_before);
    let record = Store::open(&fixture.dir, id).unwrap();
    assert!(record.save(&state).is_err());
    drop(record);
    assert_eq!(snapshot(&fixture.dir), before);
    fixture.no_vendor_calls();
}
