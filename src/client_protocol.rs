use anyhow::{bail, ensure, Context, Result};
use rmpv::Value;
use std::time::{Duration, Instant};
use uuid::Uuid;

use arterm::{
    engine::CAPS,
    store::{RetirementReason, State},
    transport::{Link, Poll},
    wire::{get, map, message, num, s, text, MAX_FRAME},
};

fn hello(client_id: Uuid, correlation_id: Uuid) -> Value {
    message(
        "Hello",
        map(vec![
            ("min_version", 1.into()),
            ("max_version", 1.into()),
            ("client_version", s(env!("CARGO_PKG_VERSION"))),
            (
                "client_instance_id",
                Value::Binary(client_id.as_bytes().to_vec()),
            ),
            (
                "correlation_id",
                Value::Binary(correlation_id.as_bytes().to_vec()),
            ),
            (
                "capabilities",
                Value::Array(CAPS.iter().map(|value| s(value)).collect()),
            ),
            ("max_receive_frame", (MAX_FRAME as u64).into()),
        ]),
    )
}

fn wait_message(link: &mut impl Link, deadline: Instant) -> Result<Value> {
    loop {
        ensure!(
            Instant::now() < deadline,
            "host protocol handshake timed out"
        );
        match link.poll(Duration::from_millis(100))? {
            Poll::Message(value) => return Ok(value),
            Poll::Idle => {}
            Poll::Closed => bail!("host bridge closed during protocol handshake"),
        }
    }
}

pub fn doctor(link: &mut impl Link) -> Result<()> {
    link.send(&hello(Uuid::now_v7(), Uuid::now_v7()))?;
    let response = wait_message(link, Instant::now() + Duration::from_secs(20))?;
    ensure!(
        text(&response, "type")? == "HelloOk",
        "host rejected protocol handshake"
    );
    let body = get(&response, "body")?;
    ensure!(
        num(body, "version")? == 1,
        "unsupported host protocol version"
    );
    let capabilities = get(body, "capabilities")?
        .as_array()
        .context("invalid host capabilities")?;
    ensure!(
        CAPS.iter()
            .all(|cap| capabilities.iter().any(|value| value.as_str() == Some(cap))),
        "host lacks required vsterm-session-v1 capabilities"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    Confirmed,
    AcceptedUnconfirmed,
    Retired(RetirementReason),
    Rejected,
}

pub fn terminate(link: &mut impl Link, state: &State) -> Result<Termination> {
    let token = state
        .token
        .clone()
        .context("saved session has no resume credential")?;
    ensure!(token.len() == 32, "invalid saved resume credential");
    let origin = state.origin.as_deref().context("saved broker identity unavailable; reference retained")?;
    ensure!(origin.len() == 16, "invalid saved broker identity");
    link.send(&hello(state.client_id, state.request_id))?;
    let response = wait_message(link, Instant::now() + Duration::from_secs(20))?;
    ensure!(
        text(&response, "type")? == "HelloOk",
        "host rejected protocol handshake"
    );
    let hello_body = get(&response, "body")?;
    ensure!(num(hello_body, "version")? == 1, "unsupported termination protocol version");
    let broker = arterm::wire::bin16(hello_body, "broker_instance_id")?;
    if broker != origin { return Ok(Termination::Retired(RetirementReason::BrokerChanged)); }
    let confirmed_capability = get(hello_body, "capabilities").ok().and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|v| v.as_str() == Some("session-termination-confirmed")));
    link.send(&message(
        "TerminateSession",
        map(vec![
            ("session_id", s(&state.id.to_string())),
            ("resume_token", Value::Binary(token)),
            ("grace_ms", 5000.into()),
            ("reason", s("explicit")),
        ]),
    ))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut accepted = false;
    loop {
        let response = match wait_message(link, deadline) {
            Ok(response) => response,
            Err(error) if accepted => {
                arterm::statusln!("[terminate] Request accepted, but exit confirmation failed: {error:#}");
                return Ok(Termination::AcceptedUnconfirmed);
            }
            Err(error) => return Err(error),
        };
        let kind = text(&response, "type")?;
        let body = get(&response, "body")?;
        match kind {
            "TerminateAccepted" | "SessionExited" | "SessionTerminated" => {
                ensure!(
                    text(body, "session_id")? == state.id.to_string(),
                    "remote session ID mismatch"
                );
                if kind != "TerminateAccepted" { return Ok(Termination::Confirmed); }
                accepted = true;
                if !confirmed_capability { return Ok(Termination::AcceptedUnconfirmed); }
            }

            "Ping" => link.send(&message(
                "Pong",
                map(vec![
                    ("nonce", get(body, "nonce")?.clone()),
                    ("broker_time_ms", 0.into()),
                ]),
            ))?,
            "Error" if accepted && text(body, "code")? == "TerminationUnconfirmed" => {
                arterm::statusln!("[terminate] Request accepted; process exit is not confirmed.");
                return Ok(Termination::AcceptedUnconfirmed);
            }
            "Error" => bail!("host rejected termination; reference retained"),
            "Unauthorized" => return Ok(Termination::Rejected),
            _ => bail!("unexpected host protocol response; reference retained"),
        }
    }
}

fn management_handshake(link: &mut impl Link) -> Result<Value> {
    let id = Uuid::now_v7();
    let mut greeting = hello(id, id);
    if let Value::Map(fields) = &mut greeting {
        if let Some((_, Value::Map(body))) = fields.iter_mut().find(|(key, _)| key.as_str() == Some("body")) {
            if let Some((_, Value::Array(caps))) = body.iter_mut().find(|(key, _)| key.as_str() == Some("capabilities")) {
                caps.push(s("host-owner-management-v1"));
            }
        }
    }
    link.send(&greeting)?;
    let response = wait_message(link, Instant::now() + Duration::from_secs(20))?;
    ensure!(text(&response, "type")? == "HelloOk", "management handshake rejected");
    ensure!(get(get(&response, "body")?, "capabilities")?.as_array().context("invalid capabilities")?
        .iter().any(|v| v.as_str() == Some("host-owner-management-v1")), "host does not support owner inventory");
    Ok(get(&response, "body")?.clone())
}

fn inventory_response(link: &mut impl Link) -> Result<serde_json::Value> {
    link.send(&message("ListSessions", map(vec![])))?;
    let response = wait_message(link, Instant::now() + Duration::from_secs(20))?;
    ensure!(text(&response, "type")? == "SessionInventory", "remote inventory rejected");
    Ok(serde_json::from_str(text(get(&response, "body")?, "json")?)?)
}

pub fn list_sessions(link: &mut impl Link) -> Result<serde_json::Value> {
    management_handshake(link)?;
    inventory_response(link)
}

pub fn retirement_from_inventory(link: &mut impl Link, state: &State) -> Result<Option<RetirementReason>> {
    let origin = state.origin.as_deref().context("no saved broker identity; existence remains unknown")?;
    ensure!(origin.len() == 16, "invalid saved broker identity");
    let body = management_handshake(link)?;
    ensure!(num(&body, "version")? == 1, "unsupported management protocol version");
    let broker = arterm::wire::bin16(&body, "broker_instance_id")?;
    if broker != origin { return Ok(Some(RetirementReason::BrokerChanged)); }
    #[derive(serde::Deserialize)]
    struct Session { id: Uuid, pid: u32, exited: bool, attached: bool, state: String }
    #[derive(serde::Deserialize)]
    struct Inventory { authorization_scope: String, broker_instance_id: String, sessions: Vec<Session> }
    let inventory: Inventory = serde_json::from_value(inventory_response(link)?)
        .context("invalid owner inventory")?;
    let expected = broker.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    ensure!(inventory.authorization_scope == "host-windows-owner" && inventory.broker_instance_id == expected,
        "owner inventory identity/scope mismatch");
    let mut ids = std::collections::BTreeSet::new();
    for session in &inventory.sessions {
        ensure!(ids.insert(session.id) && session.pid > 0
            && session.state == if session.exited { "exited" } else { "running" }
            && (!session.exited || !session.attached), "invalid owner inventory session");
    }
    Ok(match inventory.sessions.iter().find(|session| session.id == state.id) {
        Some(session) if !session.exited => None,
        Some(_) => Some(RetirementReason::Completed),
        None => Some(RetirementReason::Missing),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakeLink {
        sent: Vec<Value>,
        received: VecDeque<Value>,
    }
    impl Link for FakeLink {
        fn send(&mut self, value: &Value) -> Result<()> {
            self.sent.push(value.clone());
            Ok(())
        }
        fn poll(&mut self, _timeout: Duration) -> Result<Poll> {
            Ok(self
                .received
                .pop_front()
                .map(Poll::Message)
                .unwrap_or(Poll::Closed))
        }
    }

    #[test]
    fn terminate_uses_saved_token() {
        let mut state = State::new("target".into(), "powershell.exe".into(), vec![], None);
        state.token = Some(vec![9; 32]);
        state.origin = Some(vec![1; 16]);
        let mut link = FakeLink {
            sent: vec![],
            received: VecDeque::from([
                message("HelloOk", map(vec![("version", 1.into()), ("broker_instance_id", Value::Binary(vec![1;16]))])),
                message(
                    "TerminateAccepted",
                    map(vec![("session_id", s(&state.id.to_string()))]),
                ),
            ]),
        };
        assert_eq!(terminate(&mut link, &state).unwrap(), Termination::AcceptedUnconfirmed);
        assert_eq!(text(&link.sent[1], "type").unwrap(), "TerminateSession");
        assert_eq!(
            arterm::wire::binary(get(&link.sent[1], "body").unwrap(), "resume_token")
                .unwrap(),
            vec![9; 32]
        );
    }

    fn management_link(broker: u8, inventory: serde_json::Value) -> FakeLink {
        FakeLink {
            sent: Vec::new(),
            received: VecDeque::from([
                message("HelloOk", map(vec![("version", 1.into()),
                    ("broker_instance_id", Value::Binary(vec![broker;16])),
                    ("capabilities", Value::Array(vec![s("host-owner-management-v1")]))])),
                message("SessionInventory", map(vec![("json", s(&inventory.to_string()))])),
            ]),
        }
    }

    #[test]
    fn owner_inventory_proof_is_bound_to_scope_broker_and_well_formed_session_rows() {
        let mut state = State::new("target".into(), "powershell.exe".into(), vec![], None);
        state.origin = Some(vec![1;16]);
        let inventory = serde_json::json!({"authorization_scope":"host-windows-owner",
            "broker_instance_id":"01010101010101010101010101010101","sessions":[]});
        let mut link = management_link(1, inventory.clone());
        assert_eq!(retirement_from_inventory(&mut link, &state).unwrap(), Some(RetirementReason::Missing));
        assert!(get(get(&link.sent[0], "body").unwrap(), "resume_token").is_err());
        for (exited, attached, expected) in [
            (false, false, None), (false, true, None), (true, false, Some(RetirementReason::Completed)),
        ] {
            let mut value = inventory.clone();
            value["sessions"] = serde_json::json!([{"id":state.id,"pid":42,"exited":exited,
                "attached":attached,"state":if exited {"exited"} else {"running"}}]);
            assert_eq!(retirement_from_inventory(&mut management_link(1, value), &state).unwrap(), expected);
        }
        for value in [
            serde_json::json!({"authorization_scope":"wrong","broker_instance_id":inventory["broker_instance_id"],"sessions":[]}),
            serde_json::json!({"authorization_scope":"host-windows-owner","broker_instance_id":"ffffffffffffffffffffffffffffffff","sessions":[]}),
            serde_json::json!({"authorization_scope":"host-windows-owner","broker_instance_id":inventory["broker_instance_id"],"sessions":[{"id":state.id}]}),
        ] {
            assert!(retirement_from_inventory(&mut management_link(1, value), &state).is_err());
        }
        let mut changed = management_link(2, inventory);
        assert_eq!(retirement_from_inventory(&mut changed, &state).unwrap(), Some(RetirementReason::BrokerChanged));
        assert_eq!(changed.sent.len(), 1);
        assert_eq!(state.origin, Some(vec![1;16]));
    }

    #[test]
    fn unsupported_inventory_and_unauthorized_do_not_establish_nonexistence() {
        let mut state = State::new("target".into(), "powershell.exe".into(), vec![], None);
        state.token = Some(vec![9;32]);
        state.origin = Some(vec![1;16]);
        let mut link = FakeLink { sent: vec![], received: VecDeque::from([
            message("HelloOk", map(vec![("version", 1.into()), ("broker_instance_id", Value::Binary(vec![1;16])),
                ("capabilities", Value::Array(vec![]))])),
            message("Unauthorized", map(vec![("session_id", s(&state.id.to_string()))])),
        ]) };
        assert_eq!(terminate(&mut link, &state).unwrap(), Termination::Rejected);
        assert!(!state.ended && state.retirement_reason.is_none());
        let mut older = FakeLink { sent: vec![], received: VecDeque::from([
            message("HelloOk", map(vec![("version", 1.into()), ("broker_instance_id", Value::Binary(vec![1;16])),
                ("capabilities", Value::Array(vec![]))])),
        ]) };
        assert!(retirement_from_inventory(&mut older, &state).is_err());
        assert_eq!(older.sent.len(), 1);
    }

    #[test]
    fn termination_keeps_unconfirmed_acceptance_separate_and_never_sends_tokens_to_changed_broker() {
        let mut state = State::new("target".into(), "powershell.exe".into(), vec![], None);
        state.token = Some(vec![9;32]);
        state.origin = Some(vec![1;16]);
        for (reply, expected) in [
            ("SessionTerminated", Termination::Confirmed),
            ("Error", Termination::AcceptedUnconfirmed),
        ] {
            let body = if reply == "Error" { map(vec![("code", s("TerminationUnconfirmed"))]) }
                else { map(vec![("session_id", s(&state.id.to_string()))]) };
            let mut link = FakeLink { sent: vec![], received: VecDeque::from([
                message("HelloOk", map(vec![("version", 1.into()), ("broker_instance_id", Value::Binary(vec![1;16])),
                    ("capabilities", Value::Array(vec![s("session-termination-confirmed")]))])),
                message("TerminateAccepted", map(vec![("session_id", s(&state.id.to_string()))])),
                message(reply, body),
            ]) };
            assert_eq!(terminate(&mut link, &state).unwrap(), expected);
        }
        let mut changed = FakeLink { sent: vec![], received: VecDeque::from([
            message("HelloOk", map(vec![("version", 1.into()), ("broker_instance_id", Value::Binary(vec![2;16]))])),
        ]) };
        assert_eq!(terminate(&mut changed, &state).unwrap(), Termination::Retired(RetirementReason::BrokerChanged));
        assert_eq!(changed.sent.len(), 1);
        let mut unsupported = FakeLink { sent: vec![], received: VecDeque::from([
            message("HelloOk", map(vec![("version", 2.into()), ("broker_instance_id", Value::Binary(vec![2;16]))])),
        ]) };
        assert!(terminate(&mut unsupported, &state).is_err());
        assert_eq!(unsupported.sent.len(), 1);
    }
}
