//! Opt-in, in-memory PowerShell integration; markers are correlation, not authorization.
//!
//! CreateSession must request `command_execution=true` after capability negotiation.
//! Only ordinary PowerShell interactive startup (-NoLogo/-NoProfile) is supported.
//! The adapter reserves __arTerm globals and wraps the existing profile prompt.
//! LASTEXITCODE is temporarily cleared to distinguish this invocation's native
//! status, and restored if no new native status is observed. Replacing prompt
//! stops managed readiness; there is no prompt-text fallback.
//! A private mailbox supplies data to PSReadLine, which returns the original
//! source to the existing shell. Foreground application input is not counted as
//! submitted shell lines. The hook checks the real editor buffer before insertion.
//! Focus notifications are non-editing. Their mode may be enabled by the local
//! parent terminal, outside the remote output stream. Other unknown replies stay guarded.
//! Records are never evicted within a live session: admission stops at the cap
//! rather than forgetting an ID and executing a retry twice.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;
use crate::readiness_diagnostics::{Details, InputSummary, ReadinessReason, ShellStatus, Snapshot};

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
    unsupported: bool,
    human_dirty: bool,
    waiting_prompt: bool,
    human_pending: u64,
    last_cr: bool,
    input_sequence: Vec<u8>,
    unclassified_input: bool,
    last_input: InputSummary,
    marker_count: u64,
    active: Option<Uuid>,
    pending_command: Option<(String, std::time::Instant)>,
    start_confirmation: Option<(Uuid, std::time::Instant)>,
    late_confirmation: Option<Uuid>,
    records: BTreeMap<Uuid, Record>,
    pub revision: u64,
}

fn summarize_character(value: u32, summary: &mut InputSummary) {
    match value {
        3 | 10 | 13 => summary.submit_or_interrupt = true,
        8 | 9 | 27 | 127 => summary.editing = true,
        0..=31 => summary.nontext_key = true,
        _ => summary.text = true,
    }
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

fn history_setup(history: Option<&std::path::Path>) -> Result<String> {
    if let Some(path) = history {
        let text = path.to_str().ok_or_else(|| anyhow::anyhow!("ShellHistoryPathInvalid"))?;
        ensure!(path.is_absolute() && !text.contains('\0'), "ShellHistoryPathMustBeAbsolute");
        Ok(format!("Set-PSReadLineOption -HistorySavePath ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))) -ErrorAction Stop", base64(text.as_bytes())))
    } else { Ok(String::new()) }
}

impl Commands {
    pub fn new() -> Self {
        Self {
            nonce: Uuid::now_v7().simple().to_string(),
            pending_marker: Vec::new(),
            ready: false,
            initialized: false,
            unsupported: false,
            human_dirty: false,
            waiting_prompt: false,
            human_pending: 0,
            last_cr: false,
            input_sequence: Vec::new(),
            unclassified_input: false,
            last_input: InputSummary::default(),
            marker_count: 0,
            active: None,
            pending_command: None,
            start_confirmation: None,
            late_confirmation: None,
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
    pub fn bootstrap_mailbox(&self, pipe: &str, host_started: u64) -> String {
        self.bootstrap_mailbox_with_history(pipe, host_started, None).expect("no history override")
    }
    pub fn bootstrap_history(history: &std::path::Path) -> Result<String> {
        let script = format!("Import-Module PSReadLine -ErrorAction Stop\n{}", history_setup(Some(history))?);
        Ok(base64(&script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>()))
    }
    pub fn bootstrap_mailbox_with_history(&self, pipe: &str, host_started: u64,
        history: Option<&std::path::Path>) -> Result<String> {
        let history_setup = history_setup(history)?;
        // The prompt runs after PowerShell has formatted the preceding pipeline.
        // Preserve a profile-defined prompt, but reserve these adapter globals.
        let script = r#"
$global:__arTermDisabled=$true
try {
Import-Module PSReadLine -ErrorAction Stop
HISTORY_SETUP
if (-not (Get-Command PSConsoleHostReadLine -ErrorAction SilentlyContinue)) { throw 'ShellIntegrationUnsupported' }
$methods=[Microsoft.PowerShell.PSConsoleReadLine].GetMethods().Name
foreach ($required in 'GetBufferState','Insert','AcceptLine') {
    if ($methods -notcontains $required) { throw 'ShellIntegrationUnsupported' }
}
Add-Type -TypeDefinition @'
using System;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Diagnostics;
using System.Text;
public static class ArTermShellMailbox {
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool GetNamedPipeServerProcessId(IntPtr h, out uint pid);
    public static string Exchange(string name, int host, long started, string request) {
        using (var pipe = new NamedPipeClientStream(".", name, PipeDirection.InOut, PipeOptions.Asynchronous)) {
            pipe.Connect(1000);
            uint pid;
            if (!GetNamedPipeServerProcessId(pipe.SafePipeHandle.DangerousGetHandle(), out pid) || pid != host)
                throw new InvalidOperationException("ShellMailboxHostRejected");
            using (var process = Process.GetProcessById(host)) {
                if (process.StartTime.ToUniversalTime().ToFileTimeUtc() != started)
                    throw new InvalidOperationException("ShellMailboxHostChanged");
            }
            byte[] bytes = Encoding.ASCII.GetBytes(request + "\n");
            var write = pipe.WriteAsync(bytes, 0, bytes.Length);
            if (!write.Wait(2000)) throw new TimeoutException("ShellMailboxWriteTimeout");
            var result = new StringBuilder();
            var buffer = new byte[1024];
            var timer = Stopwatch.StartNew();
            while (result.Length <= 24576 && timer.ElapsedMilliseconds < 2000) {
                var read = pipe.ReadAsync(buffer, 0, buffer.Length);
                if (!read.Wait(2000)) throw new TimeoutException("ShellMailboxReadTimeout");
                if (read.Result == 0) throw new InvalidOperationException("ShellMailboxClosed");
                result.Append(Encoding.ASCII.GetString(buffer, 0, read.Result));
                if (result[result.Length - 1] == '\n') {
                    pipe.WriteByte(10);
                    return result.ToString().TrimEnd('\n');
                }
            }
            throw new InvalidOperationException("ShellMailboxResponseLimit");
        }
    }
}
'@ -ErrorAction Stop
function global:__arTermExchange([string]$request) {
    [ArTermShellMailbox]::Exchange('MAILBOX', HOSTPID, HOSTSTART, $request)
}
$global:__arTermOriginalPrompt = (Get-Item Function:\prompt).ScriptBlock
$global:__arTermCommand = $null
$global:__arTermAcceptedCommand = $null
$global:__arTermReading = $false
$global:__arTermHistoryOverride=$false
function global:prompt {
    $global:__arTermSuccess = $?
    if ($global:__arTermDisabled -or $global:__arTermReading) { return (& $global:__arTermOriginalPrompt) }
    if ($global:__arTermHistoryOverride) {
        Set-PSReadLineOption -AddToHistoryHandler $global:__arTermOriginalHistory
        $global:__arTermHistoryOverride=$false
    }
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
Set-PSReadLineKeyHandler -Chord F24 -ScriptBlock {
    $line=$null; $cursor=0
    [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line,[ref]$cursor)
    try {
        $offer=__arTermExchange 'offer'
        if ($offer -eq '-') { return }
        $parts=$offer.Split(';',2)
        if ($parts.Length -ne 2) { throw 'ShellMailboxInvalid' }
        $id=$parts[0]
        if ($line.Length -ne 0) { $null=__arTermExchange ('partial;'+$id); return }
        $source=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($parts[1]))
        $tokens=$null; $errors=$null
        $null=[Management.Automation.Language.Parser]::ParseInput($source,[ref]$tokens,[ref]$errors)
        if ($errors.Count -ne 0) {
            $null=__arTermExchange ('syntax;'+$id)
            [Console]::WriteLine('arTerm: CommandSyntaxInvalid')
            return
        }
        [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line,[ref]$cursor)
        if ($line.Length -ne 0) { $null=__arTermExchange ('partial;'+$id); return }
        if ((__arTermExchange ('commit;'+$id)) -ne 'committed') { return }
        $global:__arTermOriginalHistory=(Get-PSReadLineOption).AddToHistoryHandler
        $global:__arTermHistoryOverride=$true
        Set-PSReadLineOption -AddToHistoryHandler {
            param($line)
            Set-PSReadLineOption -AddToHistoryHandler $global:__arTermOriginalHistory
            $global:__arTermHistoryOverride=$false
            return $false
        } -ErrorAction Stop
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert($source)
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
        $global:__arTermAcceptedCommand=$id
    } catch {
        if ($global:__arTermHistoryOverride) {
            Set-PSReadLineOption -AddToHistoryHandler $global:__arTermOriginalHistory
            $global:__arTermHistoryOverride=$false
        }
        [Console]::WriteLine('arTerm: ShellMailboxUnavailable; command outcome may be unknown')
    }
} -ErrorAction Stop
$global:__arTermOriginalReadLine=(Get-Item Function:\PSConsoleHostReadLine).ScriptBlock
function global:PSConsoleHostReadLine {
    $global:__arTermReading=$true
    try {
        $source = & $global:__arTermOriginalReadLine
        if ($null -ne $global:__arTermAcceptedCommand) {
            $global:__arTermError=$global:Error[0]
            $global:__arTermPreviousNative=$global:LASTEXITCODE
            $global:LASTEXITCODE=$null
            $global:__arTermCommand=$global:__arTermAcceptedCommand
            $global:__arTermAcceptedCommand=$null
            try { $null=__arTermExchange ('started;'+$global:__arTermCommand) }
            catch { [Console]::WriteLine('arTerm: ShellMailboxUnavailable; command outcome may be unknown') }
        }
        $source
    } finally {
        $global:__arTermAcceptedCommand=$null
        $global:__arTermReading=$false
    }
}
$global:__arTermDisabled=$false
} catch {
    [Console]::WriteLine('arTerm: ShellIntegrationUnsupported')
    [Console]::Write(([string][char]27 + ']633;arterm;NONCE;unsupported' + [char]7))
}
"#.replace("NONCE", &self.nonce)
    .replace("MAILBOX", pipe)
    .replace("HOSTPID", &std::process::id().to_string())
    .replace("HOSTSTART", &host_started.to_string())
    .replace("HISTORY_SETUP", &history_setup);
        Ok(base64(
            &script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        ))
    }
    pub fn shell_status(&self) -> &'static str {
        if self.unsupported {
            "unsupported"
        } else if self.active.is_some() {
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
        if self.unsupported { "integration_disabled" }
        else if !self.initialized { "initializing" }
        else if self.active.is_some() { "managed_command" }
        else if !self.input_sequence.is_empty() { "partial_input_sequence" }
        else if self.human_dirty && self.unclassified_input { "unclassified_terminal_input" }
        else if self.human_dirty { "partial_human_input" }
        else if self.human_pending > 0 { "human_command_pending" }
        else if !self.ready && self.late_confirmation.is_some() { "command_outcome_unknown" }
        else { "ready" }
    }
    pub fn records(&self) -> Vec<Record> {
        self.records.values().cloned().collect()
    }
    pub fn diagnostic_details(&self) -> Details {
        Details {
            state: Snapshot {
                status: ShellStatus::from_wire(self.shell_status()),
                reason: ReadinessReason::from_wire(self.readiness_reason()),
                initialized: Some(self.initialized),
                human_dirty: Some(self.human_dirty),
                unclassified_input: Some(self.unclassified_input),
                pending_sequence: Some(!self.input_sequence.is_empty()),
                human_pending: Some(self.human_pending),
                revision: Some(self.revision),
            },
            input: self.last_input,
        }
    }
    pub fn diagnostic_marker_count(&self) -> u64 { self.marker_count }
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
        ensure!(self.initialized, "IntegrationNotEstablished");
        ensure!(self.ready && self.active.is_none(), "CommandBusy");
        // Never evict deduplication identities then accidentally execute them again.
        ensure!(self.records.len() < MAX_RECORDS, "CommandHistoryFull");
        self.pending_command = Some((base64(command.as_bytes()), std::time::Instant::now()));
        self.late_confirmation = None;
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
        Ok((record, Some(b"\x1b[135;0;0;1;0;1_\x1b[135;0;0;0;0;1_".to_vec())))
    }
    pub fn mailbox_exchange(&mut self, request: &str) -> Result<String> {
        self.expire_pending();
        if let Some(requested) = request.strip_prefix("started;") {
            let id = Uuid::parse_str(requested)?;
            ensure!(self.confirm_started(id), "ShellMailboxCommandChanged");
            return Ok("started".into());
        }
        let Some(id) = self.active else { return Ok("-".into()); };
        if request == "offer" {
            return Ok(self.pending_command.as_ref().map_or("-".into(), |(payload, _)| format!("{id};{payload}")));
        }
        let Some((operation, requested)) = request.split_once(';') else { anyhow::bail!("ShellMailboxRequestInvalid"); };
        ensure!(requested == id.to_string() && self.records[&id].state == "accepted"
            && self.pending_command.is_some(), "ShellMailboxCommandChanged");
        match operation {
            "commit" => {
                self.pending_command = None;
                self.records.get_mut(&id).unwrap().state = "committed".into();
                self.start_confirmation = Some((id, std::time::Instant::now()));
                self.revision += 1;
                Ok("committed".into())
            }
            "partial" | "syntax" => {
                self.pending_command = None;
                let record = self.records.get_mut(&id).unwrap();
                record.state = "not_submitted".into();
                self.active = None;
                self.human_dirty = operation == "partial";
                self.ready = !self.human_dirty;
                self.revision += 1;
                Ok("rejected".into())
            }
            _ => anyhow::bail!("ShellMailboxRequestInvalid"),
        }
    }
    pub fn expire_pending(&mut self) {
        if self.start_confirmation.is_some_and(|(_, since)| since.elapsed() >= std::time::Duration::from_secs(5)) {
            let (id, _) = self.start_confirmation.take().unwrap();
            self.records.get_mut(&id).unwrap().state = "unknown".into();
            self.late_confirmation = Some(id);
            self.active = None;
            self.ready = false;
            self.revision += 1;
        }
        if self.pending_command.as_ref().is_some_and(|(_, since)| since.elapsed() >= std::time::Duration::from_secs(5)) {
            self.pending_command = None;
            if let Some(id) = self.active.take() {
                self.records.get_mut(&id).unwrap().state = "not_submitted".into();
            }
            // No cooperating hook proved an empty editor. Do not claim readiness.
            self.unsupported = true;
            self.ready = false;
            self.revision += 1;
        }
    }
    fn confirm_started(&mut self, id: Uuid) -> bool {
        let eligible = self.active == Some(id) && self.records[&id].state == "committed"
            || self.active.is_none() && self.late_confirmation == Some(id)
                && self.records.get(&id).is_some_and(|record| record.state == "unknown");
        if eligible {
            self.records.get_mut(&id).unwrap().state = "running".into();
            self.active = Some(id);
            self.ready = false;
            self.start_confirmation = None;
            self.late_confirmation = None;
            self.revision += 1;
        }
        eligible
    }
    pub fn submission_failed(&mut self, id: Uuid) {
        if self.active == Some(id) {
            self.pending_command = None;
            self.start_confirmation = None;
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
        let mut summary = InputSummary::default();
        for &byte in bytes {
            if pending.is_empty() {
                if byte == 27 { pending.push(byte); } else {
                    summarize_character(u32::from(byte), &mut summary);
                    keys.push((u32::from(byte), 1));
                }
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
                summary.focus = true;
            } else if let Some(fields) = crate::console::console_input::win32_fields(&pending) {
                let modifier = fields[2] == 0
                    && crate::console::console_input::is_modifier_key(fields[0]);
                if fields[3] == 0 { summary.key_release = true; }
                else if modifier { summary.modifier = true; }
                else {
                    summarize_character(fields[2], &mut summary);
                    if fields[2] == 0 && matches!(fields[0], 0x08 | 0x09 | 0x21..=0x28 | 0x2d | 0x2e) {
                        summary.editing = true;
                    }
                }
                let private_hook = fields[0] == 135 && fields[2] == 0;
                if fields[3] == 1 && !modifier && !private_hook { keys.push((fields[2], fields[5])); }
                // Key-up still reaches ConPTY unchanged; it does not edit a shell line.
                pending.clear();
            } else {
                unclassified = true;
                summary.unclassified = true;
                keys.extend(pending.drain(..).map(|byte| (u32::from(byte), 1)));
            }
        }
        summary.incomplete = !pending.is_empty();
        self.last_input = summary;
        if !keys.is_empty() || !pending.is_empty() {
            ensure!(self.initialized, "IntegrationNotEstablished");
            ensure!(self.active.is_none()
                || self.active.is_some_and(|id| matches!(self.records[&id].state.as_str(),
                    "committed" | "running" | "finishing")), "CommandBusy");
        }
        if self.active.is_some_and(|id| matches!(self.records[&id].state.as_str(), "committed" | "running"))
            || self.active.is_none() && self.late_confirmation.is_some() && !self.ready {
            // Foreground input belongs to the running application, not to the
            // PSReadLine editor. The next mailbox hook checks the actual buffer.
            self.input_sequence = pending;
            self.revision += 1;
            return Ok(());
        }
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
            ["unsupported"] => {
                self.unsupported = true;
                self.initialized = false;
                self.ready = false;
                self.revision += 1;
            }
            ["ready"]
                if !self.unsupported && (self.active.is_none()
                    || self
                        .active
                        .is_some_and(|id| self.records[&id].state == "finishing")) =>
            {
                self.marker_count = self.marker_count.saturating_add(1);
                let managed_prompt = self.active.is_some();
                if let Some(id) = self.active.take() {
                    self.records.get_mut(&id).unwrap().state = "completed".into();
                }
                if !managed_prompt && self.human_pending > 0 {
                    self.human_pending -= 1;
                }
                let ready = self.human_pending == 0 && !self.human_dirty && self.input_sequence.is_empty();
                let changed = self.ready != ready || managed_prompt || !self.initialized;
                self.ready = ready;
                self.initialized = true;
                self.waiting_prompt = self.human_pending > 0;
                if changed {
                    self.revision += 1;
                }
            }
            ["started", id] => {
                if let Ok(id) = Uuid::parse_str(id) {
                    if self.confirm_started(id) {
                        self.marker_count = self.marker_count.saturating_add(1);
                    }
                }
            }
            ["done", id, success, exit] => {
                if let Ok(id) = Uuid::parse_str(id) {
                    if matches!(*success, "True" | "False") { self.confirm_started(id); }
                    if self.active == Some(id) && matches!(*success, "True" | "False") {
                        let record = self.records.get_mut(&id).unwrap();
                        if record.state == "running" {
                            self.marker_count = self.marker_count.saturating_add(1);
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
    fn ready_marker_publishes_completion_while_a_partial_tail_blocks_readiness() {
        let mut commands = Commands::new();
        let ready = format!("\x1b]633;arterm;{};ready\x07", commands.nonce);
        commands.output(ready.as_bytes());
        let id = Uuid::now_v7();
        commands.submit(id, "owned command").unwrap();
        commands.mailbox_exchange(&format!("commit;{id}")).unwrap();
        commands.mailbox_exchange(&format!("started;{id}")).unwrap();
        commands.output(format!("\x1b]633;arterm;{};done;{id};True;\x07", commands.nonce).as_bytes());
        commands.input(b"$Tail='").unwrap();
        assert_eq!(commands.record(id).unwrap().state, "finishing");
        let before = commands.revision;
        commands.output(ready.as_bytes());
        assert_eq!(commands.record(id).unwrap().state, "completed");
        assert!(commands.active().is_none());
        assert_eq!(commands.shell_status(), "not_ready");
        assert_eq!(commands.readiness_reason(), "partial_human_input");
        assert_eq!(commands.revision, before + 1, "completion must publish even when ready remains false");
        commands.output(ready.as_bytes());
        assert_eq!(commands.revision, before + 1, "an unchanged prompt must not publish another transition");
        assert!(commands.submit(Uuid::now_v7(), "must not overwrite").is_err());
        commands.input(b"retained'\r").unwrap();
        commands.output(ready.as_bytes());
        assert_eq!(commands.shell_status(), "ready");
    }

    #[test]
    fn ready_marker_publishes_initialization_even_when_readiness_stays_false() {
        let mut commands = Commands::new();
        commands.human_dirty = true;
        let before = commands.revision;
        commands.output(format!("\x1b]633;arterm;{};ready\x07", commands.nonce).as_bytes());
        assert!(commands.input_ready());
        assert_eq!(commands.shell_status(), "not_ready");
        assert_eq!(commands.revision, before + 1);
    }

    #[test]
    fn committed_watchdog_is_ambiguous_and_late_confirmation_is_id_scoped() {
        let mut commands = Commands::new();
        let ready = format!("\x1b]633;arterm;{};ready\x07", commands.nonce);
        commands.output(ready.as_bytes());
        let id = Uuid::now_v7();
        commands.submit(id, "owned command").unwrap();
        commands.mailbox_exchange(&format!("commit;{id}")).unwrap();
        assert_eq!(commands.record(id).unwrap().state, "committed");
        commands.start_confirmation.as_mut().unwrap().1 -= std::time::Duration::from_secs(6);
        commands.expire_pending();
        assert_eq!(commands.record(id).unwrap().state, "unknown");
        assert_ne!(commands.shell_status(), "busy");
        assert!(commands.submit(id, "owned command").unwrap().1.is_none());
        commands.input(b"possible foreground answer\r").unwrap();
        assert_eq!(commands.human_pending, 0);
        assert!(commands.mailbox_exchange(&format!("started;{}", Uuid::now_v7())).is_err());
        commands.mailbox_exchange(&format!("started;{id}")).unwrap();
        assert_eq!(commands.record(id).unwrap().state, "running");
        commands.output(format!("\x1b]633;arterm;{};done;{id};True;\x07", commands.nonce).as_bytes());
        commands.input(b"next partial").unwrap();
        commands.output(ready.as_bytes());
        assert_eq!(commands.record(id).unwrap().state, "completed");
        assert_eq!(commands.readiness_reason(), "partial_human_input");
        assert!(commands.submit(Uuid::now_v7(), "cannot overwrite").is_err());
    }

    #[test]
    fn history_override_requires_absolute_data_path() {
        let commands = Commands::new();
        assert!(commands.bootstrap_mailbox_with_history("fixture", 1,
            Some(std::path::Path::new("relative-history.txt"))).is_err());
        assert!(commands.bootstrap_mailbox_with_history("fixture", 1,
            Some(std::path::Path::new(r"C:\owned\quote'and-unicode-history.txt"))).is_ok());
    }

    #[test]
    fn mailbox_commit_allows_foreground_answers_without_phantom_lines() {
        let mut commands = Commands::new();
        let ready = format!("\x1b]633;arterm;{};ready\x07", commands.nonce);
        commands.output(ready.as_bytes());
        assert_eq!(commands.mailbox_exchange("offer").unwrap(), "-");
        let id = Uuid::now_v7();
        let (_, wake) = commands.submit(id, "Read-Host; Read-Host").unwrap();
        assert_eq!(wake.unwrap(), b"\x1b[135;0;0;1;0;1_\x1b[135;0;0;0;0;1_");
        assert!(commands.input(b"early\r").is_err());
        assert!(commands.mailbox_exchange("commit;wrong").is_err());
        assert!(commands.mailbox_exchange("offer").unwrap().starts_with(&id.to_string()));
        assert_eq!(commands.mailbox_exchange(&format!("commit;{id}")).unwrap(), "committed");
        commands.input(b"first\rsecond\r#tail").unwrap();
        assert_eq!(commands.human_pending, 0);
        assert!(commands.submit(Uuid::now_v7(), "must not answer").is_err());
        commands.output(format!("\x1b]633;arterm;{};done;{id};True;\x07", commands.nonce).as_bytes());
        commands.output(ready.as_bytes());
        assert_eq!(commands.shell_status(), "ready");
        let next = Uuid::now_v7();
        commands.submit(next, "must not overwrite").unwrap();
        commands.mailbox_exchange(&format!("partial;{next}")).unwrap();
        assert_eq!(commands.readiness_reason(), "partial_human_input");
        assert!(commands.submit(Uuid::now_v7(), "must not overwrite").is_err());
        assert!(commands.submit(id, "Read-Host; Read-Host").unwrap().1.is_none());
    }

    #[test]
    fn unsupported_integration_cannot_become_ready() {
        let mut commands = Commands::new();
        commands.output(format!("\x1b]633;arterm;{};unsupported\x07", commands.nonce).as_bytes());
        commands.output(format!("\x1b]633;arterm;{};ready\x07", commands.nonce).as_bytes());
        assert_eq!(commands.shell_status(), "unsupported");
        assert!(commands.submit(Uuid::now_v7(), "must not run").is_err());
    }
    #[test]
    fn mailbox_deadline_and_manual_private_key_do_not_execute() {
        let mut commands = Commands::new();
        commands.output(format!("\x1b]633;arterm;{};ready\x07", commands.nonce).as_bytes());
        commands.input(b"\x1b[135;0;0;1;0;1_\x1b[135;0;0;0;0;1_").unwrap();
        assert_eq!(commands.shell_status(), "ready");
        let id = Uuid::now_v7();
        commands.submit(id, "must expire").unwrap();
        commands.pending_command.as_mut().unwrap().1 -= std::time::Duration::from_secs(6);
        assert_eq!(commands.mailbox_exchange(&format!("commit;{id}")).unwrap(), "-");
        assert_eq!(commands.record(id).unwrap().state, "not_submitted");
        assert_eq!(commands.mailbox_exchange("offer").unwrap(), "-");
        assert!(commands.submit(id, "must expire").unwrap().1.is_none());
    }
    #[test]
    fn modifier_key_down_without_text_never_marks_the_line_dirty() {
        let mut commands = Commands::new();
        let ready = format!("\x1b]633;arterm;{};ready\x07", commands.nonce);
        commands.output(ready.as_bytes());
        for key in [16, 17, 18, 91, 92, 160, 161, 162, 163, 164, 165] {
            commands.input(format!("\x1b[{key};0;0;1;0;1_").as_bytes()).unwrap();
            assert_eq!(commands.readiness_reason(), "ready", "modifier {key} dirtied an empty line");
        }
        commands.input(b"\x1b[16;42;97;1;16;1_").unwrap();
        assert_eq!(commands.readiness_reason(), "partial_human_input", "text-bearing records remain guarded");
        commands.input(b"\r").unwrap();
        commands.output(ready.as_bytes());
        commands.input(b"private-input").unwrap();
        commands.input(b"\x1b[16;42;0;1;0;1_").unwrap();
        assert_eq!(commands.readiness_reason(), "partial_human_input");
        commands.input(b"\r").unwrap();
        commands.output(ready.as_bytes());
        commands.submit(Uuid::now_v7(), "Start-Sleep -Seconds 1").unwrap();
        commands.input(b"\x1b[17;29;0;1;8;1_").unwrap();
        assert_eq!(commands.shell_status(), "busy");
        assert!(commands.input(b"\x1b[37;75;0;1;0;1_").is_err(), "navigation must remain guarded");
    }

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
        commands.mailbox_exchange(&format!("commit;{id}")).unwrap();
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
        commands.output(b"\x1b[?1004h\x1b[6n\x1b[?1;2c");
        assert!(!commands.input_ready(), "VT capability is not shell integration");
        assert_eq!(commands.readiness_reason(), "initializing");
        assert_eq!(commands.input(b"\x1b[?99z").unwrap_err().to_string(), "IntegrationNotEstablished");
        assert_eq!(commands.submit(Uuid::now_v7(), "x").unwrap_err().to_string(), "IntegrationNotEstablished");
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
