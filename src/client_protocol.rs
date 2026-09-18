use anyhow::{bail, ensure, Context, Result};
use rmpv::Value;
use std::time::{Duration, Instant};
use uuid::Uuid;

use arterm::{
    engine::CAPS,
    store::State,
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

pub fn terminate(link: &mut impl Link, state: &State) -> Result<()> {
    let token = state
        .token
        .clone()
        .context("saved session has no resume credential")?;
    ensure!(token.len() == 32, "invalid saved resume credential");
    link.send(&hello(state.client_id, state.request_id))?;
    let response = wait_message(link, Instant::now() + Duration::from_secs(20))?;
    ensure!(
        text(&response, "type")? == "HelloOk",
        "host rejected protocol handshake"
    );
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
    loop {
        let response = wait_message(link, deadline)?;
        let kind = text(&response, "type")?;
        let body = get(&response, "body")?;
        match kind {
            "TerminateAccepted" | "SessionExited" => {
                ensure!(
                    text(body, "session_id")? == state.id.to_string(),
                    "remote session ID mismatch"
                );
                return Ok(());
            }
            "Ping" => link.send(&message(
                "Pong",
                map(vec![
                    ("nonce", get(body, "nonce")?.clone()),
                    ("broker_time_ms", 0.into()),
                ]),
            ))?,
            "Error" => bail!("host rejected termination: {}", text(body, "code")?),
            "Unauthorized" => bail!("host rejected the saved session credential"),
            other => bail!("unexpected host protocol message while terminating: {other}"),
        }
    }
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
        let mut link = FakeLink {
            sent: vec![],
            received: VecDeque::from([
                message("HelloOk", map(vec![])),
                message(
                    "TerminateAccepted",
                    map(vec![("session_id", s(&state.id.to_string()))]),
                ),
            ]),
        };
        terminate(&mut link, &state).unwrap();
        assert_eq!(text(&link.sent[1], "type").unwrap(), "TerminateSession");
        assert_eq!(
            arterm::wire::binary(get(&link.sent[1], "body").unwrap(), "resume_token")
                .unwrap(),
            vec![9; 32]
        );
    }
}
