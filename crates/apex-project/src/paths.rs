//! Platform configuration, data and runtime directories.

use std::path::PathBuf;

/// Application identifier used for directory names. Change here to rename.
pub const APP_NAME: &str = "Apex";
pub const APP_ID: &str = "apex";

fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("cx", "dixon", APP_NAME)
}

/// `~/.config/apex` style directory for user-editable configuration.
pub fn config_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("APEX_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    dirs().map(|d| d.config_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from(".").join(".apex"))
}

/// Data directory for sessions, recovery journals and caches.
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("APEX_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs().map(|d| d.data_local_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from(".").join(".apex-data"))
}

/// Per-user runtime directory for the single-instance socket (never world-readable).
pub fn runtime_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("APEX_RUNTIME_DIR") {
        return PathBuf::from(p);
    }
    dirs().and_then(|d| d.runtime_dir().map(|p| p.to_path_buf())).unwrap_or_else(data_dir)
}

/// Create `dir` with owner-only permissions where the platform supports it.
pub fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(dir)?.permissions();
        if perm.mode() & 0o077 != 0 {
            perm.set_mode(0o700);
            std::fs::set_permissions(dir, perm)?;
        }
    }
    Ok(())
}
