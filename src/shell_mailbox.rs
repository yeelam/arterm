//! Private, bounded transport for the owned cooperating PowerShell process.
//! This is deliberately separate from signed-client IPC authentication.
use anyhow::{ensure, Result};
use std::{
    fs::File,
    os::windows::io::AsRawHandle,
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{ERROR_PIPE_CONNECTED, WAIT_TIMEOUT},
    System::{
        Pipes::{ConnectNamedPipe, DisconnectNamedPipe, GetNamedPipeClientProcessId},
        Threading::WaitForSingleObject,
    },
};

pub struct Mailbox {
    pub name: String,
    pub host_started: u64,
    pipe: File,
}

impl Mailbox {
    pub fn new() -> Result<Self> {
        let name = format!("arterm-shell-{}", uuid::Uuid::now_v7().simple());
        let pipe = crate::local_control::server_pipe_instance(&format!(r"\\.\pipe\{name}"), true)?;
        Ok(Self {
            name,
            pipe,
            host_started: crate::peer_auth::own_creation_time()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::windows::process::CommandExt,
        process::{Command, Stdio},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    #[test]
    fn another_same_user_process_cannot_query_owned_shell_payload() {
        let child = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-Command",
                "Start-Sleep -Seconds 10",
            ])
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mailbox = Mailbox::new().unwrap();
        let name = format!(r"\\.\pipe\{}", mailbox.name);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let pid = child.id();
        let _guard = ChildGuard(child);
        mailbox
            .start(pid, move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok("PRIVATE_FIXTURE_PAYLOAD".into())
            })
            .unwrap();
        let mut intruder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .unwrap();
        let _ = intruder.write_all(b"offer\n");
        let mut response = [0u8; 128];
        let received = intruder.read(&mut response).unwrap_or(0);
        assert_eq!(received, 0, "unauthorized process received mailbox data");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

impl Mailbox {
    pub fn start(
        self,
        pid: u32,
        mut exchange: impl FnMut(&str) -> Result<String> + Send + 'static,
    ) -> Result<()> {
        // Holding the authenticated process handle prevents PID recycling from
        // granting a later process access to a pending command.
        let child = crate::peer_auth::owned_shell(pid)?;
        thread::spawn(move || {
            while unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } == WAIT_TIMEOUT {
                let connected =
                    unsafe { ConnectNamedPipe(self.pipe.as_raw_handle(), std::ptr::null_mut()) }
                        != 0
                        || std::io::Error::last_os_error().raw_os_error()
                            == Some(ERROR_PIPE_CONNECTED as i32);
                if !connected {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                let result = (|| -> Result<()> {
                    let mut peer = 0;
                    ensure!(
                        unsafe {
                            GetNamedPipeClientProcessId(self.pipe.as_raw_handle(), &mut peer)
                        } != 0
                            && peer == pid,
                        "ShellMailboxPeerRejected"
                    );
                    ensure!(
                        unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } == WAIT_TIMEOUT,
                        "ShellMailboxChildExited"
                    );
                    let _current_context = crate::peer_auth::owned_shell(peer)?;
                    let deadline = Instant::now() + Duration::from_secs(2);
                    let mut request = Vec::new();
                    loop {
                        ensure!(
                            request.len() < 80 && Instant::now() < deadline,
                            "ShellMailboxRequestLimit"
                        );
                        let mut byte = [0];
                        crate::local_control::transfer_timeout(
                            &self.pipe,
                            &mut byte,
                            false,
                            deadline.saturating_duration_since(Instant::now()),
                        )?;
                        if byte[0] == b'\n' {
                            break;
                        }
                        request.push(byte[0]);
                    }
                    let request = std::str::from_utf8(&request)?;
                    let mut response = exchange(request)?.into_bytes();
                    ensure!(response.len() <= 24 * 1024, "ShellMailboxResponseLimit");
                    response.push(b'\n');
                    crate::local_control::transfer_timeout(
                        &self.pipe,
                        &mut response,
                        true,
                        Duration::from_secs(2),
                    )?;
                    // Keep the server end alive until the shell has read the response.
                    let mut ack = [0];
                    crate::local_control::transfer_timeout(
                        &self.pipe,
                        &mut ack,
                        false,
                        Duration::from_secs(2),
                    )?;
                    ensure!(ack == [b'\n'], "ShellMailboxAckInvalid");
                    Ok(())
                })();
                if result.is_err() {
                    crate::statusln!("[shell-mailbox] exchange rejected or unavailable");
                }
                unsafe {
                    DisconnectNamedPipe(self.pipe.as_raw_handle());
                }
            }
        });
        Ok(())
    }
}
