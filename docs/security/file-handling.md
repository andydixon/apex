# File handling guarantees and limitations

## Loading

* Files are read fully into memory; files over 2 GiB are refused with a clear
  error rather than attempted.
* Encoding detection order: BOM → UTF-16 heuristic (NUL distribution plus
  printable-ASCII check over the first 8 KiB) → valid UTF-8 → configured
  fallback (Windows-1252 by default, which decodes every byte sequence).
* If decoding replaced invalid sequences with U+FFFD, the document is flagged
  and the status bar warns that saving will not restore the original bytes.
* A NUL byte in the first 8 KiB (that is not part of UTF-16) marks the file as
  binary; it opens as a read-only hexadecimal preview and cannot be saved over.

## Saving

`apex_project::fileio::save`:

1. Resolves a symlinked destination so the link is preserved and the target
   file is rewritten.
2. Compares the destination's current identity (size, modification time, and
   inode/device on Unix) with the identity recorded at load or last save. A
   mismatch refuses the save; the user chooses Overwrite, Save As or Cancel.
3. Creates a temporary file in the destination directory with `O_EXCL`
   (`create_new`), so a planted symlink at the temporary name cannot redirect
   the write.
4. Writes, `fsync`s the file, copies the destination's permission bits (and
   ownership where the process is allowed to), renames over the destination,
   then `fsync`s the directory.

### Platform notes

* **Linux/macOS**: `rename(2)` is atomic; readers see either the old or the new
  file. The directory fsync makes the rename durable.
* **Windows**: `MoveFileEx` with replace is used through `std::fs::rename`. If
  the destination is held open by another process the rename can fail; Apex
  retries briefly and then falls back to copying over the file after taking a
  `.apex-bak` copy, which is not atomic.
* **Hard links**: a rename-based save breaks hard links (the other names keep
  the old inode). Enable in-place saving for a workspace if you rely on them;
  in-place saves are not atomic.
* **Network and FUSE filesystems** may not honour fsync or atomic rename.

## Recovery and session data

Session, recovery and recent-file data are stored under the platform data
directory in a `0700` directory. Recovery can be disabled in settings.
