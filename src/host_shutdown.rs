//! Explicit installation-scoped recovery when a host cannot answer its control pipe.
use anyhow::{bail, ensure, Context, Result};
use std::{
    ffi::OsString,
    io,
    mem::{size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            GetCurrentProcessId, OpenProcess, TerminateProcess, WaitForSingleObject,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        },
    },
};

fn same_path(left: &Path, right: &Path) -> bool {
    let fold = |unit: u16| {
        if (b'A' as u16..=b'Z' as u16).contains(&unit) {
            unit + 32
        } else {
            unit
        }
    };
    left.as_os_str()
        .encode_wide()
        .map(fold)
        .eq(right.as_os_str().encode_wide().map(fold))
}

struct Entry {
    pid: u32,
    parent: u32,
    name: PathBuf,
}
struct Target {
    pid: u32,
    process: OwnedHandle,
    image: PathBuf,
    created: u64,
}

fn snapshot() -> Result<Vec<Entry>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    ensure!(
        snapshot != INVALID_HANDLE_VALUE,
        "cannot enumerate host processes: {}",
        io::Error::last_os_error()
    );
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut found = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    let mut entries = Vec::new();
    while found != 0 {
        let end = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = PathBuf::from(OsString::from_wide(&entry.szExeFile[..end]));
        entries.push(Entry {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            name,
        });
        found = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    ensure!(
        io::Error::last_os_error().raw_os_error() == Some(ERROR_NO_MORE_FILES as i32),
        "host process enumeration failed: {}",
        io::Error::last_os_error()
    );
    Ok(entries)
}

fn open(pid: u32) -> Result<Option<OwnedHandle>> {
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(None);
        }
        return Err(error).with_context(|| format!("cannot inspect process PID {pid}"));
    }
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(None),
        WAIT_TIMEOUT => Ok(Some(process)),
        _ => bail!(
            "cannot inspect process PID {pid}: {}",
            io::Error::last_os_error()
        ),
    }
}

fn child_matches(pid: u32, parent: &Target, created: u64, fresh: &[Entry]) -> bool {
    created >= parent.created
        && fresh
            .iter()
            .any(|entry| entry.pid == pid && entry.parent == parent.pid)
}

fn discover_children(targets: &mut Vec<Target>, deadline: Instant) -> Result<()> {
    let mut index = 0;
    while index < targets.len() {
        ensure!(
            Instant::now() < deadline && targets.len() <= 4096,
            "host child-process discovery exceeded its safety bound; force-stop incomplete"
        );
        for entry in snapshot()? {
            if entry.parent != targets[index].pid || targets.iter().any(|t| t.pid == entry.pid) {
                continue;
            }
            let Some(process) = open(entry.pid)? else {
                continue;
            };
            let created = crate::peer_auth::creation_time(&process)?;
            // Revalidate ancestry AFTER pinning both process objects. The earlier
            // snapshot's child PID might already have been recycled.
            if !child_matches(entry.pid, &targets[index], created, &snapshot()?) {
                continue;
            }
            ensure!(entry.pid != unsafe { GetCurrentProcessId() },
                "cannot force-stop a host from inside its own remote session; run setup in a separate local terminal");
            ensure!(
                crate::peer_auth::same_user_logon(&process)?,
                "child PID {} belongs to another user/logon; force-stop incomplete",
                entry.pid
            );
            let image = crate::peer_auth::image_path(&process)?.canonicalize()?;
            targets.push(Target {
                pid: entry.pid,
                process,
                image,
                created,
            });
        }
        index += 1;
    }
    Ok(())
}

pub(crate) fn force_stop(paths: &[PathBuf]) -> Result<usize> {
    force_stop_with_hook(paths, || Ok(()))
}

pub(crate) fn force_stop_with_hook(
    paths: &[PathBuf],
    before_stop: impl FnOnce() -> Result<()>,
) -> Result<usize> {
    ensure!(!paths.is_empty(), "no host executable paths supplied");
    let paths = paths
        .iter()
        .map(|p| {
            p.canonicalize()
                .with_context(|| format!("resolve host executable {}", p.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut targets = Vec::new();
    for entry in snapshot()? {
        let name = &entry.name;
        let matches_name = paths.iter().any(|p| {
            p.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .eq_ignore_ascii_case(&name.to_string_lossy())
            })
        });
        let pid = entry.pid;
        if matches_name && pid != unsafe { GetCurrentProcessId() } {
            let process = match open(pid) {
                Ok(Some(process)) => process,
                Ok(None) => continue,
                Err(error) => {
                    eprintln!(
                        "Cannot inspect potential host PID {pid}; it was not killed: {error:#}"
                    );
                    continue;
                }
            };
            let image = crate::peer_auth::image_path(&process)?.canonicalize()?;
            if paths.iter().any(|p| same_path(p, &image)) {
                ensure!(crate::peer_auth::same_user_logon(&process)?,
                    "refusing to force-stop PID {pid}: host belongs to another user or Windows logon session");
                let created = crate::peer_auth::creation_time(&process)?;
                targets.push(Target {
                    pid,
                    process,
                    image,
                    created,
                });
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    discover_children(&mut targets, deadline)?;
    before_stop()?;
    let mut stopped = 0;
    let mut next = 0;
    loop {
        // Keep all parent handles alive through post-termination discovery.
        for target in &targets[next..] {
            stopped += usize::from(terminate(target)?);
        }
        next = targets.len();
        discover_children(&mut targets, deadline)?;
        if targets.len() == next {
            break;
        }
    }
    Ok(stopped)
}

fn terminate(target: &Target) -> Result<bool> {
    let Target {
        pid,
        process,
        image,
        ..
    } = target;
    if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
        return Ok(false);
    }

    ensure!(
        crate::peer_auth::same_user_logon(process)?,
        "host PID {pid} changed security context; refusing force-stop"
    );
    ensure!(
        same_path(
            &crate::peer_auth::image_path(process)?.canonicalize()?,
            image
        ),
        "host PID {pid} changed image path; refusing force-stop"
    );
    // Terminate the held, verified process object, never reopen a potentially reused PID.
    if unsafe { TerminateProcess(process.as_raw_handle(), 1) } == 0 {
        let error = io::Error::last_os_error();
        ensure!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0,
            "cannot force-stop host PID {pid}: {error}"
        );
    }
    ensure!(
        unsafe { WaitForSingleObject(process.as_raw_handle(), 10_000) } == WAIT_OBJECT_0,
        "host PID {pid} did not exit after force-stop; update cancelled"
    );
    eprintln!("Force-stopped owned process PID {pid}: {}", image.display());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaced_child_pid_must_match_fresh_pinned_ancestry() {
        let parent = Target {
            pid: 100,
            process: open(std::process::id()).unwrap().unwrap(),
            image: PathBuf::new(),
            created: 1000,
        };
        let unrelated = [Entry {
            pid: 200,
            parent: 999,
            name: PathBuf::new(),
        }];
        assert!(!child_matches(200, &parent, 2000, &unrelated));
        assert!(!child_matches(200, &parent, 2000, &[]));
        let related = [Entry {
            pid: 200,
            parent: 100,
            name: PathBuf::new(),
        }];
        assert!(!child_matches(200, &parent, 999, &related));
        assert!(child_matches(200, &parent, 2000, &related));
    }
}
