use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
use std::{
    io::Read,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use arterm::console::{Console, Terminal};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, CONSOLE_SCREEN_BUFFER_INFO,
    DISABLE_NEWLINE_AUTO_RETURN, STD_OUTPUT_HANDLE,
};

struct Probe(Box<dyn Child + Send + Sync>);
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn cursor() -> (i16, i16) {
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    assert_ne!(
        unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &mut info) },
        0
    );
    (info.dwCursorPosition.X, info.dwCursorPosition.Y)
}

#[test]
fn console_alignment_child() {
    if std::env::var_os("VSTERM_ALIGNMENT_PROBE").is_none() {
        return;
    }
    let mut console = Console::new(false).unwrap();
    let mut mode = 0;
    assert_ne!(
        unsafe { GetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), &mut mode) },
        0
    );
    assert_ne!(mode & DISABLE_NEWLINE_AUTO_RETURN, 0);
    console.output(b"\r\n").unwrap();
    let (_, row) = cursor();
    console.output(b"remote prompt> ").unwrap();
    assert_eq!(cursor(), (15, row));
    arterm::statusln!("[session] Detached; remote session retained.");
    assert_eq!(
        cursor(),
        (0, row + 1),
        "diagnostics left the cursor indented"
    );
    arterm::statusln!("first\nsecond\r\nthird\rfourth");
    assert_eq!(
        cursor(),
        (0, row + 5),
        "mixed newlines drifted or doubled rows"
    );
    console.output(b"next remote line\r\n").unwrap();
    assert_eq!(cursor(), (0, row + 6), "remote output was misaligned");
}

#[test]
fn diagnostics_and_remote_output_stay_aligned_in_a_real_console() {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
    command.args(["--exact", "console_alignment_child", "--nocapture"]);
    command.env("VSTERM_ALIGNMENT_PROBE", "1");
    let mut probe = Probe(pair.slave.spawn_command(command).unwrap());
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 || tx.send(buffer[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut output = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        while let Ok(bytes) = rx.try_recv() {
            output.push_str(&String::from_utf8_lossy(&bytes));
        }
        if let Some(status) = probe.0.try_wait().unwrap() {
            assert!(status.success(), "console alignment probe failed: {output}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "console alignment probe timed out: {output}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
