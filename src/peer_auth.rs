//! Strict Windows application-peer authentication for local client IPC.
//!
//! Trust policy: OS Authenticode policy MUST succeed, then the verified primary
//! signer's complete DER must match the embedded public certificate. An untrusted
//! self-signed certificate is rejected; nothing is imported into certificate
//! stores. Revocation is cache-only, chain-excluding-root, and errors are fatal.
//! This intentionally does not implement an in-process custom root policy.
//!
//! Role policy: both ends must be the same signed client image, byte for byte.
//! The hash comparison permits a byte-identical vsterm.exe alias, not another
//! version, host or installer. Independently re-signed images may also differ.
//! Only the client executable should construct ClientIdentity.
//! Certificate identity alone does NOT prove the client role.
//!
//! This verifies the locked image obtained from the kernel-reported process
//! path, NOT kernel-attested mapped-memory identity. The process handle prevents
//! subsequent PID reuse; creation time, aliveness, token, file identity and pipe
//! PID are rechecked. Same-user injection, image ghosting, delegated/stolen pipe
//! handles and administrator attacks are outside this application boundary.
//!
//! Integration: call from both ends before sending secrets/dispatching commands,
//! retain the returned guard for the connection, and never reconnect/reuse an
//! authenticated pipe instance. Core must use local-only pipes, a same-user DACL,
//! PIPE_REJECT_REMOTE_CLIENTS, non-inheritable handles, SECURITY_IDENTIFICATION
//! on client opens, exact target/session routing, and no impersonation while
//! authenticating. Call AuthenticatedPeer::revalidate before each dispatch.
//! Matching medium/high integrity and elevation contexts are supported, including
//! two elevated peers. Cross-integrity/elevation, restricted, AppContainer and
//! UIAccess peers are rejected rather than attempting privilege translation.
//! No runtime unsigned bypass exists. The separate test_fixture API is compiled
//! only for cfg(test) or debug builds with test-unsigned-ipc explicitly enabled.
//! Production APIs remain strict even in those builds. Release artifacts must
//! be built without that feature; enabling it without debug assertions is an error.
//!
//! This is app-peer identity, not a boundary against administrators, process
//! injection, handle theft, or a same-user caller invoking the legitimate client.
//!
//! Uses windows-sys features: Win32_Foundation, Win32_Security,
//! Win32_Security_Cryptography{,_Catalog,_Sip}, Win32_Security_WinTrust,
//! Win32_Storage_FileSystem, Win32_System_{Pipes,Threading}; tests also use
//! Win32_System_IO. All are already present in the observed project manifest.
//! API contracts: learn.microsoft.com/en-us/windows/win32/api/wintrust/
//! nf-wintrust-winverifytrust and ns-wintrust-wintrust_file_info;
//! winbase/nf-winbase-{getnamedpipeclientprocessid,queryfullprocessimagenamew}.

#![cfg(windows)]

#[cfg(all(feature = "test-unsigned-ipc", not(debug_assertions)))]
compile_error!("test-unsigned-ipc is permitted only in explicit debug fixture builds");

use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStringExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{WinTrust::*, *},
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
};

pub const CERTIFICATE_SHA256: [u8; 32] = [
    0x46, 0xb3, 0xa8, 0xa9, 0x30, 0x76, 0x52, 0xd9, 0x06, 0x62, 0xdb, 0x05, 0x6f, 0xde, 0xdc, 0xb9,
    0x3c, 0x91, 0xed, 0xcf, 0x82, 0x3f, 0x6b, 0xe3, 0x00, 0x7a, 0x7e, 0x54, 0xf2, 0xad, 0x17, 0x1c,
];
const CERTIFICATE: &[u8] = include_bytes!("../signing/arTerm-Dev.cer");
// winnt.h integrity RIDs; avoid enabling the unrelated SystemServices module.
const SECURITY_MANDATORY_MEDIUM_RID: u32 = 0x2000;
#[cfg(test)]
const SECURITY_MANDATORY_LOW_RID: u32 = 0x1000;
const SECURITY_MANDATORY_HIGH_RID: u32 = 0x3000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    Os,
    UntrustedSignature,
    RootNotTrusted,
    WrongCertificate,
    MissingSigner,
    DifferentClientBuild,
    UnsafeTokenContext,
    WrongPipeEnd,
    ProcessChanged,
    ImageChanged,
}

#[derive(Debug)]
pub struct AuthError {
    pub reason: Rejection,
    pub operation: &'static str,
    /// Raw Win32 or WinTrust status, never reinterpreted as success.
    pub status: Option<u32>,
}
impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.operation, self.reason)?;
        if let Some(status) = self.status {
            write!(f, " (0x{status:08X})")?;
        }
        Ok(())
    }
}
impl std::error::Error for AuthError {}
pub type Result<T> = std::result::Result<T, AuthError>;

fn denied(reason: Rejection, operation: &'static str) -> AuthError {
    AuthError {
        reason,
        operation,
        status: None,
    }
}
fn os_error(operation: &'static str) -> AuthError {
    io_error(operation, io::Error::last_os_error())
}
fn io_error(operation: &'static str, error: io::Error) -> AuthError {
    AuthError {
        reason: Rejection::Os,
        operation,
        status: error.raw_os_error().map(|n| n as u32),
    }
}
fn check(ok: i32, operation: &'static str) -> Result<()> {
    if ok == 0 {
        Err(os_error(operation))
    } else {
        Ok(())
    }
}
fn raw(handle: &impl AsRawHandle) -> HANDLE {
    handle.as_raw_handle()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    volume: u32,
    index: u64,
    size: u64,
    modified: u64,
}
fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}
fn file_identity(file: &File) -> Result<FileIdentity> {
    let mut info = unsafe { zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    check(
        unsafe { GetFileInformationByHandle(raw(file), &mut info) },
        "file identity",
    )?;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
        return Err(denied(
            Rejection::ImageChanged,
            "image must be a regular file",
        ));
    }
    Ok(FileIdentity {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        modified: ticks(info.ftLastWriteTime),
    })
}

/// A verified file, NOT proof that any particular process executed this file.
/// Holding this object denies write/delete sharing until it is dropped.
pub struct VerifiedImage {
    file: File,
    identity: FileIdentity,
    hash: [u8; 32],
}
impl VerifiedImage {
    pub fn sha256(&self) -> [u8; 32] {
        self.hash
    }
}

fn lock_image(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| io_error("lock image against write/delete", e))
}

/// File-only diagnostic API. Does not authorize IPC or establish client role.
pub fn verify_image(path: &Path) -> Result<VerifiedImage> {
    use std::os::windows::ffi::OsStrExt;
    let path = path
        .canonicalize()
        .map_err(|e| io_error("canonical image path", e))?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file = lock_image(&path)?;
    let identity = file_identity(&file)?;
    verify_signature(&file, &wide)?;
    let hash = hash_locked_image(&mut file, identity)?;
    Ok(VerifiedImage {
        file,
        identity,
        hash,
    })
}

/// File-only role diagnostic using the exact same build predicate as IPC.
/// This does not establish process identity or authorize a connection.
pub fn verify_same_client_build(client: &VerifiedImage, candidate: &VerifiedImage) -> Result<()> {
    check_role(&client.hash, &candidate.hash)
}

fn hash_locked_image(file: &mut File, identity: FileIdentity) -> Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| io_error("seek image", e))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| io_error("hash image", e))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if file_identity(file)? != identity {
        return Err(denied(
            Rejection::ImageChanged,
            "image changed during verification",
        ));
    }
    Ok(digest.finalize().into())
}

fn check_trust(status: i32) -> Result<()> {
    if status != 0 {
        return Err(AuthError {
            reason: if status as u32 == 0x800b0109 {
                Rejection::RootNotTrusted
            } else {
                Rejection::UntrustedSignature
            },
            operation: "OS Authenticode trust required (including root trust)",
            status: Some(status as u32),
        });
    }
    Ok(())
}
fn check_certificate(der: &[u8]) -> Result<()> {
    let embedded: [u8; 32] = Sha256::digest(CERTIFICATE).into();
    let signer: [u8; 32] = Sha256::digest(der).into();
    if embedded != CERTIFICATE_SHA256 || signer != CERTIFICATE_SHA256 || der != CERTIFICATE {
        return Err(denied(
            Rejection::WrongCertificate,
            "verified signer certificate pin",
        ));
    }
    Ok(())
}

fn verify_signature(file: &File, path: &[u16]) -> Result<()> {
    unsafe {
        let mut info: WINTRUST_FILE_INFO = zeroed();
        info.cbStruct = size_of::<WINTRUST_FILE_INFO>() as u32;
        info.pcwszFilePath = path.as_ptr();
        info.hFile = raw(file);
        let mut settings: WINTRUST_SIGNATURE_SETTINGS = zeroed();
        settings.cbStruct = size_of::<WINTRUST_SIGNATURE_SETTINGS>() as u32;
        settings.dwFlags = WSS_VERIFY_SPECIFIC;
        settings.dwIndex = 0;
        let mut data: WINTRUST_DATA = zeroed();
        data.cbStruct = size_of::<WINTRUST_DATA>() as u32;
        data.dwUIChoice = WTD_UI_NONE;
        data.fdwRevocationChecks = WTD_REVOKE_NONE;
        data.dwUnionChoice = WTD_CHOICE_FILE;
        data.Anonymous.pFile = &mut info;
        data.dwStateAction = WTD_STATEACTION_VERIFY;
        data.dwProvFlags = WTD_CACHE_ONLY_URL_RETRIEVAL
            | WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT
            | WTD_DISABLE_MD2_MD4;
        data.pSignatureSettings = &mut settings;
        let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
        let status = WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
        // Keep provider state alive until the certificate is consumed; close on
        // every result, including trust failures and missing signer information.
        let result = (|| {
            check_trust(status)?;
            let provider = WTHelperProvDataFromStateData(data.hWVTStateData);
            if provider.is_null() {
                return Err(denied(Rejection::MissingSigner, "WinTrust provider state"));
            }
            let signer = WTHelperGetProvSignerFromChain(provider, 0, 0, 0);
            if signer.is_null()
                || (*signer).dwError != 0
                || (*signer).csCertChain == 0
                || (*signer).pasCertChain.is_null()
            {
                return Err(denied(Rejection::MissingSigner, "verified primary signer"));
            }
            let cert = (*(*signer).pasCertChain).pCert;
            if cert.is_null() || (*cert).pbCertEncoded.is_null() || (*cert).cbCertEncoded == 0 {
                return Err(denied(Rejection::MissingSigner, "verified signer DER"));
            }
            check_certificate(std::slice::from_raw_parts(
                (*cert).pbCertEncoded,
                (*cert).cbCertEncoded as usize,
            ))
        })();
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        let closed = WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
        result?;
        check_trust(closed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TokenContext {
    sid: Vec<u8>,
    integrity: u32,
    elevation: u32,
    elevation_type: u32,
    session: u32,
    authentication_id: (u32, i32),
    restricted: bool,
    app_container: u32,
    ui_access: u32,
}

// usize storage provides alignment for TOKEN_USER/TOKEN_MANDATORY_LABEL.
fn token_data(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>> {
    // Some fixed-size classes return ERROR_BAD_LENGTH for a null sizing probe.
    let mut data = vec![0usize; 128];
    let mut needed = 0;
    let first = unsafe {
        GetTokenInformation(
            raw(token),
            class,
            data.as_mut_ptr().cast(),
            (data.len() * size_of::<usize>()) as u32,
            &mut needed,
        )
    };
    if first != 0 {
        return Ok(data);
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_INSUFFICIENT_BUFFER || needed == 0 || needed > 1024 * 1024 {
        return Err(AuthError {
            reason: Rejection::Os,
            operation: "read token information",
            status: Some(error),
        });
    }
    data.resize((needed as usize).div_ceil(size_of::<usize>()), 0);
    check(
        unsafe {
            GetTokenInformation(
                raw(token),
                class,
                data.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        },
        "read token information",
    )?;
    Ok(data)
}
fn token_u32(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> Result<u32> {
    let mut value = 0u32;
    let mut returned = 0;
    check(
        unsafe {
            GetTokenInformation(
                raw(token),
                class,
                (&mut value as *mut u32).cast(),
                size_of::<u32>() as u32,
                &mut returned,
            )
        },
        "read DWORD token information",
    )?;
    if returned != size_of::<u32>() as u32 {
        return Err(denied(
            Rejection::UnsafeTokenContext,
            "unexpected DWORD token information size",
        ));
    }
    Ok(value)
}
fn token_context(process: &OwnedHandle) -> Result<TokenContext> {
    unsafe {
        let mut handle = null_mut();
        check(
            OpenProcessToken(raw(process), TOKEN_QUERY, &mut handle),
            "open process token",
        )?;
        let token = OwnedHandle::from_raw_handle(handle);
        let user = token_data(&token, TokenUser)?;
        let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        if IsValidSid(sid) == 0 {
            return Err(denied(Rejection::UnsafeTokenContext, "invalid user SID"));
        }
        let sid = std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize).to_vec();
        let label = token_data(&token, TokenIntegrityLevel)?;
        let integrity_sid = (*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid;
        if IsValidSid(integrity_sid) == 0 {
            return Err(denied(
                Rejection::UnsafeTokenContext,
                "invalid integrity SID",
            ));
        }
        let count = *GetSidSubAuthorityCount(integrity_sid);
        if count != 1 {
            return Err(denied(
                Rejection::UnsafeTokenContext,
                "unexpected integrity SID",
            ));
        }
        let stats = token_data(&token, TokenStatistics)?;
        let stats = &*stats.as_ptr().cast::<TOKEN_STATISTICS>();
        Ok(TokenContext {
            sid,
            integrity: *GetSidSubAuthority(integrity_sid, 0),
            elevation: token_u32(&token, TokenElevation)?,
            elevation_type: token_u32(&token, TokenElevationType)?,
            session: token_u32(&token, TokenSessionId)?,
            authentication_id: (
                stats.AuthenticationId.LowPart,
                stats.AuthenticationId.HighPart,
            ),
            restricted: IsTokenRestricted(raw(&token)) != 0,
            app_container: token_u32(&token, TokenIsAppContainer)?,
            ui_access: token_u32(&token, TokenUIAccess)?,
        })
    }
}
fn check_context(owner: &TokenContext, peer: &TokenContext) -> Result<()> {
    // Require equality, not a privilege ordering: this works in both pipe roles
    // and cannot turn a lower-privilege caller into a higher-privilege deputy.
    if owner != peer
        || !(SECURITY_MANDATORY_MEDIUM_RID..=SECURITY_MANDATORY_HIGH_RID).contains(&owner.integrity)
        || owner.elevation > 1
        || !matches!(owner.elevation_type, 1..=3)
        || owner.restricted
        || owner.app_container != 0
        || owner.ui_access != 0
    {
        return Err(denied(
            Rejection::UnsafeTokenContext,
            "matching user/logon/session/integrity/elevation and unrestricted context required",
        ));
    }
    Ok(())
}
fn check_role(owner: &[u8; 32], peer: &[u8; 32]) -> Result<()> {
    if owner != peer {
        Err(denied(
            Rejection::DifferentClientBuild,
            "byte-identical signed client required",
        ))
    } else {
        Ok(())
    }
}

struct ProcessImage {
    process: OwnedHandle,
    pid: u32,
    created: u64,
    context: TokenContext,
    image: VerifiedImage,
}
pub(crate) fn image_path(process: &OwnedHandle) -> Result<PathBuf> {
    let mut path = vec![0u16; 32768];
    let mut size = path.len() as u32;
    check(
        unsafe { QueryFullProcessImageNameW(raw(process), 0, path.as_mut_ptr(), &mut size) },
        "query process image path",
    )?;
    path.truncate(size as usize);
    Ok(std::ffi::OsString::from_wide(&path).into())
}
pub(crate) fn same_user_logon(process: &OwnedHandle) -> Result<bool> {
    let current = token_context(&open_process(unsafe { GetCurrentProcessId() })?)?;
    let candidate = token_context(process)?;
    Ok(same_logon(&current, &candidate))
}
fn same_logon(current: &TokenContext, candidate: &TokenContext) -> bool {
    current.sid == candidate.sid
        && current.session == candidate.session
        && current.authentication_id == candidate.authentication_id
}
pub(crate) fn creation_time(process: &OwnedHandle) -> Result<u64> {
    let (mut created, mut exited, mut kernel, mut user) =
        unsafe { (zeroed(), zeroed(), zeroed(), zeroed()) };
    check(
        unsafe {
            GetProcessTimes(
                raw(process),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        },
        "process creation time",
    )?;
    Ok(ticks(created))
}
fn check_process_lifetime(process: &OwnedHandle, pid: u32, created: u64) -> Result<()> {
    match unsafe { WaitForSingleObject(raw(process), 0) } {
        WAIT_TIMEOUT => {}
        WAIT_FAILED => return Err(os_error("check process lifetime")),
        _ => return Err(denied(Rejection::ProcessChanged, "peer process exited")),
    }
    if unsafe { GetProcessId(raw(process)) } != pid || creation_time(process)? != created {
        return Err(denied(
            Rejection::ProcessChanged,
            "process identity changed",
        ));
    }
    Ok(())
}
fn open_process(pid: u32) -> Result<OwnedHandle> {
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(os_error("open peer process"));
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn recheck_process_image(
    process: &OwnedHandle,
    pid: u32,
    created: u64,
    context: &TokenContext,
    file: &File,
    identity: FileIdentity,
) -> Result<()> {
    check_process_lifetime(process, pid, created)?;
    if token_context(process)? != *context {
        return Err(denied(
            Rejection::ProcessChanged,
            "process identity changed",
        ));
    }
    let reopened = lock_image(&image_path(process)?)?;
    if file_identity(&reopened)? != identity || file_identity(file)? != identity {
        return Err(denied(
            Rejection::ImageChanged,
            "process image path changed",
        ));
    }
    Ok(())
}

impl ProcessImage {
    fn open(pid: u32) -> Result<Self> {
        let process = open_process(pid)?;
        let created = creation_time(&process)?;
        let context = token_context(&process)?;
        let image = verify_image(&image_path(&process)?)?;
        let result = Self {
            process,
            pid,
            created,
            context,
            image,
        };
        result.recheck()?;
        Ok(result)
    }
    fn recheck(&self) -> Result<()> {
        recheck_process_image(
            &self.process,
            self.pid,
            self.created,
            &self.context,
            &self.image.file,
            self.image.identity,
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PeerEnd {
    /// Authenticate the connected client using the server's pipe handle.
    Client,
    /// Authenticate the server using the connected client's pipe handle.
    Server,
}

fn peer_pid(pipe: BorrowedHandle<'_>, end: PeerEnd) -> Result<u32> {
    let mut flags = 0;
    check(
        unsafe { GetNamedPipeInfo(raw(&pipe), &mut flags, null_mut(), null_mut(), null_mut()) },
        "query named-pipe end",
    )?;
    if matches!(end, PeerEnd::Client) != (flags & PIPE_SERVER_END != 0) {
        return Err(denied(
            Rejection::WrongPipeEnd,
            "peer direction does not match pipe handle",
        ));
    }
    let mut pid = 0;
    check(
        unsafe {
            match end {
                PeerEnd::Client => GetNamedPipeClientProcessId(raw(&pipe), &mut pid),
                PeerEnd::Server => GetNamedPipeServerProcessId(raw(&pipe), &mut pid),
            }
        },
        "query named-pipe peer PID",
    )?;
    if pid == 0 {
        return Err(denied(Rejection::ProcessChanged, "invalid peer PID"));
    }
    Ok(pid)
}

fn check_pipe_peer(pipe: BorrowedHandle<'_>, end: PeerEnd, expected: u32) -> Result<()> {
    if peer_pid(pipe, end)? != expected {
        return Err(denied(Rejection::ProcessChanged, "pipe peer changed"));
    }
    Ok(())
}

/// Construct only in the client program, never host or installer code.
pub struct ClientIdentity {
    owner: ProcessImage,
}
/// Holds peer process/image locks and borrows the original pipe and owner.
/// Keep alive until connection teardown; revalidate before every dispatch.
/// Reconnection on the same pipe instance requires fresh authentication.
pub struct AuthenticatedPeer<'a> {
    owner: &'a ProcessImage,
    peer: ProcessImage,
    pipe: BorrowedHandle<'a>,
    end: PeerEnd,
}
impl AuthenticatedPeer<'_> {
    pub fn pid(&self) -> u32 {
        self.peer.pid
    }
    pub fn creation_time(&self) -> u64 {
        self.peer.created
    }
    pub fn revalidate(&self) -> Result<()> {
        check_pipe_peer(self.pipe, self.end, self.peer.pid)?;
        self.owner.recheck()?;
        self.peer.recheck()?;
        check_context(&self.owner.context, &self.peer.context)?;
        check_role(&self.owner.image.hash, &self.peer.image.hash)?;
        check_pipe_peer(self.pipe, self.end, self.peer.pid)
    }
}
impl ClientIdentity {
    pub fn current() -> Result<Self> {
        let owner = ProcessImage::open(unsafe { GetCurrentProcessId() })?;
        check_context(&owner.context, &owner.context)?;
        Ok(Self { owner })
    }

    /// PID comes exclusively from this connected pipe, never from wire/discovery.
    /// Success requires both OS-trusted exact-pinned images, the same signed
    /// image hash and matching safe token contexts. No debug/unsigned bypass.
    pub fn authenticate<'a>(
        &'a self,
        pipe: BorrowedHandle<'a>,
        end: PeerEnd,
    ) -> Result<AuthenticatedPeer<'a>> {
        let pid = peer_pid(pipe, end)?;
        self.owner.recheck()?;
        let peer = ProcessImage::open(pid)?;
        let authenticated = AuthenticatedPeer {
            owner: &self.owner,
            peer,
            pipe,
            end,
        };
        authenticated.revalidate()?;
        Ok(authenticated)
    }
}

/// Explicit unsigned functional-test policy, never a production identity.
///
/// Core may select this API only at compile time in an opted-in debug fixture
/// build. It intentionally omits Authenticode/pinning, but retains actual kernel
/// peer PID, endpoint direction, held process/image handles, creation time,
/// aliveness, token context, file identity and byte-identical image checks.
/// No fixture value can be converted into a production VerifiedImage or peer.
#[cfg(any(test, all(feature = "test-unsigned-ipc", debug_assertions)))]
pub mod test_fixture {
    use super::*;

    struct FixtureProcessImage {
        process: OwnedHandle,
        pid: u32,
        created: u64,
        context: TokenContext,
        file: File,
        identity: FileIdentity,
        hash: [u8; 32],
    }
    impl FixtureProcessImage {
        fn open(pid: u32) -> Result<Self> {
            let process = open_process(pid)?;
            let created = creation_time(&process)?;
            let context = token_context(&process)?;
            let path = image_path(&process)?
                .canonicalize()
                .map_err(|e| io_error("canonical fixture image path", e))?;
            let mut file = lock_image(&path)?;
            let identity = file_identity(&file)?;
            let hash = hash_locked_image(&mut file, identity)?;
            let result = Self {
                process,
                pid,
                created,
                context,
                file,
                identity,
                hash,
            };
            result.recheck()?;
            Ok(result)
        }
        fn recheck(&self) -> Result<()> {
            recheck_process_image(
                &self.process,
                self.pid,
                self.created,
                &self.context,
                &self.file,
                self.identity,
            )
        }
    }

    pub struct FixtureIdentity {
        owner: FixtureProcessImage,
    }
    pub struct FixturePeer<'a> {
        owner: &'a FixtureProcessImage,
        peer: FixtureProcessImage,
        pipe: BorrowedHandle<'a>,
        end: PeerEnd,
    }
    impl FixtureIdentity {
        pub fn current() -> Result<Self> {
            let owner = FixtureProcessImage::open(unsafe { GetCurrentProcessId() })?;
            check_context(&owner.context, &owner.context)?;
            Ok(Self { owner })
        }
        pub fn authenticate<'a>(
            &'a self,
            pipe: BorrowedHandle<'a>,
            end: PeerEnd,
        ) -> Result<FixturePeer<'a>> {
            let pid = peer_pid(pipe, end)?;
            self.owner.recheck()?;
            let peer = FixtureProcessImage::open(pid)?;
            let guard = FixturePeer {
                owner: &self.owner,
                peer,
                pipe,
                end,
            };
            guard.revalidate()?;
            Ok(guard)
        }
    }
    impl FixturePeer<'_> {
        pub fn pid(&self) -> u32 {
            self.peer.pid
        }
        pub fn creation_time(&self) -> u64 {
            self.peer.created
        }
        pub fn revalidate(&self) -> Result<()> {
            check_pipe_peer(self.pipe, self.end, self.peer.pid)?;
            self.owner.recheck()?;
            self.peer.recheck()?;
            check_context(&self.owner.context, &self.peer.context)?;
            check_role(&self.owner.hash, &self.peer.hash)?;
            check_pipe_peer(self.pipe, self.end, self.peer.pid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsHandle;

    #[test]
    fn only_zero_wintrust_status_is_success() {
        assert!(check_trust(0).is_ok());
        for status in [
            1,
            -1,
            0x800b0109u32 as i32,
            0x80096010u32 as i32,
            0x800b0100u32 as i32,
        ] {
            let error = check_trust(status).unwrap_err();
            assert_eq!(
                error.reason,
                if status as u32 == 0x800b0109 {
                    Rejection::RootNotTrusted
                } else {
                    Rejection::UntrustedSignature
                }
            );
            assert_eq!(error.status, Some(status as u32));
        }
    }
    #[test]
    fn exact_der_pin_not_name_or_subject() {
        check_certificate(CERTIFICATE).unwrap();
        let mut wrong = CERTIFICATE.to_vec();
        wrong[20] ^= 1;
        assert_eq!(
            check_certificate(&wrong).unwrap_err().reason,
            Rejection::WrongCertificate
        );
        assert!(check_certificate(&[]).is_err());
    }
    #[test]
    fn same_certificate_does_not_prove_role_or_version() {
        check_role(&[1; 32], &[1; 32]).unwrap();
        assert_eq!(
            check_role(&[1; 32], &[2; 32]).unwrap_err().reason,
            Rejection::DifferentClientBuild
        );
    }
    fn medium() -> TokenContext {
        TokenContext {
            sid: vec![1, 2, 3],
            integrity: SECURITY_MANDATORY_MEDIUM_RID,
            elevation: 0,
            elevation_type: TokenElevationTypeLimited as u32,
            session: 1,
            authentication_id: (7, 0),
            restricted: false,
            app_container: 0,
            ui_access: 0,
        }
    }
    #[test]
    fn context_rejects_user_logon_session_and_privilege_changes() {
        let owner = medium();
        check_context(&owner, &owner).unwrap();
        for change in 0..9 {
            let mut peer = owner.clone();
            match change {
                0 => peer.sid.push(4),
                1 => peer.integrity = SECURITY_MANDATORY_HIGH_RID,
                2 => peer.elevation = 1,
                3 => peer.elevation_type = TokenElevationTypeFull as u32,
                4 => peer.session += 1,
                5 => peer.authentication_id.0 += 1,
                6 => peer.restricted = true,
                7 => peer.app_container = 1,
                _ => peer.ui_access = 1,
            }

            assert_eq!(
                check_context(&owner, &peer).unwrap_err().reason,
                Rejection::UnsafeTokenContext
            );
        }
        for integrity in [SECURITY_MANDATORY_LOW_RID, 0x4000] {
            let mut both = owner.clone();
            both.integrity = integrity;
            assert!(check_context(&both, &both).is_err());
        }
    }
    #[test]
    fn shutdown_scope_excludes_other_users_sessions_and_logons() {
        let owner = medium();
        assert!(same_logon(&owner, &owner));
        for change in 0..3 {
            let mut peer = owner.clone();
            match change {
                0 => peer.sid.push(9),
                1 => peer.session += 1,
                _ => peer.authentication_id.0 += 1,
            }
            assert!(!same_logon(&owner, &peer));
        }
    }
    #[test]
    fn matching_elevated_peers_pass_but_neither_cross_elevation_direction_does() {
        let mut elevated = medium();
        elevated.integrity = SECURITY_MANDATORY_HIGH_RID;
        elevated.elevation = 1;
        elevated.elevation_type = TokenElevationTypeFull as u32;
        check_context(&elevated, &elevated).unwrap();
        assert!(check_context(&elevated, &medium()).is_err());
        assert!(check_context(&medium(), &elevated).is_err());
        for change in 0..3 {
            let mut both = elevated.clone();
            match change {
                0 => both.restricted = true,
                1 => both.app_container = 1,
                _ => both.ui_access = 1,
            }
            assert!(check_context(&both, &both).is_err());
        }
    }
    #[test]
    fn current_process_token_and_creation_are_queryable_without_debug_rights() {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                GetCurrentProcessId(),
            )
        };
        assert!(!handle.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        assert!(creation_time(&handle).unwrap() > 0);
        assert!(!token_context(&handle).unwrap().sid.is_empty());
        assert!(image_path(&handle).unwrap().is_absolute());
        let created = creation_time(&handle).unwrap();
        check_process_lifetime(&handle, std::process::id(), created).unwrap();
        assert_eq!(
            check_process_lifetime(&handle, std::process::id(), created + 1)
                .unwrap_err()
                .reason,
            Rejection::ProcessChanged
        );
        assert_eq!(
            check_process_lifetime(&handle, 0, created)
                .unwrap_err()
                .reason,
            Rejection::ProcessChanged
        );
    }

    #[test]
    fn connected_owned_pipe_supplies_pid_in_both_directions() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(
            r"\\.\pipe\arterm-peer-auth-test-{}-{nonce}",
            std::process::id()
        );
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        let server = unsafe { OwnedHandle::from_raw_handle(handle) };
        let client = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap();
        let connected = unsafe { ConnectNamedPipe(raw(&server), null_mut()) };
        assert!(connected != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED);
        assert_eq!(
            peer_pid(server.as_handle(), PeerEnd::Client).unwrap(),
            std::process::id()
        );
        assert_eq!(
            peer_pid(client.as_handle(), PeerEnd::Server).unwrap(),
            std::process::id()
        );
        assert_eq!(
            peer_pid(client.as_handle(), PeerEnd::Client)
                .unwrap_err()
                .reason,
            Rejection::WrongPipeEnd
        );
        assert_eq!(
            peer_pid(server.as_handle(), PeerEnd::Server)
                .unwrap_err()
                .reason,
            Rejection::WrongPipeEnd
        );
        assert_eq!(
            check_pipe_peer(server.as_handle(), PeerEnd::Client, 0)
                .unwrap_err()
                .reason,
            Rejection::ProcessChanged
        );
        assert_ne!(unsafe { DisconnectNamedPipe(raw(&server)) }, 0);
        assert!(peer_pid(server.as_handle(), PeerEnd::Client).is_err());
    }

    #[test]
    #[ignore = "owned child entry point; parent supplies stdin"]
    fn owned_lifetime_child() {
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
    }

    #[test]
    fn exited_owned_peer_is_rejected_even_while_process_handle_is_held() {
        use std::{
            io::Write,
            process::{Command, Stdio},
        };
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "peer_auth::tests::owned_lifetime_child",
                "--ignored",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                child.id(),
            )
        };
        assert!(!handle.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let created = creation_time(&handle).unwrap();
        check_process_lifetime(&handle, child.id(), created).unwrap();
        child.stdin.take().unwrap().write_all(&[1]).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(
            check_process_lifetime(&handle, child.id(), created)
                .unwrap_err()
                .reason,
            Rejection::ProcessChanged
        );
    }

    #[test]
    #[ignore = "requires OS-trusted public signed project fixtures"]
    fn signed_file_role_binding_rejects_host_and_installer_even_when_renamed() {
        use std::fs;
        let root = PathBuf::from(std::env::var_os("ARTERM_PUBLIC_SIGNED_FIXTURES").unwrap());
        let client = verify_image(&root.join("arterm.exe")).unwrap();
        let temp = fixture_directory();
        let renamed = temp.join("arterm.exe");
        for name in [
            "arterm-host.exe",
            "arTerm-Client-Setup.exe",
            "arTerm-Host-Setup.exe",
        ] {
            fs::copy(root.join(name), &renamed).unwrap();
            let other = verify_image(&renamed).unwrap();
            assert_eq!(
                check_role(&client.hash, &other.hash).unwrap_err().reason,
                Rejection::DifferentClientBuild
            );
            drop(other);
            fs::remove_file(&renamed).unwrap();
        }
        let alias = temp.join("vsterm.exe");
        fs::copy(root.join("arterm.exe"), &alias).unwrap();
        let alias_image = verify_image(&alias).unwrap();
        check_role(&client.hash, &alias_image.hash).unwrap();
        drop(alias_image);
        fs::remove_file(&alias).unwrap();
        fs::remove_dir(&temp).unwrap();
    }

    fn fixture_directory() -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("arterm-peer-files-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn locked_image_denies_write_rename_and_file_identity_detects_replacement() {
        use std::fs;
        let dir = fixture_directory();
        let path = dir.join("image");
        let replacement = dir.join("replacement");
        let retired = dir.join("retired");
        fs::write(&path, b"original unsigned test data").unwrap();
        fs::write(&replacement, b"replacement test data").unwrap();
        let held = lock_image(&path).unwrap();
        let before = file_identity(&held).unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert!(fs::rename(&path, &retired).is_err());
        drop(held);
        fs::rename(&path, &retired).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let held = lock_image(&path).unwrap();
        assert_ne!(before, file_identity(&held).unwrap());
        drop(held);
        fs::remove_file(&path).unwrap();
        fs::remove_file(&retired).unwrap();
        fs::remove_dir(&dir).unwrap();
    }
}
