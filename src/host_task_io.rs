//! Persistent standard handles for background checks and brokers.
use anyhow::{ensure, Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::windows::io::AsRawHandle,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::System::Console::{
    FreeConsole, GetConsoleProcessList, GetConsoleWindow, SetStdHandle, STD_ERROR_HANDLE,
    STD_OUTPUT_HANDLE,
};

pub struct ScheduledIo {
    _stdout: File,
    _stderr: File,
}

impl ScheduledIo {
    pub fn open(root: &Path) -> Result<Self> {
        ensure!(root.is_absolute(), "scheduled data root must be absolute");
        let host = root.join("host");
        fs::create_dir_all(&host).context("create scheduled host log directory")?;
        let mut stdout = append(&host.join("host.stdout.log"))?;
        let mut stderr = append(&host.join("host.stderr.log"))?;
        let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        writeln!(
            stdout,
            "[host] starting pid={} time_ms={started}",
            std::process::id()
        )?;
        writeln!(
            stderr,
            "[host] starting pid={} time_ms={started}",
            std::process::id()
        )?;
        let console_visible = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(GetConsoleWindow())
        } != 0;
        writeln!(stderr, "[host] initial_console_visible={console_visible}")?;

        // Detach this process only. Never hide a console HWND: it may belong to a
        // shared terminal. A console can briefly appear before our entry point.
        let mut process = 0;
        if unsafe { GetConsoleProcessList(&mut process, 1) } != 0 {
            ensure!(
                unsafe { FreeConsole() } != 0,
                "detach scheduled broker console: {}",
                std::io::Error::last_os_error()
            );
        }
        ensure!(
            unsafe { SetStdHandle(STD_OUTPUT_HANDLE, stdout.as_raw_handle()) } != 0,
            "redirect scheduled stdout: {}",
            std::io::Error::last_os_error()
        );
        ensure!(
            unsafe { SetStdHandle(STD_ERROR_HANDLE, stderr.as_raw_handle()) } != 0,
            "redirect scheduled stderr: {}",
            std::io::Error::last_os_error()
        );
        Ok(Self {
            _stdout: stdout,
            _stderr: stderr,
        })
    }
}

fn append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open scheduled host log {}", path.display()))
}
