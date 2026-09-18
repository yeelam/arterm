#[path = "console_input.rs"]
mod console_input;

use anyhow::{ensure, Result};
use console_input::{ConsoleInput, MAX_BUFFERED_INPUT};
use std::{
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{Foundation::HANDLE, Storage::FileSystem::WriteFile, System::Console::*};

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
}

struct Guard {
    hin: HANDLE,
    hout: HANDLE,
    input: u32,
    output: u32,
    cp_in: u32,
    cp_out: u32,
}
impl Guard {
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
            let guard = Self {
                hin,
                hout,
                input,
                output,
                cp_in: GetConsoleCP(),
                cp_out: GetConsoleOutputCP(),
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
            Ok(guard)
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            SetConsoleMode(self.hin, self.input);
            SetConsoleMode(self.hout, self.output);
            SetConsoleCP(self.cp_in);
            SetConsoleOutputCP(self.cp_out);
        }
    }
}

pub struct Console {
    guard: Option<Guard>,
    input: Receiver<io::Result<Vec<u8>>>,
    enabled: Arc<AtomicBool>,
    decoder: ConsoleInput,
    eof: bool,
}
impl Console {
    pub fn new(stdio: bool) -> Result<Self> {
        let guard = if stdio { None } else { Some(Guard::new()?) };
        let (tx, input) = mpsc::sync_channel(1);
        let enabled = Arc::new(AtomicBool::new(false));
        let gate = enabled.clone();
        thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            loop {
                while !gate.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(30));
                }
                let mut bytes = vec![0; 4096];
                match stdin.read(&mut bytes) {
                    Ok(n) => {
                        bytes.truncate(n);
                        if tx.send(Ok(bytes)).is_err() || n == 0 {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            guard,
            input,
            enabled,
            decoder: ConsoleInput::default(),
            eof: false,
        })
    }
}
impl Terminal for Console {
    fn output(&mut self, bytes: &[u8]) -> Result<()> {
        if let Some(guard) = &self.guard {
            let mut offset = 0;
            while offset < bytes.len() {
                let mut written = 0;
                let ok = unsafe {
                    WriteFile(
                        guard.hout,
                        bytes[offset..].as_ptr(),
                        (bytes.len() - offset) as u32,
                        &mut written,
                        std::ptr::null_mut(),
                    )
                };
                ensure!(
                    ok != 0 && written > 0,
                    "console write failed after {offset} bytes"
                );
                offset += written as usize;
            }
        } else {
            let mut stdout = io::stdout().lock();
            stdout.write_all(bytes)?;
            stdout.flush()?;
        }
        Ok(())
    }
    fn input(&mut self, accept_bytes: bool) -> Result<Input> {
        let now = Instant::now();
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
    }
}
