use anyhow::{ensure, Context, Result};
use std::{
    fs::File,
    io::{Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    ptr, thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, LocalFree, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING, PIPE_ACCESS_DUPLEX},
    System::{
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, WaitNamedPipeW,
            PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        },
        RemoteDesktop::ProcessIdToSessionId,
        Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken},
    },
};

pub struct PipePair {
    pub input: File,
    pub output: File,
}
struct SecurityDescriptor(*mut core::ffi::c_void);
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn current_session_id() -> Result<u32> {
    let mut id = 0;
    ensure!(
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) } != 0,
        "cannot resolve current logon session: {}",
        std::io::Error::last_os_error()
    );
    Ok(id)
}
pub fn current_user_sid() -> Result<String> {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0,
            "cannot open process token: {}",
            std::io::Error::last_os_error()
        );
        let mut needed = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
        ensure!(needed > 0, "cannot size token user information");
        let mut bytes = vec![0u8; needed as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            bytes.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        ensure!(
            ok != 0,
            "cannot read token user information: {}",
            std::io::Error::last_os_error()
        );
        let user = &*(bytes.as_ptr().cast::<TOKEN_USER>());
        let mut sid_text = ptr::null_mut();
        ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut sid_text) != 0,
            "cannot format current SID: {}",
            std::io::Error::last_os_error()
        );
        let mut len = 0;
        while *sid_text.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16(std::slice::from_raw_parts(sid_text, len))?;
        LocalFree(sid_text.cast());
        Ok(sid)
    }
}
fn security_descriptor() -> Result<SecurityDescriptor> {
    unsafe {
        let sid = current_user_sid()?;
        let sddl = wide(std::ffi::OsStr::new(&format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;{sid})"
        )));
        let mut descriptor = ptr::null_mut();
        ensure!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut()
            ) != 0,
            "cannot build pipe ACL: {}",
            std::io::Error::last_os_error()
        );
        Ok(SecurityDescriptor(descriptor))
    }
}
fn create_server(path: &Path) -> Result<File> {
    let descriptor = security_descriptor()?;
    let mut attributes = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(path.as_os_str());
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &mut attributes,
        )
    };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "cannot create broker pipe: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_handle(handle) })
}
fn connect_server(file: &File) -> Result<()> {
    let handle = file.as_raw_handle();
    let ok = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) };
    if ok == 0 {
        ensure!(
            std::io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32),
            "pipe accept failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let mut peer_pid = 0;
    ensure!(
        unsafe { GetNamedPipeClientProcessId(handle, &mut peer_pid) } != 0,
        "cannot identify pipe peer: {}",
        std::io::Error::last_os_error()
    );
    let mut peer_session = 0;
    ensure!(
        unsafe { ProcessIdToSessionId(peer_pid, &mut peer_session) } != 0
            && peer_session == current_session_id()?,
        "pipe peer belongs to another logon session"
    );
    Ok(())
}
pub fn accept(base: &str) -> Result<PipePair> {
    let input = create_server(Path::new(&format!(r"\\.\pipe\{base}.in")))?;
    connect_server(&input)?;
    let output = create_server(Path::new(&format!(r"\\.\pipe\{base}.out")))?;
    connect_server(&output)?;
    Ok(PipePair { input, output })
}
fn open(path: &Path, deadline: Instant) -> Result<File> {
    let name = wide(path.as_os_str());
    loop {
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(unsafe { File::from_raw_handle(handle) });
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(code) if code == ERROR_PIPE_BUSY as i32 || code == ERROR_FILE_NOT_FOUND as i32)
            || Instant::now() >= deadline
        {
            return Err(error)
                .with_context(|| format!("cannot connect to broker pipe {}", path.display()));
        }
        unsafe {
            WaitNamedPipeW(name.as_ptr(), 100);
        }
        thread::sleep(Duration::from_millis(10));
    }
}
pub fn connect(base: &str, timeout: Duration) -> Result<PipePair> {
    let deadline = Instant::now() + timeout;
    let input = open(Path::new(&format!(r"\\.\pipe\{base}.in")), deadline)?;
    let output = open(Path::new(&format!(r"\\.\pipe\{base}.out")), deadline)?;
    Ok(PipePair { input, output })
}
pub fn copy_bridge(mut pair: PipePair) -> Result<()> {
    let mut output_pipe = pair.output;
    let output = thread::spawn(move || -> Result<()> {
        let mut stdout = std::io::stdout().lock();
        let mut bytes = [0u8; 8192];
        loop {
            let read = output_pipe.read(&mut bytes)?;
            if read == 0 {
                break;
            }
            stdout.write_all(&bytes[..read])?;
            stdout.flush()?;
        }
        Ok(())
    });
    let mut stdin = std::io::stdin().lock();
    std::io::copy(&mut stdin, &mut pair.input)?;
    drop(pair.input);
    output
        .join()
        .map_err(|_| anyhow::anyhow!("bridge output thread panicked"))??;
    Ok(())
}
pub fn send_control(base: &str, request: &serde_json::Value) -> Result<serde_json::Value> {
    let mut pair = connect(base, Duration::from_secs(3))?;
    let bytes = serde_json::to_vec(request)?;
    pair.input.write_all(b"DBHC")?;
    pair.input.write_all(&(bytes.len() as u32).to_le_bytes())?;
    pair.input.write_all(&bytes)?;
    pair.input.flush()?;
    drop(pair.input);
    let mut len = [0u8; 4];
    pair.output.read_exact(&mut len)?;
    let mut bytes = vec![0u8; u32::from_le_bytes(len) as usize];
    pair.output.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
