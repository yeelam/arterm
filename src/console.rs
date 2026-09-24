#[path = "console_input.rs"]
pub(crate) mod console_input;
#[path = "console_queries.rs"]
mod console_queries;

use anyhow::{ensure, Result};
use console_input::{ConsoleInput, MAX_BUFFERED_INPUT};
use std::{
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{Foundation::HANDLE, Storage::FileSystem::{ReadFile, WriteFile}, System::Console::*};

const INPUT_MODES: &[u16] = &[1, 66, 1000, 1002, 1003, 1004, 1006, 2004, 9001];
const READER_WAKE: u16 = 0xffff;

fn write_console(handle: HANDLE, bytes: &[u8]) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let mut written = 0;
        ensure!(unsafe { WriteFile(handle, bytes[offset..].as_ptr(),
            (bytes.len() - offset) as u32, &mut written, std::ptr::null_mut()) } != 0
            && written > 0, "console write failed after {offset} bytes");
        offset += written as usize;
    }
    Ok(())
}

// Query the local console, not the remote terminal. Preserve unrelated input records.
fn collect_replies(hin: HANDLE, replies: &mut console_queries::Replies, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut records = Vec::new();
    let result = (|| -> Result<()> {
        while Instant::now() < deadline {
            let mut count = 0;
            ensure!(unsafe { GetNumberOfConsoleInputEvents(hin, &mut count) } != 0,
                "cannot inspect console mode replies");
            if count == 0 {
                if replies.outstanding.is_empty() { break; }
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
            ensure!(unsafe { ReadConsoleInputW(hin, &mut record, 1, &mut count) } != 0,
                "cannot read console mode replies");
            if record.EventType == KEY_EVENT as u16 {
                let key = unsafe { record.Event.KeyEvent };
                if replies.wake_pending && key.wVirtualKeyCode == 0
                    && unsafe { key.uChar.UnicodeChar } == READER_WAKE && key.bKeyDown == 1 {
                    replies.wake_pending = false;
                    continue;
                }
            }
            records.extend(replies.record(record));
        }
        Ok(())
    })();
    replies.preserved.extend(records);
    result
}

fn preserve_records(hin: HANDLE, records: &[INPUT_RECORD]) -> Result<()> {
    if !records.is_empty() {
        let mut written = 0;
        ensure!(unsafe { WriteConsoleInputW(hin, records.as_ptr(), records.len() as u32, &mut written) } != 0
            && written as usize == records.len(), "cannot preserve console input");
    }
    Ok(())
}

#[derive(Default)]
struct ModeFilter {
    pending: Vec<u8>,
}
impl ModeFilter {
    fn output(&mut self, bytes: &[u8], known: &[(u16, bool)]) -> Result<Vec<u8>> {
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            if byte == 27 && !self.pending.is_empty() {
                output.append(&mut self.pending);
                self.pending.push(byte);
                continue;
            }
            if self.pending.is_empty() && byte != 27 {
                output.push(byte);
                continue;
            }
            self.pending.push(byte);
            let prefix = b"\x1b[?";
            if self.pending.len() <= prefix.len() && prefix.starts_with(&self.pending) {
                continue;
            }
            if self.pending.starts_with(prefix) {
                if byte.is_ascii_digit() || byte == b';' {
                    ensure!(self.pending.len() <= 128, "oversized terminal input-mode sequence");
                    continue;
                }
                if byte == b'h' || byte == b'l' {
                    let body = std::str::from_utf8(&self.pending[3..self.pending.len() - 1])?;
                    let modes: Vec<&str> = body.split(';').filter(|value| {
                        value.parse::<u16>().map_or(true, |mode|
                            !INPUT_MODES.contains(&mode) || known.iter().any(|(saved, _)| *saved == mode))
                    }).collect();
                    if !modes.is_empty() {
                        output.extend_from_slice(prefix);
                        output.extend_from_slice(modes.join(";").as_bytes());
                        output.push(byte);
                    }
                    self.pending.clear();
                    continue;
                }
            }
            output.append(&mut self.pending);
        }
        Ok(output)
    }
}

pub enum Input {
    Bytes(Vec<u8>),
    Detach,
    Idle,
    Eof,
}
pub trait Terminal {
    fn output(&mut self, bytes: &[u8]) -> Result<()>;
    fn input(&mut self, accept_bytes: bool) -> Result<Input>;
    fn size(&self) -> (u16, u16);
    fn reading(&self, enabled: bool);
    fn connection_state(&mut self, _state: &str) {}
    fn detach_requested(&self) -> bool { false }
    fn output_gap(&mut self) {}
    fn control(&mut self, _allow_submit: bool) -> Option<crate::local_control::ControlMessage> { None }
    fn command_capability(&mut self, _supported: bool) {}
    fn command_context(&mut self, _enabled: bool, _host_version: Option<&str>) {}
    fn command_output_progress(&mut self, _seq: u64) {}
    fn command_event(&mut self, _kind: &str, _body: &rmpv::Value) {}
}

struct Guard {
    hin: HANDLE,
    hout: HANDLE,
    input: u32,
    output: u32,
    cp_in: u32,
    cp_out: u32,
    modes: Vec<(u16, bool)>,
    filter: ModeFilter,
    replies: Arc<Mutex<console_queries::Replies>>,
}
impl Guard {
    fn cleanup_replies(&self) -> std::sync::MutexGuard<'_, console_queries::Replies> {
        self.replies.lock().unwrap_or_else(|poisoned| {
            crate::statusln!("[console] Input-reader state was poisoned; attempting terminal cleanup.");
            poisoned.into_inner()
        })
    }

    fn restore_modes(&mut self) {
        let restore: String = self.modes.iter()
            .map(|(mode, enabled)| format!("\x1b[?{mode}{}", if *enabled { 'h' } else { 'l' })).collect();
        match write_console(self.hout, restore.as_bytes()) {
            Ok(()) => self.modes.clear(),
            Err(error) => crate::statusln!("[console] Cannot restore input modes: {error:#}"),
        }
    }

    fn new() -> Result<Self> {
        unsafe {
            let hin = GetStdHandle(STD_INPUT_HANDLE);
            let hout = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut input = 0;
            let mut output = 0;
            ensure!(
                GetConsoleMode(hin, &mut input) != 0 && GetConsoleMode(hout, &mut output) != 0,
                "requires a real terminal; --stdio is reserved for scripted clients"
            );
            let mut guard = Self {
                hin,
                hout,
                input,
                output,
                cp_in: GetConsoleCP(),
                cp_out: GetConsoleOutputCP(),
                modes: Vec::new(),
                filter: ModeFilter::default(),
                replies: Arc::new(Mutex::new(console_queries::Replies::new(INPUT_MODES))),
            };
            ensure!(
                SetConsoleMode(
                    hin,
                    (input & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT))
                        | ENABLE_VIRTUAL_TERMINAL_INPUT
                ) != 0,
                "cannot enable raw input"
            );
            ensure!(
                SetConsoleMode(
                    hout,
                    output
                        | ENABLE_PROCESSED_OUTPUT
                        | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                        | DISABLE_NEWLINE_AUTO_RETURN
                ) != 0,
                "cannot enable VT output"
            );
            ensure!(
                SetConsoleCP(65001) != 0 && SetConsoleOutputCP(65001) != 0,
                "cannot enable UTF-8 console"
            );
            let queries: String = INPUT_MODES.iter().map(|mode| format!("\x1b[?{mode}$p")).collect();
            write_console(hout, queries.as_bytes())?;
            {
                let mut replies = guard.replies.lock().unwrap();
                collect_replies(hin, &mut replies, Duration::from_millis(100))?;
                guard.modes = replies.states.iter().map(|(&mode, &state)| (mode, state)).collect();
                if !replies.outstanding.is_empty() {
                    crate::statusln!("[console] Incomplete mode snapshot; remote changes to unreported input modes will be suppressed.");
                }
            }
            Ok(guard)
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.restore_modes();
        {
            let mut replies = self.cleanup_replies();
            if let Err(error) = collect_replies(self.hin, &mut replies, Duration::from_millis(200)) {
                crate::statusln!("[console] Cannot drain local mode replies: {error:#}");
            }
            let records = replies.finish();
            if let Err(error) = preserve_records(self.hin, &records) {
                crate::statusln!("[console] Cannot preserve unrelated console input: {error:#}");
            }
            if !replies.outstanding.is_empty() {
                crate::statusln!("[console] Mode queries remain unanswered at bounded cleanup.");
            }
        }
        unsafe {
            for (ok, operation) in [
                (SetConsoleMode(self.hin, self.input), "input flags"),
                (SetConsoleMode(self.hout, self.output), "output flags"),
                (SetConsoleCP(self.cp_in), "input code page"),
                (SetConsoleOutputCP(self.cp_out), "output code page"),
            ] {
                if ok == 0 { crate::statusln!("[console] Cannot restore {operation}: {}", io::Error::last_os_error()); }
            }
        }
    }
}

pub struct Console {
    guard: Option<Guard>,
    input: Receiver<io::Result<Vec<u8>>>,
    enabled: Arc<AtomicBool>,
    decoder: ConsoleInput,
    eof: bool,
    headless: bool,
    stopped: Arc<AtomicBool>,
    reader: Option<thread::JoinHandle<()>>,
    read_started: AtomicBool,
}
impl Console {
    pub fn new(stdio: bool) -> Result<Self> {
        let mut mode = 0;
        let interactive = unsafe {
            GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) != 0
                && GetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), &mut mode) != 0
        };
        let headless = !stdio && !interactive;
        if headless { crate::diagnostics::nonblocking(); }
        let guard = if stdio || headless { None } else { Some(Guard::new()?) };
        let (tx, input) = mpsc::sync_channel(1);
        let enabled = Arc::new(AtomicBool::new(false));
        let gate = enabled.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let interactive_handle = guard.as_ref().map(|g| g.hin as usize);
        let replies = guard.as_ref().map(|g| g.replies.clone());
        let reader = if !headless { Some(thread::spawn(move || {
            loop {
                while !gate.load(Ordering::Acquire) {
                    if stop.load(Ordering::Acquire) { return; }
                    thread::sleep(Duration::from_millis(30));
                }
                if stop.load(Ordering::Acquire) { return; }
                let mut bytes = vec![0; 4096];
                let preserved = replies.as_ref().map(|r| r.lock().unwrap().take_preserved_bytes()).unwrap_or_default();
                let from_preserved = !preserved.is_empty();
                let read = if from_preserved {
                    bytes = preserved;
                    Ok(bytes.len())
                } else if let Some(handle) = interactive_handle {
                    let mut n = 0;
                    if unsafe { ReadFile(handle as HANDLE, bytes.as_mut_ptr(), bytes.len() as u32, &mut n, std::ptr::null_mut()) } == 0 {
                        Err(io::Error::last_os_error())
                    } else { Ok(n as usize) }
                } else { io::stdin().read(&mut bytes) };
                if stop.load(Ordering::Acquire) { return; }
                match read {
                    Ok(n) => {
                        bytes.truncate(n);
                        if let Some(replies) = &replies {
                            if !from_preserved {
                            bytes = replies.lock().unwrap().bytes(&bytes);
                            if n > 0 && bytes.is_empty() { continue; }
                            }
                        }
                        let mut message = Ok(bytes);
                        loop {
                            match tx.try_send(message) {
                                Ok(()) => break,
                                Err(mpsc::TrySendError::Full(value)) => {
                                    if stop.load(Ordering::Acquire) { return; }
                                    message = value;
                                    thread::sleep(Duration::from_millis(1));
                                }
                                Err(mpsc::TrySendError::Disconnected(_)) => return,
                            }
                        }
                        if n == 0 { break; }
                    }
                    Err(error) => {
                        let _ = tx.try_send(Err(error));
                        break;
                    }
                }
            }
        })) } else { None };
        Ok(Self {
            guard,
            input,
            enabled,
            decoder: ConsoleInput::default(),
            eof: false,
            headless,
            stopped,
            reader,
            read_started: AtomicBool::new(false),
        })
    }
}
impl Terminal for Console {
    fn output(&mut self, bytes: &[u8]) -> Result<()> {
        if self.headless {
            return Ok(());
        }
        if let Some(guard) = &mut self.guard {
            let output = guard.filter.output(bytes, &guard.modes)?;
            write_console(guard.hout, &output)?;
        } else {
            let mut stdout = io::stdout().lock();
            stdout.write_all(bytes)?;
            stdout.flush()?;
        }
        Ok(())
    }
    fn input(&mut self, accept_bytes: bool) -> Result<Input> {
        if self.headless {
            return Ok(Input::Idle);
        }
        let now = Instant::now();
        if let Some(guard) = &self.guard {
            let bytes = guard.replies.lock().unwrap().expire_ambiguous(now);
            if self.decoder.push(&bytes, now) { return Ok(Input::Detach); }
        }
        self.decoder.expire(now);
        if !self.eof && self.decoder.buffered_len() < MAX_BUFFERED_INPUT {
            match self.input.try_recv() {
                Ok(Ok(bytes)) if bytes.is_empty() => {
                    self.eof = true;
                    self.decoder.finish();
                }
                Ok(Ok(bytes)) => {
                    if self.decoder.push(&bytes, now) {
                        return Ok(Input::Detach);
                    }
                }
                Ok(Err(error)) => return Err(error.into()),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.eof = true;
                    self.decoder.finish();
                }
            }
        }
        if accept_bytes {
            if let Some(bytes) = self.decoder.take(4096) {
                return Ok(Input::Bytes(bytes));
            }
            if self.eof && self.decoder.is_empty() {
                return Ok(Input::Eof);
            }
        }
        Ok(Input::Idle)
    }
    fn reading(&self, enabled: bool) {
        if enabled { self.read_started.store(true, Ordering::Release); }
        self.enabled.store(enabled, Ordering::Release);
    }
    fn size(&self) -> (u16, u16) {
        if let Some(guard) = &self.guard {
            unsafe {
                let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
                if GetConsoleScreenBufferInfo(guard.hout, &mut info) != 0 {
                    return (
                        (info.srWindow.Right - info.srWindow.Left + 1).max(1) as u16,
                        (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as u16,
                    );
                }
            }
        }
        (80, 24)
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        self.enabled.store(false, Ordering::Release);
        self.stopped.store(true, Ordering::Release);
        if let Some(guard) = &mut self.guard { guard.restore_modes(); }
        if self.guard.is_some() {
            use std::os::windows::io::AsRawHandle;
            if let Some(reader) = self.reader.take() {
                // Complete the raw read before returning the console to the shell.
                // Cancelling a console ReadFile alone can leave its server-side read
                // consuming the first input delivered after cancellation.
                if self.read_started.load(Ordering::Acquire) {
                    let mut wake: INPUT_RECORD = unsafe { std::mem::zeroed() };
                    wake.EventType = KEY_EVENT as u16;
                    unsafe {
                        wake.Event.KeyEvent.bKeyDown = 1;
                        wake.Event.KeyEvent.wRepeatCount = 1;
                        // Console reads can erase scan-code tags, but retain this noncharacter.
                        wake.Event.KeyEvent.uChar.UnicodeChar = READER_WAKE;
                        let mut written = 0;
                        if WriteConsoleInputW(self.guard.as_ref().unwrap().hin, &wake, 1, &mut written) == 0 {
                            crate::statusln!("[console] Cannot wake input reader: {}", io::Error::last_os_error());
                        } else {
                            self.guard.as_ref().unwrap().cleanup_replies().wake_pending = true;
                        }
                    }
                }
                let deadline = Instant::now() + Duration::from_millis(100);
                while !reader.is_finished() {
                    if Instant::now() >= deadline {
                        unsafe { windows_sys::Win32::System::IO::CancelSynchronousIo(reader.as_raw_handle() as HANDLE); }
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                if reader.join().is_err() { crate::statusln!("[console] Input reader panicked"); }
            }
        }
    }
}

#[cfg(test)]
mod exit_tests {
    use super::*;

    fn input_modes(hin: HANDLE, hout: HANDLE, modes: &[u16], require_all: bool) -> Result<std::collections::BTreeMap<u16, bool>> {
        let mut replies = console_queries::Replies::new(modes);
        let queries: String = modes.iter().map(|mode| format!("\x1b[?{mode}$p")).collect();
        write_console(hout, queries.as_bytes())?;
        collect_replies(hin, &mut replies, Duration::from_secs(2))?;
        if require_all {
            ensure!(replies.outstanding.is_empty(), "supported mode observation did not finish");
            ensure!(replies.states.len() == modes.len(), "previously supported modes were not reported");
        }
        Ok(replies.states)
    }

    fn inject(hin: usize, bytes: &[u8]) {
        let records: Vec<INPUT_RECORD> = bytes.iter().map(|&byte| {
            let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
            record.EventType = KEY_EVENT as u16;
            record.Event.KeyEvent.bKeyDown = 1;
            record.Event.KeyEvent.wRepeatCount = 1;
            record.Event.KeyEvent.uChar.UnicodeChar = byte as u16;
            record
        }).collect();
        preserve_records(hin as HANDLE, &records).unwrap();
    }

    fn delayed_replies(hin: HANDLE) {
        for reading in [false, true] {
            let mut console = Console::new(false).unwrap();
            let replies = console.guard.as_ref().unwrap().replies.clone();
            {
                let mut state = replies.lock().unwrap();
                state.outstanding.insert(42499); // Model a native mode query with no reply.
                collect_replies(hin, &mut state, Duration::from_secs(2)).unwrap();
                // Native ConPTY versions need not answer every mode query. Use
                // synthetic-only IDs so their replies cannot collide with this case.
                *state = console_queries::Replies::new(&[42420, 42421]);
            }
            inject(hin as usize, b"q\x1b[?42420;");
            collect_replies(hin, &mut replies.lock().unwrap(), Duration::from_millis(100)).unwrap();
            assert_eq!(replies.lock().unwrap().outstanding.len(), 2);
            // Deliver the continuation only after discovery has timed out.
            // Scheduling a sleeping writer is not a guarantee it runs within
            // the production cleanup's deliberately bounded 200ms window.
            inject(hin as usize, b"1$y\x1b[?42421;2$yz");
            if reading {
                console.reading(true);
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"z") {
                    if let Input::Bytes(input) = console.input(true).unwrap() { bytes.extend(input); }
                    assert!(Instant::now() < deadline, "late reply stalled active input: {bytes:?}");
                    thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(bytes, b"\x1b[0;0;113;1;0;1_z");
            }
            drop(console);
            assert!(replies.lock().unwrap().outstanding.is_empty());
            if !reading {
                let mut records: [INPUT_RECORD; 2] = unsafe { std::mem::zeroed() };
                let mut count = 0;
                assert_ne!(unsafe { ReadConsoleInputW(hin, records.as_mut_ptr(), 2, &mut count) }, 0);
                assert_eq!(count, 2);
                let bytes: Vec<u8> = records.iter().map(|r| unsafe { r.Event.KeyEvent.uChar.UnicodeChar as u8 }).collect();
                assert_eq!(bytes, b"qz", "startup failure must preserve unrelated typeahead");
            }
            let mut count = 0;
            assert_ne!(unsafe { GetNumberOfConsoleInputEvents(hin, &mut count) }, 0);
            assert_eq!(count, 0, "local replies leaked into parent input");
        }
    }

    #[test]
    fn restoration_child() {
        if std::env::var_os("ARTERM_VT_EXIT_PROBE").is_none() { return; }
        unsafe {
            let hin = GetStdHandle(STD_INPUT_HANDLE);
            let hout = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut original = 0;
            assert_ne!(GetConsoleMode(hout, &mut original), 0);
            assert_ne!(SetConsoleMode(hout, original | ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
            // Emulate an output endpoint that ignores DECRQM.
            assert_ne!(SetConsoleMode(hout, original & !ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
            let started = Instant::now();
            let mut unsupported = console_queries::Replies::new(INPUT_MODES);
            collect_replies(hin, &mut unsupported, Duration::from_millis(100)).unwrap();
            assert!(unsupported.states.is_empty());
            assert!(started.elapsed() < Duration::from_millis(500));
            assert_ne!(SetConsoleMode(hout, original | ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
            delayed_replies(hin);
            let supported: Vec<u16> = input_modes(hin, hout, INPUT_MODES, false).unwrap().keys().copied().collect();
            let mut before_input = 0;
            let mut before_output = 0;
            assert_ne!(GetConsoleMode(hin, &mut before_input), 0);
            assert_ne!(GetConsoleMode(hout, &mut before_output), 0);
            let before_cp = (GetConsoleCP(), GetConsoleOutputCP());
            for inherited in [false, true] {
                write_console(hout, if inherited { b"\x1b[?1004h\x1b[?2004h\x1b[?9001h" }
                    else { b"\x1b[?1004l\x1b[?2004l\x1b[?9001l" }).unwrap();
                let before = input_modes(hin, hout, &supported, true).unwrap();
                let mut unknown = Console::new(false).unwrap();
                unknown.guard.as_mut().unwrap().modes.clear();
                unknown.output(if inherited { b"\x1b[?9001l\x1b[?1004l\x1b[?2004l" }
                    else { b"\x1b[?9001h\x1b[?1004h\x1b[?2004h" }).unwrap();
                drop(unknown);
                assert_eq!(input_modes(hin, hout, &supported, true).unwrap(), before);
                for reading in [false, true] {
                    let mut console = Console::new(false).unwrap();
                    console.reading(reading);
                    console.output(if inherited { b"\x1b[?1004l\x1b[?2004l\x1b[?9001l\x1b[?1003h" }
                        else { b"\x1b[?1004h\x1b[?2004h\x1b[?9001h\x1b[?1003h" }).unwrap();
                    thread::sleep(Duration::from_millis(40));
                    drop(console);
                    assert_eq!(input_modes(hin, hout, &supported, true).unwrap(), before);
                    let mut after_input = 0;
                    let mut after_output = 0;
                    assert_ne!(GetConsoleMode(hin, &mut after_input), 0);
                    assert_ne!(GetConsoleMode(hout, &mut after_output), 0);
                    assert_eq!((after_input, after_output), (before_input, before_output));
                    assert_eq!((GetConsoleCP(), GetConsoleOutputCP()), before_cp);
                }
            }
            let poisoned_console = Console::new(false).unwrap();
            let replies = poisoned_console.guard.as_ref().unwrap().replies.clone();
            assert!(std::panic::catch_unwind(|| {
                let _state = replies.lock().unwrap();
                panic!("intentional test-only input-state poisoning");
            }).is_err());
            drop(poisoned_console);
            let mut restored_input = 0;
            let mut restored_output = 0;
            assert_ne!(GetConsoleMode(hin, &mut restored_input), 0);
            assert_ne!(GetConsoleMode(hout, &mut restored_output), 0);
            assert_eq!((restored_input, restored_output), (before_input, before_output));
            write_console(hout, b"\x1b[?1004l\x1b[?2004l\x1b[?9001l").unwrap();
            SetConsoleMode(hout, original);
        }
    }

    #[test]
    fn unsupported_queries_preserve_unknown_modes_without_blocking_output() {
        let mut filter = ModeFilter::default();
        assert_eq!(filter.output(b"hello\x1b[?90", &[]).unwrap(), b"hello");
        assert_eq!(filter.output(b"01;1004;25h world", &[]).unwrap(), b"\x1b[?25h world");
        assert_eq!(filter.output(b"\x1b[?9001l", &[(9001, true)]).unwrap(), b"\x1b[?9001l");
        assert_eq!(filter.output(b"\x1b[31mred\x1b[0m", &[]).unwrap(), b"\x1b[31mred\x1b[0m");
        assert_eq!(filter.output(b"\x1b\x1b[?9001h", &[]).unwrap(), b"\x1b");
    }

    #[test]
    fn restores_inherited_modes_and_releases_reader_in_owned_conpty() {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};
        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args(["--exact", "console::exit_tests::restoration_child", "--nocapture"]);
        command.env("ARTERM_VT_EXIT_PROBE", "1");
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let output = thread::spawn(move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() { break status; }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("console restoration timed out");
            }
            thread::sleep(Duration::from_millis(10));
        };
        drop(pair.master);
        let bytes = output.join().unwrap();
        assert!(status.success(), "child output: {:?}", String::from_utf8_lossy(&bytes));
    }
}
