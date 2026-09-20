//! Standalone host setup and isolated VS Code tunnel lifecycle.

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, IsTerminal, Write},
    os::windows::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
    System::{
        RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId},
        Threading::{
            GetCurrentProcessId, CREATE_NO_WINDOW,
        },
    },
};

use crate::deployment::{self, Role};

const SCHEMA: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SetupConfig {
    schema: u32,
    name: String,
    code_path: PathBuf,
    accepted_server_license_terms: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthState {
    LoggedIn,
    LoggedOut,
}

#[derive(Debug)]
struct SetupArgs {
    name: Option<String>,
    code_path: Option<PathBuf>,
    no_download: bool,
    accept_server_license_terms: bool,
    terminate_sessions: bool,
    force_stop_host: bool,
}

pub fn handle(args: &[String]) -> Result<Option<i32>> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    match command {
        "setup" => {
            if help_requested(&args[1..]) {
                print_setup_help();
            } else {
                setup(parse_setup_args(&args[1..])?)?;
            }

            Ok(Some(0))
        }
        "login" => {
            ensure_help_only(&args[1..], print_login_help)?;
            if !help_requested(&args[1..]) {
                login()?;
            }
            Ok(Some(0))
        }
        "start" => {
            ensure_help_only(&args[1..], print_start_help)?;
            if !help_requested(&args[1..]) {
                start_host()?;
            }
            Ok(Some(0))
        }
        "doctor" => {
            ensure_help_only(&args[1..], print_doctor_help)?;
            if !help_requested(&args[1..]) {
                doctor()?;
            }
            Ok(Some(0))
        }
        _ => Ok(None),
    }
}

pub fn status_details() -> Result<()> {
    let root = deployment::data_root()?;
    if let Some(config) = load_config(&root)? {
        match crate::host_task::diagnostic(&current_exe()?, &root) {
            Ok(message) => println!("{message}"),
            Err(error) => println!("Task Scheduler: {error:#}"),
        }
        println!(
            "Tunnel name: {} (isolated GitHub configuration)",
            config.name
        );
        println!("Code CLI: {}", config.code_path.display());
        println!("CLI data: {}", code_data_dir(&root).display());
        if let Some(id) = registration_id(&root, &config.name) {
            println!("Current tunnel ID (diagnostic only): {id}");
        }
        println!("{}", registration_command(&config.name, &current_exe()?));
        println!(
            "Use client `arterm doctor {}` to verify name discovery and reachability.",
            config.name
        );
    } else {
        println!("Local broker only; no tunnel setup has been configured.");
    }
    Ok(())
}

/// Starts the configured VS Code tunnel for the host supervisor.
///
/// The returned child is the only tunnel process this invocation owns. The caller must terminate
/// and wait for that child when its regular supervisor exits; no global tunnel kill is performed.
pub fn start_tunnel() -> Result<Option<Child>> {
    let root = deployment::data_root()?;
    let Some(config) = load_config(&root)? else {
        return Ok(None);
    };
    validate_config(&config)?;
    let code = deployment::ensure_dependency(Role::Host, Some(&config.code_path), true)
        .context("configured VS Code tunnel CLI is unavailable or no longer trusted")?;
    ensure!(
        inspect_auth(&code, &code_data_dir(&root))? == AuthState::LoggedIn,
        "isolated VS Code tunnel account is not logged in; run `arterm-host login`"
    );
    ensure!(
        config.accepted_server_license_terms,
        "server license terms were not explicitly accepted; rerun setup"
    );

    let host_dir = host_dir(&root);
    fs::create_dir_all(&host_dir)?;
    let stdout = append_log(&host_dir.join("code-tunnel.stdout.log"))?;
    let stderr = append_log(&host_dir.join("code-tunnel.stderr.log"))?;
    let child = isolated_code_command(&code, &root)
        .args([
            "tunnel",
            "--name",
            &config.name,
            "--accept-server-license-terms",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("start isolated VS Code tunnel")?;
    Ok(Some(child))
}

fn setup(args: SetupArgs) -> Result<()> {
    require_user_session()?;
    if let Some(path) = &args.code_path {
        ensure!(
            path.is_absolute(),
            "--code-path must be an absolute EXE path"
        );
    }
    let root = deployment::data_root()?;
    let existing = load_config(&root)?;
    let (name, code_path) = setup_defaults(&args, existing.as_ref())?;
    let host = host_dir(&root);
    let cli_data = code_data_dir(&root);
    fs::create_dir_all(&host)?;
    fs::create_dir_all(&cli_data)?;
    let code =
        deployment::ensure_dependency(Role::Host, code_path.as_deref(), args.no_download)?;
    let acceptance_is_current = existing.as_ref().is_some_and(|config| {
        config.accepted_server_license_terms && config.name == name && config.code_path == code
    });
    if !acceptance_is_current {
        accept_license(args.accept_server_license_terms)?;
    }

    let proposed = SetupConfig {
        schema: SCHEMA,
        name,
        code_path: code,
        accepted_server_license_terms: true,
    };
    validate_config(&proposed)?;

    let exe = current_exe()?;
    let registration = crate::host_task::Task::current(&exe, &root)?;
    let paths = deployment::host_runtime_paths(&exe)?;
    let tasks: Vec<_> = crate::host_task::installed(&paths)?.into_iter()
        .filter(|task| args.force_stop_host || task.name == registration.name).collect();
    let new_task = if tasks.iter().any(|task| task.name == registration.name) { None } else { Some(&registration) };
    setup_transaction(&root, &tasks, new_task, |task, op| task.operation(op), || {
        prepare_setup_host(
            existing.as_ref() != Some(&proposed),
            args.terminate_sessions || args.force_stop_host,
            host_is_running,
            || {
                if args.force_stop_host {
                    deployment::force_stop_host_command_paused(&exe)
                } else {
                    deployment::stop_host_command_paused(&exe, true)
                }
            },
        )?;
        login_if_needed(&proposed.code_path, &cli_data)?;
        save_config_if_changed(&root, &proposed)?;
        registration.register()?;
        deployment::remove_legacy_startup(&paths)?;
        Ok(())
    })?;
    registration.operation("enable")?;
    if !host_is_running()? {
        start_task(&exe, &root)?;
    }
    report_registration(&proposed, &root, &exe);
    Ok(())
}

fn setup_transaction(
    root: &Path,
    tasks: &[crate::host_task::Task],
    new_task: Option<&crate::host_task::Task>,
    mut backend: impl FnMut(&crate::host_task::Task, &str) -> Result<()>,
    work: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let path = config_path(root);
    let before = if path.try_exists()? { Some(fs::read(&path)?) } else { None };
    let recovery_safe = std::cell::Cell::new(true);
    crate::host_task::install_transaction(tasks, new_task, |task, op| {
        ensure!(recovery_safe.get() || !matches!(op, "enable" | "start"),
            "configuration recovery failed; owned task remains disabled");
        backend(task, op)
    }, || {
        let result = work();
        if let Err(error) = &result {
            let restore = (|| -> Result<()> {
                let after = if path.try_exists()? { Some(fs::read(&path)?) } else { None };
                if after != before {
                    match &before {
                        Some(bytes) => write_config_bytes(root, bytes)?,
                        None => fs::remove_file(&path)?,
                    }
                }
                Ok(())
            })();
            if let Err(restore) = restore {
                recovery_safe.set(false);
                bail!("setup failed: {error:#}; configuration recovery failed: {restore:#}");
            }
        }
        result
    })
}

fn setup_defaults(args: &SetupArgs, existing: Option<&SetupConfig>) -> Result<(String, Option<PathBuf>)> {
    if let Some(config) = existing {
        validate_config(config)?;
    }
    let name = args.name.clone().or_else(|| existing.map(|c| c.name.clone()))
        .context("first setup requires --name <lowercase-name>")?;
    validate_name(&name)?;
    let code_path = args.code_path.clone().or_else(|| existing.map(|c| c.code_path.clone()));
    Ok((name, code_path))
}

fn prepare_setup_host(
    changed: bool,
    terminate_sessions: bool,
    running: impl FnOnce() -> Result<bool>,
    stop: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if terminate_sessions {
        stop()?;
    } else if changed && running()? {
        bail!("host is running; changing setup requires stopping it first or --terminate-sessions (ends all scoped sessions)");
    }
    Ok(())
}

fn login() -> Result<()> {
    require_user_session()?;
    let root = deployment::data_root()?;
    let config = require_config(&root)?;
    validate_config(&config)?;
    let code = deployment::ensure_dependency(Role::Host, Some(&config.code_path), true)
        .context("configured VS Code tunnel CLI is unavailable or no longer trusted")?;
    login_if_needed(&code, &code_data_dir(&root))
}

fn require_user_session() -> Result<()> {
    let mut session = 0;
    ensure!(
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } != 0,
        "cannot inspect current Windows session"
    );
    ensure!(
        session != 0,
        "run this command in the intended signed-in Windows session, not Session 0"
    );
    Ok(())
}

fn start_host() -> Result<()> {
    require_user_session()?;
    let root = deployment::data_root()?;
    let config = require_config(&root)?;
    validate_config(&config)?;
    let code = deployment::ensure_dependency(Role::Host, Some(&config.code_path), true)
        .context("configured VS Code tunnel CLI is unavailable or no longer trusted")?;
    ensure!(
        inspect_auth(&code, &code_data_dir(&root))? == AuthState::LoggedIn,
        "isolated VS Code tunnel account is not logged in; run `arterm-host login`"
    );
    let exe = current_exe()?;
    ensure!(config.accepted_server_license_terms, "server license terms have not been accepted; run setup");
    register_task(&exe, &root)?;
    if host_is_running()? {
        println!("arTerm broker is already running; its owned task is enabled for logon and 10-minute checks. Task activity is separate from broker status.");
        return Ok(());
    }

    start_task(&exe, &root)?;
    println!(
        "arTerm host started for tunnel name {}.",
        config.name
    );
    Ok(())
}

fn doctor() -> Result<()> {
    let root = deployment::data_root()?;
    println!("{}", crate::host_task::diagnostic(&current_exe()?, &root)?);
    let config = require_config(&root)?;
    let mut problems = Vec::new();

    if let Err(error) = validate_config(&config) {
        problems.push(format!("setup configuration: {error:#}"));
    } else {
        println!("ok: setup configuration {}", config_path(&root).display());
    }

    let code = match deployment::ensure_dependency(Role::Host, Some(&config.code_path), true) {
        Ok(code) => {
            let mut command = Command::new(&code);
            command.arg("--version");
            match crate::transport::output_bounded(command, Duration::from_secs(10)) {
                Ok(output) if output.status.success() => {
                    let version = String::from_utf8_lossy(&output.stdout);
                    println!(
                        "ok: native signed VS Code CLI {}",
                        version.lines().next().unwrap_or("version unknown")
                    );
                }
                Ok(output) => problems.push(format!(
                    "VS Code CLI version check failed with status {}",
                    output.status
                )),
                Err(error) => problems.push(format!("VS Code CLI version check failed: {error}")),
            }
            Some(code)
        }
        Err(error) => {
            problems.push(format!(
                "VS Code CLI: {error:#}; rerun setup with --code-path <absolute-code-tunnel.exe>"
            ));
            None
        }
    };

    if let Some(code) = code {
        match inspect_auth(&code, &code_data_dir(&root)) {
            Ok(AuthState::LoggedIn) => println!("ok: isolated VS Code tunnel account is logged in"),
            Ok(AuthState::LoggedOut) => problems.push(
                "isolated VS Code tunnel account is logged out; run `arterm-host login`".into(),
            ),
            Err(error) => {
                problems.push(format!("isolated VS Code tunnel account check: {error:#}"))
            }
        }
        match tunnel_status(&code, &root) {
            Ok(output) if tunnel_running(&output) => println!("ok: VS Code tunnel is running"),
            Ok(output) => problems.push(format!(
                "VS Code tunnel is not connected (status {}); run `arterm-host start` and inspect {}",
                output.status,
                host_dir(&root).display()
            )),
            Err(error) => problems.push(format!("VS Code tunnel status check failed: {error:#}")),
        }
    }

    check_session(&root, &mut problems);
    match crate::host_task::diagnostic(&current_exe()?, &root) {
        Ok(message) => println!("{message}"),
        Err(error) => problems.push(format!("Task Scheduler: {error:#}")),
    }
    if let Err(error) = crate::host_task::require_enabled(&current_exe()?, &root) {
        problems.push(format!("Task Scheduler: {error:#}"));
    }

    match host_is_running() {
        Ok(true) => println!("ok: arTerm host broker is running"),
        Ok(false) => problems
            .push("arTerm host broker is not running; run `arterm-host start`".into()),
        Err(error) => problems.push(format!("host broker status check failed: {error:#}")),
    }

    if problems.is_empty() {
        println!("Doctor found no problems.");
        return Ok(());
    }
    for problem in &problems {
        eprintln!("problem: {problem}");
    }
    bail!("doctor found {} problem(s)", problems.len())
}

fn parse_setup_args(args: &[String]) -> Result<SetupArgs> {
    let mut name = None;
    let mut code_path = None;
    let mut no_download = false;
    let mut accept_server_license_terms = false;
    let mut terminate_sessions = false;
    let mut force_stop_host = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--name" => {
                index += 1;
                name = Some(args.get(index).context("--name requires a value")?.clone());
            }
            "--code-path" => {
                index += 1;
                code_path = Some(PathBuf::from(
                    args.get(index).context("--code-path requires a path")?,
                ));
            }
            "--no-download" => no_download = true,
            "--accept-server-license-terms" => accept_server_license_terms = true,
            "--terminate-sessions" => terminate_sessions = true,
            "--force-stop-host" => force_stop_host = true,
            other => bail!("unknown setup argument: {other}"),
        }
        index += 1;
    }
    Ok(SetupArgs {
        name,
        code_path,
        no_download,
        accept_server_license_terms,
        terminate_sessions,
        force_stop_host,
    })
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        (1..=40).contains(&name.len()),
        "name must contain 1..40 characters"
    );
    ensure!(
        name.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
        "name must contain only lowercase letters, digits, and hyphens"
    );
    ensure!(
        name.as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && name
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric),
        "name must start and end with a letter or digit"
    );
    Ok(())
}

fn validate_config(config: &SetupConfig) -> Result<()> {
    ensure!(config.schema == SCHEMA, "unsupported host setup schema");
    validate_name(&config.name)?;
    ensure!(
        config.code_path.is_absolute(),
        "configured code path must be absolute"
    );
    ensure!(
        config.accepted_server_license_terms,
        "server license terms were not explicitly accepted"
    );
    Ok(())
}

fn accept_license(accepted_by_flag: bool) -> Result<()> {
    if accepted_by_flag {
        return Ok(());
    }
    ensure!(
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        "server license acceptance is required; rerun interactively or pass --accept-server-license-terms"
    );
    eprintln!("VS Code tunnel server license terms must be accepted before setup can continue.");
    eprintln!("Read https://code.visualstudio.com/license/server before accepting.");
    eprint!("Type `accept` to accept the server license terms: ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    ensure!(
        answer.trim() == "accept",
        "server license terms were not accepted"
    );
    Ok(())
}

fn login_if_needed(code: &Path, cli_data: &Path) -> Result<()> {
    if inspect_auth(code, cli_data)? == AuthState::LoggedIn {
        println!("Isolated VS Code tunnel account is already logged in.");
        return Ok(());
    }
    ensure!(
        io::stdin().is_terminal(),
        "isolated VS Code tunnel login is required; rerun `arterm-host login` interactively"
    );
    let status = Command::new(code)
        .arg("--cli-data-dir")
        .arg(cli_data)
        .args(["tunnel", "user", "login", "--provider", "github"])
        .status()
        .context("launch isolated GitHub login")?;
    ensure!(
        status.success(),
        "isolated GitHub login failed with status {status}"
    );
    ensure!(
        inspect_auth(code, cli_data)? == AuthState::LoggedIn,
        "login completed without a cached isolated tunnel account"
    );
    Ok(())
}

fn inspect_auth(code: &Path, cli_data: &Path) -> Result<AuthState> {
    let mut command = Command::new(code);
    command
        .arg("--cli-data-dir")
        .arg(cli_data)
        .args(["tunnel", "user", "show"]);
    let output = crate::transport::output_bounded(command, Duration::from_secs(10))
        .context("inspect isolated VS Code tunnel account")?;
    let text = combined_output(&output);
    let lower = text.to_ascii_lowercase();
    let logged_out = [
        "not logged in",
        "not signed in",
        "no account",
        "logged out",
        "please log in",
        "not authenticated",
        "expired",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    if logged_out {
        return Ok(AuthState::LoggedOut);
    }
    ensure!(
        output.status.success() && !text.trim().is_empty(),
        "account inspection failed with status {}: {}",
        output.status,
        text.trim()
    );
    ensure!(
        lower.contains("github"),
        "isolated tunnel account is not identified as GitHub; no account was changed"
    );
    Ok(AuthState::LoggedIn)
}

fn isolated_code_command(code: &Path, root: &Path) -> Command {
    let mut command = Command::new(code);
    command.arg("--cli-data-dir").arg(code_data_dir(root));
    command
}

fn tunnel_status(code: &Path, root: &Path) -> Result<Output> {
    let mut command = isolated_code_command(code, root);
    command.args(["tunnel", "status"]);
    crate::transport::output_bounded(command, Duration::from_secs(5))
        .context("inspect isolated VS Code tunnel status")
}

fn tunnel_running(output: &Output) -> bool {
    output.status.success()
        && serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|v| v.get("tunnel").cloned())
            .is_some_and(|v| !v.is_null())
}

fn report_registration(config: &SetupConfig, root: &Path, exe: &Path) {
    println!("Configured isolated VS Code tunnel name: {}", config.name);
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last = None;
    while Instant::now() < deadline {
        match tunnel_status(&config.code_path, root) {
            Ok(output) => {
                let connected = tunnel_running(&output);
                last = Some(output);
                if connected {
                    break;
                }
            }
            Err(error) => {
                eprintln!("Tunnel status is not available yet: {error:#}");
                break;
            }
        }
        thread::sleep(Duration::from_millis(400));
    }

    let tunnel_id = last
        .as_ref()
        .and_then(|output| exact_tunnel_id(&combined_output(output)))
        .or_else(|| registration_id(root, &config.name));
    if let Some(tunnel_id) = tunnel_id {
        println!("Current tunnel ID (diagnostic only): {tunnel_id}");
    }
    println!("The client discovers the current tunnel ID from this name on every connection.");
    println!("{}", registration_command(&config.name, exe));
    println!(
        "Run `arterm doctor {}` on the client to confirm discovery and reachability.",
        config.name
    );
}

fn registration_command(name: &str, exe: &Path) -> String {
    format!("arterm.exe add {name} --tunnel {name} --host-path '{}'",
        exe.to_string_lossy().replace('\'', "''"))
}

fn registration_id(root: &Path, expected_name: &str) -> Option<String> {
    let path = code_data_dir(root).join("code_tunnel.json");
    if !path.exists() { return None; }
    let result = (|| -> Result<String> {
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        ensure!(value.get("name").and_then(|v| v.as_str()) == Some(expected_name),
            "tunnel registration name does not match host configuration");
        let id = value.get("id").and_then(|v| v.as_str()).context("missing tunnel ID")?;
        let cluster = value.get("cluster").and_then(|v| v.as_str()).context("missing tunnel cluster")?;
        ensure!([id,cluster].iter().all(|part| !part.is_empty() &&
            part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')), "invalid tunnel identifier");
        Ok(format!("{id}.{cluster}"))
    })();
    match result {
        Ok(id) => Some(id),
        Err(error) => {
            eprintln!("Cannot use cached tunnel registration: {error:#}");
            None
        }
    }
}

fn exact_tunnel_id(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (label, value) = line.split_once(':')?;
        let normalized = label
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-', '_'], "");
        if normalized != "tunnelid" && normalized != "devtunnelid" {
            return None;
        }
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            None
        } else {
            Some(value.to_owned())
        }
    })
}

fn register_task(exe: &Path, root: &Path) -> Result<()> {
    let task = crate::host_task::Task::current(exe, root)?;
    task.register()?;
    deployment::remove_legacy_startup(&deployment::host_runtime_paths(exe)?)?;
    task.operation("enable")?;
    Ok(())
}

pub(crate) fn configured_for_start(root: &Path) -> Result<bool> {
    check_start_prerequisites(root, |config| {
        let code = deployment::ensure_dependency(Role::Host, Some(&config.code_path), true)?;
        Ok(inspect_auth(&code, &code_data_dir(root))? == AuthState::LoggedIn)
    })
}

fn check_start_prerequisites(root: &Path, probe: impl FnOnce(&SetupConfig) -> Result<bool>) -> Result<bool> {
    let Some(config) = load_config(root)? else { return Ok(false); };
    if !config.accepted_server_license_terms { return Ok(false); }
    validate_config(&config)?;
    ensure!(probe(&config)?, "isolated VS Code tunnel account is not logged in or has expired; run `arterm-host login` explicitly");
    Ok(true)
}

fn start_task(exe: &Path, root: &Path) -> Result<()> {
    crate::host_task::Task::current(exe, root)?.start()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if host_is_running()? {
            break;
        }
        if Instant::now() >= deadline {
            bail!("scheduled host did not become responsive; inspect Task Scheduler history and {}", host_dir(root).display());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn host_is_running() -> Result<bool> {
    let mut command = Command::new(current_exe()?);
    command.args(["status", "--json"]).creation_flags(CREATE_NO_WINDOW);
    let output = crate::transport::output_bounded(command, Duration::from_secs(5))
        .context("query arTerm host status")?;
    if !output.status.success() {
        return Ok(false);
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(value["ok"] == true && value["sessions"].is_u64())
}

fn append_log(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open log {}", path.display()))
}

fn host_dir(root: &Path) -> PathBuf {
    root.join("host")
}

fn code_data_dir(root: &Path) -> PathBuf {
    root.join("code-cli")
}

fn config_path(root: &Path) -> PathBuf {
    host_dir(root).join("setup.json")
}

fn load_config(root: &Path) -> Result<Option<SetupConfig>> {
    let path = config_path(root);
    if !path.try_exists().context("inspect host setup configuration")? {
        return Ok(None);
    }
    let config = serde_json::from_slice(&fs::read(&path).context("read host setup configuration")?)
        .context("invalid host setup configuration")?;
    Ok(Some(config))
}

fn require_config(root: &Path) -> Result<SetupConfig> {
    load_config(root)?.context("host is not configured; run `arterm-host setup --name <name>`")
}

fn save_config(root: &Path, config: &SetupConfig) -> Result<()> {
    validate_config(config)?;
    write_config_bytes(root, &serde_json::to_vec_pretty(config)?)
}

fn write_config_bytes(root: &Path, bytes: &[u8]) -> Result<()> {
    let path = config_path(root);
    let parent = path.parent().context("host setup path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!("setup-{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let from = wide(temp.as_os_str());
        let to = wide(path.as_os_str());
        ensure!(
            unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } != 0,
            "replace host setup configuration: {}",
            io::Error::last_os_error()
        );
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn save_config_if_changed(root: &Path, config: &SetupConfig) -> Result<()> {
    if load_config(root)?.as_ref() != Some(config) {
        save_config(root, config)?;
    }
    Ok(())
}

fn check_session(root: &Path, problems: &mut Vec<String>) {
    let mut session = 0;
    let found = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } != 0;
    if !found {
        problems.push(format!(
            "cannot resolve current Windows session: {}",
            io::Error::last_os_error()
        ));
        return;
    }
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "<unknown>".into());
    println!(
        "ok: per-user state for {user}, Windows session {session}, root {}",
        root.display()
    );
    let active = unsafe { WTSGetActiveConsoleSessionId() };
    if session == 0 {
        problems
            .push("Session 0 is not supported; run setup in your signed-in Windows session".into());
    } else if active != u32::MAX && active != session {
        println!("info: Windows session {session} differs from console session {active}; an RDP session is supported.");
    }
}

fn current_exe() -> Result<PathBuf> {
    Ok(std::env::current_exe()?.canonicalize()?)
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn help_requested(args: &[String]) -> bool {
    args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h")
}

fn ensure_help_only(args: &[String], print_help: fn()) -> Result<()> {
    if args.is_empty() {
        return Ok(());
    }
    if help_requested(args) {
        print_help();
        return Ok(());
    }
    bail!("unexpected arguments: {}", args.join(" "))
}

fn print_setup_help() {
    println!(
        "arterm-host setup [--name <lowercase-name>] [--code-path <absolute-code-tunnel.exe>] [--no-download] [--accept-server-license-terms] [--terminate-sessions] [--force-stop-host]\nExisting name and Code CLI path are retained unless explicitly supplied. First setup requires --name.\n--terminate-sessions gracefully stops this user/logon/data-root host and ends its sessions.\n--force-stop-host bypasses graceful shutdown and ends ALL sessions of matching host executable paths for the current user/logon, including other data roots. Other installations/users are not killed.\nConfigures an isolated GitHub-backed VS Code tunnel, per-user startup, and the native host."
    );
}

fn print_login_help() {
    println!(
        "arterm-host login\nLogs in the configured isolated VS Code tunnel account using GitHub."
    );
}

fn print_start_help() {
    println!("arterm-host start\nEnables the per-user task and immediately runs its hidden ensure-running check in this interactive session. Later checks run at logon and every 10 minutes. Accounts are unchanged; Scheduler policy/permission errors are not bypassed.");
}

fn print_doctor_help() {
    println!("arterm-host doctor\nChecks setup, native dependency trust/version, isolated auth/tunnel status, Windows session, startup, and broker status.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_readiness_separates_missing_setup_or_license_from_expired_auth() {
        let root = std::env::temp_dir().join(format!("arterm-readiness-policy-{}", uuid::Uuid::now_v7()));
        assert!(!check_start_prerequisites(&root, |_| panic!("no setup must not probe auth")).unwrap());
        fs::create_dir_all(host_dir(&root)).unwrap();
        let mut config = SetupConfig { schema: SCHEMA, name: "configured".into(), code_path: r"C:\fixture\code-tunnel.exe".into(), accepted_server_license_terms: false };
        fs::write(config_path(&root), serde_json::to_vec(&config).unwrap()).unwrap();
        assert!(!check_start_prerequisites(&root, |_| panic!("missing license must not probe auth")).unwrap());
        config.accepted_server_license_terms = true;
        save_config(&root, &config).unwrap();
        let original = fs::read(config_path(&root)).unwrap();
        let expired = check_start_prerequisites(&root, |_| Ok(false)).unwrap_err();
        assert!(format!("{expired:#}").contains("arterm-host login"));
        let network = check_start_prerequisites(&root, |_| bail!("network probe failure")).unwrap_err();
        assert!(format!("{network:#}").contains("network probe failure"));
        assert!(check_start_prerequisites(&root, |_| Ok(true)).unwrap());
        assert_eq!(fs::read(config_path(&root)).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    fn setup_task_fixture(root: &Path, enabled: bool) -> crate::host_task::Task {
        crate::host_task::Task {
            name: "isolated-setup-boundary".into(), sid: "S-1-12-1-1-2-3-4".into(),
            exe: r"C:\fixture\arterm-host.exe".into(), launcher: r"C:\Windows\System32\wscript.exe".into(),
            root: root.to_string_lossy().into_owned(), enabled, running: false,
            snapshot_xml: Some("verified fixture XML".into()),
        }
    }

    #[test]
    fn setup_keeps_scheduler_paused_through_stop_login_save_and_registration() {
        use std::cell::{Cell, RefCell};
        let root = std::env::temp_dir().join(format!("arterm-setup-boundary-{}", uuid::Uuid::now_v7()));
        let old = SetupConfig { schema: SCHEMA, name: "old".into(), code_path: r"C:\fixture\code-tunnel.exe".into(), accepted_server_license_terms: true };
        let mut new = old.clone();
        new.name = "new".into();
        save_config(&root, &old).unwrap();
        let task = setup_task_fixture(&root, true);
        let enabled = Cell::new(true);
        let broker_name = RefCell::new(Some("old".to_owned()));
        let ticks = RefCell::new(Vec::new());
        let tick = |stage: &str| {
            ticks.borrow_mut().push(stage.to_owned());
            if enabled.get() && broker_name.borrow().is_none() {
                *broker_name.borrow_mut() = Some(require_config(&root).unwrap().name);
            }
        };
        setup_transaction(&root, &[task], None, |_, op| {
            match op {
                "disable" => enabled.set(false),
                "register" => assert!(!enabled.get(), "preflight registration must not enable the task"),
                "enable" => { assert_eq!(require_config(&root)?.name, "new"); enabled.set(true); tick("commit"); },
                other => panic!("unexpected operation {other}"),
            }
            Ok(())
        }, || {
            prepare_setup_host(true, true, || panic!("explicit stop needs no status query"), || {
                *broker_name.borrow_mut() = None;
                tick("stop");
                Ok(())
            })?;
            tick("login");
            assert!(broker_name.borrow().is_none());
            save_config_if_changed(&root, &new)?;
            tick("save");
            tick("registration");
            assert!(!enabled.get() && broker_name.borrow().is_none());
            Ok(())
        }).unwrap();
        assert_eq!(broker_name.borrow().as_deref(), Some("new"));
        assert_eq!(*ticks.borrow(), ["stop", "login", "save", "registration", "commit"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn setup_failure_restores_exact_config_before_previous_task_state() {
        use std::cell::Cell;
        for was_enabled in [false, true] {
            for failure in ["login", "registration"] {
                let root = std::env::temp_dir().join(format!("arterm-setup-rollback-{}", uuid::Uuid::now_v7()));
                let old = SetupConfig { schema: SCHEMA, name: "old".into(), code_path: r"C:\fixture\code-tunnel.exe".into(), accepted_server_license_terms: true };
                save_config(&root, &old).unwrap();
                let original = [fs::read(config_path(&root)).unwrap(), b"\n".to_vec()].concat();
                fs::write(config_path(&root), &original).unwrap();
                fs::write(root.join("credentials.fixture"), b"untouched").unwrap();
                let enabled = Cell::new(was_enabled);
                let task = setup_task_fixture(&root, was_enabled);
                let result = setup_transaction(&root, &[task], None, |_, op| {
                    match op {
                        "disable" => enabled.set(false),
                        "register" => assert!(!enabled.get()),
                        "restore" => assert_eq!(fs::read(config_path(&root))?, original),
                        "enable" => { assert_eq!(fs::read(config_path(&root))?, original); enabled.set(true); },
                        other => panic!("unexpected operation {other}"),
                    }
                    Ok(())
                }, || {
                    assert!(!enabled.get());
                    if failure == "registration" {
                        let mut new = old.clone();
                        new.name = "new".into();
                        save_config(&root, &new)?;
                    }
                    bail!("injected {failure} failure")
                });
                assert!(result.is_err());
                assert_eq!(enabled.get(), was_enabled);
                assert_eq!(fs::read(config_path(&root)).unwrap(), original);
                assert_eq!(fs::read(root.join("credentials.fixture")).unwrap(), b"untouched");
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn setup_default_refusal_never_stops_and_failed_config_recovery_stays_disabled() {
        use std::cell::Cell;
        let root = std::env::temp_dir().join(format!("arterm-setup-refusal-{}", uuid::Uuid::now_v7()));
        let old = SetupConfig { schema: SCHEMA, name: "old".into(), code_path: r"C:\fixture\code-tunnel.exe".into(), accepted_server_license_terms: true };
        save_config(&root, &old).unwrap();
        let task = setup_task_fixture(&root, true);
        let enabled = Cell::new(true);
        let backend = |_: &crate::host_task::Task, op: &str| {
            match op {
                "disable" | "restore" => enabled.set(false),
                "enable" => enabled.set(true),
                "register" => assert!(!enabled.get()),
                _ => panic!("unexpected operation"),
            }
            Ok(())
        };
        let result = setup_transaction(&root, &[task.clone()], None, backend, || {
            prepare_setup_host(true, false, || Ok(true), || panic!("default setup must not stop sessions"))
        });
        assert!(result.is_err() && enabled.get());
        let result = setup_transaction(&root, &[task], None, backend, || {
            fs::remove_file(config_path(&root))?;
            fs::create_dir(config_path(&root))?;
            bail!("injected save/registration failure")
        });
        assert!(format!("{:#}", result.unwrap_err()).contains("configuration recovery failed"));
        assert!(!enabled.get(), "unsafe configuration recovery must not re-enable checks");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn setup_preserves_saved_defaults_and_state_on_repeated_runs() {
        let root = std::env::temp_dir().join(format!("arterm-setup-preserve-{}", uuid::Uuid::now_v7()));
        let config = SetupConfig {
            schema: SCHEMA,
            name: "registered-box".into(),
            code_path: PathBuf::from(r"C:\custom-tools\code-tunnel.exe"),
            accepted_server_license_terms: true,
        };
        save_config(&root, &config).unwrap();
        // Unchanged setup must retain the original bytes, not merely equivalent JSON.
        let original = format!("{}\n", String::from_utf8(fs::read(config_path(&root)).unwrap()).unwrap());
        fs::write(config_path(&root), &original).unwrap();
        let files = [
            r"code-cli\code_tunnel.json", r"code-cli\token.json",
            r"client\config.json", r"client\sessions\target-id\saved.dpapi",
            r"client\sessions\target-id\ref-work.json", r"host\requests\saved.dpapi",
            r"identity.cer",
        ];
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file.as_bytes()).unwrap();
        }
        for _ in 0..2 {
            let args = parse_setup_args(&[]).unwrap();
            let existing = load_config(&root).unwrap().unwrap();
            let (name, path) = setup_defaults(&args, Some(&existing)).unwrap();
            assert_eq!(name, config.name);
            assert_eq!(path.as_ref(), Some(&config.code_path));
            assert!(!args.terminate_sessions);
            save_config_if_changed(&root, &existing).unwrap();
            assert_eq!(fs::read(config_path(&root)).unwrap(), original.as_bytes());
            for file in files {
                assert_eq!(fs::read(root.join(file)).unwrap(), file.as_bytes());
            }
        }
        let args = parse_setup_args(&["--name".into(), "explicit-name".into()]).unwrap();
        let (name, path) = setup_defaults(&args, Some(&config)).unwrap();
        assert_eq!(name, "explicit-name");
        assert_eq!(path.as_ref(), Some(&config.code_path));
        let args = parse_setup_args(&["--code-path".into(), r"C:\other\code-tunnel.exe".into()]).unwrap();
        let (name, path) = setup_defaults(&args, Some(&config)).unwrap();
        assert_eq!(name, config.name);
        assert_eq!(path, Some(PathBuf::from(r"C:\other\code-tunnel.exe")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_saved_setup_is_reported_without_resetting_state() {
        let root = std::env::temp_dir().join(format!("arterm-invalid-setup-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(host_dir(&root)).unwrap();
        let bytes = b"{not valid setup";
        fs::write(config_path(&root), bytes).unwrap();
        assert!(load_config(&root).is_err());
        assert_eq!(fs::read(config_path(&root)).unwrap(), bytes);
        let config = SetupConfig {
            schema: SCHEMA + 1,
            name: "retained-name".into(),
            code_path: PathBuf::from(r"C:\saved\code-tunnel.exe"),
            accepted_server_license_terms: true,
        };
        assert!(setup_defaults(&parse_setup_args(&[]).unwrap(), Some(&config)).is_err());
        assert_eq!(fs::read(config_path(&root)).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn setup_shutdown_is_opt_in_and_failures_abort() {
        use std::cell::Cell;
        let stops = Cell::new(0);
        let stop = || { stops.set(stops.get() + 1); Ok(()) };
        prepare_setup_host(false, false, || Ok(true), stop).unwrap();
        assert!(prepare_setup_host(true, false, || Ok(true), stop).is_err());
        prepare_setup_host(true, false, || Ok(false), stop).unwrap();
        assert_eq!(stops.get(), 0);
        let args = parse_setup_args(&["--terminate-sessions".into()]).unwrap();
        prepare_setup_host(false, args.terminate_sessions, || Ok(true), stop).unwrap();
        assert_eq!(stops.get(), 1);
        let args = parse_setup_args(&["--force-stop-host".into()]).unwrap();
        assert!(args.force_stop_host);
        assert!(!args.terminate_sessions);
        prepare_setup_host(false, args.terminate_sessions || args.force_stop_host,
            || bail!("force-stop must not require a responsive status query"), stop).unwrap();
        assert_eq!(stops.get(), 2);
        assert!(prepare_setup_host(true, true, || Ok(true), || bail!("mock stop failure")).is_err());
        assert!(prepare_setup_host(true, false, || bail!("mock status failure"), stop).is_err());
        assert_eq!(stops.get(), 2);
    }

    #[test]
    fn successful_status_exit_does_not_imply_a_running_tunnel() {
        use std::os::windows::process::ExitStatusExt;
        let output = |json: &str| Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: json.as_bytes().to_vec(),
            stderr: Vec::new(),
        };
        assert!(!tunnel_running(&output(
            r#"{"tunnel":null,"service_installed":false}"#
        )));
        assert!(tunnel_running(&output(r#"{"tunnel":{"name":"box"}}"#)));
        assert!(!tunnel_running(&output("invalid")));
    }

    #[test]
    fn names_are_strict_and_bounded() {
        for valid in ["a", "box-7", "a234567890123456789012345678901234567890"] {
            validate_name(valid).unwrap();
        }

        for invalid in [
            "",
            "-box",
            "box-",
            "Box",
            "box_name",
            "a2345678901234567890123456789012345678901",
        ] {
            assert!(validate_name(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn registration_keeps_the_friendly_name_when_the_service_id_changes() {
        let root = std::env::temp_dir().join(format!("arterm-registration-{}",uuid::Uuid::now_v7()));
        fs::create_dir_all(code_data_dir(&root)).unwrap();
        let path = code_data_dir(&root).join("code_tunnel.json");
        fs::write(&path,r#"{"name":"dev01","id":"actual-id","cluster":"usw2"}"#).unwrap();
        assert_eq!(registration_id(&root,"dev01").as_deref(),Some("actual-id.usw2"));
        assert!(registration_id(&root,"other").is_none());
        let exe = Path::new(r"C:\owner's tools\arterm-host.exe");
        let command = registration_command("dev01", exe);
        assert_eq!(command,
            "arterm.exe add dev01 --tunnel dev01 --host-path 'C:\\owner''s tools\\arterm-host.exe'");
        fs::write(&path,r#"{"name":"dev01","id":"replacement-id","cluster":"use2"}"#).unwrap();
        assert_eq!(registration_id(&root,"dev01").as_deref(),Some("replacement-id.use2"));
        assert_eq!(registration_command("dev01", exe), command);
        fs::remove_file(path).unwrap();
        fs::remove_dir(code_data_dir(&root)).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn setup_parser_requires_name_and_absolute_code_path() {
        assert!(setup_defaults(&parse_setup_args(&[]).unwrap(), None).is_err());
        let parsed = parse_setup_args(&[
            "--name".into(),
            "box-1".into(),
            "--code-path".into(),
            r"C:\Program Files\Microsoft VS Code\bin\code-tunnel.exe".into(),
            "--no-download".into(),
            "--accept-server-license-terms".into(),
        ])
        .unwrap();
        assert_eq!(parsed.name.as_deref(), Some("box-1"));
        assert!(parsed.code_path.unwrap().is_absolute());
        assert!(parsed.no_download);
        assert!(parsed.accept_server_license_terms);
    }

    #[test]
    fn tunnel_id_requires_an_explicit_label() {
        assert_eq!(
            exact_tunnel_id("Tunnel ID: exact.wus2"),
            Some("exact.wus2".into())
        );
        assert_eq!(
            exact_tunnel_id("Dev tunnel id: exact-id"),
            Some("exact-id".into())
        );
        assert_eq!(exact_tunnel_id("Machine name: friendly-name"), None);
        assert_eq!(
            exact_tunnel_id("https://vscode.dev/tunnel/friendly-name"),
            None
        );
    }

    #[test]
    fn missing_setup_is_non_mutating_for_tunnel_start() {
        let root =
            std::env::temp_dir().join(format!("devbox-host-setup-test-{}", uuid::Uuid::now_v7()));
        let old = std::env::var_os("VSTERM_REMOTE_HOME");
        assert_eq!(handle(&["setup".into(), "--help".into()]).unwrap(), Some(0));
        assert!(!root.exists());
        std::env::set_var("VSTERM_REMOTE_HOME", &root);
        assert!(start_tunnel().unwrap().is_none());
        assert!(!root.exists());
        match old {
            Some(value) => std::env::set_var("VSTERM_REMOTE_HOME", value),
            None => std::env::remove_var("VSTERM_REMOTE_HOME"),
        }
    }
}
