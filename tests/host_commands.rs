use std::process::Command;

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
