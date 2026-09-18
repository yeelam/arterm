use std::process::Command;

#[test]
fn absent_bridge_broker_reports_host_recovery_without_creating_state() {
    let home = std::env::temp_dir().join(format!("arterm-absent-broker-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
        .env("VSTERM_REMOTE_HOME", &home)
        .args(["bridge", "--protocol", "vsterm-session-v1"])
        .output()
        .unwrap();
    let entries = std::fs::read_dir(&home).unwrap().count();
    std::fs::remove_dir(&home).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "cannot connect to broker pipe \\\\.\\pipe\\vsterm-",
        "no broker pipe was found in this scope",
        "`arterm-host start`",
        "same Windows user, logon session, and data root",
        "VSTERM_REMOTE_HOME",
        "Restarting a broker does not restore its old sessions",
        "os error 2",
    ] {
        assert!(error.contains(expected), "missing {expected:?}: {error}");
    }
    assert_eq!(entries, 0, "bridge must not create a broker or session state");
}

#[test]
fn host_help_and_version_use_neutral_product_branding() {
    for (arg, expected) in [
        ("--help", "arTerm host".to_owned()),
        ("--version", format!("arTerm host {}", env!("CARGO_PKG_VERSION"))),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
            .arg(arg).output().unwrap();
        assert!(output.status.success());
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
        assert_eq!(text.lines().next(), Some(expected.as_str()));
    }

}

#[test]
fn host_output_flags_are_documented_and_invalid_options_do_not_touch_state() {
    let home = std::env::temp_dir().join(format!("arterm-host-output-{}", uuid::Uuid::now_v7()));
    let help = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
        .env("VSTERM_REMOTE_HOME", &home).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stderr).unwrap();
    assert!(help.contains("status [--json] | sessions [--json]"));
    assert!(help.contains("terminate <session-id> --yes [--json]"));
    for verb in ["status", "sessions", "terminate", "stop"] {
        let output = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
            .env("VSTERM_REMOTE_HOME", &home)
            .args([verb, "--json", "--json"]).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate option: --json"));
    }
    assert!(!home.exists());
}

#[test]
fn onboarding_validation_never_signs_in_or_starts_an_unconfigured_host() {
    let home = std::env::temp_dir().join(format!("devbox-host-cli-{}", uuid::Uuid::now_v7()));
    for args in [
        vec!["setup", "--name", "Invalid-Name", "--no-download"],
        vec![
            "setup",
            "--name",
            "valid-name",
            "--code-path",
            "relative.exe",
            "--no-download",
        ],
        vec!["start"],
        vec!["doctor"],
        vec!["terminate", "00000000-0000-0000-0000-000000000000"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
            .env("VSTERM_REMOTE_HOME", &home)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "unexpected success: {args:?}");
        assert!(
            !home.exists(),
            "invalid/unconfigured command changed state: {args:?}"
        );
    }
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["setup", "--help"],
        vec!["login", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_arterm-host"))
            .env("VSTERM_REMOTE_HOME", &home)
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "help failed: {args:?}");
        assert!(!home.exists());
    }
}
