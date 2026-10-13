//! Reading and safely writing documents.
//!
//! This is the only place that reads a document from disk or writes one back.
//! `load` is called when a tab opens or an external change is reloaded;
//! `save` by Save/Save As and by Find-and-Replace in files; `save_atomic`
//! directly by modules that persist their own small files (sessions,
//! recovery snapshots, the user dictionary). Decoding and encoding are
//! delegated to `apex_buffer`; identity tracking (inode/mtime/size) to
//! `apex_edit::FileIdentity`.
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

/// Everything that can go wrong opening or saving a document. The `Display`
/// text is shown to the user as-is.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    /// Any underlying filesystem error.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The file exceeds `MAX_FILE_BYTES`; carries the actual size.
    #[error("file is too large to open ({0} bytes; the limit is {MAX_FILE_BYTES})")]
    TooLarge(u64),
    /// The path names a directory, not a file.
    #[error("{0} is a directory")]
    IsDirectory(PathBuf),
    /// The path names a device, FIFO or socket. Reading one could block
    /// forever (a FIFO) or never end (`/dev/zero`), so it is refused.
    #[error("{0} is not a regular file")]
    NotRegular(PathBuf),
    /// Reserved for callers that refuse binary content; `load` itself returns
    /// binary files as a `LoadedFile` with `binary` set rather than this.
    #[error("file contains binary data")]
    Binary,
    /// `SaveOptions::expected` did not match the file on disk.
    #[error("the file changed on disk since it was opened")]
    ChangedOnDisk,
}

/// A decoded file ready to become a document.
#[derive(Debug)]
pub struct LoadedFile {
    /// The path as given to `load` (not symlink-resolved).
    pub path: PathBuf,
    /// Decoded text with the file's original line endings intact.
    pub text: String,
    /// Encoding that was detected or forced; used again on save.
    pub encoding: Encoding,
    /// True if decoding substituted replacement characters.
    pub had_decode_errors: bool,
    /// Dominant line ending, used as the default for new lines and on save.
    pub line_ending: LineEnding,
    /// True when more than one line-ending style was seen.
    pub mixed_line_endings: bool,
    /// On-disk identity at load time, for change detection before saving.
    pub identity: FileIdentity,
    /// True when the file is not writable by us, or is binary.
    pub read_only: bool,
    /// The raw bytes when the file was detected as binary (for the hex preview).
    pub binary: Option<Vec<u8>>,
}

/// Read and decode a file. Binary files are returned with `binary = Some(bytes)`
/// and empty text so the caller can show a read-only preview.
///
/// `forced_encoding` skips detection (including binary detection, so a user
/// can reopen a file "as UTF-8"); `fallback` is used when no BOM or valid
/// UTF-8 is found. Errors: directory, too large, or I/O.
pub fn load(path: &Path, forced_encoding: Option<Encoding>, fallback: Encoding) -> Result<LoadedFile, FileError> {
    let md = fs::metadata(path)?;
    if md.is_dir() {
        return Err(FileError::IsDirectory(path.to_path_buf()));
    }
    // `metadata` follows symlinks, so a link to `/dev/zero` or a FIFO in a
    // cloned repository is caught here: those report a size of 0 and would
    // otherwise be read without end (memory) or block the caller (FIFO).
    if !md.is_file() {
        return Err(FileError::NotRegular(path.to_path_buf()));
    }
    if md.len() > MAX_FILE_BYTES {
        return Err(FileError::TooLarge(md.len()));
    }
    let mut bytes = Vec::with_capacity(md.len() as usize);
    // Bounded read: a file that grows after the size check (or was swapped
    // for something larger) still cannot exceed the limit in memory.
    File::open(path)?.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(FileError::TooLarge(bytes.len() as u64));
    }
    let identity = FileIdentity::from_metadata(&md);
    // The readonly bit alone misses ACLs and ownership; `access(2)` asks the
    // kernel the real question.
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
    // Only the first 4 MiB are scanned for line endings; enough to pick the
    // dominant style without a full pass over huge files.
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

/// Can the current user write to `path`? Uses `access(2)` on Unix (which
/// honours ACLs and the effective uid); elsewhere falls back to the readonly
/// attribute.
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
    /// Target encoding; lossy conversions are reported, not refused.
    pub encoding: Encoding,
    /// Every line ending in the text is normalised to this before encoding.
    pub line_ending: LineEnding,
    /// Expected on-disk identity; if it differs the save is refused with
    /// [`FileError::ChangedOnDisk`] unless `force` is set.
    pub expected: Option<FileIdentity>,
    /// Skip the `expected` check (the user chose "overwrite anyway").
    pub force: bool,
    /// Write in place (truncate + write) instead of rename. Preserves hard
    /// links and special files at the cost of atomicity.
    pub in_place: bool,
}

/// Encode `text` and write it safely. Returns the new identity.
///
/// The second element of the result is true when the encoding was lossy
/// (characters not representable in `opts.encoding` were replaced). The
/// identity check and the write both target the symlink-resolved path.
pub fn save(path: &Path, text: &str, opts: &SaveOptions) -> Result<(FileIdentity, bool), FileError> {
    let target = resolve_symlink(path);
    // A missing file (deleted since open, or Save As to a new name) is not
    // "changed": only compare when metadata is readable.
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
///
/// Size must match; on Unix so must device and inode (a rename-based save by
/// another editor changes the inode). A missing mtime on either side is
/// treated as a match rather than a change.
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

/// Truncate and rewrite `target` in place, then fsync. Not atomic: a crash
/// mid-write leaves a partial file. Used only when `SaveOptions::in_place`.
fn write_in_place(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(target)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Temp-file + fsync + rename.
///
/// The temp file is created with `create_new` in the target's directory, so
/// the final rename never crosses filesystems and an attacker who can write
/// to the directory cannot pre-plant a symlink at the temp name. On any
/// failure the temp file is removed and the original is left untouched.
pub fn save_atomic(target: &Path, bytes: &[u8]) -> io::Result<()> {
    // `parent()` of a bare file name is `Some("")`; treat that as `.`.
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
    // Closure so every `?` funnels into one cleanup path below.
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        // Close before rename: Windows cannot rename an open file.
        drop(file);
        copy_permissions(target, &tmp);
        replace_file(&tmp, target)?;
        // Make the rename itself durable, not just the file contents.
        sync_dir(dir);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Copy mode bits (and, on Unix, owner/group) from the original file to the
/// temp file so the renamed result looks like the original. Best effort: a
/// missing original (new file) or a failed `chown` is silently ignored.
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

/// Move the temp file over the target. Windows version with retry and a
/// copy-over fallback (see the module docs); the backup copy is removed only
/// when the copy succeeded, so a failure leaves `target.apex-bak` behind.
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

/// Move the temp file over the target. On POSIX `rename(2)` replaces the
/// destination atomically.
#[cfg(not(windows))]
fn replace_file(tmp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(tmp, target)
}

/// fsync the directory so the rename survives a crash (Unix only; elsewhere
/// directories cannot be opened for sync and this is a no-op).
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

    /// BOM and CRLF survive a save/load cycle and no temp file is left over.
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

    /// A stale `expected` identity blocks the save unless `force` is set.
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

    /// Saving through a symlink keeps the link and the target's mode bits.
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

    /// A file with NUL bytes comes back as a read-only binary preview.
    #[cfg(unix)]
    #[test]
    fn refuses_devices_and_fifos_even_through_symlinks() {
        // A repository can contain a symlink to /dev/zero; reading it would
        // never finish. A FIFO would block the caller.
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("README");
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
        assert!(matches!(load(&link, None, Encoding::Utf8), Err(FileError::NotRegular(_))));
        let fifo = dir.path().join("pipe");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path; mkfifo only reads it.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(matches!(load(&fifo, None, Encoding::Utf8), Err(FileError::NotRegular(_))));
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
