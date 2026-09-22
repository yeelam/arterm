use std::{fs, process::Command};
use uuid::Uuid;

fn run(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arterm"))
        .env("VSTERM_REMOTE_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn registration_commands_are_native_isolated_and_preserve_recovery_data() {
    let home = std::env::temp_dir().join(format!("devbox-client-commands-{}", Uuid::now_v7()));
    fs::create_dir_all(&home).unwrap();
    let host = std::env::current_exe().unwrap();
    let added = run(
        &home,
        &[
            "add",
            "work",
            "--tunnel",
            "tunnel-1",
            "--host-path",
            host.to_str().unwrap(),
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let duplicate = run(
        &home,
        &[
            "add",
            "work",
            "--tunnel",
            "tunnel-2",
            "--host-path",
            host.to_str().unwrap(),
        ],
    );
    assert!(!duplicate.status.success());
    let listed = run(&home, &["list"]);
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("work\ttunnel-1"));
    assert!(home.join("client").join("config.json").is_file());
    let removed = run(&home, &["remove", "work"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!String::from_utf8_lossy(&run(&home, &["list"]).stdout).contains("work\t"));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn concurrent_registration_processes_preserve_all_successful_writes() {
    let home = std::env::temp_dir().join(format!("arterm-client-writers-{}", Uuid::now_v7()));
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8).map(|index| {
            let home = &home;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                run(home, &["add", &format!("box-{index}"), "--tunnel", "my-box",
                    "--host-path", r"C:\Tools\arterm-host.exe"])
            })
        }).collect();
        for worker in workers {
            let output = worker.join().unwrap();
            assert_eq!(output.status.code(), Some(0), "{}", String::from_utf8_lossy(&output.stderr));
        }
    });
    let config = arterm::client_config::load(&home).unwrap();
    assert_eq!(config.targets.len(), 8);
    for index in 0..8 {
        assert_eq!(config.targets[&format!("box-{index}")].tunnel_id, "my-box");
    }
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn help_version_and_validation_do_not_require_setup() {
    let home = std::env::temp_dir().join(format!("devbox-client-help-{}", Uuid::now_v7()));
    assert!(run(&home, &["--help"]).status.success());
    let help = String::from_utf8(run(&home, &["--help"]).stdout).unwrap();
    assert!(help.contains("--file") && help.contains("arterm receive"));
    assert!(!help.contains("--unblock") && help.contains("automatically unblocked") && help.contains("not a malware scan"));
    assert!(run(&home, &["--version"]).status.success());
    assert!(!run(
        &home,
        &[
            "add",
            "Bad Alias",
            "--tunnel",
            "x",
            "--host-path",
            r"C:\host.exe"
        ]
    )
    .status
    .success());
    assert!(!home.join("client").join("config.json").exists());
    for args in [
        vec!["resume", "work", "session"],
        vec!["send", "work", "session"],
        vec!["send", "work", "session", "--command", "x", "--file", "x"],
        vec!["send", "work", "session", "--command", "x", "--wait"],
        vec!["send", "work", "session", "--command", "x", "--wait", "--timeout", "0s"],
        vec!["send", "work", "session", "--file", "x", "--wait", "--timeout", "60s"],
        vec!["read", "work", "session", "--lines", "0"],
        vec!["receive", "work", "session"],
        vec!["detach", "work", "session", "--target", "work"],
    ] {
        assert!(!run(&home, &args).status.success(), "{args:?}");
    }

    assert!(!home.exists(), "invalid CLI must not create local state");
}

#[test]
fn removed_unblock_option_is_rejected_before_configuration_or_side_effects() {
    let home = std::env::temp_dir().join(format!("arterm-unblock-cli-{}", Uuid::now_v7()));
    for args in [
        vec!["send", "work", "session", "--file", r"C:\owned.txt", "--unblock"],
        vec!["receive", "work", "session", "--file", r"C:\owned.txt", "--unblock"],
        vec!["send", "work", "session", "--command", "echo should-not-run", "--unblock"],
        vec!["send", "work", "session", "--unblock"],
        vec!["receive", "work", "session", "--unblock"],
        vec!["read", "work", "session", "--unblock"],
        vec!["detach", "work", "session", "--unblock"],
        vec!["connect", "work", "session", "--unblock"],
        vec!["send", "work", "session", "--file", r"C:\owned.txt", "--unblock", "--wait"],
        vec!["send", "work", "session", "--file", r"C:\owned.txt", "--unblock", "--timeout", "1s"],
        vec!["send", "work", "session", "--file", r"C:\owned.txt", "--unblock", "--unblock"],
        vec!["receive", "work", "session", "--file", r"C:\owned.txt", "--unblock", "--command", "echo no"],
    ] {
        let result = run(&home, &args);
        assert!(!result.status.success(), "{args:?}");
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains("--unblock") || error.contains("--file") || error.contains("--command"),
            "{args:?}: {error}");
        assert!(!home.exists(), "invalid CLI invocation created local state");
    }
}

#[test]
fn automation_output_defaults_to_human_and_json_is_explicit() {
    let home = std::env::temp_dir().join(format!("arterm-output-{}", Uuid::now_v7()));
    let human = run(&home, &["list", "--client"]);
    assert!(human.status.success());
    assert_eq!(String::from_utf8(human.stdout).unwrap().trim(), "No active managed local connections.");
    let json = run(&home, &["list", "--client", "--json"]);
    assert!(json.status.success());
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&json.stdout).unwrap(), serde_json::json!([]));
    assert!(json.stderr.is_empty());
    for args in [
        vec!["list", "--client", "--json", "--json"],
        vec!["list", "--server", "work", "--json", "--json"],
        vec!["terminate", "work", "shell", "--json", "--json"],
        vec!["send", "work", "shell", "--command", "x", "--json", "--json"],
        vec!["read", "work", "shell", "--json", "--json"],
        vec!["interrupt", "work", "shell", "--json", "--json"],
        vec!["detach", "work", "shell", "--json", "--json"],
    ] {
        let output = run(&home, &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
    assert!(!home.exists());
}

#[test]
#[cfg_attr(not(feature = "test-unsigned-ipc"), ignore = "requires explicit unsigned functional fixture feature")]
fn headless_failure_preserves_final_error_with_drained_stderr() {
    let home = std::env::temp_dir().join(format!("arterm-diagnostics-{}", Uuid::now_v7()));
    assert!(run(&home, &["add", "work", "--tunnel", "fixture", "--host-path", r"C:\host.exe"]).status.success());
    for _ in 0..30 {
        let output = run(&home, &["connect", "work", "errorprobe", "--retries", "0"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("[client] retry limit reached"),
            "final error lost: {}", String::from_utf8_lossy(&output.stderr));
    }
    fs::remove_dir_all(home).unwrap();
}

#[test]
#[cfg_attr(feature = "test-unsigned-ipc", ignore = "requires default strict production policy")]
fn identical_unsigned_production_clients_cannot_start_ipc() {
    let home = std::env::temp_dir().join(format!("arterm-strict-unsigned-{}", Uuid::now_v7()));
    fs::create_dir_all(&home).unwrap();
    assert!(run(&home, &["add","work","--tunnel","fixture","--host-path",r"C:\host.exe"]).status.success());
    let first = home.join("owner.exe");
    let second = home.join("controller.exe");
    fs::copy(env!("CARGO_BIN_EXE_arterm"), &first).unwrap();
    fs::copy(&first, &second).unwrap();
    assert_eq!(fs::read(&first).unwrap(), fs::read(&second).unwrap());
    for executable in [&first, &second] {
        let output = Command::new(executable).env("VSTERM_REMOTE_HOME", &home)
            .args(["connect","work","probe","--retries","0"]).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("authenticate local client program") && error.contains("UntrustedSignature"),
            "unsigned same-image client did not fail certificate gate: {error}");
    }
    assert!(!home.join("client").join("active").exists());
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn registration_persists_the_friendly_name_not_a_generated_tunnel_id() {
    let home = std::env::temp_dir().join(format!("arterm-name-registration-{}", Uuid::now_v7()));
    let output = run(&home, &[
        "add", "dev01", "--tunnel", "dev01", "--host-path",
        r"C:\Users\remote\AppData\Local\Programs\VsTerm\Host\arterm-host.exe",
    ]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let path = home.join("client").join("config.json");
    let config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(config["targets"]["dev01"]["tunnel_id"], "dev01");
    assert!(String::from_utf8_lossy(&run(&home, &["list"]).stdout).contains("dev01\tdev01\t"));
    let disabled = Command::new(env!("CARGO_BIN_EXE_arterm"))
        .env("VSTERM_REMOTE_HOME", &home)
        .env("VSTERM_NO_HISTORY", "1")
        .args(["connect", "dev01", "--retries", "0"])
        .output().unwrap();
    assert!(disabled.status.success(), "no-reference connect must only print");
    let stderr = String::from_utf8_lossy(&disabled.stderr);
    assert!(!stderr.contains("Resume history"));
    assert!(!stderr.contains("Resume command saved to local PowerShell history"));
    assert!(run(&home, &["remove", "dev01"]).status.success());
    fs::remove_dir_all(home).unwrap();
}

#[test]
#[cfg_attr(not(feature = "test-unsigned-ipc"), ignore = "requires explicit unsigned functional fixture feature")]
fn failed_retries_keep_one_persisted_guid() {
    let home = std::env::temp_dir().join(format!("devbox-client-retry-{}", Uuid::now_v7()));
    fs::create_dir_all(&home).unwrap();
    let host = std::env::current_exe().unwrap();
    assert!(run(
        &home,
        &[
            "add",
            "work",
            "--tunnel",
            "test",
            "--host-path",
            host.to_str().unwrap()
        ]
    )
    .status
    .success());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let history = home.join("powershell-history.txt");
    let output = Command::new(env!("CARGO_BIN_EXE_arterm"))
        .env("VSTERM_REMOTE_HOME", &home)
        .env("VSTERM_HISTORY_PATH", &history)
        .env_remove("VSTERM_NO_HISTORY")
        .args([
            "connect",
            "work",
            "MyWork",
            "--address",
            &address,
            "--stdio",
            "--retries",
            "1",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!history.exists(), "failed creation must not insert resume history");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let ids = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[session] Resume GUID: "))
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 4, "{stderr}");
    assert!(ids.iter().all(|id| *id == ids[0]));
    Uuid::parse_str(ids[0]).unwrap();
    assert!(stderr.contains("connect 'work' mywork"));
    let records = walk(&home)
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dpapi"))
        })
        .count();
    assert_eq!(records, 1);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn no_reference_only_prints_a_powershell_safe_command_and_preserves_flags() {
    let home = std::env::temp_dir().join(format!("arterm-print-only-{}", Uuid::now_v7()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let output = run(&home, &["connect", "work", "--shell", "C:\\Tools\\It's PS.exe",
        "--cwd", "C:\\My Work", "--address", &address, "--stdio", "--retries", "0"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("connect 'work' "));
    assert!(text.contains("--shell 'C:\\Tools\\It''s PS.exe'"));
    assert!(text.contains("--cwd 'C:\\My Work'"));
    assert!(text.contains(&format!("--address '{address}'")));
    assert!(text.contains("--stdio"));
    assert!(text.contains("--retries 0"));
    assert_eq!(text.lines().count(), 1);
    assert!(listener.accept().is_err());
    assert!(!home.exists(), "print-only must not write state or history");
}

#[test]
#[cfg_attr(not(feature = "test-unsigned-ipc"), ignore = "requires explicit unsigned functional fixture feature")]
fn unused_guid_lookup_does_not_consume_printed_reference() {
    let home = std::env::temp_dir().join(format!("arterm-unused-guid-{}", Uuid::now_v7()));
    assert!(run(&home, &["add", "work", "--tunnel", "fixture",
        "--host-path", r"C:\Tools\arterm-host.exe"]).status.success());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    for verb in ["terminate"] {
        let printed = run(&home, &["connect", "work"]);
        assert!(printed.status.success());
        let command = String::from_utf8(printed.stdout).unwrap();
        let id = command.split_whitespace().find(|part| Uuid::parse_str(part).is_ok()).unwrap();
        let args = vec![verb, "work", id, "--address", &address, "--stdio"];
        assert!(!run(&home, &args).status.success());
        assert!(!walk(&home).iter().any(|path|
            path.file_name().unwrap().to_string_lossy() == format!("{id}.lock")),
            "{verb} reserved an unused GUID");
        let created = run(&home, &["connect", "work", id, "--address", &address,
            "--stdio", "--retries", "0"]);
        assert!(!created.status.success(), "fixture address must be unavailable");
        let error = String::from_utf8_lossy(&created.stderr);
        assert!(error.contains("Connection failed:"), "{error}");
        assert!(walk(&home).iter().any(|path|
            path.file_name().unwrap().to_string_lossy() == format!("{id}.dpapi")));
    }
    fs::remove_dir_all(home).unwrap();
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                result.push(path);
            }
        }
    }
    result
}
