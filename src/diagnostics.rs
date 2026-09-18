use std::{
    fmt,
    io::{self, Write},
    sync::{mpsc, OnceLock},
    time::Duration,
};

enum Entry {
    Line(Vec<u8>),
    Flush(mpsc::SyncSender<()>),
}
static ASYNC: OnceLock<mpsc::SyncSender<Entry>> = OnceLock::new();

pub fn nonblocking() {
    ASYNC.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Entry>(64);
        std::thread::spawn(move || {
            let mut stderr = io::stderr().lock();
            while let Ok(entry) = rx.recv() {
                match entry {
                    Entry::Line(bytes) => {
                        if stderr
                            .write_all(&bytes)
                            .and_then(|_| stderr.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                    Entry::Flush(ack) => {
                        let _ = stderr.flush();
                        let _ = ack.try_send(());
                    }
                }
            }
        });
        tx
    });
}

pub fn line(args: fmt::Arguments<'_>) {
    let output = normalize(&args.to_string());
    if let Some(sender) = ASYNC.get() {
        let _ = sender.try_send(Entry::Line(output));
        return;
    }
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(&output);
    let _ = stderr.flush();
}

/// A healthy drained stderr receives final diagnostics; a blocked sink cannot hold exit.
pub fn drain(timeout: Duration) {
    if let Some(sender) = ASYNC.get() {
        let (tx, rx) = mpsc::sync_channel(1);
        if sender.try_send(Entry::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(timeout);
        }
    }
}

fn normalize(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() + 2);
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                output.extend_from_slice(b"\r\n");
                index += 1;
                if bytes.get(index) == Some(&b'\n') {
                    index += 1;
                }
            }
            b'\n' => {
                output.extend_from_slice(b"\r\n");
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    if !output.ends_with(b"\r\n") {
        output.extend_from_slice(b"\r\n");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_use_crlf_without_a_leading_carriage_return() {
        assert_eq!(normalize("status"), b"status\r\n");
        assert_eq!(normalize("one\ntwo"), b"one\r\ntwo\r\n");
        assert_eq!(normalize("one\r\ntwo\rthree"), b"one\r\ntwo\r\nthree\r\n");
        assert!(!normalize("status").starts_with(b"\r"));
    }
    #[test]
    fn blocked_sink_child() {
        if std::env::var_os("ARTERM_DIAGNOSTIC_TEST_CHILD").is_none() {
            return;
        }
        nonblocking();
        let message = "x".repeat(8192);
        for _ in 0..256 {
            line(format_args!("{message}"));
        }
        line(format_args!("[client] final error"));
        drain(Duration::from_millis(250));
    }
    #[test]
    fn diagnostic_drain_is_bounded_with_unread_stderr() {
        use std::{
            process::{Command, Stdio},
            time::Instant,
        };
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Child(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "diagnostics::tests::blocked_sink_child",
                    "--nocapture",
                ])
                .env("ARTERM_DIAGNOSTIC_TEST_CHILD", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "unread stderr held process exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
