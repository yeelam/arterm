//! Opt-in, in-memory PowerShell integration; markers are correlation, not authorization.
//!
//! CreateSession must request `command_execution=true` after capability negotiation.
//! Only ordinary PowerShell interactive startup (-NoLogo/-NoProfile) is supported.
//! The adapter reserves __arTerm globals and wraps the existing profile prompt.
//! LASTEXITCODE is temporarily cleared to distinguish this invocation's native
//! status, and restored if no new native status is observed. Replacing prompt or
//! entering nested input stops managed readiness; there is no prompt-text fallback.
//! Raw typeahead remains usable. Managed readiness requires observed line
//! submissions to reach prompt boundaries and no partial input to remain.
//! Focus notifications are non-editing. Their mode may be enabled by the local
//! parent terminal, outside the remote output stream. Other unknown replies stay guarded.
//! Records are never evicted within a live session: admission stops at the cap
//! rather than forgetting an ID and executing a retry twice.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const CAPABILITY: &str = "command-execution-v1";
const MAX_COMMAND: usize = 16 * 1024;
const MAX_RECORDS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub command_id: Uuid,
    pub request_hash: String,
    pub state: String,
    pub succeeded: Option<bool>,
    pub exit_code: Option<i32>,
    pub interrupt_requested: bool,
}

pub struct Commands {
    nonce: String,
    pending_marker: Vec<u8>,
    ready: bool,
    initialized: bool,
    human_dirty: bool,
    waiting_prompt: bool,
    human_pending: u64,
    last_cr: bool,
    input_sequence: Vec<u8>,
    unclassified_input: bool,
    active: Option<Uuid>,
    records: BTreeMap<Uuid, Record>,
    pub revision: u64,
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as usize;
        let b = chunk.get(1).copied().unwrap_or(0) as usize;
        let c = chunk.get(2).copied().unwrap_or(0) as usize;
        result.push(ALPHABET[a >> 2] as char);
        result.push(ALPHABET[((a & 3) << 4) | (b >> 4)] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[((b & 15) << 2) | (c >> 6)] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[c & 63] as char
        } else {
            '='
        });
    }
    result
}

impl Commands {
    pub fn new() -> Self {
        Self {
            nonce: Uuid::now_v7().simple().to_string(),
            pending_marker: Vec::new(),
            ready: false,
            initialized: false,
            human_dirty: false,
            waiting_prompt: false,
            human_pending: 0,
            last_cr: false,
            input_sequence: Vec::new(),
            unclassified_input: false,
            active: None,
            records: BTreeMap::new(),
            revision: 0,
        }
    }
    pub fn supported(shell: &str, args: &[String]) -> bool {
        let name = std::path::Path::new(shell)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        matches!(
            name.as_str(),
            "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
        ) && args
            .iter()
            .all(|arg| matches!(arg.to_ascii_lowercase().as_str(), "-nologo" | "-noprofile"))
    }
    pub fn bootstrap_encoded(&self) -> String {
        // The prompt runs after PowerShell has formatted the preceding pipeline.
        // Preserve a profile-defined prompt, but reserve these adapter globals.
        let script = r#"
$global:__arTermOriginalPrompt = (Get-Item Function:\prompt).ScriptBlock
$global:__arTermCommand = $null
function global:prompt {
    if ($null -ne $global:__arTermCommand) {
        $global:__arTermSuccess = $global:__arTermSuccess -and [Object]::ReferenceEquals($global:__arTermError, $global:Error[0])
        $global:__arTermNative = $global:LASTEXITCODE
        if ($null -ne $global:__arTermNative) { $global:__arTermSuccess = $global:__arTermSuccess -and ($global:__arTermNative -eq 0) }
        if ($null -eq $global:__arTermNative) { $global:LASTEXITCODE = $global:__arTermPreviousNative }
        [Console]::Write(([string][char]27 + ']633;arterm;NONCE;done;' + $global:__arTermCommand + ';' + $global:__arTermSuccess + ';' + $global:__arTermNative + [char]7))
        $global:__arTermCommand = $null
    }
    $global:__arTermPromptText = & $global:__arTermOriginalPrompt
    [Console]::Write(([string][char]27 + ']633;arterm;NONCE;ready' + [char]7))
    $global:__arTermPromptText
}
"#.replace("NONCE", &self.nonce);
        base64(
            &script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
    }
    pub fn shell_status(&self) -> &'static str {
        if self.active.is_some() {
            "busy"
        } else if self.ready {
            "ready"
        } else {
            "not_ready"
        }
    }
    pub fn record(&self, id: Uuid) -> Option<Record> {
        self.records.get(&id).cloned()
    }
    pub fn active(&self) -> Option<Uuid> {
        self.active
    }
    pub fn input_ready(&self) -> bool {
        self.initialized
    }
    pub fn readiness_reason(&self) -> &'static str {
        if !self.initialized { "initializing" }
        else if self.active.is_some() { "managed_command" }
        else if !self.input_sequence.is_empty() { "partial_input_sequence" }
        else if self.human_dirty && self.unclassified_input { "unclassified_terminal_input" }
        else if self.human_dirty { "partial_human_input" }
        else if self.human_pending > 0 { "human_command_pending" }
        else { "ready" }
    }
    pub fn records(&self) -> Vec<Record> {
        self.records.values().cloned().collect()
    }
    pub fn submit(&mut self, id: Uuid, command: &str) -> Result<(Record, Option<Vec<u8>>)> {
        ensure!(
            !command.is_empty() && command.len() <= MAX_COMMAND,
            "CommandSize"
        );
        let hash = format!("{:x}", Sha256::digest(command.as_bytes()));
        if let Some(record) = self.records.get(&id) {
            ensure!(record.request_hash == hash, "CommandIdConflict");
            return Ok((record.clone(), None));
        }
        ensure!(self.ready && self.active.is_none(), "CommandBusy");
        // Never evict deduplication identities then accidentally execute them again.
        ensure!(self.records.len() < MAX_RECORDS, "CommandHistoryFull");
        let script = format!(
            "$global:__arTermCommand='{id}';$global:__arTermSuccess=$false;$global:__arTermError=$global:Error[0];\
             $global:__arTermPreviousNative=$global:LASTEXITCODE;$global:LASTEXITCODE=$null;\
             [Console]::Write(([string][char]27+']633;arterm;{};started;{id}'+[char]7));\
             . ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))));\
             $global:__arTermSuccess=$?",
            self.nonce, base64(command.as_bytes()));
        let line = format!(
            ". ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))))\r",
            base64(script.as_bytes()));
        let record = Record {
            command_id: id,
            request_hash: hash,
            state: "accepted".into(),
            succeeded: None,
            exit_code: None,
            interrupt_requested: false,
        };
        self.records.insert(id, record.clone());
        self.active = Some(id);
        self.ready = false;
        self.revision += 1;
        Ok((record, Some(line.into_bytes())))
    }
    pub fn submission_failed(&mut self, id: Uuid) {
        if self.active == Some(id) {
            if let Some(record) = self.records.get_mut(&id) {
                record.state = "unknown".into();
            }
            self.revision += 1;
        }
    }
    pub fn input(&mut self, bytes: &[u8]) -> Result<()> {
        let mut pending = self.input_sequence.clone();
        let mut keys = Vec::new();
        let mut unclassified = self.unclassified_input;
        for &byte in bytes {
            if pending.is_empty() {
                if byte == 27 { pending.push(byte); } else { keys.push((u32::from(byte), 1)); }
                continue;
            }
            pending.push(byte);
            let prefix = pending == b"\x1b" || pending == b"\x1b["
                || (pending.starts_with(b"\x1b[") && pending[2..].iter().all(|b| b.is_ascii_digit() || *b == b';'));
            if prefix && pending.len() <= 128 { continue; }
            if matches!(pending.as_slice(), b"\x1b[I" | b"\x1b[O") {
                // The local parent may enable these reports without remote output.
                // Preserve their bytes for ConPTY, but do not treat focus as typing.
                pending.clear();
            } else if let Some(fields) = crate::console::console_input::win32_fields(&pending) {
                if fields[3] == 1 { keys.push((fields[2], fields[5])); }
                // Key-up still reaches ConPTY unchanged; it does not edit a shell line.
                pending.clear();
            } else {
                unclassified = true;
                keys.extend(pending.drain(..).map(|byte| (u32::from(byte), 1)));
            }
        }
        ensure!((keys.is_empty() && pending.is_empty()) || (self.initialized && self.active.is_none()), "CommandBusy");
        self.input_sequence = pending;
        self.unclassified_input = unclassified;
        for (key, mut repeat) in keys {
            if key == 10 && self.last_cr {
                self.last_cr = false;
                repeat -= 1;
                if repeat == 0 { continue; }
            }
            self.last_cr = key == 13;
            if matches!(key, 3 | 10 | 13) {
                self.human_pending = self.human_pending.saturating_add(u64::from(repeat));
                self.human_dirty = false;
                self.unclassified_input = false;
            } else {
                self.human_dirty = true;
            }
        }
        self.waiting_prompt = self.human_pending > 0;
        self.ready = self.initialized && self.active.is_none() && self.human_pending == 0
            && !self.human_dirty && self.input_sequence.is_empty();
        self.revision += 1;
        Ok(())
    }
    pub fn interrupt(&mut self, id: Uuid) -> Result<()> {
        ensure!(
            self.active == Some(id) && self.records.get(&id).is_some_and(|r| r.state == "running"),
            "CommandNotRunning"
        );
        self.records.get_mut(&id).unwrap().interrupt_requested = true;
        self.revision += 1;
        Ok(())
    }
    pub fn exited(&mut self) {
        if let Some(id) = self.active.take() {
            self.records.get_mut(&id).unwrap().state = "unknown".into();
        }
        self.ready = false;
        self.initialized = false;
        self.revision += 1;
    }
    fn marker(&mut self, body: &[u8]) -> bool {
        let Ok(body) = std::str::from_utf8(body) else {
            return false;
        };
        let prefix = format!("633;arterm;{};", self.nonce);
        let Some(body) = body.strip_prefix(&prefix) else {
            return false;
        };
        let fields = body.split(';').collect::<Vec<_>>();
        match fields.as_slice() {
            ["ready"]
                if self.active.is_none()
                    || self
                        .active
                        .is_some_and(|id| self.records[&id].state == "finishing") =>
            {
                if let Some(id) = self.active.take() {
                    self.records.get_mut(&id).unwrap().state = "completed".into();
                }
                if self.human_pending > 0 {
                    self.human_pending -= 1;
                }
                let ready = self.human_pending == 0 && !self.human_dirty && self.input_sequence.is_empty();
                let changed = self.ready != ready;
                self.ready = ready;
                self.initialized = true;
                self.waiting_prompt = self.human_pending > 0;
                if changed {
                    self.revision += 1;
                }
            }
            ["started", id] => {
                if let Ok(id) = Uuid::parse_str(id) {
                    if self.active == Some(id) {
                        let record = self.records.get_mut(&id).unwrap();
                        if record.state == "accepted" {
                            record.state = "running".into();
                            self.revision += 1;
                        }
                    }
                }
            }
            ["done", id, success, exit] => {
                if let Ok(id) = Uuid::parse_str(id) {
                    if self.active == Some(id) && matches!(*success, "True" | "False") {
                        let record = self.records.get_mut(&id).unwrap();
                        if record.state == "running" {
                            record.state = "finishing".into();
                            record.succeeded = Some(*success == "True");
                            record.exit_code = exit.parse().ok();
                            self.revision += 1;
                        }
                    }
                }
            }
            _ => {}
        }
        true
    }
    pub fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            if self.pending_marker.is_empty() {
                if byte == 27 {
                    self.pending_marker.push(byte);
                } else {
                    output.push(byte);
                }
                continue;
            }
            self.pending_marker.push(byte);
            if self.pending_marker.len() == 2 && byte != b']' {
                output.append(&mut self.pending_marker);
                continue;
            }
            let length = self.pending_marker.len();
            let ending = if byte == 7 {
                1
            } else if self.pending_marker.ends_with(b"\x1b\\") {
                2
            } else {
                0
            };
            if ending != 0 {
                let marker = std::mem::take(&mut self.pending_marker);
                if !self.marker(&marker[2..length - ending]) {
                    output.extend(marker);
                }
            } else if length > 1024 {
                output.append(&mut self.pending_marker);
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn focus_notifications_do_not_require_remote_mode_or_clear_edits_or_busy() {
        fn ready() -> Commands {
            let mut commands = Commands::new();
            commands.output(format!("\x1b]633;arterm;{};ready\x07", commands.nonce).as_bytes());
            commands
        }
        let mut initializing = Commands::new();
        initializing.input(b"\x1b[O\x1b[I").unwrap();
        assert_eq!(initializing.readiness_reason(), "initializing");
        for split in 0..=6 {
            let mut commands = ready();
            commands.input(&b"\x1b[O\x1b[I"[..split]).unwrap();
            commands.input(&b"\x1b[O\x1b[I"[split..]).unwrap();
            assert_eq!(commands.shell_status(), "ready",
                "focus reporting may have been enabled by the local parent terminal");
        }
        let mut unknown = ready();
        unknown.input(b"\x1b[0;0R\x1b[I").unwrap();
        unknown.output(format!("\x1b]633;arterm;{};ready\x07", unknown.nonce).as_bytes());
        assert_eq!(unknown.readiness_reason(), "unclassified_terminal_input",
            "a delayed prompt marker does not prove unknown input left the edit buffer empty");
        assert!(unknown.submit(Uuid::now_v7(), "must not run").is_err());

        let mut commands = ready();
        commands.input(b"\x1b[").unwrap();
        assert_eq!(commands.readiness_reason(), "partial_input_sequence");
        commands.input(b"O\x1b[I").unwrap();
        assert_eq!(commands.shell_status(), "ready");
        commands.input(b"x\x1b[O\x1b[I").unwrap();
        assert_eq!(commands.readiness_reason(), "partial_human_input");
        assert!(commands.submit(Uuid::now_v7(), "must not run").is_err());

        let mut busy = ready();
        busy.submit(Uuid::now_v7(), "Start-Sleep -Seconds 1").unwrap();
        busy.input(b"\x1b[O\x1b[I").unwrap();
        assert_eq!(busy.shell_status(), "busy");
        assert!(busy.input(b"x").is_err());
        assert!(busy.input(b"\x1b[0;0R").is_err(), "unrelated replies remain guarded");
    }
    #[test]
    fn win32_releases_do_not_edit_but_keydown_and_unknown_replies_remain_guarded() {
        let mut commands = Commands::new();
        let ready = format!("\x1b]633;arterm;{};ready\x07", commands.nonce);
        commands.output(ready.as_bytes());
        commands.input(b"\x1b[16;42;").unwrap();
        assert_eq!(commands.readiness_reason(), "partial_input_sequence");
        assert!(commands.submit(Uuid::now_v7(), "unexpected").is_err());
        commands.input(b"0;0;0;1_").unwrap();
        assert_eq!(commands.shell_status(), "ready");
        commands.input(b"\x1b[65;30;97;1;0;1_\x1b[65;30;97;0;0;1_").unwrap();
        assert_eq!(commands.readiness_reason(), "partial_human_input");
        assert!(commands.submit(Uuid::now_v7(), "unexpected").is_err());
        commands.input(b"\x1b[13;28;13;1;0;1_\x1b[13;28;13;0;0;1_").unwrap();
        assert_eq!(commands.readiness_reason(), "human_command_pending");
        commands.output(ready.as_bytes());
        assert_eq!(commands.shell_status(), "ready");
        commands.input(b"\x1b[1;2R").unwrap();
        assert_eq!(commands.readiness_reason(), "unclassified_terminal_input", "unsolicited replies must not be blindly ignored");
        commands.input(b"\r").unwrap();
        commands.output(ready.as_bytes());
        commands.submit(Uuid::now_v7(), "Start-Sleep -Seconds 1").unwrap();
        commands.input(b"\x1b[16;42;0;0;0;1_").unwrap();
        assert_eq!(commands.shell_status(), "busy");
        assert!(commands.input(b"\x1b[65;30;97;1;0;1_").is_err());
        assert!(commands.input(b"\x1b[16;42;").is_err(), "unknown partial sequences remain rejected while busy");
    }
    #[test]
    fn correlated_markers_fragmentation_and_deduplication() {
        let mut commands = Commands::new();
        let ready = format!("\x1b]633;arterm;{};ready\x07", commands.nonce);
        for byte in ready.bytes() {
            assert!(commands.output(&[byte]).is_empty());
        }
        let id = Uuid::now_v7();
        let (record, bytes) = commands.submit(id, "$x=1").unwrap();
        assert_eq!(record.state, "accepted");
        assert!(bytes.is_some());
        assert!(commands.input(b"x").is_err());
        assert!(commands.submit(Uuid::now_v7(), "bad").is_err());
        assert!(commands.submit(id, "$x=2").is_err());
        assert!(commands.submit(id, "$x=1").unwrap().1.is_none());
        let events = format!("\x1b]633;arterm;{};started;{id}\x1b\\visible\r\n\x1b]633;arterm;{};done;{id};True;0\x07\x1b]633;arterm;{};ready\x07",
            commands.nonce, commands.nonce, commands.nonce);
        let mut visible = Vec::new();
        for byte in events.bytes() {
            visible.extend(commands.output(&[byte]));
        }
        assert_eq!(visible, b"visible\r\n");
        assert_eq!(commands.record(id).unwrap().state, "completed");
        let before = commands.revision;
        commands.output(events.as_bytes());
        assert_eq!(commands.revision, before);
    }
    #[test]
    fn unknown_markers_shells_and_dirty_input_never_enable_execution() {
        assert!(!Commands::supported("cmd.exe", &[]));
        assert!(!Commands::supported(
            "powershell.exe",
            &["-File".into(), "script.ps1".into()]
        ));
        let mut commands = Commands::new();
        let fake = b"\x1b]633;D;0\x07";
        assert_eq!(commands.output(fake), fake);
        assert!(commands.submit(Uuid::now_v7(), "x").is_err());
        commands.output(format!("\x1b]633;arterm;{};ready\x07", commands.nonce).as_bytes());
        commands.input(b"x").unwrap();
        commands.output(format!("\x1b]633;arterm;{};ready\x07", commands.nonce).as_bytes());
        assert!(commands.submit(Uuid::now_v7(), "x").is_err());
        commands.input(b"a\rb\r").unwrap();
        commands.input(b"\r").unwrap();
        for _ in 0..3 {
            commands.output(format!("\x1b]633;arterm;{};ready\x07", commands.nonce).as_bytes());
        }
        assert_eq!(commands.shell_status(), "ready");
        assert!(commands.record(Uuid::now_v7()).is_none());
        let utf8 = "ordinary \u{20ac} output".as_bytes();
        let mut result = Vec::new();
        for byte in utf8 {
            result.extend(commands.output(&[*byte]));
        }
        assert_eq!(result, utf8);
    }
}
