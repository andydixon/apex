//! Checking GitHub for a newer Apex release.
//!
//! One request to the GitHub API's "latest release" endpoint, made with the
//! system's `curl` (`APEX_CURL` overrides), the way git and ssh are run: no
//! HTTP stack is linked in. Blocking; the UI calls it on a worker thread.
//! Links from the reply are kept only if they point into this project's
//! GitHub pages, so a tampered reply cannot send the user anywhere else.

use std::path::PathBuf;
use std::process::Stdio;

/// The API endpoint for the newest published (non-draft, non-prerelease) release.
pub const LATEST_URL: &str = "https://api.github.com/repos/andydixon/apex/releases/latest";

/// Every link the update check will open starts with this.
const PROJECT_PAGES: &str = "https://github.com/andydixon/apex/";

/// The newest release, as shown in the update dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Version without the tag's leading `v` (`0.3.0`).
    pub version: String,
    /// Publication date, `YYYY-MM-DD`.
    pub date: String,
    /// The release page on GitHub.
    pub page: String,
    /// The installer or archive for this platform, if the release has one.
    pub download: Option<String>,
}

/// Ask GitHub for the newest release.
pub fn latest() -> Result<Release, String> {
    let program = std::env::var_os("APEX_CURL").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("curl"));
    let out = crate::git_cli::base_command(&program, None)
        .args(["-fsSL", "--max-time", "15", "-H", "Accept: application/vnd.github+json"])
        .args(["-A", &format!("Apex/{}", env!("CARGO_PKG_VERSION")), LATEST_URL])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().last().map(str::trim).filter(|l| !l.is_empty()).unwrap_or("could not reach GitHub").to_string());
    }
    parse(&String::from_utf8_lossy(&out.stdout), std::env::consts::OS)
}

/// Read the API reply, choosing the download for `os` (`std::env::consts::OS`).
pub fn parse(json: &str, os: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|_| "unexpected reply from GitHub".to_string())?;
    let tag = v["tag_name"].as_str().ok_or("the reply has no version")?;
    let page = v["html_url"].as_str().filter(|u| u.starts_with(PROJECT_PAGES)).ok_or("the reply has no release page")?;
    let suffix = match os {
        "macos" => "-macos.dmg",
        "windows" => "-setup.exe",
        _ => "-linux-x86_64.tar.gz",
    };
    let download = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["name"].as_str().is_some_and(|n| n.ends_with(suffix)))
        .find_map(|a| a["browser_download_url"].as_str().filter(|u| u.starts_with(PROJECT_PAGES)))
        .map(str::to_string);
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        date: v["published_at"].as_str().unwrap_or("").chars().take(10).collect(),
        page: page.to_string(),
        download,
    })
}

/// Whether `latest` is a higher version than `current` (dotted numbers;
/// anything after a `-` is ignored).
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parts = |s: &str| s.trim_start_matches('v').split('-').next().unwrap_or("").split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (mut a, mut b) = (parts(latest), parts(current));
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = r#"{
        "tag_name": "v0.3.0",
        "html_url": "https://github.com/andydixon/apex/releases/tag/v0.3.0",
        "published_at": "2026-10-20T09:15:00Z",
        "assets": [
            {"name": "Apex-0.3.0-macos.dmg", "browser_download_url": "https://github.com/andydixon/apex/releases/download/v0.3.0/Apex-0.3.0-macos.dmg"},
            {"name": "Apex-0.3.0-setup.exe", "browser_download_url": "https://evil.example/Apex-0.3.0-setup.exe"},
            {"name": "apex-0.3.0-linux-x86_64.tar.gz", "browser_download_url": "https://github.com/andydixon/apex/releases/download/v0.3.0/apex-0.3.0-linux-x86_64.tar.gz"}
        ]
    }"#;

    #[test]
    fn reads_the_release_for_each_platform() {
        let mac = parse(REPLY, "macos").unwrap();
        assert_eq!((mac.version.as_str(), mac.date.as_str()), ("0.3.0", "2026-10-20"));
        assert_eq!(mac.page, "https://github.com/andydixon/apex/releases/tag/v0.3.0");
        assert!(mac.download.unwrap().ends_with("macos.dmg"));
        assert!(parse(REPLY, "linux").unwrap().download.unwrap().ends_with(".tar.gz"));
        // A link outside the project is dropped, not opened.
        assert_eq!(parse(REPLY, "windows").unwrap().download, None);
        assert!(parse(&REPLY.replace("github.com/andydixon/apex/releases/tag", "evil.example"), "linux").is_err());
        assert!(parse("<html>", "linux").is_err());
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.1"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.2", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
    }
}
