//! Owned ConPTY production regressions. History is redirected before any ReadLine.
use arterm::{shell_integration::Commands, shell_mailbox::Mailbox};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for bytes in bytes.chunks(3) {
        let a = bytes[0] as usize;
        let b = bytes.get(1).copied().unwrap_or(0) as usize;
        let c = bytes.get(2).copied().unwrap_or(0) as usize;
        out.push(ALPHABET[a >> 2] as char);
        out.push(ALPHABET[(a & 3) << 4 | b >> 4] as char);
        out.push(if bytes.len() > 1 { ALPHABET[(b & 15) << 2 | c >> 6] as char } else { '=' });
        out.push(if bytes.len() > 2 { ALPHABET[c & 63] as char } else { '=' });
    }
    out
}
fn decode_script(encoded: &str) -> String {
    let mut bytes = Vec::new();
    let mut bits = 0u32;
    let mut count = 0;
    for value in encoded.bytes().take_while(|value| *value != b'=') {
        bits = (bits << 6) | ALPHABET.iter().position(|b| *b == value).unwrap() as u32;
        count += 6;
        if count >= 8 { count -= 8; bytes.push((bits >> count) as u8); }
    }
    assert_eq!(bytes.len() % 2, 0);
    String::from_utf16(&bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect::<Vec<_>>()).unwrap()
}

struct Terminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
    home: PathBuf,
}
impl Terminal {
    fn new(shell: &str, encoded: &str) -> Self {
        let home = std::env::temp_dir().join(format!("arterm-shell-fixture-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&home).unwrap();
        let history = home.join("history.txt").to_str().unwrap().replace('\'', "''");
        let script = format!(
            "Import-Module PSReadLine -ErrorAction Stop\nSet-PSReadLineOption -HistorySavePath '{history}' -ErrorAction Stop\nif ((Get-PSReadLineOption).HistorySavePath -ne '{history}') {{ throw 'FixtureHistoryIsolationFailed' }}\n{}",
            decode_script(encoded));
        let encoded = base64(&script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
        let pair = native_pty_system().openpty(PtySize {
            rows: 30, cols: 100, pixel_width: 0, pixel_height: 0,
        }).unwrap();
        let mut command = CommandBuilder::new(shell);
        command.env("ARTERM_SHELL_HISTORY_PATH", home.join("history.txt"));
        command.args(["-NoLogo", "-NoProfile", "-NoExit", "-EncodedCommand", &encoded]);
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let responder = writer.clone();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let (tx, output) = mpsc::channel();
        thread::spawn(move || {
            let mut parser = vt100::Parser::new(30, 100, 0);
            let mut tail = Vec::new();
            let mut bytes = [0; 4096];
            while let Ok(n) = reader.read(&mut bytes) {
                if n == 0 { break; }
                parser.process(&bytes[..n]);
                let previous = tail.len();
                tail.extend_from_slice(&bytes[..n]);
                for query in [b"\x1b[6n".as_slice(), b"\x1b[c", b"\x1b[>c"] {
                    for _ in tail.windows(query.len()).enumerate()
                        .filter(|(i, v)| *i + query.len() > previous && *v == query) {
                        let (row, col) = parser.screen().cursor_position();
                        let reply = match query {
                            b"\x1b[6n" => format!("\x1b[{};{}R", row + 1, col + 1),
                            b"\x1b[c" => "\x1b[?1;2c".into(),
                            _ => "\x1b[>0;10;1c".into(),
                        };
                        if responder.lock().unwrap().write_all(reply.as_bytes()).is_err() { return; }
                    }
                }
                if tail.len() > 8 { tail.drain(..tail.len() - 8); }
                if tx.send(bytes[..n].to_vec()).is_err() { break; }
            }
        });
        Self { child, _master: pair.master, writer, output, seen: Vec::new(), home }
    }
    fn input(&self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }
    fn until(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !String::from_utf8_lossy(&self.seen).contains(needle) {
            assert!(Instant::now() < deadline, "missing fixture category {needle}");
            match self.output.recv_timeout(Duration::from_millis(100)) {
                Ok(bytes) => self.seen.extend(bytes),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => panic!("fixture shell exited"),
            }
        }
    }
    fn hook(&self) {
        self.input(b"\x1b[135;0;0;1;0;1_\x1b[135;0;0;0;0;1_");
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::fs::remove_dir_all(&self.home).expect("remove only owned fixture history");
    }
}

#[test]
fn managed_completion_waits_for_execution_and_output_after_readline_prompt() {
    for shell in ["powershell.exe", "pwsh.exe"] {
        let mailbox = Mailbox::new().unwrap();
        let commands = Arc::new(Mutex::new(Commands::new()));
        let script = commands.lock().unwrap().bootstrap_mailbox(&mailbox.name, mailbox.host_started);
        // A custom read function may redraw the prompt after acceptance, but
        // before returning the source to the shell for normal execution.
        let script = decode_script(&script).replace("$global:__arTermOriginalPrompt =", r#"
$global:FixtureReadLine=(Get-Item Function:\PSConsoleHostReadLine).ScriptBlock
function global:PSConsoleHostReadLine {
    $source = & $global:FixtureReadLine
    [Console]::WriteLine('FIXTURE_READ_ACCEPTED')
    $null = prompt
    [Console]::WriteLine('FIXTURE_READ_CALLBACK_DONE')
    while (-not [IO.File]::Exists($env:ARTERM_SHELL_HISTORY_PATH+'.release')) {
        Start-Sleep -Milliseconds 10
    }
    $source
}
$global:__arTermOriginalPrompt ="#);
        let encoded = base64(&script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
        let mut terminal = Terminal::new(shell, &encoded);
        let target = commands.clone();
        mailbox.start(terminal.child.process_id().unwrap(), move |request|
            target.lock().unwrap().mailbox_exchange(request)).unwrap();
        terminal.until(";ready");
        commands.lock().unwrap().output(&terminal.seen);
        terminal.seen.clear();
        let gate = terminal.home.join("executing");
        let release = terminal.home.join("release");
        let path = |p: &std::path::Path| p.to_str().unwrap().replace('\'', "''");
        let source = format!(
            "[IO.File]::WriteAllText('{}','entered'); while (-not [IO.File]::Exists('{}')) {{ Start-Sleep -Milliseconds 10 }}; Write-Output ('FIXTURE_'+'EXECUTED')",
            path(&gate), path(&release));
        let id = uuid::Uuid::now_v7();
        let (_, wake) = commands.lock().unwrap().submit(id, &source).unwrap();
        terminal.input(&wake.unwrap());
        terminal.until("FIXTURE_READ_CALLBACK_DONE");
        commands.lock().unwrap().output(&terminal.seen);
        assert!(!gate.exists(), "{shell}: source executed before read function returned");
        assert_ne!(commands.lock().unwrap().record(id).unwrap().state, "completed",
            "{shell}: completed before read function returned");
        assert!(!String::from_utf8_lossy(&terminal.seen).contains(";done;"),
            "{shell}: read callback emitted done before execution");
        std::fs::write(terminal.home.join("history.txt.release"), "release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !gate.exists() {
            assert!(Instant::now() < deadline, "fixture execution gate deadline");
            thread::sleep(Duration::from_millis(10));
        }
        while let Ok(bytes) = terminal.output.try_recv() { terminal.seen.extend(bytes); }
        commands.lock().unwrap().output(&terminal.seen);
        let state = commands.lock().unwrap().record(id).unwrap().state;
        assert_ne!(state, "completed", "{shell}: completed during read callback before gate release");
        assert!(!String::from_utf8_lossy(&terminal.seen).contains(";done;"),
            "{shell}: done marker during read callback before gate release");
        std::fs::write(&release, "release").unwrap();
        terminal.until(";done;");
        terminal.until(";ready");
        commands.lock().unwrap().output(&terminal.seen);
        assert_eq!(commands.lock().unwrap().record(id).unwrap().state, "completed");
        let bytes = String::from_utf8_lossy(&terminal.seen);
        assert!(bytes.find("FIXTURE_EXECUTED").is_some_and(|output|
            output < bytes.find(";done;").unwrap()), "{shell}: done preceded executed output");
        println!("{shell}: read callback, read release, execution gate, output, done, ready");
    }
}

#[test]
fn production_mailbox_partial_race_is_definitively_not_submitted() {
    for shell in ["powershell.exe", "pwsh.exe"] {
        let mailbox = Mailbox::new().unwrap();
        let commands = Arc::new(Mutex::new(Commands::new()));
        let script = commands.lock().unwrap().bootstrap_mailbox(&mailbox.name, mailbox.host_started);
        let mut terminal = Terminal::new(shell, &script);
        let target = commands.clone();
        mailbox.start(terminal.child.process_id().unwrap(), move |request|
            target.lock().unwrap().mailbox_exchange(request)).unwrap();
        terminal.until(";ready");
        commands.lock().unwrap().output(&terminal.seen);
        terminal.seen.clear();
        let id = uuid::Uuid::now_v7();
        let (_, wake) = commands.lock().unwrap().submit(id, "$global:RaceRan=$true").unwrap();
        terminal.input(b"Write-Output ('RACE_'+");
        terminal.input(&wake.unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if commands.lock().unwrap().record(id).unwrap().state == "not_submitted" { break; }
            assert!(Instant::now() < deadline, "partial-race rejection deadline");
            thread::sleep(Duration::from_millis(20));
        }
        assert!(commands.lock().unwrap().submit(id, "$global:RaceRan=$true").unwrap().1.is_none());
        terminal.input(b"'PRESERVED')\r");
        terminal.until("RACE_PRESERVED");
        terminal.hook();
        terminal.input(b"if ($null -eq $global:RaceRan) { Write-Output ('RACE_'+'NOTRUN') }\r");
        terminal.until("RACE_NOTRUN");
        let visible = String::from_utf8_lossy(&terminal.seen);
        assert!(!visible.contains("FromBase64String") && !visible.contains("ScriptBlock"));
    }
}

#[test]
fn production_broken_mailbox_expires_without_late_execution() {
    for shell in ["powershell.exe", "pwsh.exe"] {
        let mailbox = Mailbox::new().unwrap();
        let mut commands = Commands::new();
        let script = commands.bootstrap_mailbox(&mailbox.name, mailbox.host_started);
        drop(mailbox);
        let mut terminal = Terminal::new(shell, &script);
        terminal.until(";ready");
        commands.output(&terminal.seen);
        terminal.seen.clear();
        let id = uuid::Uuid::now_v7();
        let (_, wake) = commands.submit(id, "$global:BrokenMailboxRan=$true").unwrap();
        terminal.input(&wake.unwrap());
        terminal.until("ShellMailboxUnavailable");
        let deadline = Instant::now() + Duration::from_secs(6);
        while commands.record(id).unwrap().state != "not_submitted" {
            commands.expire_pending();
            assert!(Instant::now() < deadline, "broken mailbox expiration deadline");
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(commands.mailbox_exchange("offer").unwrap(), "-");
        terminal.input(b"if ($null -eq $global:BrokenMailboxRan) { Write-Output ('BROKEN_'+'NOTRUN') }\r");
        terminal.until("BROKEN_NOTRUN");
    }
}

#[test]
fn production_managed_history_preserves_current_manual_filter() {
    for shell in ["powershell.exe", "pwsh.exe"] {
        let mailbox = Mailbox::new().unwrap();
        let commands = Arc::new(Mutex::new(Commands::new()));
        let script = commands.lock().unwrap().bootstrap_mailbox(&mailbox.name, mailbox.host_started);
        let mut terminal = Terminal::new(shell, &script);
        let target = commands.clone();
        mailbox.start(terminal.child.process_id().unwrap(), move |request|
            target.lock().unwrap().mailbox_exchange(request)).unwrap();
        terminal.until(";ready");
        terminal.seen.clear();
        terminal.input(b"Set-PSReadLineOption -AddToHistoryHandler { param($line) return (-not $line.Contains('POLICY_BLOCK_TOKEN')) }; $global:PolicySaved=(Get-PSReadLineOption).AddToHistoryHandler; Write-Output ('POLICY_'+'INSTALLED')\r");
        terminal.until("POLICY_INSTALLED");
        terminal.until(";ready");
        commands.lock().unwrap().output(&terminal.seen);
        terminal.seen.clear();
        let managed = format!("$global:PolicyManaged_{}=1", uuid::Uuid::now_v7().simple());
        let (_, wake) = commands.lock().unwrap().submit(uuid::Uuid::now_v7(), &managed).unwrap();
        terminal.input(&wake.unwrap());
        terminal.until(";done;");
        terminal.until(";ready");
        terminal.seen.clear();
        terminal.input(b"$global:POLICY_BLOCK_TOKEN=1\r");
        terminal.input(b"$global:POLICY_MANUAL_ALLOWED=1\r");
        let check = format!(
            "$h=[Microsoft.PowerShell.PSConsoleReadLine]::GetHistoryItems().CommandLine; if ([Object]::ReferenceEquals($global:PolicySaved,(Get-PSReadLineOption).AddToHistoryHandler) -and $h -notcontains '{managed}' -and $h -notcontains '$global:POLICY_BLOCK_TOKEN=1' -and $h -contains '$global:POLICY_MANUAL_ALLOWED=1') {{ Write-Output ('POLICY_'+'PRESERVED') }}\r");
        terminal.input(check.as_bytes());
        terminal.until("POLICY_PRESERVED");
        let history = std::fs::read_to_string(terminal.home.join("history.txt")).unwrap();
        assert!(!history.lines().any(|line| line == managed));
        assert!(!history.lines().any(|line| line == "$global:POLICY_BLOCK_TOKEN=1"));
        assert!(history.lines().any(|line| line == "$global:POLICY_MANUAL_ALLOWED=1"));
    }
}

#[test]
fn lost_commit_reply_is_unknown_and_manual_input_recovers_without_reexecution() {
    for shell in ["powershell.exe", "pwsh.exe"] {
        let mailbox = Mailbox::new().unwrap();
        let commands = Arc::new(Mutex::new(Commands::new()));
        let script = commands.lock().unwrap().bootstrap_mailbox(&mailbox.name, mailbox.host_started);
        let mut terminal = Terminal::new(shell, &script);
        let target = commands.clone();
        mailbox.start(terminal.child.process_id().unwrap(), move |request| {
            let response = target.lock().unwrap().mailbox_exchange(request)?;
            if request.starts_with("commit;") { anyhow::bail!("InjectedCommitReplyLoss"); }
            Ok(response)
        }).unwrap();
        terminal.until(";ready");
        commands.lock().unwrap().output(&terminal.seen);
        terminal.seen.clear();
        let id = uuid::Uuid::now_v7();
        let source = "$global:NoRunCanary=$true";
        let (_, wake) = commands.lock().unwrap().submit(id, source).unwrap();
        terminal.input(&wake.unwrap());
        terminal.until("ShellMailboxUnavailable");
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            let mut state = commands.lock().unwrap();
            state.expire_pending();
            if state.record(id).unwrap().state == "unknown" { break; }
            assert!(Instant::now() < deadline, "commit confirmation watchdog deadline");
            drop(state);
            thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(commands.lock().unwrap().shell_status(), "busy");
        assert!(commands.lock().unwrap().submit(id, source).unwrap().1.is_none());
        terminal.input(b"if ($null -eq $global:NoRunCanary) { Write-Output ('LOSS_'+'MANUAL_OK') }\r");
        terminal.until("LOSS_MANUAL_OK");
        terminal.until(";ready");
        commands.lock().unwrap().output(&terminal.seen);
        assert_eq!(commands.lock().unwrap().shell_status(), "ready");
        assert_eq!(commands.lock().unwrap().record(id).unwrap().state, "unknown");
        terminal.seen.clear();
        terminal.hook();
        terminal.input(b"if ($null -eq $global:NoRunCanary) { Write-Output ('LOSS_'+'NO_REPLAY') }\r");
        terminal.until("LOSS_NO_REPLAY");
        println!("{shell}: authenticated commit/reply loss bounded to unknown; manual recovery and no replay");
    }
}
