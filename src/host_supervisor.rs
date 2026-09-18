use anyhow::Result;
use std::{
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct TunnelSupervisor {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}
fn stop_child(child: &mut Child) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = Command::new("taskkill.exe")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}
impl TunnelSupervisor {
    pub fn start() -> Result<Self> {
        let Some(child) = crate::host_setup::start_tunnel()? else {
            return Ok(Self {
                stop: None,
                thread: None,
            });
        };
        let (stop, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut child = Some(child);
            loop {
                if rx.recv_timeout(Duration::from_secs(1)) != Err(mpsc::RecvTimeoutError::Timeout) {
                    break;
                }
                if let Some(current) = &mut child {
                    match current.try_wait() {
                        Ok(None) => continue,
                        Ok(Some(status)) => eprintln!(
                            "owned code tunnel exited ({status}); sessions remain running"
                        ),
                        Err(error) => {
                            eprintln!("cannot inspect owned tunnel: {error}");
                            stop_child(current);
                        }
                    }
                    child = None;
                }
                if rx.recv_timeout(Duration::from_secs(5)) != Err(mpsc::RecvTimeoutError::Timeout) {
                    break;
                }
                match crate::host_setup::start_tunnel() {
                    Ok(next) => child = next,
                    Err(error) => {
                        eprintln!("tunnel restart failed; host sessions retained: {error:#}")
                    }
                }
            }
            if let Some(child) = &mut child {
                stop_child(child);
            }
        });
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}
impl Drop for TunnelSupervisor {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
