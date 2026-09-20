use anyhow::{bail, ensure, Context, Result};
use arterm::{
    file_transfer::{
        pin_source, AuthorizedSession, Limits, TransferManager, TransferState, MAX_CHUNK_BYTES,
    },
    local_control::Operation,
    store::State,
    transfer_admission::{Ticket, CAPABILITY},
    transfer_payload::{self, PayloadReceipt, PayloadStatus},
    transport::{Link, Poll},
    wire::{get, map, message, s, text},
};
use rmpv::Value;
use std::{
    cell::{Cell, RefCell},
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Default)]
pub struct Files {
    manager: Option<TransferManager>,
}

fn receive(link: &mut impl Link, check: &dyn Fn() -> Result<()>) -> Result<Value> {
    receive_with_idle(link, check, transfer_payload::RESPONSE_IDLE_TIMEOUT)
}

fn receive_with_idle(
    link: &mut impl Link,
    check: &dyn Fn() -> Result<()>,
    idle: Duration,
) -> Result<Value> {
    let deadline = Instant::now() + idle;
    loop {
        check()?;
        ensure!(
            Instant::now() < deadline,
            "file response timed out; outcome may be unknown"
        );
        match link.poll(Duration::from_millis(50))? {
            Poll::Message(value) => {
                check()?;
                ensure!(
                    Instant::now() < deadline,
                    "file response timed out; outcome may be unknown"
                );
                return Ok(value);
            }
            Poll::Idle => {}
            Poll::Closed => bail!("file bridge disconnected; outcome may be unknown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakeLink {
        messages: VecDeque<Value>,
        sent: Vec<Value>,
        delay: Duration,
        idle: bool,
        echo_ping: bool,
    }
    impl Link for FakeLink {
        fn send(&mut self, value: &Value) -> Result<()> {
            self.sent.push(value.clone());
            if self.echo_ping && text(value, "type")? == "Ping" {
                self.messages.push_back(message(
                    "Pong",
                    map(vec![("nonce", get(get(value, "body")?, "nonce")?.clone())]),
                ));
            }
            Ok(())
        }
        fn poll(&mut self, timeout: Duration) -> Result<Poll> {
            std::thread::sleep(self.delay);
            if let Some(value) = self.messages.pop_front() {
                return Ok(Poll::Message(value));
            }
            if self.idle {
                std::thread::sleep(timeout.min(Duration::from_millis(5)));
                Ok(Poll::Idle)
            } else {
                Ok(Poll::Closed)
            }
        }
    }
    fn progress(id: &str, phase: &str) -> Value {
        message(
            "FileProgress",
            map(vec![("request_id", s(id)), ("phase", s(phase))]),
        )
    }
    fn completed(id: &str) -> Value {
        message(
            "FileResult",
            map(vec![
                ("request_id", s(id)),
                ("json", s("{}")),
                ("bytes", Value::Nil),
            ]),
        )
    }

    #[test]
    fn correlated_progress_renews_idle_until_a_real_result_arrives() {
        let mut link = FakeLink {
            delay: Duration::from_millis(50),
            ..Default::default()
        };
        link.messages
            .extend((0..5).map(|_| progress("request", "preparing")));
        link.messages.push_back(completed("request"));
        let started = Instant::now();
        let result =
            receive_response(&mut link, "request", &|| Ok(()), Duration::from_millis(200)).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_eq!(result.0, serde_json::json!({}));
        assert!(result.1.is_empty());
    }

    #[test]
    fn continuous_progress_cannot_extend_the_original_operation_deadline() {
        let mut link = FakeLink {
            delay: Duration::from_millis(20),
            ..Default::default()
        };
        link.messages
            .extend((0..100).map(|_| progress("request", "publishing")));
        let started = Instant::now();
        let deadline = started + Duration::from_millis(100);
        let result = receive_response(
            &mut link,
            "request",
            &|| {
                ensure!(Instant::now() < deadline, "overall deadline expired");
                Ok(())
            },
            Duration::from_millis(200),
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("overall deadline"));
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!link.messages.is_empty());
    }

    #[test]
    fn foreign_or_invalid_progress_is_not_accepted() {
        for value in [
            progress("other-request", "preparing"),
            progress("request", "completed"),
        ] {
            let mut link = FakeLink {
                messages: VecDeque::from([value]),
                ..Default::default()
            };
            assert!(
                receive_response(&mut link, "request", &|| Ok(()), Duration::from_secs(1)).is_err()
            );
        }
    }

    #[test]
    fn progress_without_a_result_never_claims_completion() {
        let mut link = FakeLink {
            messages: VecDeque::from([progress("request", "verifying")]),
            ..Default::default()
        };
        let result = receive_response(&mut link, "request", &|| Ok(()), Duration::from_secs(1));
        assert!(format!("{:#}", result.unwrap_err()).contains("disconnected"));
    }

    #[test]
    fn silent_file_phase_expires_at_the_idle_deadline() {
        let mut link = FakeLink {
            idle: true,
            ..Default::default()
        };
        let started = Instant::now();
        let result = receive_response(&mut link, "request", &|| Ok(()), Duration::from_millis(20));
        assert!(format!("{:#}", result.unwrap_err()).contains("response timed out"));
        assert!(started.elapsed() >= Duration::from_millis(20));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn local_preparation_uses_correlated_keepalives_without_background_workers() {
        let mut link = FakeLink {
            echo_ping: true,
            ..Default::default()
        };
        let value = with_keepalive(&mut link, &|| Ok(()), Duration::ZERO, |check| {
            check()?;
            check()?;
            Ok(42)
        })
        .unwrap();
        assert_eq!(value, 42);
        assert_eq!(link.sent.len(), 4);
        assert!(link.sent.iter().all(|v| text(v, "type").unwrap() == "Ping"));
        assert!(link.messages.is_empty());
    }

    #[test]
    fn cancelled_preparation_never_runs_or_sends_keepalive() {
        let mut link = FakeLink {
            echo_ping: true,
            ..Default::default()
        };
        let called = Cell::new(false);
        let result = with_keepalive(
            &mut link,
            &|| bail!("caller exited"),
            Duration::ZERO,
            |_| {
                called.set(true);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(!called.get());
        assert!(link.sent.is_empty());
    }
}

fn receive_response(
    link: &mut impl Link,
    id: &str,
    check: &dyn Fn() -> Result<()>,
    idle: Duration,
) -> Result<(serde_json::Value, Vec<u8>)> {
    loop {
        let response = receive_with_idle(link, check, idle)?;
        let body = get(&response, "body")?;
        ensure!(
            text(body, "request_id")? == id,
            "file response correlation mismatch"
        );
        match text(&response, "type")? {
            "FileProgress" => {
                ensure!(
                    matches!(
                        text(body, "phase")?,
                        "preparing" | "publishing" | "verifying" | "transferring"
                    ),
                    "invalid file progress phase"
                );
                // Only idle time is renewed. The owner/lease checks retain their
                // original overall deadline and run for every progress frame.
                continue;
            }
            "FileError" => bail!("file request rejected: {}", text(body, "detail")?),
            "FileResult" => {}
            _ => bail!("invalid file response"),
        }
        let bytes = match get(body, "bytes")? {
            Value::Nil => Vec::new(),
            Value::Binary(bytes) => bytes.clone(),
            _ => bail!("invalid file response bytes"),
        };
        ensure!(bytes.len() <= MAX_CHUNK_BYTES, "oversized file response");
        return Ok((serde_json::from_str(text(body, "json")?)?, bytes));
    }
}

fn with_keepalive<T>(
    link: &mut impl Link,
    check: &dyn Fn() -> Result<()>,
    interval: Duration,
    operation: impl FnOnce(&dyn Fn() -> Result<()>) -> Result<T>,
) -> Result<T> {
    let link = RefCell::new(link);
    let last = Cell::new(Instant::now());
    let pulse = || {
        check()?;
        if last.get().elapsed() >= interval {
            let nonce = Uuid::now_v7().to_string();
            let mut link = link.borrow_mut();
            link.send(&message("Ping", map(vec![("nonce", s(&nonce))])))?;
            let reply = receive(&mut **link, check)?;
            ensure!(
                text(&reply, "type")? == "Pong" && text(get(&reply, "body")?, "nonce")? == nonce,
                "file preparation keepalive mismatch"
            );
            last.set(Instant::now());
        }
        check()
    };
    pulse()?;
    let result = operation(&pulse)?;
    pulse()?;
    Ok(result)
}

fn rpc(
    link: &mut impl Link,
    kind: &str,
    mut fields: Vec<(&str, Value)>,
    check: &dyn Fn() -> Result<()>,
) -> Result<(serde_json::Value, Vec<u8>)> {
    check()?;
    let id = Uuid::now_v7().to_string();
    fields.push(("request_id", s(&id)));
    link.send(&message(kind, map(fields)))?;
    receive_response(link, &id, check, transfer_payload::RESPONSE_IDLE_TIMEOUT)
}

pub fn transfer(
    link: &mut impl Link,
    state: &State,
    files: &mut Files,
    ticket: &Ticket,
    operation_id: Uuid,
    action: Operation,
    caller: &dyn Fn() -> Result<()>,
) -> Result<serde_json::Value> {
    let check = || {
        caller()?;
        ticket.check()
    };
    check()?;
    link.send(&message(
        "Hello",
        map(vec![
            ("min_version", 1.into()),
            ("max_version", 1.into()),
            (
                "client_instance_id",
                Value::Binary(state.client_id.as_bytes().to_vec()),
            ),
            (
                "capabilities",
                Value::Array(
                    [CAPABILITY, transfer_payload::METADATA_CAPABILITY]
                        .into_iter()
                        .chain(
                            transfer_payload::DIRECTORY_ADAPTER_INSTALLED
                                .then_some(transfer_payload::DIRECTORY_CAPABILITY),
                        )
                        .map(s)
                        .collect(),
                ),
            ),
        ]),
    ))?;
    let response = receive(link, &check)?;
    ensure!(
        text(&response, "type")? == "HelloOk",
        "file handshake rejected"
    );
    let body = get(&response, "body")?;
    ensure!(
        arterm::wire::binary(body, "broker_instance_id")?
            == *state.origin.as_ref().context("missing broker identity")?,
        "file broker changed"
    );
    let capabilities = get(body, "capabilities")?
        .as_array()
        .context("invalid capabilities")?;
    for capability in [CAPABILITY, transfer_payload::METADATA_CAPABILITY] {
        ensure!(
            capabilities.iter().any(|v| v.as_str() == Some(capability)),
            "host does not support {capability}"
        );
    }
    let directory_supported = transfer_payload::DIRECTORY_ADAPTER_INSTALLED
        && capabilities
            .iter()
            .any(|v| v.as_str() == Some(transfer_payload::DIRECTORY_CAPABILITY));
    check()?;
    link.send(&message("FileAuthorize", map(ticket.authorization()?)))?;
    ensure!(
        text(&receive(link, &check)?, "type")? == "FileAuthorized",
        "file authorization rejected"
    );
    check()?;
    let prepared = if let Operation::FileSend { path } = &action {
        Some(with_keepalive(
            link,
            &check,
            transfer_payload::PROGRESS_INTERVAL,
            |preparing| {
                transfer_payload::prepare_source(
                    pin_source(Path::new(path))?,
                    directory_supported,
                    preparing,
                    transfer_payload::archive_unavailable,
                )
            },
        )?)
    } else {
        None
    };
    let mut source_metadata = prepared.as_ref().map(|source| source.metadata().clone());
    if files.manager.is_none() {
        files.manager = Some(TransferManager::new(
            AuthorizedSession::after_authorization(state.id),
            &std::env::temp_dir()
                .components()
                .collect::<std::path::PathBuf>(),
            Limits::default(),
        )?);
    }
    let manager = files.manager.as_mut().context("missing file manager")?;
    let mut local_id = None;
    let mut remote_id = None;
    let mut destination = None;
    let mut destination_id = None;
    let mut commit_started = false;
    let source = match &action {
        Operation::FileSend { path } | Operation::FileReceive { path } => path.clone(),
        _ => bail!("invalid file operation"),
    };
    let result = (|| -> Result<PayloadReceipt> {
        match action {
            Operation::FileSend { .. } => {
                check()?;
                let source = prepared.as_ref().context("missing prepared source")?;
                let download = manager.begin_download(source.payload_path())?;
                local_id = Some(download.transfer_id);
                let (status, _) = rpc(
                    link,
                    "FileBeginUpload",
                    vec![
                        ("operation_id", s(&operation_id.to_string())),
                        ("path", s(&source.metadata().payload_basename())),
                        ("source", source.metadata().to_wire()),
                        ("size", download.expected_bytes.into()),
                    ],
                    &check,
                )?;
                let status: PayloadStatus = serde_json::from_value(status)?;
                ensure!(
                    status.source == *source.metadata(),
                    "upload source metadata mismatch"
                );
                let status = status.payload;
                remote_id = Some(status.transfer_id);
                destination = Some(source.metadata().completion_path(&status.actual_path)?);
                destination_id = Some(status.transfer_id);
                ensure!(
                    status.session_id == state.id
                        && status.state == TransferState::Uploading
                        && status.bytes == 0
                        && status.expected_bytes == download.expected_bytes,
                    "upload admission mismatch"
                );
                let mut offset = 0;
                loop {
                    check()?;
                    let chunk =
                        manager.read_chunk(download.transfer_id, offset, MAX_CHUNK_BYTES)?;
                    if !chunk.bytes.is_empty() {
                        let next = offset + chunk.bytes.len() as u64;
                        let (ack, _) = rpc(
                            link,
                            "FileWrite",
                            vec![
                                ("transfer_id", s(&status.transfer_id.to_string())),
                                ("offset", offset.into()),
                                ("bytes", Value::Binary(chunk.bytes)),
                            ],
                            &check,
                        )?;
                        ensure!(
                            ack["offset"].as_u64() == Some(next),
                            "upload offset mismatch"
                        );
                        offset = next;
                    }
                    if chunk.eof {
                        break;
                    }
                }
                check()?;
                let source_receipt = manager.close(download.transfer_id)?;
                with_keepalive(
                    link,
                    &check,
                    transfer_payload::PROGRESS_INTERVAL,
                    |verifying| source.verify_sources(verifying),
                )?;
                // Sending Finish starts a potentially committing remote operation.
                check()?;
                commit_started = true;
                let (receipt, _) = rpc(
                    link,
                    "FileFinish",
                    vec![
                        ("transfer_id", s(&status.transfer_id.to_string())),
                        ("sha256", Value::Binary(source_receipt.sha256.to_vec())),
                    ],
                    &check,
                )?;
                let receipt: PayloadReceipt = serde_json::from_value(receipt)?;
                receipt.validate_completion()?;
                ensure!(
                    receipt.source == *source.metadata()
                        && receipt.payload.session_id == state.id
                        && receipt.payload.transfer_id == status.transfer_id
                        && receipt.payload.bytes == source_receipt.bytes
                        && receipt.payload.sha256 == source_receipt.sha256,
                    "remote receipt mismatch; outcome unknown"
                );
                Ok(receipt)
            }
            Operation::FileReceive { path } => {
                let (status, _) = rpc(
                    link,
                    "FileBeginDownload",
                    vec![
                        ("operation_id", s(&operation_id.to_string())),
                        ("path", s(&path)),
                    ],
                    &check,
                )?;
                let status: PayloadStatus = serde_json::from_value(status)?;
                status
                    .source
                    .require_directory_support(directory_supported)?;
                source_metadata = Some(status.source.clone());
                let metadata = status.source;
                let status = status.payload;
                remote_id = Some(status.transfer_id);
                ensure!(
                    status.session_id == state.id
                        && status.state == TransferState::Downloading
                        && status.bytes == 0,
                    "download admission mismatch"
                );
                check()?;
                let upload =
                    manager.begin_upload(&metadata.payload_basename(), status.expected_bytes)?;
                local_id = Some(upload.transfer_id);
                destination = Some(metadata.completion_path(&upload.actual_path)?);
                destination_id = Some(upload.transfer_id);
                let mut offset = 0;
                loop {
                    let (chunk, bytes) = rpc(
                        link,
                        "FileRead",
                        vec![
                            ("transfer_id", s(&status.transfer_id.to_string())),
                            ("offset", offset.into()),
                        ],
                        &check,
                    )?;
                    ensure!(
                        chunk["offset"].as_u64() == Some(offset),
                        "download offset mismatch"
                    );
                    check()?;
                    if !bytes.is_empty() {
                        offset = manager.write_chunk(upload.transfer_id, offset, &bytes)?;
                    }
                    if chunk["eof"].as_bool().context("invalid EOF")? {
                        break;
                    }
                    ensure!(!bytes.is_empty(), "empty nonterminal chunk");
                }
                let (receipt, _) = rpc(
                    link,
                    "FileClose",
                    vec![("transfer_id", s(&status.transfer_id.to_string()))],
                    &check,
                )?;
                let receipt: PayloadReceipt = serde_json::from_value(receipt)?;
                ensure!(
                    receipt.source == metadata
                        && receipt.extracted_bytes.is_none()
                        && receipt.payload.session_id == state.id
                        && receipt.payload.transfer_id == status.transfer_id
                        && receipt.payload.bytes == status.expected_bytes,
                    "remote source receipt mismatch"
                );
                let receipt =
                    manager.finish_guarded(upload.transfer_id, receipt.payload.sha256, || {
                        caller()?;
                        let guard = ticket.commit_guard()?;
                        commit_started = true;
                        Ok(guard)
                    })?;
                with_keepalive(
                    link,
                    &check,
                    transfer_payload::PROGRESS_INTERVAL,
                    |extracting| {
                        transfer_payload::complete_payload(
                            manager,
                            metadata,
                            receipt,
                            extracting,
                            transfer_payload::extraction_unavailable,
                        )
                    },
                )
            }
            _ => bail!("invalid file operation"),
        }
    })();
    match result {
        Ok(PayloadReceipt {
            payload: receipt,
            source: metadata,
            extracted_bytes,
        }) => Ok(serde_json::json!({
            "status":"completed", "transfer_id":receipt.transfer_id, "remote_transfer_id":remote_id,
            "source":source, "actual_path":receipt.actual_path, "bytes":receipt.bytes,
            "source_kind":metadata.kind, "original_basename":metadata.original_basename,
            "extracted_bytes":extracted_bytes,
            "sha256":receipt.sha256.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        })),
        Err(error) => {
            if let Some(id) = local_id {
                if matches!(
                    manager.status(id)?.state,
                    TransferState::Uploading | TransferState::Downloading
                ) {
                    manager.cancel(id).context("local partial cleanup failed")?;
                }
            }
            Ok(serde_json::json!({
                "status":if commit_started { "unknown" } else { "error" },
                "error":format!("{error:#}"),
                "source":source, "actual_path":destination,
                "source_metadata":source_metadata,
                "transfer_id":destination_id, "remote_transfer_id":remote_id,
                "commit_started":commit_started, "automatic_retry":false,
            }))
        }
    }
}
