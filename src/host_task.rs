//! Per-user Task Scheduler registrations. No credentials, elevation, or Run-key fallback.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const SOURCE: &str = "arTerm.Host.Task.v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub name: String,
    pub sid: String,
    pub exe: String,
    pub launcher: String,
    pub root: String,
    pub enabled: bool,
    pub running: bool,
    #[serde(default)]
    pub snapshot_xml: Option<String>,
}

fn path_text(path: &Path) -> Result<String> {
    let path = std::path::absolute(path)?;
    let raw = path
        .to_str()
        .context("task paths must be Unicode")?
        .trim_start_matches(r"\\?\");
    let text = if raw.len() == 3 && raw.ends_with(":\\") {
        raw
    } else {
        raw.trim_end_matches('\\')
    }
    .to_owned();
    ensure!(!text.contains(['"', '%', '\r', '\n']), "invalid task path");
    Ok(text)
}

fn identity(sid: &str, root: &str) -> String {
    let hash = Sha256::digest(format!("{sid}\0{}", root.to_lowercase()).as_bytes());
    format!("arTerm-Host-{:x}", hash)
}

fn quote(value: &str) -> String {
    // Windows CommandLineToArgvW: double trailing backslashes before the closing quote.
    let trailing = value.chars().rev().take_while(|c| *c == '\\').count();
    format!("\"{value}{}\"", "\\".repeat(trailing))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

impl Task {
    pub fn current(exe: &Path, root: &Path) -> Result<Self> {
        let sid: String = invoke("identity", serde_json::Value::Null)?;
        // Pipe identity historically hashes the original root spelling, not a canonical path.
        ensure!(root.is_absolute(), "Task Scheduler requires an absolute VSTERM_REMOTE_HOME; existing direct-run roots are unchanged");
        let root = root
            .to_str()
            .context("task data root must be Unicode")?
            .to_owned();
        ensure!(
            !root.contains(['"', '%', '\r', '\n']),
            "task data root cannot contain quotes, percent expansion or newlines"
        );
        let expected = Self {
            name: identity(&sid, &root),
            sid,
            exe: path_text(exe)?,
            launcher: path_text(
                &PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot is unset")?)
                    .join(r"System32\wscript.exe"),
            )?,
            root,
            enabled: false,
            running: false,
            snapshot_xml: None,
        };
        let paths = crate::deployment::host_runtime_paths(exe)?;
        if paths.len() > 1 {
            if let Some(task) = installed(&paths)?
                .into_iter()
                .find(|task| task.name == expected.name)
            {
                ensure!(
                    task.sid == expected.sid && task.root == expected.root,
                    "host task SID/data-root scope mismatch"
                );
                // Keep the exact registered action. Do not weaken Assert-Owned to filename matching.
                return Ok(task);
            }
        }
        Ok(expected)
    }

    fn arguments(&self) -> String {
        format!(
            "//B //Nologo //E:VBScript {} {} {}",
            quote(&self.script_path().to_string_lossy()),
            quote(&self.exe),
            quote(&self.root)
        )
    }

    fn script_path(&self) -> PathBuf {
        bootstrap_path(Path::new(&self.exe))
    }

    fn xml(&self) -> String {
        let sid = xml_escape(&self.sid);
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
<RegistrationInfo><Source>{SOURCE}</Source><Description>{root}</Description></RegistrationInfo>
<Triggers><LogonTrigger><Repetition><Interval>PT10M</Interval><StopAtDurationEnd>false</StopAtDurationEnd></Repetition><Enabled>true</Enabled><UserId>{sid}</UserId></LogonTrigger></Triggers>
<Principals><Principal id="User"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowHardTerminate>false</AllowHardTerminate><StartWhenAvailable>false</StartWhenAvailable><RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable><IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings><AllowStartOnDemand>true</AllowStartOnDemand><Enabled>{enabled}</Enabled><RunOnlyIfIdle>false</RunOnlyIfIdle><ExecutionTimeLimit>PT0S</ExecutionTimeLimit></Settings>
<Actions Context="User"><Exec><Command>{exe}</Command><Arguments>{args}</Arguments></Exec></Actions></Task>"#,
            root = xml_escape(&self.root),
            enabled = self.enabled,
            exe = xml_escape(&quote(&self.launcher)),
            args = xml_escape(&self.arguments())
        )
    }

    pub fn operation(&self, op: &str) -> Result<()> {
        if op == "register" {
            prepare_bootstrap(self)?;
        }
        invoke::<serde_json::Value>(
            op,
            serde_json::json!({
                "task": self, "arguments": self.arguments(), "xml": self.xml(), "source": SOURCE
            }),
        )?;
        Ok(())
    }

    pub fn register(&self) -> Result<()> {
        self.operation("register")
    }

    pub fn start(&self) -> Result<()> {
        self.operation("enable")?;
        self.operation("start")
    }

    pub fn enabled(&self) -> Result<bool> {
        let state: serde_json::Value = invoke(
            "state",
            serde_json::json!({
                "task":self,"arguments":self.arguments(),"source":SOURCE
            }),
        )?;
        state["enabled"]
            .as_bool()
            .context("invalid task enabled-state response")
    }
}

pub(crate) fn bootstrap_path(exe: &Path) -> PathBuf {
    exe.with_file_name("arterm-host-check.vbs")
}

pub(crate) fn verify_bootstrap(path: &Path) -> Result<()> {
    ensure!(
        std::fs::read(path)? == include_bytes!("host_task.vbs"),
        "refusing modified/unowned hidden bootstrap: {}",
        path.display()
    );
    Ok(())
}

fn prepare_bootstrap(task: &Task) -> Result<()> {
    use std::{fs, io::Write};
    let path = task.script_path();
    fs::create_dir_all(path.parent().context("bootstrap has no directory")?)?;
    if path.try_exists()? {
        verify_bootstrap(&path)?;
    } else {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        file.write_all(include_bytes!("host_task.vbs"))?;
        file.sync_all()?;
    }
    let mut command = Command::new(&task.launcher);
    command
        .args(["//B", "//Nologo", "//E:VBScript"])
        .arg(&path)
        .arg("--probe")
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let result = crate::transport::output_bounded(command, Duration::from_secs(10))
        .context("hidden host bootstrap probe failed; Windows Script Host/VBScript is required and may be disabled by policy")?;
    ensure!(result.status.success(), "Windows Script Host/VBScript is unavailable or blocked ({}); hidden host startup was not registered, no visible fallback", result.status);
    Ok(())
}

fn invoke<T: serde::de::DeserializeOwned>(op: &str, value: serde_json::Value) -> Result<T> {
    let system = PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot is unset")?);
    let mut command = Command::new(system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    let script = include_str!("host_task.ps1").replace(
        "__BOOTSTRAP_SHA256__",
        &format!("{:x}", Sha256::digest(include_bytes!("host_task.vbs"))),
    );
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .env(
            "ARTERM_TASK_REQUEST",
            serde_json::to_string(&serde_json::json!({"op":op,"value":value}))?,
        )
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let output = crate::transport::output_bounded(command, Duration::from_secs(20))
        .context("Task Scheduler helper failed/timed out; inspect task state before retrying")?;
    ensure!(
        output.status.success(),
        "Task Scheduler operation {op} failed (no elevation or startup fallback): {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("invalid Task Scheduler response")
}

pub fn installed(paths: &[PathBuf]) -> Result<Vec<Task>> {
    let paths = paths
        .iter()
        .map(|p| path_text(p))
        .collect::<Result<Vec<_>>>()?;
    let tasks: Vec<Task> = invoke("list", serde_json::json!({"paths":paths,"source":SOURCE}))?;
    for task in &tasks {
        ensure!(
            task.name == identity(&task.sid, &task.root),
            "task identity mismatch: {}",
            task.name
        );
    }
    Ok(tasks)
}

pub fn diagnostic(exe: &Path, root: &Path) -> Result<String> {
    let expected = Task::current(exe, root)?;
    let tasks = installed(&crate::deployment::host_runtime_paths(exe)?)?;
    match tasks.iter().find(|t| t.name == expected.name) {
        Some(t) => Ok(format!(
            "Task Scheduler: {} (enabled={}, task_active={})",
            t.name, t.enabled, t.running
        )),
        None => anyhow::bail!("owned per-user host task is missing; run explicit host setup"),
    }
}

pub fn require_enabled(exe: &Path, root: &Path) -> Result<()> {
    let expected = Task::current(exe, root)?;
    let tasks = installed(&crate::deployment::host_runtime_paths(exe)?)?;
    let task = tasks
        .iter()
        .find(|task| task.name == expected.name)
        .context("owned host task is missing; run explicit setup")?;
    ensure!(
        task.enabled,
        "host task is disabled; complete setup or explicitly start it"
    );
    Ok(())
}

pub fn intentional_stop(work: impl FnOnce() -> Result<()>, disable: bool) -> Result<()> {
    let root = crate::deployment::data_root()?;
    // Direct-run brokers (including resume E2E fixtures) do not require Task Scheduler.
    if std::env::var_os("ARTERM_TASK_PAUSED").is_some()
        || (!disable && !root.join("host").join("setup.json").try_exists()?)
    {
        ensure!(
            !disable,
            "persistent disable requires a configured task-backed host"
        );
        return work();
    }
    let exe = std::env::current_exe()?;
    let expected = Task::current(&exe, &root)?;
    let tasks: Vec<_> = installed(&crate::deployment::host_runtime_paths(&exe)?)?
        .into_iter()
        .filter(|t| t.name == expected.name)
        .collect();
    ensure!(!disable || !tasks.is_empty(), "owned host task is missing");
    paused(&tasks, !disable, |t, op| t.operation(op), work)
}

/// Disable first, then mutate. Restore all previous states, reporting every recovery failure.
pub fn paused<T>(
    tasks: &[Task],
    restore_on_success: bool,
    mut backend: impl FnMut(&Task, &str) -> Result<()>,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let mut touched = Vec::new();
    let result = (|| {
        for task in tasks {
            touched.push(task);
            backend(task, "disable")?;
        }
        work()
    })();
    let mut recovery = Vec::new();
    for task in touched {
        if result.is_ok() && !restore_on_success {
            continue;
        }
        if result.is_err() {
            if let Err(error) = backend(task, "restore") {
                recovery.push(format!("{} definition restore: {error:#}", task.name));
                continue;
            }
        }
        if let Err(error) = backend(task, if task.enabled { "enable" } else { "disable" }) {
            recovery.push(format!("{}: {error:#}", task.name));
        } else if result.is_err() && task.running {
            if let Err(error) = backend(task, "start") {
                recovery.push(format!("{} restart: {error:#}", task.name));
            }
        }
    }
    if !recovery.is_empty() {
        anyhow::bail!(
            "operation: {}; task recovery failed: {}",
            match &result {
                Ok(_) => "completed".into(),
                Err(e) => format!("{e:#}"),
            },
            recovery.join("; ")
        );
    }
    result
}

/// Preflight task permissions before stopping or writing; snapshots remain disabled during work.
pub fn install_transaction<T>(
    tasks: &[Task],
    new_task: Option<&Task>,
    mut backend: impl FnMut(&Task, &str) -> Result<()>,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let mut touched = Vec::new();
    let mut attempted_create = false;
    let result = (|| {
        if let Some(task) = new_task {
            attempted_create = true;
            backend(task, "register")?;
        }
        for task in tasks {
            touched.push(task);
            backend(task, "disable")?;
        }
        for task in tasks {
            backend(task, "register")?;
        }
        work()
    })();
    let mut recovery = Vec::new();
    for task in touched {
        if result.is_err() {
            if let Err(error) = backend(task, "restore") {
                recovery.push(format!("{} definition restore: {error:#}", task.name));
                continue;
            }
        }
        if let Err(error) = backend(task, if task.enabled { "enable" } else { "disable" }) {
            recovery.push(format!("{} state restore: {error:#}", task.name));
        } else if result.is_err() && task.running {
            if let Err(error) = backend(task, "start") {
                recovery.push(format!("{} restart: {error:#}", task.name));
            }
        }
    }
    if result.is_err() && attempted_create {
        if let Err(error) = backend(new_task.unwrap(), "remove-if-owned") {
            recovery.push(format!("new task cleanup: {error:#}"));
        }
    }
    ensure!(
        recovery.is_empty(),
        "operation: {}; recovery failed: {}",
        match &result {
            Ok(_) => "completed".into(),
            Err(error) => format!("{error:#}"),
        },
        recovery.join("; ")
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Task {
        Task {
            name: "fixture".into(),
            sid: "S-1-5-21-123".into(),
            exe: r"C:\Program Files\arTerm\host.exe".into(),
            launcher: r"C:\Windows\System32\wscript.exe".into(),
            root: r"C:\isolated & root".into(),
            enabled: false,
            running: false,
            snapshot_xml: None,
        }
    }
    #[test]
    fn exact_scope_action_and_settings() {
        let task = fixture();
        let xml = task.xml();
        for expected in [
            "<LogonType>InteractiveToken</LogonType>",
            "<RunLevel>LeastPrivilege</RunLevel>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<Command>&quot;C:\\Windows\\System32\\wscript.exe&quot;</Command>",
            "<Arguments>//B //Nologo //E:VBScript &quot;C:\\Program Files\\arTerm\\arterm-host-check.vbs&quot; &quot;C:\\Program Files\\arTerm\\host.exe&quot; &quot;C:\\isolated &amp; root&quot;</Arguments>",
            "<Repetition><Interval>PT10M</Interval><StopAtDurationEnd>false</StopAtDurationEnd></Repetition>",
            "<Enabled>false</Enabled>",
            "<StopOnIdleEnd>false</StopOnIdleEnd>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
        ] {
            assert!(xml.contains(expected), "{expected}");
        }
        assert!(!xml.contains("RestartOnFailure"));
        assert!(!xml.contains("<Duration>"));
        assert_eq!(quote(r"C:\"), "\"C:\\\\\"");
        assert_eq!(identity("a", r"C:\ROOT"), identity("a", r"c:\root"));
        assert_ne!(identity("a", "root"), identity("b", "root"));
        assert_ne!(identity("a", "root"), identity("a", "other"));
    }

    #[test]
    fn entra_sid_is_used_verbatim_for_principal_and_logon_trigger() {
        for sid in [
            "S-1-12-1-1234567890-2345678901-3456789012-456789012",
            "S-1-12-1-4294967295-0-1-2147483648",
            "S-1-5-21-111111111-222222222-333333333-1001",
        ] {
            let mut task = fixture();
            task.sid = sid.into();
            let xml = task.xml();
            assert_eq!(xml.matches(&format!("<UserId>{sid}</UserId>")).count(), 2);
            assert!(xml.contains(&format!(
                "<Enabled>true</Enabled><UserId>{sid}</UserId></LogonTrigger>"
            )));
            assert!(xml.contains(&format!(
                "<Principal id=\"User\"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel>"
            )));
            assert_ne!(identity(sid, &task.root), identity("S-1-5-18", &task.root));
        }
    }

    #[test]
    fn native_helper_identity_matches_current_process_token() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::ConvertSidToStringSidW, GetTokenInformation, TokenUser, TOKEN_QUERY,
                TOKEN_USER,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };
        let expected = unsafe {
            let mut raw = std::ptr::null_mut();
            assert_ne!(
                OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw),
                0
            );
            let token = OwnedHandle::from_raw_handle(raw);
            let mut needed = 0;
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut needed,
            );
            assert!(needed > 0);
            let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
            assert_ne!(
                GetTokenInformation(
                    token.as_raw_handle(),
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    needed,
                    &mut needed
                ),
                0
            );
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut text = std::ptr::null_mut();
            assert_ne!(ConvertSidToStringSidW(user.User.Sid, &mut text), 0);
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            let sid = String::from_utf16(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            sid.unwrap()
        };
        let actual: String = invoke("identity", serde_json::Value::Null).unwrap();
        // Do not print the user's SID or account name, even on mismatch.
        assert!(
            actual == expected,
            "scheduler helper identity differs from calling process token"
        );
    }
    #[test]
    fn pause_precedes_work_and_refusal_restores_state() {
        let mut task = fixture();
        task.enabled = true;
        let events = std::cell::RefCell::new(Vec::new());
        let result: Result<()> = paused(
            &[task],
            true,
            |_, op| {
                events.borrow_mut().push(op.to_owned());
                Ok(())
            },
            || {
                events.borrow_mut().push("refuse-live-sessions".into());
                anyhow::bail!("live sessions")
            },
        );
        assert!(result.is_err());
        assert_eq!(
            *events.borrow(),
            ["disable", "refuse-live-sessions", "restore", "enable"]
        );
    }
    #[test]
    fn failed_pause_never_runs_work_and_recovery_is_not_hidden() {
        let task = fixture();
        let result = paused(
            &[task],
            true,
            |_, _| anyhow::bail!("policy denied"),
            || -> Result<()> { panic!("must not stop or write") },
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("recovery failed"));
    }

    #[test]
    fn disabled_states_and_uninstall_success_are_preserved() {
        let mut enabled = fixture();
        enabled.enabled = true;
        enabled.name = "enabled".into();
        let disabled = fixture();
        let events = std::cell::RefCell::new(Vec::new());
        paused(
            &[enabled.clone(), disabled.clone()],
            true,
            |t, op| {
                events.borrow_mut().push(format!("{}:{op}", t.name));
                Ok(())
            },
            || {
                events.borrow_mut().push("write".into());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            *events.borrow(),
            [
                "enabled:disable",
                "fixture:disable",
                "write",
                "enabled:enable",
                "fixture:disable"
            ]
        );
        events.borrow_mut().clear();
        paused(
            &[enabled, disabled],
            false,
            |t, op| {
                events.borrow_mut().push(format!("{}:{op}", t.name));
                Ok(())
            },
            || {
                events.borrow_mut().push("remove".into());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            *events.borrow(),
            ["enabled:disable", "fixture:disable", "remove"]
        );
    }

    #[test]
    fn failure_restores_running_tasks_and_reports_restart_failures() {
        let mut task = fixture();
        task.enabled = true;
        task.running = true;
        let events = std::cell::RefCell::new(Vec::new());
        let result: Result<()> = paused(
            &[task],
            true,
            |_, op| {
                events.borrow_mut().push(op.to_owned());
                if op == "start" {
                    anyhow::bail!("restart denied");
                }
                Ok(())
            },
            || anyhow::bail!("write failed"),
        );
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("write failed") && error.contains("restart denied"));
        assert_eq!(*events.borrow(), ["disable", "restore", "enable", "start"]);
    }

    #[test]
    fn install_preflight_failure_never_stops_or_writes_and_cleans_only_new_task() {
        let task = fixture();
        let events = std::cell::RefCell::new(Vec::new());
        let result = install_transaction(
            &[],
            Some(&task),
            |_, op| {
                events.borrow_mut().push(op.to_owned());
                if op == "register" {
                    anyhow::bail!("policy denied");
                }
                Ok(())
            },
            || -> Result<()> {
                panic!("must not stop, write payload, or migrate Run");
            },
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("policy denied"));
        assert_eq!(*events.borrow(), ["register", "remove-if-owned"]);
    }

    #[test]
    fn all_owned_definitions_are_updated_while_disabled_before_stop_and_write() {
        let mut first = fixture();
        first.name = "first".into();
        first.enabled = true;
        let second = fixture();
        let events = std::cell::RefCell::new(Vec::new());
        install_transaction(
            &[first, second],
            None,
            |task, op| {
                events.borrow_mut().push(format!("{}:{op}", task.name));
                Ok(())
            },
            || {
                events
                    .borrow_mut()
                    .extend(["stop", "write", "metadata", "migrate-run"].map(String::from));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            *events.borrow(),
            [
                "first:disable",
                "fixture:disable",
                "first:register",
                "fixture:register",
                "stop",
                "write",
                "metadata",
                "migrate-run",
                "first:enable",
                "fixture:disable",
            ]
        );
    }

    #[test]
    fn failed_update_restores_original_definition_and_removes_new_registration() {
        let mut old = fixture();
        old.snapshot_xml = Some("verified original definition".into());
        old.enabled = true;
        old.running = true;
        let mut new = fixture();
        new.name = "new".into();
        let events = std::cell::RefCell::new(Vec::new());
        let result: Result<()> = install_transaction(
            &[old],
            Some(&new),
            |task, op| {
                if op == "restore" {
                    assert_eq!(
                        task.snapshot_xml.as_deref(),
                        Some("verified original definition")
                    );
                }
                events.borrow_mut().push(format!("{}:{op}", task.name));
                Ok(())
            },
            || {
                events.borrow_mut().push("refuse-live-sessions".into());
                anyhow::bail!("live sessions; no stop or payload writes")
            },
        );
        assert!(result.is_err());
        assert_eq!(
            *events.borrow(),
            [
                "new:register",
                "fixture:disable",
                "fixture:register",
                "refuse-live-sessions",
                "fixture:restore",
                "fixture:enable",
                "fixture:start",
                "new:remove-if-owned",
            ]
        );
    }

    #[test]
    fn uninstall_failure_recreates_deleted_definitions_before_restoring_state() {
        let mut task = fixture();
        task.enabled = true;
        task.snapshot_xml = Some("original XML".into());
        let exists = std::cell::Cell::new(true);
        let enabled = std::cell::Cell::new(true);
        let result: Result<()> = paused(
            &[task],
            false,
            |task, op| {
                match op {
                    "disable" => enabled.set(false),
                    "restore" => {
                        assert_eq!(task.snapshot_xml.as_deref(), Some("original XML"));
                        exists.set(true);
                    }
                    "enable" => {
                        assert!(exists.get());
                        enabled.set(true);
                    }
                    _ => panic!("unexpected operation"),
                }
                Ok(())
            },
            || {
                exists.set(false);
                anyhow::bail!("second task deletion/payload removal failed")
            },
        );
        assert!(result.is_err());
        assert!(exists.get() && enabled.get());
    }

    #[test]
    #[ignore = "explicit opt-in: unique disabled Task Scheduler fixture; never starts a process"]
    fn native_unique_disabled_registration() {
        let root = std::env::temp_dir().join(format!("arterm-task-test-{}", uuid::Uuid::now_v7()));
        let mut task = Task::current(&std::env::current_exe().unwrap(), &root).unwrap();
        task.name = format!("arTerm-Test-{}", uuid::Uuid::now_v7());
        // Exercise the real SID-scoped logon trigger, but never enable this fixture.
        let bootstrap_existed = task.script_path().exists();
        let result = task.register();
        if let Err(error) = result {
            // A timeout may have completed registration. Only the unique, ownership-checked
            // fixture is eligible for cleanup; report uncertainty rather than overwrite it.
            if let Err(cleanup) = task.operation("remove") {
                panic!(
                    "native registration failed: {error:#}; unique fixture cleanup: {cleanup:#}"
                );
            }
            panic!("native registration failed: {error:#}");
        }
        let check = (|| -> Result<()> {
            task.operation("check")?;
            let mut legacy = task.clone();
            legacy.snapshot_xml = Some(
                task.xml()
                    .replace(
                        &xml_escape(&quote(&task.launcher)),
                        &xml_escape(&quote(&task.exe)),
                    )
                    .replace(
                        &xml_escape(&task.arguments()),
                        &xml_escape(&format!("run --data-root {}", quote(&task.root))),
                    ),
            );
            legacy.operation("restore")?;
            let mut desired = task.clone();
            desired.enabled = true;
            desired.register()?;
            ensure!(
                !task.enabled()?,
                "updating a paused definition re-enabled it"
            );
            let snapshots: Vec<Task> = invoke(
                "list",
                serde_json::json!({"paths":[task.exe],"source":SOURCE}),
            )?;
            let snapshot = snapshots
                .iter()
                .find(|t| t.name == task.name)
                .context("missing task snapshot")?;
            task.operation("remove")?;
            snapshot.operation("restore")?;
            ensure!(
                !task.enabled()?,
                "recreating a deleted snapshot re-enabled it"
            );
            let mut foreign = task.clone();
            foreign.root.push_str("-foreign");
            ensure!(
                foreign.operation("remove").is_err(),
                "foreign root was accepted"
            );
            foreign = task.clone();
            foreign.exe.push_str("-foreign");
            ensure!(
                foreign.register().is_err(),
                "foreign action was overwritten"
            );
            foreign = task.clone();
            foreign.sid = "S-1-12-1-1-2-3-4".into();
            ensure!(
                foreign.register().is_err(),
                "foreign principal was accepted"
            );
            let foreign_trigger = task.xml().replacen(
                &format!("<UserId>{}</UserId>", task.sid),
                "<UserId>S-1-5-18</UserId>",
                1,
            );
            ensure!(invoke::<serde_json::Value>("register", serde_json::json!({
                "task":task,"arguments":task.arguments(),"xml":foreign_trigger,"source":SOURCE
            })).is_err(), "foreign logon trigger was accepted");
            Ok(())
        })();
        let cleanup = task.operation("remove");
        cleanup.expect("remove unique ownership-checked disabled fixture");
        if !bootstrap_existed {
            std::fs::remove_file(task.script_path()).unwrap();
        }
        check.unwrap();
    }

    #[test]
    #[ignore = "explicit opt-in: ARTERM_TEST_HOST_EXE with test-unsigned-ipc; both aliases in unique owned installations"]
    fn native_owned_alias_roundtrip_and_upgrade() {
        use std::{fs, thread, time::Instant};
        let source = std::env::var_os("ARTERM_TEST_HOST_EXE").expect("explicit test-built host");
        let bytes = fs::read(source).unwrap();
        let dir = std::env::temp_dir().join(format!("arterm-task-alias-{}", uuid::Uuid::now_v7()));
        let install = dir.join("Host");
        crate::deployment::write_payload(&install, crate::deployment::Role::Host, &bytes).unwrap();
        let canonical = install.join("arterm-host.exe");
        let legacy = install.join("vsterm-host.exe");
        let root = dir.join("data");
        struct Fixture {
            dir: PathBuf,
            paths: Vec<PathBuf>,
            task: Option<Task>,
            cleaned: bool,
        }
        impl Fixture {
            fn cleanup(&mut self) -> Result<()> {
                if let Some(task) = &self.task {
                    if task.enabled()? {
                        task.operation("disable")?;
                    }
                }
                crate::host_shutdown::force_stop(&self.paths)?;
                if let Some(task) = &self.task {
                    task.operation("remove-if-owned")?;
                }
                std::fs::remove_dir_all(&self.dir)?;
                self.cleaned = true;
                Ok(())
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if !self.cleaned {
                    if let Err(error) = self.cleanup() {
                        eprintln!("alias fixture cleanup failed: {error:#}");
                    }
                }
            }
        }
        let mut fixture = Fixture {
            dir: dir.clone(),
            paths: vec![canonical.clone(), legacy.clone()],
            task: None,
            cleaned: false,
        };
        let body = (|| -> Result<()> {
            for (registered_exe, caller) in [(&legacy, &canonical), (&canonical, &legacy)] {
                let task = Task::current(registered_exe, &root)?;
                fixture.task = Some(task.clone());
                task.register()?;
                let resolved = Task::current(caller, &root)?;
                ensure!(
                    resolved.exe == task.exe,
                    "runtime alias did not retain the owned action"
                );
                resolved.register()?;
                resolved.start()?;
                let deadline = Instant::now() + Duration::from_secs(40);
                loop {
                    let state: serde_json::Value = invoke(
                        "state",
                        serde_json::json!({
                            "task":task,"arguments":task.arguments(),"source":SOURCE
                        }),
                    )?;
                    if state["running"] == false {
                        ensure!(
                            state["last_result"] == 0 && state["state"] == 3,
                            "alias task check failed: {state}"
                        );
                        break;
                    }
                    ensure!(Instant::now() < deadline, "alias check did not finish");
                    thread::sleep(Duration::from_millis(100));
                }
                let broker: serde_json::Value =
                    serde_json::from_slice(&fs::read(root.join("host").join("broker.json"))?)?;
                verify_broker_logon(broker["pid"].as_u64().context("missing broker PID")?)?;
                let mut stop = Command::new(caller);
                stop.args(["stop", "--disable", "--json"])
                    .env("VSTERM_REMOTE_HOME", &root);
                let stopped = crate::transport::output_bounded(stop, Duration::from_secs(35))?;
                ensure!(
                    stopped.status.success(),
                    "other alias stop --disable failed: {}",
                    String::from_utf8_lossy(&stopped.stderr)
                );
                ensure!(
                    !task.enabled()?,
                    "other alias did not persistently disable task"
                );

                let snapshots = installed(&crate::deployment::host_runtime_paths(&canonical)?)?;
                install_transaction(
                    &snapshots,
                    None,
                    |t, op| t.operation(op),
                    || {
                        crate::deployment::write_payload(
                            &install,
                            crate::deployment::Role::Host,
                            &bytes,
                        )
                    },
                )?;
                let upgraded = Task::current(caller, &root)?;
                ensure!(
                    upgraded.exe == task.exe && !upgraded.enabled()?,
                    "upgrade changed alias binding or enabled state"
                );

                let foreign_dir = dir.join("OtherHost");
                crate::deployment::write_payload(
                    &foreign_dir,
                    crate::deployment::Role::Host,
                    &bytes,
                )?;
                let foreign = Task::current(&foreign_dir.join("arterm-host.exe"), &root)?;
                ensure!(
                    foreign.operation("check").is_err(),
                    "different installation was accepted"
                );
                let marker = install.join("installed.json");
                let saved = fs::read(&marker)?;
                fs::write(
                    &marker,
                    br#"{"publisher":"foreign","role":"Host","version":"fixture"}"#,
                )?;
                let rejected = Task::current(caller, &root).is_err();
                fs::write(&marker, saved)?;
                ensure!(rejected, "foreign publisher marker was accepted");

                task.operation("remove")?;
                fixture.task = None;
                println!("Native runtime alias roundtrip, persistent stop, and paused installer upgrade verified for {}.", registered_exe.file_name().unwrap().to_string_lossy());
            }
            Ok(())
        })();
        if let Err(error) = &body {
            eprintln!("native alias fixture failed: {error:#}");
        }
        fixture.cleanup().expect("clean unique owned alias fixture");
        body.unwrap();
    }

    #[test]
    #[ignore = "explicit opt-in: ARTERM_TEST_HOST_EXE; verifies hidden periodic checks with a unique owned task"]
    fn native_periodic_check_starts_noops_and_recovers() {
        native_broker_fixture(false);
    }

    #[test]
    #[ignore = "explicit opt-in: ARTERM_TEST_HOST_EXE; verifies stop --disable against a unique hidden periodic task"]
    fn native_disabled_task_prevents_periodic_start() {
        native_broker_fixture(true);
    }

    fn verify_broker_logon(pid: u64) -> Result<()> {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::{
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{
                GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE,
            },
        };
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                u32::try_from(pid)?,
            )
        };
        ensure!(
            !raw.is_null(),
            "cannot inspect owned scheduled broker token: {}",
            std::io::Error::last_os_error()
        );
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        ensure!(
            crate::peer_auth::same_user_logon(&process)?,
            "scheduled broker SID, session or authentication ID differs from caller"
        );
        let mut session = 0;
        ensure!(
            unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } != 0,
            "cannot inspect caller Windows session"
        );
        ensure!(session != 0, "fixture must run in an interactive session");
        println!("Verified scheduled broker in caller Windows session {session}; token SID and logon authentication ID match.");
        Ok(())
    }

    fn crash_owned_broker(pid: u64, exe: &Path) -> Result<()> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        };
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                0,
                u32::try_from(pid)?,
            )
        };
        ensure!(!raw.is_null(), "cannot open owned broker PID");
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        ensure!(
            crate::peer_auth::same_user_logon(&process)?,
            "broker token/logon mismatch"
        );
        ensure!(
            crate::peer_auth::image_path(&process)?.canonicalize()? == exe.canonicalize()?,
            "broker executable does not match isolated fixture"
        );
        ensure!(
            unsafe { TerminateProcess(process.as_raw_handle(), 1) } != 0,
            "terminate held broker failed"
        );
        ensure!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 5000) } == 0,
            "broker did not exit"
        );
        Ok(())
    }

    fn native_broker_fixture(disable: bool) {
        use std::{fs, thread, time::Instant};
        let source =
            std::env::var_os("ARTERM_TEST_HOST_EXE").expect("explicit test-built host executable");
        let dir =
            std::env::temp_dir().join(format!("arterm-task-recovery-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&dir).unwrap();
        let exe = dir.join("arterm-test-host.exe");
        fs::copy(source, &exe).unwrap();
        let root = dir.join("data");
        let task = Task::current(&exe, &root).unwrap();
        struct Fixture {
            task: Task,
            dir: PathBuf,
            cleaned: bool,
        }
        impl Fixture {
            fn cleanup(&mut self) -> Result<()> {
                if self.task.enabled()? {
                    self.task.operation("disable")?;
                }
                crate::host_shutdown::force_stop(&[PathBuf::from(&self.task.exe)])?;
                self.task.operation("remove-if-owned")?;
                std::fs::remove_dir_all(&self.dir)?;
                self.cleaned = true;
                Ok(())
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if !self.cleaned {
                    if let Err(error) = self.cleanup() {
                        eprintln!("unique native fixture cleanup failed: {error:#}");
                    }
                }
            }
        }
        let mut fixture = Fixture {
            task,
            dir,
            cleaned: false,
        };
        let task = &fixture.task;
        let body = (|| -> Result<()> {
            task.register()?;
            let wait_ready = |expected_result: u32| -> Result<()> {
                let deadline = Instant::now() + Duration::from_secs(35);
                loop {
                    let state: serde_json::Value = invoke(
                        "state",
                        serde_json::json!({
                            "task":task,"arguments":task.arguments(),"source":SOURCE
                        }),
                    )?;
                    ensure!(
                        state["interval"] == "PT10M",
                        "native repetition interval changed: {state}"
                    );
                    if state["running"] == false {
                        ensure!(
                            state["last_result"] == expected_result,
                            "periodic check result differed: {state}"
                        );
                        ensure!(
                            state["state"] == 3,
                            "task is not Ready after check: {state}"
                        );
                        return Ok(());
                    }
                    ensure!(Instant::now() < deadline, "short task check did not finish");
                    thread::sleep(Duration::from_millis(100));
                }
            };
            task.start()?;
            let broker = root.join("host").join("broker.json");
            let wait_pid = |previous: Option<u64>, timeout: Duration| -> Result<u64> {
                let deadline = Instant::now() + timeout;
                loop {
                    if broker.try_exists()? {
                        let value: serde_json::Value = serde_json::from_slice(&fs::read(&broker)?)?;
                        if let Some(pid) = value["pid"].as_u64() {
                            if Some(pid) != previous {
                                return Ok(pid);
                            }
                        }
                    }
                    if Instant::now() >= deadline {
                        let state: serde_json::Value = invoke(
                            "state",
                            serde_json::json!({
                                "task":task,"arguments":task.arguments(),"source":SOURCE
                            }),
                        )?;
                        let log = fs::read_to_string(root.join("host").join("host.stderr.log"))?;
                        anyhow::bail!(
                            "scheduled fixture did not start/recover; state={state}; stderr={log}"
                        );
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            };
            let first = wait_pid(None, Duration::from_secs(30))?;
            wait_ready(0)?;
            verify_broker_logon(first)?;
            task.operation("start")?;
            wait_ready(0)?;
            let same: serde_json::Value = serde_json::from_slice(&fs::read(&broker)?)?;
            ensure!(
                same["pid"] == first,
                "second check replaced the existing broker"
            );
            if disable {
                let mut stop = Command::new(&exe);
                stop.args(["stop", "--disable", "--json"])
                    .env("VSTERM_REMOTE_HOME", &root);
                let output = crate::transport::output_bounded(stop, Duration::from_secs(25))?;
                ensure!(
                    output.status.success(),
                    "persistent stop failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                ensure!(
                    task.operation("start").is_err(),
                    "disabled task was runnable"
                );
                ensure!(!task.enabled()?, "disabled task was re-enabled");
                println!("Native persistent-disable check passed.");
                return Ok(());
            }
            crash_owned_broker(first, &exe)?;
            {
                use std::os::windows::fs::OpenOptionsExt;
                let pipe = same["pipe"]
                    .as_str()
                    .context("fixture broker pipe missing")?;
                let _unresponsive = fs::OpenOptions::new()
                    .write(true)
                    .share_mode(0)
                    .open(root.join("host").join(format!("{pipe}.lock")))?;
                task.operation("start")?;
                wait_ready(1)?;
                let unchanged: serde_json::Value = serde_json::from_slice(&fs::read(&broker)?)?;
                ensure!(
                    unchanged["pid"] == first,
                    "locked/unresponsive broker was replaced"
                );
            }
            task.operation("start")?;
            let second = wait_pid(Some(first), Duration::from_secs(30))?;
            wait_ready(0)?;
            verify_broker_logon(second)?;
            println!("Hidden check started PID {first}, preserved it on second run, then started PID {second} after owned broker exit; task Ready/result 0, interval PT10M.");
            let output = crate::transport::output_bounded(
                {
                    let mut command = Command::new(&exe);
                    command
                        .args(["stop", "--json"])
                        .env("VSTERM_REMOTE_HOME", &root);
                    command
                },
                Duration::from_secs(15),
            )?;
            ensure!(
                output.status.success(),
                "isolated broker graceful stop failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = fs::read_to_string(root.join("host").join("host.stdout.log"))?;
            let stderr = fs::read_to_string(root.join("host").join("host.stderr.log"))?;
            let expected_starts = 6;
            ensure!(
                stdout.matches("[host] starting pid=").count() == expected_starts,
                "stdout startup logs were not retained across recovery"
            );
            ensure!(
                stderr.matches("[host] starting pid=").count() == expected_starts
                    && stderr.contains("[host] stopped normally"),
                "stderr lifecycle logs are incomplete"
            );
            ensure!(
                stderr.contains("[check] broker already running; no action"),
                "no-op check was not logged"
            );
            ensure!(
                !stderr.contains("initial_console_visible=true"),
                "background check/broker had a visible console"
            );
            ensure!(
                stderr.contains("refusing replacement"),
                "locked/unresponsive state was not diagnosed"
            );
            Ok(())
        })();
        if let Err(error) = &body {
            eprintln!("native periodic fixture failed: {error:#}");
        }
        let cleanup = fixture.cleanup();
        cleanup.expect("cleanup ownership-checked native recovery fixture");
        body.unwrap();
    }
}
