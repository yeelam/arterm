use anyhow::{ensure, Result};
use serde_json::Value;

fn text(value: &Value) -> String {
    match value {
        Value::String(value) => value
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect(),
        Value::Bool(value) => if *value { "yes" } else { "no" }.into(),
        Value::Number(value) => value.to_string(),
        _ => "unknown".into(),
    }
}

pub fn exit_code(response: &Value) -> u32 {
    match response["status"].as_str() {
        Some("timeout") => 124,
        Some("unknown") => 6,
        Some("completed") => {
            if response["record"]["succeeded"] == true || response["transfer_id"].is_string() {
                0
            } else {
                1
            }
        }
        Some(
            "ok"
            | "accepted"
            | "detach_requested"
            | "interrupt_requested"
            | "terminated"
            | "termination_accepted",
        ) => 0,
        _ => 1,
    }
}

pub fn result(response: &Value, machine: &str, session: &str, json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string(response)?);
    }
    let status = match response["status"].as_str() {
        Some("completed") if response["transfer_id"].is_string() => "File transfer completed.",
        Some("accepted") => "Command accepted; completion not yet confirmed.",
        Some("completed") if response["record"]["succeeded"] == true => {
            "Command completed successfully."
        }
        Some("completed") => "Command completed without success.",
        Some("timeout") if response["submitted"] == false => {
            "Readiness timed out; no command was submitted and interactive input was not changed."
        }
        Some("timeout") => {
            "Wait timed out; the remote command was not cancelled. Query its command ID."
        }
        Some("unknown") if response["commit_started"] == true =>
            "File commit outcome unknown; inspect the destination before retrying.",
        Some("unknown") => "Command outcome unknown; query its command ID rather than resubmit.",
        Some("detach_requested") => "Detach requested; remote shell retained.",
        Some("interrupt_requested") => "Interrupt requested; command completion is not confirmed.",
        Some("terminated") => "Session terminated; exit confirmed.",
        Some("termination_accepted") => "Termination accepted; exit not confirmed.",
        Some("ok") => "Command status received.",
        _ => "Operation failed.",
    };
    let mut output = format!(
        "{} / {}: {status}",
        text(&machine.into()),
        text(&session.into())
    );
    if let Some(error) = response["error"].as_str() {
        output.push_str(&format!("\nError: {}", text(&error.into())));
    }
    for (label, value) in [
        ("Command ID", &response["command_id"]),
        ("Session ID", &response["session_id"]),
        ("State", &response["record"]["state"]),
        ("Succeeded", &response["record"]["succeeded"]),
        ("Exit code", &response["record"]["exit_code"]),
        ("Transfer ID", &response["transfer_id"]),
        ("Source", &response["source"]),
        ("Destination", &response["actual_path"]),
        ("Bytes", &response["bytes"]),
        ("SHA-256", &response["sha256"]),
    ] {
        if !value.is_null() {
            output.push_str(&format!("\n{label}: {}", text(value)));
        }
    }
    if exit_code(response) != 0 && !response["shell_status"].is_null() {
        output.push_str(&format!("\n{}", readiness(response)));
    }
    Ok(output)
}

fn readiness(response: &Value) -> String {
    format!(
        "Shell: {}; readiness: {}; command capability: {}; command integration: {}; host: {}",
        text(&response["shell_status"]),
        text(&response["readiness_reason"]),
        text(&response["command_capability"]),
        text(&response["command_execution"]),
        text(&response["host_version"])
    )
}

fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .map(|row| row[index].chars().count())
                .chain([header.len()])
                .max()
                .unwrap()
        })
        .collect();
    let mut output = Vec::new();
    for row in std::iter::once(headers.iter().map(|s| s.to_string()).collect()).chain(rows) {
        output.push(
            row.iter()
                .enumerate()
                .map(|(index, cell)| {
                    format!("{cell}{}", " ".repeat(widths[index] - cell.chars().count()))
                })
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_owned(),
        );
    }
    output.join("\n")
}

pub fn clients(connections: &[Value], json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string(connections)?);
    }
    if connections.is_empty() {
        return Ok("No active managed local connections.".into());
    }
    let rows = connections
        .iter()
        .map(|item| {
            let identity = &item["identity"];
            vec![
                text(&identity["machine"]),
                text(&identity["reference"]),
                text(&identity["session_id"]),
                text(&item["connection_state"]),
                text(&item["shell_status"]),
            ]
        })
        .collect();
    let mut output = table(
        &["MACHINE", "SESSION", "SESSION ID", "CONNECTION", "SHELL"],
        rows,
    );
    for item in connections {
        output.push_str(&format!(
            "\n\n{} / {}\n{}",
            text(&item["identity"]["machine"]),
            text(&item["identity"]["reference"]),
            readiness(item)
        ));
    }
    Ok(output)
}

pub fn server(inventory: &Value, json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string(inventory)?);
    }
    let sessions = inventory["sessions"].as_array();
    ensure!(sessions.is_some(), "invalid server inventory");
    let sessions = sessions.unwrap();
    let heading = format!("Sessions on {}", text(&inventory["machine"]));
    if sessions.is_empty() {
        return Ok(format!("{heading}: none."));
    }
    let rows = sessions
        .iter()
        .map(|item| {
            vec![
                text(&item["local_reference"]),
                text(&item["id"]),
                text(&item["pid"]),
                text(&item["state"]),
                text(&item["attached"]),
                text(&item["has_local_recovery_record"]),
            ]
        })
        .collect();
    Ok(format!(
        "{heading}\n{}",
        table(
            &[
                "SESSION",
                "SESSION ID",
                "PID",
                "STATE",
                "ATTACHED",
                "RECOVERY"
            ],
            rows
        )
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_preserves_full_response_and_inventory_schema() {
        let response = json!({"schema_version":1,"status":"rejected","error":"busy",
            "identity":{"pipe":"private routing"},"extra":{"future":true}});
        assert_eq!(
            serde_json::from_str::<Value>(&result(&response, "work", "shell", true).unwrap())
                .unwrap(),
            response
        );
        assert_eq!(
            serde_json::from_str::<Value>(&clients(&[response.clone()], true).unwrap()).unwrap(),
            json!([response])
        );
        let inventory =
            json!({"machine":"work","sessions":[],"authorization_scope":"host-windows-owner"});
        assert_eq!(
            serde_json::from_str::<Value>(&server(&inventory, true).unwrap()).unwrap(),
            inventory
        );
    }

    #[test]
    fn result_codes_and_messages_distinguish_acceptance_completion_and_uncertainty() {
        for (status, code, message) in [
            ("accepted", 0, "completion not yet confirmed"),
            ("completed", 1, "without success"),
            ("timeout", 124, "not cancelled"),
            ("unknown", 6, "rather than resubmit"),
            ("rejected", 1, "Operation failed"),
            ("detach_requested", 0, "shell retained"),
            ("interrupt_requested", 0, "not confirmed"),
            ("terminated", 0, "exit confirmed"),
            ("termination_accepted", 0, "exit not confirmed"),
            ("ok", 0, "status received"),
        ] {
            let value = json!({"status":status,"command_id":"command-1"});
            assert_eq!(exit_code(&value), code);
            let human = result(&value, "work", "shell", false).unwrap();
            assert!(
                human.contains(message) && human.contains("Command ID: command-1"),
                "{human}"
            );
        }
        let value = json!({"status":"completed","record":{"succeeded":true,"exit_code":0}});
        assert_eq!(exit_code(&value), 0);
        assert!(result(&value, "work", "shell", false)
            .unwrap()
            .contains("Exit code: 0"));
    }

    #[test]
    fn failure_retains_readiness_without_raw_identity_or_duplicate_error() {
        let value = json!({"status":"rejected","error":"pending input","shell_status":"busy",
            "readiness_reason":"interactive_input_pending","command_capability":true,
            "command_execution":false,"host_version":null,"identity":{"pipe":"secret-routing"}});
        let human = result(&value, "work", "shell", false).unwrap();
        assert_eq!(human.matches("pending input").count(), 1);
        assert!(human.contains("interactive_input_pending") && human.contains("host: unknown"));
        assert!(!human.contains("secret-routing") && !human.contains('{'));
        let timeout = json!({"status":"timeout","phase":"readiness","submitted":false});
        let human = result(&timeout, "work", "shell", false).unwrap();
        assert!(human.contains("no command was submitted"));
        assert!(!human.contains("Query its command ID"));
        assert_eq!(exit_code(&timeout), 124);
    }

    #[test]
    fn inventories_have_headers_empty_states_and_readable_identity() {
        assert_eq!(
            clients(&[], false).unwrap(),
            "No active managed local connections."
        );
        let value = json!({"identity":{"machine":"work","reference":"shell","session_id":"id"},
            "connection_state":"connected","shell_status":"ready"});
        let human = clients(&[value], false).unwrap();
        assert!(
            human.contains("MACHINE") && human.contains("work") && human.contains("Shell: ready")
        );
        let inventory = json!({"machine":"work","sessions":[{"id":"id","pid":42,"state":"running",
            "attached":false,"has_local_recovery_record":true,"local_reference":"shell"}]});
        let human = server(&inventory, false).unwrap();
        assert!(human.contains("ATTACHED") && human.contains("running") && human.contains("shell"));
        assert!(server(&json!({"machine":"work","sessions":[]}), false)
            .unwrap()
            .contains("none."));
        assert!(server(&json!({}), false).is_err());
    }
}
