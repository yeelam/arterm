use crate::diagnostics;
use crate::{
    console::{Input, Terminal},
    store::{PendingInput, State},
    transport::{Link, Poll},
    wire::{bin16, binary, get, map, message, num, s, text},
};
use anyhow::{bail, ensure, Context, Result};
use rmpv::Value;
use std::time::{Duration, Instant};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock predates Unix epoch")
        .as_millis() as u64
}
pub const CAPS: &[&str] = &[
    "session-create",
    "create-reservation",
    "session-resume",
    "output-sequence",
    "input-ack",
    "writer-lease",
    "connection-epoch",
    "resize-generation",
    "client-session-id",
];
pub const ENDED_SESSION_CAPABILITY: &str = "ended-session-rejection";
#[derive(Debug, PartialEq)]
pub enum End {
    Disconnected,
    Detached,
    Exited(u32),
}
pub struct Engine {
    pub state: State,
    pub output_seq: u64,
    attachment: Option<Vec<u8>>,
    lease: Option<Vec<u8>>,
    credit: u64,
    pending_sent: bool,
    resize_generation: u64,
    last_size: (u16, u16),
    allow_legacy_resume: bool,
    command_capable: bool,
    active_command: Option<uuid::Uuid>,
    command_input_ready: bool,
    transfer_admission: crate::transfer_admission::Admission,
    transfer_capable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use uuid::Uuid;

    #[derive(Default)]
    struct FakeTerminal {
        bytes: Vec<u8>,
        input: VecDeque<Input>,
        fail_output: bool,
    }
    impl Terminal for FakeTerminal {
        fn output(&mut self, bytes: &[u8]) -> Result<()> {
            if self.fail_output {
                bail!("simulated sink failure");
            }
            self.bytes.extend_from_slice(bytes);
            Ok(())
        }
        fn input(&mut self, accept_bytes: bool) -> Result<Input> {
            let immediate = matches!(self.input.front(), Some(Input::Detach) | Some(Input::Eof));
            if accept_bytes || immediate {
                Ok(self.input.pop_front().unwrap_or(Input::Idle))
            } else {
                Ok(Input::Idle)
            }
        }
        fn size(&self) -> (u16, u16) {
            (80, 24)
        }
        fn reading(&self, _: bool) {}
    }
    struct FakeLink {
        events: VecDeque<Value>,
        sent: Vec<Value>,
        id: String,
        broker: Vec<u8>,
        lose_create: bool,
        lose_ack: bool,
        output: bool,
        committed: u64,
        creates: usize,
        executions: usize,
        recovered: bool,
        unavailable: bool,
        reject_input: Option<&'static str>,
        supports_ended_rejection: bool,
        supports_commands: bool,
        exit_on_attach: Option<u32>,
    }
    impl FakeLink {
        fn new(id: Uuid) -> Self {
            Self {
                events: VecDeque::new(),
                sent: vec![],
                id: id.to_string(),
                broker: vec![1; 16],
                lose_create: false,
                lose_ack: false,
                output: false,
                committed: 0,
                creates: 0,
                executions: 0,
                recovered: false,
                unavailable: false,
                reject_input: None,
                supports_ended_rejection: true,
                supports_commands: false,
                exit_on_attach: None,
            }
        }
        fn push(&mut self, kind: &str, fields: Vec<(&str, Value)>) {
            self.events.push_back(message(kind, map(fields)));
        }
    }
    impl Link for FakeLink {
        fn send(&mut self, value: &Value) -> Result<()> {
            self.sent.push(value.clone());
            let body = get(value, "body")?;
            match text(value, "type")? {
                "Hello" => self.push(
                    "HelloOk",
                    vec![
                        ("version", 1.into()),
                        ("broker_instance_id", Value::Binary(self.broker.clone())),
                        (
                            "capabilities",
                            Value::Array(CAPS.iter().copied()
                                .chain(self.supports_ended_rejection.then_some(ENDED_SESSION_CAPABILITY))
                                .chain(self.supports_commands.then_some(crate::shell_integration::CAPABILITY))
                                .map(s).collect()),
                        ),
                        ("max_frame", (1024 * 1024).into()),
                        ("max_input_window_bytes", 65536.into()),
                    ],
                ),
                "CreateSession" => {
                    assert_eq!(text(body, "requested_session_id")?, self.id);
                    if !self.recovered && !self.unavailable {
                        self.creates += 1;
                    }
                    if self.lose_create {
                        self.lose_create = false;
                        return Ok(());
                    }
                    if self.unavailable {
                        self.push("CreateRecoveryUnavailable", vec![]);
                        return Ok(());
                    }
                    self.push(
                        "SessionCreated",
                        vec![
                            ("session_id", s(&self.id)),
                            ("resume_token", Value::Binary(vec![2; 32])),
                            ("requires_attach", self.recovered.into()),
                            ("recovered", self.recovered.into()),
                            ("attachment_id", Value::Binary(vec![3; 16])),
                            ("lease_id", Value::Binary(vec![4; 16])),
                            ("input_committed_through", 0.into()),
                        ],
                    );
                }
                "AttachSession" => {
                    assert_eq!(text(body, "session_id")?, self.id);
                    self.push(
                        "SessionAttached",
                        vec![
                            ("session_id", s(&self.id)),
                            ("mode", s("writer")),
                            (
                                "accepted_connection_epoch",
                                num(body, "connection_epoch")?.into(),
                            ),
                            ("attachment_id", Value::Binary(vec![5; 16])),
                            ("lease_id", Value::Binary(vec![6; 16])),
                            ("input_committed_through", self.committed.into()),
                            ("resize_generation", 0.into()),
                        ],
                    );
                    if let Some(code) = self.exit_on_attach {
                        self.push("SessionExited", vec![
                            ("session_id", s(&self.id)),
                            ("exit_code", code.into()),
                            ("reason", s("shell-exited")),
                        ]);
                    }
                }
                "Input" => {
                    let seq = num(body, "input_seq")?;
                    if let Some(kind) = self.reject_input {
                        self.push(
                            kind,
                            vec![
                                ("session_id", s(&self.id)),
                                ("expected", seq.into()),
                                ("retry_after_ms", 10.into()),
                                ("available_window_bytes", 0.into()),
                            ],
                        );
                        return Ok(());
                    }
                    if seq > self.committed {
                        self.executions += 1;
                        self.committed = seq;
                    }
                    if self.lose_ack {
                        self.lose_ack = false;
                        return Ok(());
                    }
                    self.push(
                        "InputAck",
                        vec![
                            ("session_id", s(&self.id)),
                            (
                                "client_instance_id",
                                get(body, "client_instance_id")?.clone(),
                            ),
                            ("committed_through", self.committed.into()),
                            ("available_window_bytes", 65536.into()),
                        ],
                    );
                }
                "Resize" if self.output => {
                    self.output = false;
                    self.push(
                        "Output",
                        vec![
                            ("session_id", s(&self.id)),
                            ("output_seq", 1.into()),
                            ("bytes", Value::Binary(b"hello".to_vec())),
                        ],
                    );
                }
                _ => {}
            }
            Ok(())
        }
        fn poll(&mut self, _: Duration) -> Result<Poll> {
            if self.events.is_empty() && self.reject_input.is_some() {
                std::thread::sleep(Duration::from_millis(50));
                return Ok(Poll::Message(message("Pong", map(vec![]))));
            }
            Ok(self
                .events
                .pop_front()
                .map(Poll::Message)
                .unwrap_or(Poll::Closed))
        }
    }
    fn state() -> State {
        let mut s = State::new("box".into(), "pwsh".into(), vec![], None);
        s.claim = vec![7; 32];
        s
    }
    #[test]
    fn reusable_connect_rejects_legacy_host_before_create_or_attach() {
        for saved_token in [true, false] {
            let mut state = state();
            if saved_token {
                state.token = Some(vec![2; 32]);
                state.origin = Some(vec![1; 16]);
            }
            let mut engine = Engine::new(state);
            let mut server = FakeLink::new(engine.state.id);
            server.supports_ended_rejection = false;
            server.exit_on_attach = Some(0);
            let mut snapshots = 0;
            let result = engine.run(&mut server, &mut FakeTerminal::default(), &mut |_| {
                snapshots += 1;
                Ok(())
            });
            assert!(result.is_err(), "unsupported host was accepted: {result:?}");
            assert!(result.unwrap_err().to_string().contains("host lacks ended-session-rejection"));
            assert_eq!(server.sent.len(), 1, "only Hello may be sent");
            assert_eq!(server.creates, 0);
            assert_eq!(snapshots, 0);
            assert!(!engine.state.ended, "unsupported does not mean ended");
        }
    }
    #[test]
    fn legacy_guid_resume_accepts_old_host_only_with_unnamed_saved_credential() {
        for (named, credential) in [(false, true), (true, true), (false, false)] {
            let mut state = state();
            state.origin = Some(vec![1; 16]);
            state.create_deadline_ms = Some(now_ms() + 60_000);
            if named {
                state.reference = Some("mywork".into());
            }
            if credential {
                state.token = Some(vec![2; 32]);
            }
            let mut engine = Engine::resume_guid(state);
            let mut server = FakeLink::new(engine.state.id);
            server.supports_ended_rejection = false;
            let result = engine.run(&mut server, &mut FakeTerminal::default(), &mut |_| Ok(()));
            if !named && credential {
                assert_eq!(result.unwrap(), End::Disconnected);
                assert!(server.sent.iter().any(|v| text(v, "type").unwrap() == "AttachSession"));
            } else {
                assert!(result.is_err());
                assert_eq!(server.sent.len(), 1);
            }
            assert_eq!(server.creates, 0);
        }
    }

    #[test]
    fn capability_presence_preserves_live_attachment_exit_codes() {
        for code in [0, 7] {
            let mut state = state();
            state.token = Some(vec![2; 32]);
            state.origin = Some(vec![1; 16]);
            let mut engine = Engine::new(state);
            let mut server = FakeLink::new(engine.state.id);
            server.exit_on_attach = Some(code);
            let end = engine.run(&mut server, &mut FakeTerminal::default(), &mut |_| Ok(())).unwrap();
            assert_eq!(end, End::Exited(code));
            assert!(engine.state.ended);
        }
    }
    #[test]
    fn lost_create_recovers_same_guid_then_fresh_attach() {
        let mut engine = Engine::new(state());
        let mut server = FakeLink::new(engine.state.id);
        let mut terminal = FakeTerminal::default();
        let mut snapshots = vec![];
        let mut save = |s: &State| {
            snapshots.push(s.clone());
            Ok(())
        };
        server.lose_create = true;
        server.supports_commands = true;
        assert_eq!(
            engine.run(&mut server, &mut terminal, &mut save).unwrap(),
            End::Disconnected
        );
        assert!(engine.state.token.is_none());
        assert_eq!(engine.state.command_execution, Some(true));
        server.recovered = true;
        assert_eq!(
            engine.run(&mut server, &mut terminal, &mut save).unwrap(),
            End::Disconnected
        );
        let creates: Vec<_> = server
            .sent
            .iter()
            .filter(|v| text(v, "type").unwrap() == "CreateSession")
            .collect();
        assert_eq!(
            get(creates[0], "body").unwrap(),
            get(creates[1], "body").unwrap()
        );
        assert_eq!(server.creates, 1);
        assert!(server
            .sent
            .iter()
            .any(|v| text(v, "type").unwrap() == "AttachSession"));
        assert!(snapshots[0].origin.is_some());
    }
    #[test]
    fn saved_session_rejects_restarted_broker_without_attach_or_replacement() {
        let mut engine = Engine::new(state());
        let mut server = FakeLink::new(engine.state.id);
        let mut terminal = FakeTerminal::default();
        engine.run(&mut server, &mut terminal, &mut |_| Ok(())).unwrap();
        assert!(engine.state.token.is_some());
        let saved = engine.state.clone();
        let mut engine = Engine::new(saved.clone());
        server.broker = vec![9; 16];
        let before = server.sent.len();
        let mut persisted = None;
        let error = engine.run(&mut server, &mut terminal, &mut |state| {
            persisted = Some(state.clone());
            Ok(())
        }).unwrap_err();
        assert!(error.to_string().contains("refusing to recreate session"));
        assert_eq!(server.sent.len(), before + 1);
        assert_eq!(text(&server.sent[before], "type").unwrap(), "Hello");
        assert_eq!(server.creates, 1);
        let persisted = persisted.unwrap();
        assert!(persisted.ended);
        assert_eq!(persisted.id, saved.id);
        assert_eq!(persisted.token, saved.token);
    }

    #[test]
    fn restart_and_expired_create_fail_without_replacement() {
        let mut engine = Engine::new(state());
        let mut server = FakeLink::new(engine.state.id);
        let mut terminal = FakeTerminal::default();
        server.lose_create = true;
        engine
            .run(&mut server, &mut terminal, &mut |_| Ok(()))
            .unwrap();
        let pending = engine.state.clone();
        server.unavailable = true;
        assert!(engine
            .run(&mut server, &mut terminal, &mut |_| Ok(()))
            .is_err());
        assert_eq!(server.creates, 1);
        assert!(engine.state.ended);
        let before = server.sent.len();
        assert!(engine.run(&mut server, &mut terminal, &mut |_| Ok(())).is_err());
        assert_eq!(server.sent.len(), before);
        let mut engine = Engine::new(pending.clone());
        server.broker = vec![9; 16];
        let before = server.sent.len();
        assert!(engine
            .run(&mut server, &mut terminal, &mut |_| Ok(()))
            .is_err());
        assert_eq!(server.sent.len(), before + 1); // only Hello, not another create
        assert!(engine.state.ended);
        let mut engine = Engine::new(pending);
        engine.state.create_deadline_ms = Some(0);
        server.broker = vec![1; 16];
        let before = server.sent.len();
        assert!(engine.run(&mut server, &mut terminal, &mut |_| Ok(())).is_err());
        assert_eq!(server.sent.len(), before + 1);
        assert!(engine.state.ended);
    }
    #[test]
    fn lost_input_ack_reconciles_after_client_restart() {
        let mut engine = Engine::new(state());
        let mut server = FakeLink::new(engine.state.id);
        let mut terminal = FakeTerminal::default();
        terminal
            .input
            .push_back(Input::Bytes(b"echo hello\r".to_vec()));
        server.lose_ack = true;
        let mut saved = engine.state.clone();
        engine
            .run(&mut server, &mut terminal, &mut |s| {
                saved = s.clone();
                Ok(())
            })
            .unwrap();
        assert!(saved.pending.is_some());
        let old_epoch = saved.epoch;
        let mut restarted = Engine::new(saved);
        restarted
            .run(&mut server, &mut terminal, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(server.executions, 1);
        assert!(restarted.state.pending.is_none());
        assert!(restarted.state.epoch > old_epoch);
    }

    #[test]
    fn rejected_input_cannot_stall_forever_while_heartbeats_continue() {
        for rejection in ["InputBackpressure", "InputSequenceGap"] {
            let mut engine = Engine::new(state());
            let mut server = FakeLink::new(engine.state.id);
            server.reject_input = Some(rejection);
            let mut terminal = FakeTerminal::default();
            terminal.input.push_back(Input::Bytes(vec![b'x'; 4096]));
            let started = Instant::now();
            assert_eq!(
                engine
                    .run(&mut server, &mut terminal, &mut |_| Ok(()))
                    .unwrap(),
                End::Disconnected
            );
            assert!(started.elapsed() < Duration::from_secs(8));
            assert!(engine.state.pending.is_some());
            assert_eq!(server.executions, 0);
        }
    }
    #[test]
    fn detach_is_polled_while_input_is_pending_and_credit_is_exhausted() {
        let mut engine = Engine::new(state());
        let mut server = FakeLink::new(engine.state.id);
        server.reject_input = Some("InputBackpressure");
        let mut terminal = FakeTerminal::default();
        terminal.input.push_back(Input::Bytes(b"pending".to_vec()));
        terminal.input.push_back(Input::Detach);

        assert_eq!(
            engine
                .run(&mut server, &mut terminal, &mut |_| Ok(()))
                .unwrap(),
            End::Detached
        );
        assert!(engine.state.pending.is_some());
        assert_eq!(engine.credit, 0);
    }

    #[test]
    fn failed_sink_never_advances_cursor_or_acknowledges() {
        let mut engine = Engine::new(state());
        let mut server = FakeLink::new(engine.state.id);
        let mut terminal = FakeTerminal {
            fail_output: true,
            ..Default::default()
        };
        server.output = true;
        assert!(engine
            .run(&mut server, &mut terminal, &mut |_| Ok(()))
            .is_err());
        assert_eq!(engine.output_seq, 0);
        assert!(!server
            .sent
            .iter()
            .any(|v| text(v, "type").unwrap() == "OutputAck"));
    }
    #[test]
    fn replay_deduplicates_only_completed_output() {
        let mut engine = Engine::new(state());
        let mut server = FakeLink::new(engine.state.id);
        let mut terminal = FakeTerminal::default();
        server.output = true;
        engine
            .run(&mut server, &mut terminal, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(engine.output_seq, 1);
        server.output = true; // sends same chunk again after attach
        engine
            .run(&mut server, &mut terminal, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(&terminal.bytes, b"hello");
        assert_eq!(server.creates, 1);
    }
    #[test]
    fn persistence_failure_prevents_create_and_detach_never_kills() {
        let mut engine = Engine::new(state());
        let mut server = FakeLink::new(engine.state.id);
        let mut terminal = FakeTerminal::default();
        assert!(engine
            .run(&mut server, &mut terminal, &mut |_| bail!("disk full"))
            .is_err());
        assert_eq!(server.creates, 0);
        terminal.input.push_back(Input::Bytes(vec![0x1d]));
        assert_eq!(
            engine
                .run(&mut server, &mut terminal, &mut |_| Ok(()))
                .unwrap(),
            End::Detached
        );
        assert!(server
            .sent
            .iter()
            .any(|v| text(v, "type").unwrap() == "Detach"));
        assert!(!server
            .sent
            .iter()
            .any(|v| text(v, "type").unwrap() == "Terminate"));
    }
}
impl Engine {
    pub fn new(state: State) -> Self {
        Self {
            state,
            output_seq: 0,
            attachment: None,
            lease: None,
            credit: 0,
            pending_sent: false,
            resize_generation: 0,
            last_size: (0, 0),
            allow_legacy_resume: false,
            command_capable: false,
            active_command: None,
            command_input_ready: false,
            transfer_admission: crate::transfer_admission::Admission::default(),
            transfer_capable: false,
        }
    }
    /// Explicit GUID resume may attach to an older host with an existing credential.
    /// Named records and tokenless creation recovery still require the new contract.
    pub fn resume_guid(state: State) -> Self {
        let allow_legacy_resume = state.reference.is_none() && state.token.is_some();
        let mut engine = Self::new(state);
        engine.allow_legacy_resume = allow_legacy_resume;
        engine
    }
    pub fn transfer_admission(&self) -> crate::transfer_admission::Admission {
        self.transfer_admission.clone()
    }
    fn admit_transfers(&self) -> Result<()> {
        if self.transfer_capable {
            self.transfer_admission.publish(&self.state,
                self.attachment.as_deref().context("missing attachment")?,
                self.lease.as_deref().context("missing writer lease")?)?;
        }
        Ok(())
    }
    fn base(&self) -> Vec<(&'static str, Value)> {
        vec![("session_id", s(&self.state.id.to_string()))]
    }
    fn attached(&self) -> Result<Vec<(&'static str, Value)>> {
        let mut fields = self.base();
        fields.push((
            "attachment_id",
            Value::Binary(self.attachment.clone().context("not attached")?),
        ));
        Ok(fields)
    }
    fn detach(&self) -> Result<Value> {
        let mut fields = self.attached()?;
        fields.extend([
            ("last_output_ack", self.output_seq.into()),
            ("last_input_ack", self.state.input_ack.into()),
            ("keep_running", true.into()),
        ]);
        Ok(message("Detach", map(fields)))
    }
    fn writer(&self) -> Result<Vec<(&'static str, Value)>> {
        let mut fields = self.attached()?;
        fields.push((
            "lease_id",
            Value::Binary(self.lease.clone().context("no writer lease")?),
        ));
        fields.push((
            "client_instance_id",
            Value::Binary(self.state.client_id.as_bytes().to_vec()),
        ));
        Ok(fields)
    }
    fn attach(
        &mut self,
        size: (u16, u16),
        save: &mut impl FnMut(&State) -> Result<()>,
    ) -> Result<Value> {
        self.state.epoch = self.state.epoch.checked_add(1).context("epoch exhausted")?;
        save(&self.state)?;
        let mut fields = self.base();
        fields.extend([
            (
                "resume_token",
                Value::Binary(self.state.token.clone().context("missing token")?),
            ),
            ("mode", s("writer")),
            ("takeover", false.into()),
            ("after_output_seq", self.output_seq.into()),
            (
                "client_instance_id",
                Value::Binary(self.state.client_id.as_bytes().to_vec()),
            ),
            ("connection_epoch", self.state.epoch.into()),
            ("last_input_ack", self.state.input_ack.into()),
            ("cols", size.0.into()),
            ("rows", size.1.into()),
        ]);
        Ok(message("AttachSession", map(fields)))
    }
    fn create(&self) -> Value {
        message(
            "CreateSession",
            map(vec![
                (
                    "request_id",
                    Value::Binary(self.state.request_id.as_bytes().to_vec()),
                ),
                ("requested_session_id", s(&self.state.id.to_string())),
                ("create_claim", Value::Binary(self.state.claim.clone())),
                (
                    "origin_broker_instance_id",
                    Value::Binary(self.state.origin.clone().unwrap()),
                ),
                ("connection_epoch", self.state.create_epoch.into()),
                ("shell", s(&self.state.shell)),
                (
                    "args",
                    Value::Array(self.state.args.iter().map(|v| s(v)).collect()),
                ),
                (
                    "cwd",
                    self.state.cwd.as_ref().map(|v| s(v)).unwrap_or(Value::Nil),
                ),
                ("env", map(vec![])),
                ("cols", self.state.create_cols.into()),
                ("rows", self.state.create_rows.into()),
                ("attach_mode", s("writer")),
                ("command_execution", self.state.command_execution.unwrap_or(false).into()),
                ("after_output_seq", 0.into()),
            ]),
        )
    }
    fn acknowledge_input(
        &mut self,
        ack: u64,
        save: &mut impl FnMut(&State) -> Result<()>,
    ) -> Result<()> {
        let highest = self
            .state
            .pending
            .as_ref()
            .map(|p| p.seq)
            .unwrap_or(self.state.input_ack);
        ensure!(
            ack >= self.state.input_ack && ack <= highest,
            "server input acknowledgment inconsistent with local state"
        );
        if ack > self.state.input_ack {
            self.state.input_ack = ack;
            self.state.pending = None;
            self.pending_sent = false;
            save(&self.state)?;
        }
        Ok(())
    }
    fn same_session(&self, body: &Value) -> Result<()> {
        ensure!(
            text(body, "session_id")? == self.state.id.to_string(),
            "remote session ID mismatch"
        );
        Ok(())
    }
    pub fn run(
        &mut self,
        link: &mut impl Link,
        terminal: &mut impl Terminal,
        save: &mut impl FnMut(&State) -> Result<()>,
    ) -> Result<End> {
        let _transfer_scope = self.transfer_admission.connection_scope();
        ensure!(!self.state.ended, "session has ended; choose a NEW reference");
        terminal.reading(false);
        self.attachment = None;
        self.lease = None;
        self.pending_sent = false;
        self.command_input_ready = false;
        self.transfer_capable = false;
        self.credit = 0;
        let mut phase = "hello";
        terminal.connection_state("connecting");
        let mut last_received = Instant::now();
        let mut heartbeat = Instant::now();
        let mut pending_since = self.state.pending.as_ref().map(|_| Instant::now());
        let mut retry_input_at = Instant::now();
        let mut handshake_deadline = Instant::now() + Duration::from_secs(20);
        let hello = message(
            "Hello",
            map(vec![
                ("min_version", 1.into()),
                ("max_version", 1.into()),
                ("client_version", s(env!("CARGO_PKG_VERSION"))),
                (
                    "client_instance_id",
                    Value::Binary(self.state.client_id.as_bytes().to_vec()),
                ),
                (
                    "correlation_id",
                    Value::Binary(self.state.request_id.as_bytes().to_vec()),
                ),
                (
                    "capabilities",
                    Value::Array(CAPS.iter().copied()
                        .chain([ENDED_SESSION_CAPABILITY, crate::shell_integration::CAPABILITY]).map(s).collect()),
                ),
                ("max_receive_frame", (crate::wire::MAX_FRAME as u64).into()),
            ]),
        );
        if link.send(&hello).is_err() {
            return Ok(End::Disconnected);
        }
        loop {
            if terminal.detach_requested() {
                if self.attachment.is_some() {
                    let _ = link.send(&self.detach()?);
                }
                terminal.reading(false);
                return Ok(End::Detached);
            }
            let polled = match link.poll(Duration::from_millis(30)) {
                Ok(p) => p,
                Err(error)
                    if error
                        .chain()
                        .any(|e| e.is::<std::io::Error>() || e.is::<rmpv::decode::Error>()) =>
                {
                    terminal.reading(false);
                    return Ok(End::Disconnected);
                }
                Err(error) => return Err(error),
            };
            if let Poll::Closed = polled {
                terminal.reading(false);
                return Ok(End::Disconnected);
            }
            if let Poll::Message(value) = polled {
                last_received = Instant::now();
                let kind = text(&value, "type")?;
                let body = get(&value, "body")?;
                let mut reply = None;
                match kind {
                    "HelloOk" => {
                        ensure!(phase == "hello", "unexpected HelloOk");
                        ensure!(
                            num(body, "version")? == 1,
                            "unsupported host session protocol"
                        );
                        let caps = get(body, "capabilities")?
                            .as_array()
                            .context("invalid capabilities")?;
                        self.command_capable = caps.iter().any(|v| v.as_str() == Some(crate::shell_integration::CAPABILITY));
                        self.transfer_capable = [crate::transfer_admission::CAPABILITY, crate::transfer_payload::METADATA_CAPABILITY]
                            .iter().all(|cap| caps.iter().any(|v| v.as_str() == Some(cap)));
                        terminal.command_capability(self.command_capable);
                        ensure!(
                            CAPS.iter()
                                .all(|c| caps.iter().any(|v| v.as_str() == Some(c))),
                            "host lacks required devbox-session-v1 capabilities"
                        );
                        if !caps.iter().any(|value| value.as_str() == Some(ENDED_SESSION_CAPABILITY)) {
                            ensure!(
                                self.allow_legacy_resume,
                                "host lacks ended-session-rejection; named connections require arTerm host 0.3 or later. No create or attach was sent. Only an existing unnamed GUID credential can connect to a legacy host"
                            );
                            diagnostics::line(format_args!(
                                "[session] Legacy GUID resume: host lacks ended-session rejection; a retained exited session may report its old exit code. Finish existing sessions before upgrading the host."
                            ));
                        }
                        ensure!(
                            num(body, "max_frame")? >= 8192,
                            "host frame limit too small"
                        );
                        self.credit = num(body, "max_input_window_bytes")?.min(65536);
                        ensure!(
                            self.credit >= 4096,
                            "host input window must support a 4096-byte chunk"
                        );
                        let choice_changed = self.state.command_execution.is_none();
                        if choice_changed {
                            self.state.command_execution = Some(self.command_capable
                                && self.state.origin.is_none() && self.state.token.is_none()
                                && self.state.create_deadline_ms.is_none()
                                && crate::shell_integration::Commands::supported(&self.state.shell, &self.state.args));
                        }
                        ensure!(!self.state.command_execution.unwrap_or(false) || self.command_capable,
                            "saved command-enabled creation requires command-execution-v1; refusing fingerprint change");
                        terminal.command_context(self.state.command_execution.unwrap_or(false), text(body, "host_version").ok());
                        let broker = bin16(body, "broker_instance_id")?;
                        if let Some(origin) = &self.state.origin {
                            if *origin != broker {
                                self.state.ended = true;
                                save(&self.state)?;
                                bail!("Terminal host restarted; refusing to recreate session or replay uncertain input");
                            }
                            if choice_changed { save(&self.state)?; }
                        } else {
                            self.state.origin = Some(broker);
                            self.state.create_cols = terminal.size().0;
                            self.state.create_rows = terminal.size().1;
                            self.state.epoch = self.state.create_epoch;
                            self.state.create_deadline_ms = Some(now_ms() + 600_000);
                            save(&self.state)?;
                        }
                        if self.state.token.is_some() {
                            reply = Some(self.attach(terminal.size(), save)?);
                            phase = "attaching";
                        } else {
                            if self.state.create_deadline_ms.is_some_and(|deadline| now_ms() >= deadline) {
                                self.state.ended = true;
                                save(&self.state)?;
                                bail!("create recovery window expired; refusing to create a replacement");
                            }
                            ensure!(
                                now_ms()
                                    < self
                                        .state
                                        .create_deadline_ms
                                        .context("missing create recovery deadline")?,
                                "create recovery window expired; refusing to create a replacement"
                            );
                            reply = Some(self.create());
                            phase = "creating";
                        }
                    }
                    "SessionCreated" => {
                        ensure!(phase == "creating", "unexpected create response");
                        self.same_session(body)?;
                        let token = binary(body, "resume_token")?;
                        ensure!(token.len() == 32, "invalid resume token");
                        self.state.token = Some(token);
                        save(&self.state)?;
                        let requires_attach = get(body, "requires_attach")?
                            .as_bool()
                            .context("invalid requires_attach")?;
                        let recovered = get(body, "recovered")?
                            .as_bool()
                            .context("invalid recovered")?;
                        ensure!(
                            !recovered || requires_attach,
                            "recovered creation must issue fresh attach"
                        );
                        if requires_attach {
                            reply = Some(self.attach(terminal.size(), save)?);
                            phase = "attaching";
                        } else {
                            ensure!(
                                num(body, "input_committed_through")? == 0,
                                "new session has prior input"
                            );
                            self.attachment = Some(bin16(body, "attachment_id")?);
                            self.lease = Some(bin16(body, "lease_id")?);
                            self.last_size = (0, 0);
                            phase = "attached";
                            self.admit_transfers()?;
                            terminal.connection_state("connected");
                        }
                        diagnostics::line(format_args!("[session] {}", self.state.id));
                    }
                    "SessionAttached" => {
                        ensure!(phase == "attaching", "unexpected attach response");
                        self.same_session(body)?;
                        ensure!(
                            text(body, "mode")? == "writer",
                            "writer attachment required"
                        );
                        ensure!(
                            num(body, "accepted_connection_epoch")? == self.state.epoch,
                            "stale connection epoch"
                        );
                        self.attachment = Some(bin16(body, "attachment_id")?);
                        self.lease = Some(bin16(body, "lease_id")?);
                        self.acknowledge_input(num(body, "input_committed_through")?, save)?;
                        self.resize_generation = num(body, "resize_generation")?;
                        self.last_size = (0, 0);
                        phase = "attached";
                        self.admit_transfers()?;
                        terminal.connection_state("connected");
                    }
                    "Output" if phase == "attached" => {
                        self.same_session(body)?;
                        let seq = num(body, "output_seq")?;
                        if seq > self.output_seq {
                            ensure!(
                                seq == self
                                    .output_seq
                                    .checked_add(1)
                                    .context("output sequence exhausted")?,
                                "output gap without ReplayGap"
                            );
                            terminal.output(&binary(body, "bytes")?)?;
                            self.output_seq = seq;
                            terminal.command_output_progress(seq);
                        }
                        let mut fields = self.attached()?;
                        fields.push(("delivered_through", self.output_seq.into()));
                        reply = Some(message("OutputAck", map(fields)));
                    }
                    "Output" if phase == "attaching" => {
                        // Old create-attachment output is replayed by the fresh attachment.
                        self.same_session(body)?;
                    }
                    "ReplayGap" if phase == "attached" => {
                        terminal.output_gap();
                        self.same_session(body)?;
                        ensure!(
                            num(body, "requested_after")? == self.output_seq,
                            "unexpected replay gap cursor"
                        );
                        let first = num(body, "earliest_available")?;
                        ensure!(first > self.output_seq, "invalid replay gap");
                        diagnostics::line(format_args!("[session] Output history expired; exact screen restoration is unavailable."));
                        terminal.output(b"\x1b[!p")?;
                        self.output_seq = first - 1;
                    }
                    "InputAck" if phase == "attached" => {
                        self.same_session(body)?;
                        ensure!(
                            bin16(body, "client_instance_id")? == self.state.client_id.as_bytes(),
                            "input client mismatch"
                        );
                        self.acknowledge_input(num(body, "committed_through")?, save)?;
                        self.credit = num(body, "available_window_bytes")?.min(65536);
                    }
                    "CommandState" | "CommandAccepted" | "CommandStatus" | "CommandRejected"
                    | "CommandInterruptAccepted" | "SessionInterruptAccepted" if phase == "attached" && self.command_capable => {
                        if let Ok(value) = get(body, "input_ready") {
                            self.command_input_ready = value.as_bool().context("invalid input_ready")?;
                        }
                        if let Ok(records) = get(body, "records").and_then(|v| v.as_array().context("invalid command records")) {
                            for record in records {
                                if matches!(text(record, "state")?, "accepted" | "running") {
                                    self.active_command = Some(uuid::Uuid::parse_str(text(record, "command_id")?)?);
                                }
                            }
                        }
                        if text(body, "shell_status").ok() == Some("ready") { self.active_command = None; }
                        terminal.command_event(kind, body);
                    }
                    "InputRejected" if phase == "attached" && self.command_capable => {
                        self.same_session(body)?;
                        ensure!(self.state.pending.as_ref().map(|p| p.seq) == Some(num(body, "input_seq")?),
                            "unexpected input rejection");
                        self.state.pending = None;
                        self.pending_sent = false;
                        save(&self.state)?;
                        diagnostics::line(format_args!("[input] Host rejected human input: {}; input was not queued", text(body, "code")?));
                    }
                    "InputBackpressure" if phase == "attached" => {
                        self.same_session(body)?;
                        self.pending_sent = false;
                        self.credit = num(body, "available_window_bytes")?.min(65536);
                        retry_input_at = Instant::now()
                            + Duration::from_millis(num(body, "retry_after_ms")?.min(5000));
                    }
                    "InputSequenceGap" if phase == "attached" => {
                        self.same_session(body)?;
                        ensure!(
                            self.state.pending.as_ref().map(|p| p.seq)
                                == Some(num(body, "expected")?),
                            "input recovery bytes no longer retained"
                        );
                        self.pending_sent = false;
                    }
                    "SessionExited" if phase == "attached" => {
                        self.same_session(body)?;
                        terminal.reading(false);
                        self.state.ended = true;
                        save(&self.state)?;
                        let code = get(body, "exit_code")?;
                        if code.is_nil() {
                            diagnostics::line(format_args!(
                                "[session] Ended without an exit code: {}",
                                text(body, "reason")?
                            ));
                            return Ok(End::Exited(1));
                        }
                        let code = code.as_i64().context("invalid exit code")?;
                        ensure!(
                            code >= i32::MIN as i64 && code <= u32::MAX as i64,
                            "exit code out of range"
                        );
                        return Ok(End::Exited(code as u32));
                    }
                    "Ping" => {
                        reply = Some(message(
                            "Pong",
                            map(vec![
                                ("nonce", get(body, "nonce")?.clone()),
                                ("broker_time_ms", now_ms().into()),
                            ]),
                        ))
                    }
                    "Pong" | "SessionReady" | "ResizeAck" | "OutputEvicted" => {}
                    "WriterBusy" => {
                        // Same-client epoch migration is required; never spin or take over another writer.
                        bail!("session writer is busy; existing writer retained; retry resume after detaching it");
                    }
                    "Error" => {
                        let code = text(body, "code")?;
                        bail!("host rejected request: {code}; no replacement shell was created");
                    }
                    "SessionEnded" => {
                        self.same_session(body)?;
                        self.state.ended = true;
                        save(&self.state)?;
                        bail!("session has ended; choose a NEW reference");
                    }
                    "CreateRecoveryUnavailable" => {
                        self.state.ended = true;
                        save(&self.state)?;
                        bail!("host returned {kind}; session was not replaced");
                    }
                    "CreateRequestConflict"
                    | "SessionIdConflict"
                    | "Unauthorized"
                    | "LeaseRevoked"
                    | "ConnectionSuperseded" => {
                        bail!("host returned {kind}; session was not replaced");
                    }
                    _ => bail!("unexpected host protocol message {kind} in {phase}"),
                }
                if let Some(value) = reply {
                    if link.send(&value).is_err() {
                        terminal.reading(false);
                        return Ok(End::Disconnected);
                    }
                }
            }
            if phase != "attached" {
                if Instant::now() > handshake_deadline {
                    bail!("host handshake timed out; required session API unavailable");
                }
                continue;
            }
                if let Some(control) = terminal.control() {
                    use crate::local_control::Operation;
                    let mut fields = self.writer()?;
                    fields.push(("connection_epoch", self.state.epoch.into()));
                    fields.push(("operation_id", s(&control.operation_id.to_string())));
                    let (kind, id) = match control.action {
                        Operation::Send { command, .. } => {
                            if self.state.pending.is_some() {
                                terminal.command_event("CommandRejected", &map(vec![
                                    ("command_id", s(&control.operation_id.to_string())),
                                    ("code", s("local human input pending")),
                                ]));
                                continue;
                            }
                            fields.push(("command", s(&command)));
                            ("CommandSubmit", control.operation_id)
                        }
                        Operation::CommandStatus { command_id } => ("CommandStatus", command_id),
                        Operation::Interrupt => {
                            if let Some(id) = self.active_command { ("CommandInterrupt", id) }
                            else {
                                fields.push(("expected_command_id", rmpv::Value::Nil));
                                ("SessionInterrupt", control.operation_id)
                            }
                        }
                        _ => bail!("unexpected engine control operation"),
                    };
                    fields.push(("command_id", s(&id.to_string())));
                    if control.submission.as_ref().is_some_and(|submission| !submission.dispatch()) {
                        terminal.command_event("CommandNotSubmitted", &map(vec![
                            ("command_id", s(&control.operation_id.to_string())),
                            ("code", s("caller left or readiness deadline expired before dispatch")),
                        ]));
                        continue;
                    }
                    if link.send(&message(kind, map(fields))).is_err() {
                        terminal.reading(false);
                        return Ok(End::Disconnected);
                    }
                }
            handshake_deadline = Instant::now() + Duration::from_secs(20);
            if last_received.elapsed() > Duration::from_secs(15) {
                terminal.reading(false);
                return Ok(End::Disconnected);
            }
            if heartbeat.elapsed() > Duration::from_secs(5) {
                if link
                    .send(&message(
                        "Ping",
                        map(vec![
                            ("nonce", self.state.epoch.into()),
                            ("sent_at_ms", now_ms().into()),
                        ]),
                    ))
                    .is_err()
                {
                    return Ok(End::Disconnected);
                }
                heartbeat = Instant::now();
            }
            let size = terminal.size();
            if size != self.last_size {
                self.resize_generation = self
                    .resize_generation
                    .checked_add(1)
                    .context("resize generation exhausted")?;
                let mut fields = self.writer()?;
                fields.extend([
                    ("cols", size.0.into()),
                    ("rows", size.1.into()),
                    ("resize_generation", self.resize_generation.into()),
                ]);
                if link.send(&message("Resize", map(fields))).is_err() {
                    return Ok(End::Disconnected);
                }
                self.last_size = size;
            }
            if self.state.pending.is_some() {
                if pending_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(5)
                {
                    // Rejected input can exhaust credit too; liveness must not depend on pending_sent.
                    terminal.reading(false);
                    return Ok(End::Disconnected);
                }
            } else {
                pending_since = None;
            }
            terminal.reading(true);
            let accept_bytes = self.state.pending.is_none()
                && (!self.state.command_execution.unwrap_or(false) || self.command_input_ready);
            match terminal.input(accept_bytes)? {
                Input::Eof | Input::Detach => {
                    let _ = link.send(&self.detach()?);
                    terminal.reading(false);
                    return Ok(End::Detached);
                }
                Input::Bytes(bytes) => {
                    ensure!(
                        accept_bytes,
                        "terminal returned bytes while input was pending"
                    );
                    ensure!(
                        !bytes.is_empty() && bytes.len() <= 4096,
                        "invalid input chunk"
                    );
                    if bytes.contains(&0x1d) {
                        // Keep raw GS support for terminals that do not use Win32 input mode.
                        let _ = link.send(&self.detach()?);
                        terminal.reading(false);
                        return Ok(End::Detached);
                    }
                    let seq = self
                        .state
                        .input_ack
                        .checked_add(1)
                        .context("input sequence exhausted")?;
                    self.state.pending = Some(PendingInput { seq, bytes });
                    save(&self.state)?;
                }
                Input::Idle => {}
            }
            if !self.pending_sent && Instant::now() >= retry_input_at {
                if let Some(p) = &self.state.pending {
                    if p.bytes.len() as u64 <= self.credit {
                        let mut fields = self.writer()?;
                        fields.extend([
                            ("input_seq", p.seq.into()),
                            ("bytes", Value::Binary(p.bytes.clone())),
                        ]);
                        if link.send(&message("Input", map(fields))).is_err() {
                            terminal.reading(false);
                            return Ok(End::Disconnected);
                        }
                        self.credit -= p.bytes.len() as u64;
                        self.pending_sent = true;
                    }
                }
            }
        }
    }
}
