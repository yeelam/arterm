#[path = "../host_broker.rs"]
mod host_broker;
#[path = "../host_pipe.rs"]
mod host_pipe;

use anyhow::{bail, ensure, Result};
use std::time::Duration;

fn usage() {
    eprintln!("arTerm host\n\
arterm-host setup [--name NAME] [--code-path PATH] [--no-download] [--accept-server-license-terms] [--terminate-sessions] [--force-stop-host]\n\
arterm-host login | start | doctor\n\
arterm-host status [--json] | sessions [--json]\n\
arterm-host terminate <session-id> --yes [--json]\n\
arterm-host stop [--terminate-sessions] [--json]\n\
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
    let (args, json) = output_options(&args)?;
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
            println!("{}", status_output(&value, json)?);
            if !json {
                arterm::host_setup::status_details()?;
            }
            Ok(())
        }
        Some("sessions") if args.len() == 1 => {
            let value = host_broker::control("sessions", None, false)?;
            println!("{}", sessions_output(&value["sessions"], json)?);
            Ok(())
        }
        Some("terminate") if args.len() == 3 && args[2] == "--yes" => {
            let value = host_broker::control("terminate", Some(&args[1]), false)?;
            println!(
                "{}",
                control_output(
                    &value,
                    &format!("Termination requested for session {}.", args[1]),
                    json
                )?
            );
            Ok(())
        }
        Some("stop") => {
            ensure!(
                args.len() == 1 || args.as_slice() == ["stop", "--terminate-sessions"],
                "invalid stop arguments"
            );
            host_broker::stop(args.len() == 2)?;
            println!(
                "{}",
                control_output(
                    &serde_json::json!({"ok":true}),
                    "Host stop requested (or host was already stopped).",
                    json
                )?
            );
            Ok(())
        }

        Some("--help") | Some("-h") => {
            usage();
            Ok(())
        }
        Some("--version") if args.len() == 1 => {
            println!("arTerm host {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            usage();
            bail!("invalid command")
        }
    }
}

fn output_options(args: &[String]) -> Result<(Vec<String>, bool)> {
    if !matches!(
        args.first().map(String::as_str),
        Some("status" | "sessions" | "terminate" | "stop")
    ) {
        return Ok((args.to_vec(), false));
    }
    let count = args.iter().filter(|arg| arg.as_str() == "--json").count();
    ensure!(count <= 1, "duplicate option: --json");
    Ok((
        args.iter()
            .filter(|arg| arg.as_str() != "--json")
            .cloned()
            .collect(),
        count == 1,
    ))
}

fn status_output(value: &serde_json::Value, json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string(value)?);
    }
    let sessions = value["sessions"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("invalid host session count"))?;
    Ok(format!("Host running. {sessions} retained session(s)."))
}

fn sessions_output(value: &serde_json::Value, json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(value)?);
    }
    let sessions = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid host session inventory"))?;
    if sessions.is_empty() {
        return Ok("No retained host sessions.".into());
    }
    let mut output = format!(
        "{:<36}  {:>10}  {:<8}  ATTACHED",
        "SESSION ID", "PID", "STATE"
    );
    for session in sessions {
        let id = session["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid session ID"))?;
        let id = uuid::Uuid::parse_str(id)?;
        let pid = session["pid"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("invalid session PID"))?;
        let exited = session["exited"]
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("invalid session exit state"))?;
        let attached = session["attached"]
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("invalid session attachment state"))?;
        output.push_str(&format!(
            "\n{id:<36}  {pid:>10}  {:<8}  {}",
            if exited { "exited" } else { "running" },
            if attached { "yes" } else { "no" }
        ));
    }
    Ok(output)
}

fn control_output(value: &serde_json::Value, message: &str, json: bool) -> Result<String> {
    if json {
        Ok(serde_json::to_string(value)?)
    } else {
        Ok(message.into())
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn inventories_are_readable_and_json_retains_original_shapes() {
        let sessions = json!([{"id":"01234567-89ab-4cde-8fab-0123456789ab",
                    "pid":42,"state":"running","exited":false,"attached":true,"future":7}]);
        let response = json!({"ok":true,"sessions":1});
        assert_eq!(
            status_output(&response, false).unwrap(),
            "Host running. 1 retained session(s)."
        );
        let human = sessions_output(&sessions, false).unwrap();
        assert!(human.contains("SESSION ID") && human.contains("running") && human.contains("yes"));
        assert!(!human.contains('{'));
        assert_eq!(
            sessions_output(&sessions, true).unwrap(),
            serde_json::to_string_pretty(&sessions).unwrap()
        );
        assert_eq!(
            serde_json::from_str::<Value>(&status_output(&response, true).unwrap()).unwrap(),
            response
        );
        assert_eq!(
            sessions_output(&json!([]), false).unwrap(),
            "No retained host sessions."
        );
        assert_eq!(status_output(&json!({"ok":true,"sessions":0}), false).unwrap(),
            "Host running. 0 retained session(s).");
        let mut exited = sessions.clone();
        exited[0]["exited"] = true.into();
        exited[0]["attached"] = false.into();
        let human = sessions_output(&exited, false).unwrap();
        assert!(human.contains("exited") && human.ends_with("no"));
        assert!(status_output(&json!({}), false).is_err());
        assert!(sessions_output(&json!([{}]), false).is_err());
    }

    #[test]
    fn control_acknowledgements_and_output_options_are_explicit() {
        assert_eq!(
            control_output(&json!({"ok":true}), "Stop requested.", false).unwrap(),
            "Stop requested."
        );
        assert_eq!(
            control_output(&json!({"ok":true}), "Stop requested.", true).unwrap(),
            "{\"ok\":true}"
        );
        for verb in ["status", "sessions", "terminate", "stop"] {
            let (args, json) = output_options(&[verb.into(), "--json".into()]).unwrap();
            assert_eq!(args, [verb]);
            assert!(json);
            assert!(output_options(&[verb.into(), "--json".into(), "--json".into()]).is_err());
            assert!(!output_options(&[verb.into()]).unwrap().1);
        }
        assert_eq!(
            output_options(&["setup".into(), "--json".into()])
                .unwrap()
                .0,
            ["setup", "--json"]
        );
    }
}
