#[path = "../client_config.rs"]
mod client_config;
#[path = "../client_protocol.rs"]
mod client_protocol;

use anyhow::{bail, ensure, Context, Result};
use arterm::statusln as eprintln;
use client_config::{ClientConfig, Target};
use arterm::{
    console::{Console, Terminal},
    deployment::{self, Role},
    engine::{End, Engine},
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
  arterm resume ALIAS REF [--retries 0..20]\n\
  arterm terminate ALIAS REF --yes\n\
  arterm doctor [ALIAS]\n\
  arterm --help | --version\n\n\
Without REF, connect only prints a reusable command. Names are not passwords.\n\
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

fn session_command(alias: &str, reference: &SessionReference, retries: u32, flags: &ConnectionFlags, create: bool) -> Result<String> {
    let exe = std::env::current_exe()?;
    let mut parts = vec![
        "&".into(),
        quote_command_arg(&exe.to_string_lossy()),
        if create { "connect".into() } else { "resume".into() },
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
    let config = client_config::load(root)?;
    let target = client_config::target(&config, alias)?.clone();
    let retries = flags.retries.unwrap_or(DEFAULT_RETRIES);
    let state_dir = client_config::state_dir(root, &target);
    let (store, state) = Store::resolve(&state_dir, &target.target_id, reference, create, flags.shell.as_deref(), flags.cwd.as_deref())?;
    let id = state.id;
    let command = session_command(alias, reference, retries, &flags, create)?;
    eprintln!("[session] GUID {id}; Ctrl+] detaches without killing the remote shell.");
    print_recovery(id, &command);
    let mut engine = if !create && Uuid::parse_str(reference.as_str()).is_ok() {
        Engine::resume_guid(state)
    } else {
        Engine::new(state)
    };
    let mut terminal = Console::new(flags.stdio)?;
    let result = (|| -> Result<u32> {
        for attempt in 0..=retries {
            if attempt > 0 {
                terminal.reading(false);
                let jitter = u16::from_le_bytes(store::random_claim()?[..2].try_into().unwrap())
                    as u64
                    % 251;
                let delay = (500u64 * (1u64 << (attempt - 1).min(6))).min(15_000) + jitter;
                eprintln!("[session] Reconnecting {attempt}/{retries} in {delay}ms; reusing {id}");
                thread::sleep(Duration::from_millis(delay));
            }
            let mut link = match connect_link(&config, &target, flags.address.as_deref()) {
                Ok(link) => link,
                Err(error) => {
                    eprintln!("[session] Connection failed: {error:#}");
                    print_recovery(id, &command);
                    if error.is::<LoginRequired>() {
                        return Err(error);
                    }
                    continue;
                }
            };
            match engine.run(&mut link, &mut terminal, &mut |state| {
                store.save(state)
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

fn terminate_session(root: &Path, args: &[String]) -> Result<()> {
    ensure!(args.len() >= 3, "terminate requires ALIAS GUID --yes");
    let alias = &args[0];
    let reference = SessionReference::parse(&args[1])?;
    ensure!(
        args.iter().skip(2).any(|arg| arg == "--yes"),
        "terminate requires --yes"
    );
    let extra = args
        .iter()
        .skip(2)
        .filter(|arg| arg.as_str() != "--yes")
        .cloned()
        .collect::<Vec<_>>();
    let flags = parse_connection_flags(&extra, false)?;
    ensure!(
        flags.retries.is_none(),
        "terminate does not accept --retries"
    );
    let config = client_config::load(root)?;
    let target = client_config::target(&config, alias)?.clone();
    let (store, mut state) = Store::resolve(&client_config::state_dir(root, &target), &target.target_id, &reference, false, None, None)?;
    let id = state.id;
    let mut link = connect_link(&config, &target, flags.address.as_deref())?;
    client_protocol::terminate(&mut link, &state)?;
    state.ended = true;
    store.save(&state)?;
    println!("Termination accepted for {id}.");
    Ok(())
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

fn command(args: &[String]) -> Result<u32> {
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
        Some("resume") => {
            let alias = args.get(1).context("resume requires an alias")?;
            let reference = SessionReference::parse(args.get(2).context("resume requires a reference")?)?;
            let flags = parse_connection_flags(&args[3..], false)?;
            run_session(&root, alias, &reference, false, flags)
        }
        Some("terminate") => {
            terminate_session(&root, &args[1..])?;
            Ok(0)
        }
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
    match command(&args) {
        Ok(code) => std::process::exit(code as i32),
        Err(error) => {
            eprintln!("[client] {error:#}");
            std::process::exit(1);
        }
    }
}
