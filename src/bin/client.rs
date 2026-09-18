#[path = "../client_config.rs"]
mod client_config;
#[path = "../client_protocol.rs"]
mod client_protocol;
#[path = "../client_output.rs"]
mod client_output;

use anyhow::{bail, ensure, Context, Result};
use arterm::statusln as eprintln;
use client_config::{ClientConfig, Target};
use arterm::{
    console::{Console, Terminal},
    deployment::{self, Role},
    engine::{End, Engine},
    local_control::{self, Operation, Owner},
    store::{self, SessionReference, Store},
    transport::{authenticated, Forward, LoginRequired, TunnelLink},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};
use uuid::Uuid;

const DEFAULT_RETRIES: u32 = 5;

#[derive(Default)]
struct ConnectionFlags {
    address: Option<String>,
    stdio: bool,
    retries: Option<u32>,
    shell: Option<String>,
    cwd: Option<String>,
}

fn usage() {
    println!(
        "arTerm client {}\n\
Usage:\n\
  arterm setup [--devtunnel-path PATH] [--no-download]\n\
  arterm login | logout\n\
  arterm add ALIAS --tunnel NAME-OR-ID --host-path ABSOLUTE\n\
  arterm list\n\
  arterm remove ALIAS\n\
  arterm connect ALIAS [REF] [--shell EXE] [--cwd PATH] [--retries 0..20]\n\
  arterm list --client [--json] | list --server MACHINE [--json]\n\
  arterm send MACHINE SESSION --command TEXT [--command-id UUID] [--timeout 60s] [--wait] [--json]\n\
  arterm read MACHINE SESSION [--lines N | --command-id UUID] [--json]\n\
  arterm interrupt MACHINE SESSION [--json] | detach MACHINE SESSION [--json]\n\
  arterm terminate MACHINE SESSION [--json]\n\
  arterm doctor [ALIAS]\n\
  arterm --help | --version\n\n\
Without REF, connect only prints a reusable command. Names are not passwords.\n\
Control commands print readable results by default; use --json for structured automation output.\n\
Commands require a compatible, command-enabled PowerShell session. Existing sessions are not retrofitted.\n\
Send waits up to 30s for readiness by default; --timeout overrides it. --wait requires --timeout and shares its budget with completion.\n\
Command IDs are retained for the session lifetime; capacity is 256, with no silent eviction or replacement.\n\
Local IPC requires OS trust, the pinned certificate, and byte-identical client builds; cross-elevation is rejected.\n\
Ctrl+] detaches without terminating the remote session.",
        env!("CARGO_PKG_VERSION")
    );
}

fn value(args: &[String], index: &mut usize, flag: &str) -> Result<String> {
    *index += 1;
    let value = args
        .get(*index)
        .with_context(|| format!("{flag} requires a value"))?;
    ensure!(!value.is_empty(), "{flag} requires a non-empty value");
    Ok(value.clone())
}

fn parse_connection_flags(args: &[String], allow_create: bool) -> Result<ConnectionFlags> {
    let mut flags = ConnectionFlags::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--address" => flags.address = Some(value(args, &mut index, "--address")?),
            "--stdio" => flags.stdio = true,
            "--retries" => {
                let retries: u32 = value(args, &mut index, "--retries")?
                    .parse()
                    .context("invalid retry count")?;
                ensure!(retries <= 20, "retry count must be 0..20");
                flags.retries = Some(retries);
            }
            "--shell" if allow_create => flags.shell = Some(value(args, &mut index, "--shell")?),
            "--cwd" if allow_create => {
                let cwd = value(args, &mut index, "--cwd")?;
                ensure!(Path::new(&cwd).is_absolute(), "remote cwd must be absolute");
                flags.cwd = Some(cwd);
            }
            other => bail!("unknown option: {other}"),
        }
        index += 1;
    }
    if let Some(address) = &flags.address {
        let endpoint: std::net::SocketAddr = address.parse().context("invalid --address")?;
        ensure!(endpoint.ip().is_loopback(), "--address must be loopback");
    }
    Ok(flags)
}

fn devtunnel_path(config: &ClientConfig) -> Result<&Path> {
    config
        .devtunnel_path
        .as_deref()
        .context("client is not set up; run arterm setup")
}

fn run_vendor(path: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new(path)
        .args(args)
        .status()
        .with_context(|| format!("start {}", path.display()))?;
    ensure!(status.success(), "{} exited with {status}", path.display());
    Ok(())
}

fn setup(root: &Path, args: &[String]) -> Result<()> {
    let mut override_path = None;
    let mut no_download = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--devtunnel-path" => {
                override_path = Some(PathBuf::from(value(args, &mut index, "--devtunnel-path")?))
            }
            "--no-download" => no_download = true,
            other => bail!("unknown setup option: {other}"),
        }
        index += 1;
    }
    if let Some(path) = &override_path {
        ensure!(path.is_absolute(), "--devtunnel-path must be absolute");
    }
    let selected =
        deployment::ensure_dependency(Role::Client, override_path.as_deref(), no_download)?;
    let mut config = client_config::load(root)?;
    config.devtunnel_path = Some(selected.clone());
    client_config::save(root, &config)?;
    let signed_in = authenticated(&selected)?;
    if !signed_in {
        run_vendor(&selected, &["user", "login", "--github"])?;
        ensure!(authenticated(&selected)?, "login did not establish usable devtunnel credentials; run the client's login command again");
    }
    println!("Client configured with {}", selected.display());
    Ok(())
}

fn add_target(root: &Path, args: &[String]) -> Result<()> {
    let alias = args.first().context("add requires an alias")?;
    let mut tunnel = None;
    let mut host_path = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--tunnel" => tunnel = Some(value(args, &mut index, "--tunnel")?),
            "--host-path" => host_path = Some(value(args, &mut index, "--host-path")?),
            other => bail!("unknown add option: {other}"),
        }
        index += 1;
    }
    let mut config = client_config::load(root)?;
    let target = client_config::add(
        &mut config,
        alias,
        tunnel.as_deref().context("add requires --tunnel")?,
        host_path.as_deref().context("add requires --host-path")?,
    )?;
    client_config::save(root, &config)?;
    println!(
        "Added {alias} -> {} ({})",
        target.tunnel_id, target.host_path
    );
    Ok(())
}

fn list_targets(root: &Path, args: &[String]) -> Result<()> {
    ensure!(args.is_empty(), "list takes no parameters");
    let config = client_config::load(root)?;
    if config.targets.is_empty() {
        println!("No registered boxes.");
    } else {
        for (alias, target) in config.targets {
            println!("{alias}\t{}\t{}", target.tunnel_id, target.host_path);
        }
    }
    Ok(())
}

fn remove_target(root: &Path, args: &[String]) -> Result<()> {
    ensure!(args.len() == 1, "remove requires exactly one alias");
    let mut config = client_config::load(root)?;
    client_config::remove(&mut config, root, &args[0])?;
    client_config::save(root, &config)?;
    println!("Removed alias {}. Recovery records were retained.", args[0]);
    Ok(())
}

fn connect_link(
    config: &ClientConfig,
    target: &Target,
    address: Option<&str>,
) -> Result<TunnelLink> {
    if let Some(address) = address {
        return TunnelLink::connect(address, &target.host_path, None);
    }
    let (forward, address) = Forward::start(devtunnel_path(config)?, &target.tunnel_id)?;
    TunnelLink::connect(&address, &target.host_path, Some(forward))
}

fn quote_command_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn session_command(alias: &str, reference: &SessionReference, retries: u32, flags: &ConnectionFlags, _create: bool) -> Result<String> {
    let exe = std::env::current_exe()?;
    let mut parts = vec![
        "&".into(),
        quote_command_arg(&exe.to_string_lossy()),
        "connect".into(),
        quote_command_arg(alias),
        reference.as_str().to_owned(),
        "--retries".into(),
        retries.to_string(),
    ];
    if let Some(address) = &flags.address {
        parts.extend(["--address".into(), quote_command_arg(address)]);
    }
    if flags.stdio {
        parts.push("--stdio".into());
    }
    if let Some(shell) = &flags.shell {
        parts.extend(["--shell".into(), quote_command_arg(shell)]);
    }
    if let Some(cwd) = &flags.cwd {
        parts.extend(["--cwd".into(), quote_command_arg(cwd)]);
    }
    Ok(parts.join(" "))
}

fn print_recovery(id: Uuid, command: &str) {
    eprintln!("[session] Resume GUID: {id}");
    eprintln!("[session] Resume command: {command}");
}

fn run_session(
    root: &Path,
    alias: &str,
    reference: &SessionReference,
    create: bool,
    flags: ConnectionFlags,
) -> Result<u32> {
    let console = Console::new(flags.stdio)?;
    let config = client_config::load(root)?;
    let target = client_config::target(&config, alias)?.clone();
    let retries = flags.retries.unwrap_or(DEFAULT_RETRIES);
    let state_dir = client_config::state_dir(root, &target);
    let (store, state) = Store::resolve(&state_dir, &target.target_id, reference, create, flags.shell.as_deref(), flags.cwd.as_deref())?;
    let store = std::sync::Arc::new(store);
    let service_store = store.clone();
    let id = state.id;
    let command = session_command(alias, reference, retries, &flags, create)?;
    eprintln!("[session] GUID {id}; Ctrl+] detaches without killing the remote shell.");
    print_recovery(id, &command);
    let mut owner = Owner::start(root, &target.target_id, alias, id, state.reference.clone())?;
    let snapshot = std::sync::Arc::new(std::sync::Mutex::new(state.clone()));
    let service_snapshot = snapshot.clone();
    let service_config = config.clone();
    let service_target = target.clone();
    let service_address = flags.address.clone();
    owner.set_service(std::sync::Arc::new(move |_operation_id, action| {
        ensure!(matches!(action, Operation::Terminate), "invalid management operation");
        let state = service_snapshot.lock().unwrap().clone();
        ensure!(!state.ended && state.token.is_some(), "session is not authorized or has ended");
        let mut link = connect_link(&service_config, &service_target, service_address.as_deref())?;
        let confirmed = client_protocol::terminate(&mut link, &state)?;
        let mut snapshot = service_snapshot.lock().unwrap();
        snapshot.ended = true;
        service_store.save(&snapshot)?;
        Ok(serde_json::json!({"status":if confirmed { "terminated" } else { "termination_accepted" }, "session_id":state.id}))
    }));
    let mut engine = if Uuid::parse_str(reference.as_str()).is_ok()
        && state.reference.is_none() && state.token.is_some() {
        Engine::resume_guid(state)
    } else {
        Engine::new(state)
    };
    let mut terminal = owner.terminal(console);
    let result = (|| -> Result<u32> {
        for attempt in 0..=retries {
            if owner.detached() { return Ok(0); }
            if attempt > 0 {
                owner.set_state("reconnecting");
                terminal.reading(false);
                let jitter = u16::from_le_bytes(store::random_claim()?[..2].try_into().unwrap())
                    as u64
                    % 251;
                let delay = (500u64 * (1u64 << (attempt - 1).min(6))).min(15_000) + jitter;
                eprintln!("[session] Reconnecting {attempt}/{retries} in {delay}ms; reusing {id}");
                for _ in 0..(delay / 25 + 1) {
                    if owner.detached() { return Ok(0); }
                    thread::sleep(Duration::from_millis(25));
                }
            }
            let mut link = match connect_link(&config, &target, flags.address.as_deref()) {
                Ok(link) => link,
                Err(error) => {
                    eprintln!("[session] Connection failed: {error:#}");
                    print_recovery(id, &command);
                    if error.is::<LoginRequired>() {
                        owner.set_state("auth_required");
                        return Err(error);
                    }
                    continue;
                }
            };
            match engine.run(&mut link, &mut terminal, &mut |state| {
                let mut snapshot = snapshot.lock().unwrap();
                let mut updated = state.clone();
                updated.ended |= snapshot.ended;
                store.save(&updated)?;
                *snapshot = updated;
                Ok(())
            })? {
                End::Detached => {
                    eprintln!("[session] Detached; remote session retained.");
                    return Ok(0);
                }
                End::Exited(code) => {
                    eprintln!("[session] Remote exit code: {code}");
                    return Ok(code);
                }
                End::Disconnected => {
                    owner.set_state("reconnecting");
                    terminal.reading(false);
                    eprintln!("[session] Attachment lost; remote session was not terminated.");
                    print_recovery(id, &command);
                }
            }
        }
        bail!("retry limit reached; recovery record retained for {id}")
    })();
    terminal.reading(false);
    drop(terminal);
    if result.is_err() {
        eprintln!("[session] Automatic recovery stopped. The saved GUID does not guarantee the remote session is still alive.");
        print_recovery(id, &command);
    }
    result
}

fn terminate_session(root: &Path, args: &[String]) -> Result<u32> {
    ensure!(args.len() >= 2, "terminate requires MACHINE SESSION");
    let alias = &args[0];
    let reference = SessionReference::parse(&args[1])?;
    let (flags, json) = output_connection_flags(&args[2..])?;
    ensure!(
        flags.retries.is_none(),
        "terminate does not accept --retries"
    );
    let config = client_config::load(root)?;
    let target = client_config::target(&config, alias)?.clone();
    let owners = local_control::discover(root)?.into_iter().filter(|owner| owner.target_id == target.target_id
        && (owner.reference.as_deref() == Some(reference.as_str()) || owner.session_id.to_string() == reference.as_str()))
        .collect::<Vec<_>>();
    ensure!(owners.len() <= 1, "multiple local owners match the session");
    if let Some(owner) = owners.first() {
        let response = local_control::request(owner, Operation::Terminate)?;
        println!("{}", client_output::result(&response, alias, reference.as_str(), json)?);
        return Ok(if matches!(response["status"].as_str(), Some("terminated" | "termination_accepted")) { 0 } else { 1 });
    }
    let (store, mut state) = Store::resolve(&client_config::state_dir(root, &target), &target.target_id, &reference, false, None, None)?;
    let id = state.id;
    let mut link = connect_link(&config, &target, flags.address.as_deref())?;
    let confirmed = client_protocol::terminate(&mut link, &state)?;
    state.ended = true;
    store.save(&state)?;
    let response = serde_json::json!({"status":if confirmed {"terminated"} else {"termination_accepted"}, "session_id":id});
    println!("{}", client_output::result(&response, alias, reference.as_str(), json)?);
    Ok(0)
}

fn output_connection_flags(args: &[String]) -> Result<(ConnectionFlags, bool)> {
    let mut json = false;
    let mut options = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--json" {
            ensure!(!json, "duplicate option: --json");
            json = true;
        } else {
            options.push(args[index].clone());
            if matches!(args[index].as_str(), "--address" | "--retries") {
                let flag = args[index].clone();
                options.push(value(args, &mut index, &flag)?);
            }
        }
        index += 1;
    }
    Ok((parse_connection_flags(&options, false)?, json))
}

fn doctor(root: &Path, args: &[String]) -> Result<()> {
    let alias = args.first().filter(|arg| !arg.starts_with("--")).cloned();
    let start = usize::from(alias.is_some());
    let flags = parse_connection_flags(&args[start..], false)?;
    ensure!(flags.retries.is_none(), "doctor does not accept --retries");
    ensure!(
        flags.address.is_none() || alias.is_some(),
        "--address requires doctor ALIAS"
    );
    let config = client_config::load(root)?;
    if flags.address.is_none() {
        let path = devtunnel_path(&config)?;
        ensure!(
            path.is_file(),
            "configured devtunnel executable is missing: {}",
            path.display()
        );
        ensure!(
            authenticated(path)?,
            "devtunnel is not signed in; run arterm login"
        );
        println!("Dependency and authentication are ready.");
    }
    if let Some(alias) = alias {
        let target = client_config::target(&config, &alias)?.clone();
        let mut link = connect_link(&config, &target, flags.address.as_deref())?;
        client_protocol::doctor(&mut link)?;
        println!("Tunnel and host handshake succeeded for {alias}.");
    }
    Ok(())
}

fn local_command(root: &Path, args: &[String]) -> Result<u32> {
    let verb = &args[0];
    let machine = args.get(1).context("command requires MACHINE SESSION")?;
    client_config::validate_alias(machine)?;
    let reference = SessionReference::parse(args.get(2).context("command requires MACHINE SESSION")?)?;
    let mut lines = None;
    let mut command = None;
    let mut wait = false;
    let mut timeout = None;
    let mut json = false;
    let mut command_id = None;
    let mut index = 3;
    while index < args.len() {
        match args[index].as_str() {
            "--json" if !json => json = true,
            "--file" | "--transfer-id" => bail!("file transfer is deferred and is not available in arTerm 0.5"),
            "--command-id" if matches!(verb.as_str(), "read" | "send") && command_id.is_none() => {
                command_id = Some(Uuid::parse_str(&value(args, &mut index, "--command-id")?).context("invalid command ID")?);
            }
            "--lines" if verb == "read" && lines.is_none() => {
                let n: usize = value(args, &mut index, "--lines")?.parse().context("invalid line count")?;
                ensure!((1..=2000).contains(&n), "lines must be 1..=2000");
                lines = Some(n);
            }
            "--command" if verb == "send" && command.is_none() => {
                command = Some(value(args, &mut index, "--command")?);
            }
            "--wait" if verb == "send" && !wait => wait = true,
            "--timeout" if verb == "send" && timeout.is_none() => {
                let text = value(args, &mut index, "--timeout")?;
                let seconds: u64 = text.strip_suffix('s').context("timeout must be whole seconds, e.g. 60s")?
                    .parse().context("invalid timeout")?;
                ensure!(seconds > 0, "timeout must be positive and finite");
                timeout = Some(seconds.checked_mul(1000).context("timeout overflow")?);
            }
            other => bail!("unknown or duplicate option: {other}"),
        }
        index += 1;
    }
    let action = match verb.as_str() {
        "read" => {
            ensure!(command_id.is_none() || lines.is_none(), "--command-id and --lines are mutually exclusive");
            if let Some(command_id) = command_id { Operation::CommandStatus { command_id } }
            else { Operation::Read { lines: lines.unwrap_or(20) } }
        }
        "detach" => Operation::Detach,
        "interrupt" => Operation::Interrupt,
        "send" => {
            ensure!(command.is_some(), "send requires --command");
            ensure!(!wait || timeout.is_some(), "--wait requires explicit --timeout");
            ensure!(!wait || command.is_some(), "--wait is only valid with --command");
            ensure!(command_id.is_none() || command.is_some(), "--command-id is only valid with --command");
            Operation::Send { command: command.unwrap(), timeout_ms: timeout }
        }
        _ => unreachable!(),
    };
    let deadline = timeout.map(|ms| std::time::Instant::now().checked_add(Duration::from_millis(ms))
        .context("timeout exceeds supported clock range")).transpose()?;
    let config = client_config::load(root)?;
    let target = client_config::target(&config, machine)?;
    let matches = local_control::discover(root)?.into_iter().filter(|identity| {
        identity.target_id == target.target_id
            && (identity.reference.as_deref() == Some(reference.as_str())
                || identity.session_id.to_string() == reference.as_str())
    }).collect::<Vec<_>>();
    ensure!(matches.len() == 1,
        "no unique active managed local client for {machine} {}; connect explicitly (an older unmanaged client cannot be controlled)",
        reference.as_str());
    let operation_id = if verb == "send" { command_id.unwrap_or_else(Uuid::now_v7) } else { Uuid::now_v7() };
    let action = if let Operation::Send { command, timeout_ms } = action {
        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()));
        if remaining.is_some_and(|remaining| remaining.is_zero()) {
            let response = serde_json::json!({"status":"timeout","phase":"readiness","command_id":operation_id,"submitted":false,"cancelled":false});
            println!("{}", client_output::result(&response, machine, reference.as_str(), json)?);
            return Ok(124);
        }
        Operation::Send { command, timeout_ms: remaining.map(|remaining| remaining.as_millis().max(1) as u64).or(timeout_ms) }
    } else { action };
    let mut response = local_control::request_with_id(&matches[0], action, operation_id)
        .with_context(|| format!("local request failed; command/operation ID {operation_id}; query rather than resubmit"))?;
    if verb == "send" && response["status"] == "ok" && response["host"]["lookup"] == "unknown" {
        response["status"] = "unknown".into();
        response["error"] = "broker has no command record; the request was not resubmitted".into();
    }
    if wait && (response["status"] == "accepted" || response["status"] == "ok") {
        let remaining = deadline.unwrap().saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            response = serde_json::json!({"status":"timeout","phase":"completion","command_id":operation_id,"submitted":true,"cancelled":false});
        } else {
            response = local_control::request(&matches[0], Operation::Wait {
                command_id: operation_id, timeout_ms: remaining.as_millis().max(1).try_into().context("timeout overflow")?,
            })?;
        }
    }
    if json {
        println!("{}", serde_json::to_string(&response)?);
    } else if verb == "read" && command_id.is_none() && response["status"] == "ok" {
        if let Some(lines) = response["output"]["lines"].as_array() {
            for line in lines { println!("{}", line.as_str().unwrap_or("")); }
        }
        if response["output"]["truncated"] == true { eprintln!("[read] Output truncated to retained/requested lines."); }
        if response["output"]["replay_gap"] == true { eprintln!("[read] Replay gap; output may be incomplete."); }
        if response["output"]["alternate_screen"] == true { eprintln!("[read] Alternate screen snapshot."); }
    } else {
        println!("{}", client_output::result(&response, machine, reference.as_str(), false)?);
    }
    Ok(client_output::exit_code(&response))
}

fn command(args: &[String]) -> Result<u32> {
    ensure!(args.first().map(String::as_str) != Some("receive"),
        "file transfer is deferred and is not available in arTerm 0.5");
    let root = deployment::data_root()?;
    match args.first().map(String::as_str) {
        Some("setup") => {
            setup(&root, &args[1..])?;
            Ok(0)
        }
        Some("login") if args.len() == 1 => {
            let config = client_config::load(&root)?;
            run_vendor(devtunnel_path(&config)?, &["user", "login", "--github"])?;
            ensure!(
                authenticated(devtunnel_path(&config)?)?,
                "devtunnel login did not establish usable credentials"
            );
            Ok(0)
        }
        Some("logout") if args.len() == 1 => {
            let config = client_config::load(&root)?;
            ensure!(
                !client_config::any_active_session(&root, &config)?,
                "logout refused while a local session attachment is active"
            );
            run_vendor(devtunnel_path(&config)?, &["user", "logout"])?;
            Ok(0)
        }
        Some("add") => {
            add_target(&root, &args[1..])?;
            Ok(0)
        }
        Some("list") if args.get(1).map(String::as_str) == Some("--client") => {
            ensure!(args.len() == 2 || (args.len() == 3 && args[2] == "--json"), "invalid list --client options");
            let mut connections = Vec::new();
            for identity in local_control::discover(&root)? {
                connections.push(local_control::request(&identity, Operation::Inspect)?);
            }
            println!("{}", client_output::clients(&connections, args.len() == 3)?);
            Ok(0)
        }
        Some("list") if args.get(1).map(String::as_str) == Some("--server") => {
            ensure!(args.len() >= 3, "list --server requires MACHINE");
            let machine = &args[2];
            client_config::validate_alias(machine)?;
            let (flags, json) = output_connection_flags(&args[3..])?;
            ensure!(flags.retries.is_none() && !flags.stdio, "invalid server inventory options");
            let config = client_config::load(&root)?;
            let target = client_config::target(&config, machine)?;
            let mut link = connect_link(&config, target, flags.address.as_deref())?;
            let mut inventory = client_protocol::list_sessions(&mut link)?;
            let dir = client_config::state_dir(&root, target);
            let mut labels = std::collections::BTreeMap::new();
            if dir.exists() {
                for entry in std::fs::read_dir(&dir)? {
                    let path = entry?.path();
                    if let Some(name) = path.file_name().and_then(|name| name.to_str())
                        .and_then(|name| name.strip_prefix("ref-")).and_then(|name| name.strip_suffix(".json")) {
                        let id: Uuid = serde_json::from_slice(&std::fs::read(&path)?).context("invalid local session mapping")?;
                        labels.insert(id.to_string(), name.to_owned());
                    }
                }
            }
            for session in inventory["sessions"].as_array_mut().context("invalid inventory")? {
                let id = Uuid::parse_str(session["id"].as_str().context("invalid session ID")?)?.to_string();
                session["local_reference"] = labels.get(&id).cloned().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null);
                session["has_local_recovery_record"] = dir.join(format!("{id}.dpapi")).is_file().into();
            }
            inventory["machine"] = machine.clone().into();
            println!("{}", client_output::server(&inventory, json)?);
            Ok(0)
        }
        Some("list") => {
            list_targets(&root, &args[1..])?;
            Ok(0)
        }
        Some("remove") => {
            remove_target(&root, &args[1..])?;
            Ok(0)
        }
        Some("connect") => {
            let alias = args.get(1).context("connect requires an alias")?;
            client_config::validate_alias(alias)?;
            let reference = args.get(2).filter(|value| !value.starts_with("--"));
            let flags = parse_connection_flags(&args[if reference.is_some() { 3 } else { 2 }..], true)?;
            if let Some(reference) = reference {
                run_session(&root, alias, &SessionReference::parse(reference)?, true, flags)
            } else {
                let reference = SessionReference::parse(&Uuid::now_v7().to_string())?;
                println!("{}", session_command(alias, &reference, flags.retries.unwrap_or(DEFAULT_RETRIES), &flags, true)?);
                Ok(0)
            }
        }
        Some("read" | "detach" | "interrupt" | "send") => local_command(&root, args),
        Some("terminate") => terminate_session(&root, &args[1..]),
        Some("doctor") => {
            doctor(&root, &args[1..])?;
            Ok(0)
        }
        Some("--version") | Some("-V") if args.len() == 1 => {
            println!("arterm {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Some("--help") | Some("-h") if args.len() == 1 => {
            usage();
            Ok(0)
        }
        None => {
            usage();
            Ok(0)
        }
        _ => {
            usage();
            bail!("invalid command or arguments")
        }
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let code = match command(&args) {
        Ok(code) => code as i32,
        Err(error) => {
            eprintln!("[client] {error:#}");
            1
        }
    };
    arterm::diagnostics::drain(Duration::from_millis(250));
    std::process::exit(code);
}
