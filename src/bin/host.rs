#[path = "../host_broker.rs"]
mod host_broker;
#[path = "../host_pipe.rs"]
mod host_pipe;

use anyhow::{bail, ensure, Result};
use std::time::Duration;

fn usage() {
    eprintln!("arTerm host\n\
arterm-host setup --name NAME [--code-path PATH] [--no-download] [--accept-server-license-terms]\n\
arterm-host login | start | doctor | status | sessions\n\
arterm-host terminate <session-id> --yes\n\
arterm-host stop [--terminate-sessions]\n\
arterm-host --help | --version\n\
Internal: run | bridge --protocol vsterm-session-v1\n\
Sessions have no idle or age TTL; they end only on shell exit, explicit termination, host failure, logoff, or reboot.");
}
fn main() {
    if let Err(error) = entry() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
fn entry() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(code) = arterm::host_setup::handle(&args)? {
        ensure!(code == 0, "host setup command failed with exit code {code}");
        return Ok(());
    }
    match args.first().map(String::as_str) {
        Some("run") if args.len() == 1 => {
            host_broker::run_with_transport(arterm::host_supervisor::TunnelSupervisor::start)
        }
        Some("bridge") => {
            ensure!(
                args.as_slice() == ["bridge", "--protocol", "vsterm-session-v1"],
                "bridge requires --protocol vsterm-session-v1"
            );
            let root = arterm::deployment::data_root()?;
            let pipe = host_broker::pipe_name(&root)?;
            host_pipe::copy_bridge(host_pipe::connect(&pipe, Duration::from_secs(3))?)
        }
        Some("status") if args.len() == 1 => {
            let value = host_broker::control("status", None, false)?;
            println!("running sessions={}", value["sessions"]);
            arterm::host_setup::status_details()?;
            Ok(())
        }
        Some("sessions") if args.len() == 1 => {
            let value = host_broker::control("sessions", None, false)?;
            println!("{}", serde_json::to_string_pretty(&value["sessions"])?);
            Ok(())
        }
        Some("terminate") if args.len() == 3 && args[2] == "--yes" => {
            host_broker::control("terminate", Some(&args[1]), false)?;
            Ok(())
        }
        Some("stop") => {
            ensure!(
                args.len() == 1 || args.as_slice() == ["stop", "--terminate-sessions"],
                "invalid stop arguments"
            );
            host_broker::stop(args.len() == 2)?;
            Ok(())
        }
        Some("--help") | Some("-h") => {
            usage();
            Ok(())
        }
        Some("--version") if args.len() == 1 => {
            println!(
                "arTerm host {}",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
        _ => {
            usage();
            bail!("invalid command")
        }
    }
}
