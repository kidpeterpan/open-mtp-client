//! The mark a browser puts on a download (`com.apple.quarantine`): files the
//! app writes from a phone carry it, so the Mac checks one before it runs
//! (a phone can hold a `.command`, a script or an installer) whoever opens it:
//! this app, Finder, or a Terminal.
//!
//! Written straight with `setxattr` (one call per file; a process per file
//! would cost seconds for a camera folder). A disk that can't hold the mark
//! (FAT, exFAT) is told about by the error, which callers may let pass.

use std::path::Path;

/// The value a browser writes for a fresh download.
fn value() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    format!("0083;{now:08x};open-mtp-client;")
}

/// Marks `path` as come from elsewhere (not following a link).
pub(crate) fn mark(path: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes())?;
    let value = value();
    // SAFETY: both strings are NUL-terminated and outlive the call; the value
    // is passed with its length and not read past it.
    let written = unsafe {
        libc::setxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if written == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
pub(crate) fn read(path: &Path) -> Option<String> {
    let out = std::process::Command::new("xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(path)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_marked_as_a_browser_marks_a_download() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("run me.command");
        std::fs::write(&file, b"echo hi").unwrap();

        mark(&file).unwrap();

        let mark = read(&file).unwrap();
        assert!(
            mark.starts_with("0083;") && mark.ends_with(";open-mtp-client;"),
            "{mark}"
        );
        assert!(super::mark(&dir.path().join("gone")).is_err());
    }
}
