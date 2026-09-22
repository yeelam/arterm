//! Automatic receiver-side Mark-of-the-Web removal on owned staging files only.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::Path,
};
use windows_sys::Win32::Foundation::GENERIC_READ;
use windows_sys::Win32::Storage::FileSystem::{
    GetFinalPathNameByHandleW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use crate::file_transfer::{delete_open_file, handle_identity, validate_absolute_path};

pub const CAPABILITY: &str = "recipient-unblock-v1";
const DELETE: u32 = 0x0001_0000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Applied {
    pub zone_identifier_absent: bool,
    pub files: u64,
}

fn stream(base: &File, delete: bool) -> Result<Option<File>> {
    ensure!(
        base.metadata()?.is_file(),
        "recipient metadata requires a regular file"
    );
    handle_identity(base)?;
    let mut buffer = vec![0u16; 32_768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            base.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    ensure!(
        length > 0 && length < buffer.len(),
        "resolve pinned metadata path: {}",
        std::io::Error::last_os_error()
    );
    let name = String::from_utf16(&buffer[..length]).context("invalid metadata path")?;
    let path = Path::new(
        name.strip_prefix(r"\\?\")
            .context("metadata requires a local DOS path")?,
    );
    // The upload's reserved staging basename is intentionally not user-addressable.
    if path
        .file_name()
        .is_some_and(|name| name == ".arterm-partial")
    {
        validate_absolute_path(path.parent().context("staging path has no parent")?)?;
    } else {
        validate_absolute_path(path)?;
    }
    let resolved = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&name)?;
    ensure!(
        handle_identity(base)? == handle_identity(&resolved)?,
        "metadata base identity changed"
    );
    let mut name = std::ffi::OsString::from(name);
    name.push(":Zone.Identifier");
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    if delete {
        options.access_mode(GENERIC_READ | DELETE);
    }
    let stream = match options.open(name) {
        Ok(file) => file,
        Err(error) if error.raw_os_error() == Some(2) => return Ok(None),
        Err(error) => return Err(error).context("open exact Zone.Identifier stream"),
    };
    ensure!(
        handle_identity(base)? == handle_identity(&stream)?,
        "Zone.Identifier does not belong to the pinned file"
    );
    Ok(Some(stream))
}

/// Caller must own the new staging file and retain its no-delete-sharing handle
/// and ancestor pins until publication. Never call this on a source file.
pub fn ensure_unblocked(base: &File) -> Result<Applied> {
    if let Some(ads) = stream(base, true)? {
        delete_open_file(&ads).context("remove recipient Zone.Identifier")?;
        drop(ads);
    }
    ensure!(
        stream(base, false)?.is_none(),
        "recipient Zone.Identifier still exists"
    );
    Ok(Applied {
        zone_identifier_absent: true,
        files: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    #[test]
    fn exclusive_base_pin_allows_exact_stream_removal_and_absence_is_idempotent() {
        assert_eq!(CAPABILITY, "recipient-unblock-v1");
        let root = std::env::temp_dir().join(format!("arterm-zone-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        for name in ["a.txt", "a.ps1", "a.exe", "a.zip"] {
            let path = root.join(name);
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .share_mode(0)
                .open(&path)
                .unwrap();
            file.write_all(b"dummy bytes; never execute").unwrap();
            let before = Sha256::digest(b"dummy bytes; never execute");
            let ads = format!("{}:Zone.Identifier", path.display());
            std::fs::write(&ads, b"[ZoneTransfer]\r\nZoneId=4\r\n").unwrap();
            let mut unrelated = path.as_os_str().to_os_string();
            unrelated.push(":unrelated");
            std::fs::write(&unrelated, b"retain").unwrap();
            assert!(ensure_unblocked(&file).unwrap().zone_identifier_absent);
            assert!(ensure_unblocked(&file).unwrap().zone_identifier_absent);
            assert_eq!(std::fs::read(&ads).unwrap_err().raw_os_error(), Some(2));
            assert_eq!(std::fs::read(unrelated).unwrap(), b"retain");
            assert!(std::fs::rename(&path, root.join("moved")).is_err());
            drop(file);
            assert_eq!(Sha256::digest(std::fs::read(&path).unwrap()), before);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stream_errors_are_not_treated_as_absence() {
        let root = std::env::temp_dir().join(format!("arterm-zone-parse-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("owned.txt");
        std::fs::write(&path, b"owned").unwrap();
        let ads = format!("{}:Zone.Identifier", path.display());
        let base = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();
        std::fs::write(&ads, b"opaque fixture mark; content is not parsed").unwrap();
        let locked = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&ads)
            .unwrap();
        assert!(ensure_unblocked(&base).is_err());
        drop(locked);
        assert!(ensure_unblocked(&base).unwrap().zone_identifier_absent);
        drop(base);
        std::fs::remove_dir_all(root).unwrap();
    }
}
