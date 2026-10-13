//! Platform configuration, data and runtime directories.
//!
//! Single source of truth for where Apex keeps its files. Every other module
//! (settings, sessions, spell dictionaries, the single-instance socket) asks
//! here rather than computing a path itself. Each directory can be overridden
//! with an environment variable (`APEX_CONFIG_DIR`, `APEX_DATA_DIR`,
//! `APEX_RUNTIME_DIR`), which the test suite and sandboxed installs rely on.
//! Nothing here creates directories except `ensure_private_dir`.

use std::path::PathBuf;

/// Application identifier used for directory names. Change here to rename.
pub const APP_NAME: &str = "Apex";
/// Lower-case identifier for file names, socket names and similar.
pub const APP_ID: &str = "apex";

/// Platform directory set for this application (`~/.config/Apex`,
/// `%APPDATA%\dixon\Apex`, ...). `None` when no home directory can be found.
fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("cx", "dixon", APP_NAME)
}

/// `~/.config/apex` style directory for user-editable configuration.
///
/// Honours `APEX_CONFIG_DIR`; falls back to `./.apex` when no home directory
/// is known so the editor still works in a bare container.
pub fn config_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("APEX_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    dirs().map(|d| d.config_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from(".").join(".apex"))
}

/// Data directory for sessions, recovery journals and caches.
///
/// Honours `APEX_DATA_DIR`. Uses the local (non-roaming) data directory so
/// large caches are not synchronised between machines.
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("APEX_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs().map(|d| d.data_local_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from(".").join(".apex-data"))
}

/// Per-user runtime directory for the single-instance socket (never world-readable).
///
/// Honours `APEX_RUNTIME_DIR`. Platforms without a runtime directory (macOS,
/// Windows, Linux without `XDG_RUNTIME_DIR`) fall back to `data_dir`, which
/// callers should pass through `ensure_private_dir` before creating sockets.
pub fn runtime_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("APEX_RUNTIME_DIR") {
        return PathBuf::from(p);
    }
    dirs().and_then(|d| d.runtime_dir().map(|p| p.to_path_buf())).unwrap_or_else(data_dir)
}

/// Create `dir` with owner-only permissions where the platform supports it.
///
/// On Unix an existing directory that is group- or world-accessible is
/// tightened to `0700`; on other platforms only the creation happens.
/// Errors are the underlying `std::fs` errors.
pub fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(dir)?.permissions();
        // Only rewrite the mode when some group/other bit is set, so a
        // directory that is already private keeps its exact mode.
        if perm.mode() & 0o077 != 0 {
            perm.set_mode(0o700);
            std::fs::set_permissions(dir, perm)?;
        }
    }
    Ok(())
}
