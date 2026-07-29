//! Reading and safely writing documents.
//!
//! # Safe save
//!
//! [`save_atomic`] writes to a temporary file in the *target directory*,
//! flushes and fsyncs it, copies the original permissions, then renames it
//! over the destination. On POSIX `rename(2)` is atomic for readers; on
//! Windows we use `ReplaceFileW` semantics via `std::fs::rename`, which may
//! briefly fail if another process holds the file open, in which case we fall
//! back to a direct write after taking a backup copy.
//!
//! Symlinks: if the destination is a symlink we resolve it first and write
//! through to the target so that editing a symlinked config file does not
//! replace the link with a regular file. Hard links cannot be preserved by a
//! rename-based save; this is documented in `docs/security/file-handling.md`
//! and the user can opt into in-place writes per workspace.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use apex_buffer::{DecodeOutcome, Encoding, LineEnding};
use apex_edit::FileIdentity;

/// Refuse to read files larger than this into memory (2 GiB).
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("file is too large to open ({0} bytes; the limit is {MAX_FILE_BYTES})")]
    TooLarge(u64),
    #[error("{0} is a directory")]
    IsDirectory(PathBuf),
    #[error("file contains binary data")]
    Binary,
    #[error("the file changed on disk since it was opened")]
    ChangedOnDisk,
}

/// A decoded file ready to become a document.
#[derive(Debug)]
pub struct LoadedFile {
    pub path: PathBuf,
    pub text: String,
    pub encoding: Encoding,
    pub had_decode_errors: bool,
    pub line_ending: LineEnding,
    pub mixed_line_endings: bool,
    pub identity: FileIdentity,
    pub read_only: bool,
    /// The raw bytes when the file was detected as binary (for the hex preview).
    pub binary: Option<Vec<u8>>,
}

/// Read and decode a file. Binary files are returned with `binary = Some(bytes)`
/// and empty text so the caller can show a read-only preview.
pub fn load(path: &Path, forced_encoding: Option<Encoding>, fallback: Encoding) -> Result<LoadedFile, FileError> {
    let md = fs::metadata(path)?;
    if md.is_dir() {
        return Err(FileError::IsDirectory(path.to_path_buf()));
    }
    if md.len() > MAX_FILE_BYTES {
        return Err(FileError::TooLarge(md.len()));
    }
    let mut bytes = Vec::with_capacity(md.len() as usize);
    File::open(path)?.read_to_end(&mut bytes)?;
    let identity = FileIdentity::from_metadata(&md);
    let read_only = md.permissions().readonly() || !is_writable(path);
    if forced_encoding.is_none() && apex_buffer::detect_binary(&bytes) {
        return Ok(LoadedFile {
            path: path.to_path_buf(),
            text: String::new(),
            encoding: Encoding::Utf8,
            had_decode_errors: false,
            line_ending: LineEnding::native(),
            mixed_line_endings: false,
            identity,
            read_only: true,
            binary: Some(bytes),
        });
    }
    let DecodeOutcome { text, encoding, had_errors } = apex_buffer::decode(&bytes, forced_encoding, fallback);
    let stats = apex_buffer::line_ending::scan(&text, 4 << 20);
    Ok(LoadedFile {
        path: path.to_path_buf(),
        line_ending: stats.dominant(LineEnding::native()),
        mixed_line_endings: stats.is_mixed(),
        text,
        encoding,
        had_decode_errors: had_errors,
        identity,
        read_only,
        binary: None,
    })
}

fn is_writable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return false };
        // SAFETY: `c` is a valid NUL-terminated path; access(2) only reads it.
        unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        fs::metadata(path).map(|m| !m.permissions().readonly()).unwrap_or(false)
    }
}

/// Options for writing a document.
#[derive(Debug, Clone)]
pub struct SaveOptions {
    pub encoding: Encoding,
    pub line_ending: LineEnding,
    /// Expected on-disk identity; if it differs the save is refused with
    /// [`FileError::ChangedOnDisk`] unless `force` is set.
    pub expected: Option<FileIdentity>,
    pub force: bool,
    /// Write in place (truncate + write) instead of rename. Preserves hard
    /// links and special files at the cost of atomicity.
    pub in_place: bool,
}

/// Encode `text` and write it safely. Returns the new identity.
pub fn save(path: &Path, text: &str, opts: &SaveOptions) -> Result<(FileIdentity, bool), FileError> {
    let target = resolve_symlink(path);
    if !opts.force
        && let (Some(expected), Ok(md)) = (opts.expected, fs::metadata(&target))
    {
        let now = FileIdentity::from_metadata(&md);
        if !same_identity(&expected, &now) {
            return Err(FileError::ChangedOnDisk);
        }
    }
    let normalised = apex_buffer::line_ending::normalise(text, opts.line_ending);
    let (bytes, lossy) = apex_buffer::encode(&normalised, opts.encoding);
    if opts.in_place {
        write_in_place(&target, &bytes)?;
    } else {
        save_atomic(&target, &bytes)?;
    }
    let md = fs::metadata(&target)?;
    Ok((FileIdentity::from_metadata(&md), lossy))
}

/// Compare identities ignoring sub-second timestamp noise on filesystems that
/// truncate mtimes.
pub fn same_identity(a: &FileIdentity, b: &FileIdentity) -> bool {
    if a.len != b.len {
        return false;
    }
    #[cfg(unix)]
    if a.inode != b.inode || a.dev != b.dev {
        return false;
    }
    match (a.modified, b.modified) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// Resolve a symlink chain, but stay with the original path if resolution
/// fails (e.g. dangling link: writing will create the target).
fn resolve_symlink(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_symlink() => fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn write_in_place(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(target)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Temp-file + fsync + rename.
pub fn save_atomic(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let file_name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    // Unique temp name in the same directory so rename is on one filesystem.
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let mut tmp = dir.join(format!(".{file_name}.apex-{pid}-{nanos}.tmp"));
    let mut attempt = 0;
    let mut file = loop {
        // `create_new` fails if the path exists (including as a symlink), which
        // defeats symlink-planting attacks on the temp name.
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => break f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempt < 16 => {
                attempt += 1;
                tmp = dir.join(format!(".{file_name}.apex-{pid}-{nanos}-{attempt}.tmp"));
            }
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        copy_permissions(target, &tmp);
        replace_file(&tmp, target)?;
        sync_dir(dir);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn copy_permissions(from: &Path, to: &Path) {
    if let Ok(md) = fs::metadata(from) {
        let _ = fs::set_permissions(to, md.permissions());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            use std::os::unix::fs::MetadataExt;
            // Best effort: keep ownership when running as a user allowed to chown.
            let c = std::ffi::CString::new(to.as_os_str().as_bytes());
            if let Ok(c) = c {
                // SAFETY: valid C string; chown only reads it. Failure is ignored.
                unsafe {
                    libc::chown(c.as_ptr(), md.uid(), md.gid());
                }
            }
        }
    }
}

#[cfg(windows)]
fn replace_file(tmp: &Path, target: &Path) -> io::Result<()> {
    match fs::rename(tmp, target) {
        Ok(()) => Ok(()),
        Err(_) if target.exists() => {
            // Rename over an existing file can fail on Windows when the target
            // is open elsewhere; retry briefly, then fall back to copy-over.
            for _ in 0..5 {
                std::thread::sleep(std::time::Duration::from_millis(20));
                if fs::rename(tmp, target).is_ok() {
                    return Ok(());
                }
            }
            let backup = target.with_extension("apex-bak");
            fs::copy(target, &backup)?;
            let r = fs::copy(tmp, target).map(|_| ());
            if r.is_ok() {
                let _ = fs::remove_file(&backup);
                let _ = fs::remove_file(tmp);
            }
            r
        }
        Err(e) => Err(e),
    }
}

#[cfg(not(windows))]
fn replace_file(tmp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(tmp, target)
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Current identity of a path, or `None` if it no longer exists.
pub fn identity(path: &Path) -> Option<FileIdentity> {
    fs::metadata(path).ok().map(|m| FileIdentity::from_metadata(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_round_trip_with_crlf_and_bom() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        let opts = SaveOptions { encoding: Encoding::Utf8Bom, line_ending: LineEnding::CrLf, expected: None, force: false, in_place: false };
        let (id, lossy) = save(&p, "x\ny\n", &opts).unwrap();
        assert!(!lossy);
        let raw = fs::read(&p).unwrap();
        assert_eq!(raw, b"\xEF\xBB\xBFx\r\ny\r\n");
        let l = load(&p, None, Encoding::Utf8).unwrap();
        assert_eq!(l.text, "x\r\ny\r\n");
        assert_eq!(l.encoding, Encoding::Utf8Bom);
        assert_eq!(l.line_ending, LineEnding::CrLf);
        assert!(same_identity(&id, &l.identity));
        assert!(fs::read_dir(dir.path()).unwrap().count() == 1, "temp file left behind");
    }

    #[test]
    fn refuses_to_overwrite_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        fs::write(&p, "one").unwrap();
        let stale = identity(&p).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, "two changed").unwrap();
        let opts = SaveOptions { encoding: Encoding::Utf8, line_ending: LineEnding::Lf, expected: Some(stale), force: false, in_place: false };
        assert!(matches!(save(&p, "three", &opts), Err(FileError::ChangedOnDisk)));
        assert_eq!(fs::read_to_string(&p).unwrap(), "two changed");
        let forced = SaveOptions { force: true, ..opts };
        save(&p, "three", &forced).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "three");
    }

    #[cfg(unix)]
    #[test]
    fn preserves_permissions_and_writes_through_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.sh");
        fs::write(&real, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("link.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let opts = SaveOptions { encoding: Encoding::Utf8, line_ending: LineEnding::Lf, expected: None, force: false, in_place: false };
        save(&link, "#!/bin/sh\necho hi\n", &opts).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "#!/bin/sh\necho hi\n");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn binary_files_are_not_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin");
        fs::write(&p, b"\x00\x01\x02abc").unwrap();
        let l = load(&p, None, Encoding::Utf8).unwrap();
        assert!(l.binary.is_some());
        assert!(l.read_only);
    }
}
