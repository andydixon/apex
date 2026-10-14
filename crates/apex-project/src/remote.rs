//! Files on other machines, reached through the user's `ssh` binary.
//!
//! Every call runs one `ssh host sh -c '…'` with a small POSIX script, so the
//! user's `~/.ssh/config`, keys and agent apply and nothing needs installing
//! on the server beyond `sh`, `cat`, `ls` and `cksum`. Calls block; the UI
//! runs them on worker threads.
//!
//! * Keys and the agent are tried first. Password and passphrase prompts go
//!   through Apex's askpass helper: an [`Auth`] with a prompter asks the user
//!   once and remembers the answer in [`Credentials`] (locked memory, see
//!   `secret`); one without (background checks) answers only from what is
//!   remembered and otherwise fails quietly. A rejected answer is forgotten.
//! * On Unix, connections are multiplexed (`ControlMaster`), so after the
//!   first call each one costs a round trip rather than a handshake.
//! * A file's *stamp* is its `cksum` output: comparing stamps detects
//!   changes made on the server without transferring the file.
//! * Writes upload to a temporary file beside the target, check the byte
//!   count arrived intact, then copy it over the target (keeping its owner
//!   and permissions). A dropped connection never truncates the original.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use crate::secret::Secret;

/// Asks the user `prompt` for `host`; `None` = cancelled.
pub type Prompter = Arc<dyn Fn(&str, &str) -> Option<Secret> + Send + Sync>;

/// Passwords and passphrases already given, by host and prompt. Clones share
/// one store; dropping or forgetting an entry wipes it.
#[derive(Clone, Default)]
pub struct Credentials(Arc<Mutex<HashMap<(String, String), Secret>>>);

impl Credentials {
    /// Wipe everything remembered for `host`.
    pub fn forget(&self, host: &str) {
        self.0.lock().retain(|(h, _), _| h != host);
    }

    /// Hosts with something remembered.
    pub fn hosts(&self) -> Vec<String> {
        let mut v: Vec<String> = self.0.lock().keys().map(|(h, _)| h.clone()).collect();
        v.sort();
        v.dedup();
        v
    }
}

/// How a call may authenticate.
#[derive(Clone)]
pub struct Auth {
    pub credentials: Credentials,
    /// Set for calls the user started; background calls never ask.
    pub prompter: Option<Prompter>,
}

/// Only reusable answers are remembered: one-time codes are asked each time.
fn rememberable(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    p.contains("password") || p.contains("passphrase")
}

/// The multiplexing socket path, or `None` where it is not used.
fn control_path() -> Option<String> {
    #[cfg(unix)]
    {
        let dir = crate::paths::runtime_dir();
        let socket = dir.join("ssh-%C");
        // Unix socket paths are limited to ~104 bytes; skip multiplexing rather than fail.
        if socket.as_os_str().len() < 90 && crate::paths::ensure_private_dir(&dir).is_ok() {
            return Some(socket.display().to_string());
        }
    }
    None
}

/// Close the shared connection to `host`, if one is open, so nothing can
/// reuse its login after Apex has forgotten the password.
pub fn disconnect(host: &str) {
    if let Some(path) = control_path()
        && !host.starts_with('-')
    {
        let _ = crate::git_cli::base_command(&program(), None)
            .args(["-o", &format!("ControlPath={path}"), "-O", "exit", "--", host])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// `APEX_SSH` (tests, unusual installs) or `ssh` from `PATH`.
fn program() -> PathBuf {
    std::env::var_os("APEX_SSH").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("ssh"))
}

/// Quote `s` as one POSIX shell word.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run `script` with `sh` on `host`, feeding it `stdin`. Returns stdout, or
/// the last line ssh or the script wrote to stderr.
fn run(auth: &Auth, host: &str, script: &str, stdin: Option<&[u8]>) -> Result<Vec<u8>, String> {
    // A leading dash would be read as an ssh option.
    if host.is_empty() || host.starts_with('-') || host.chars().any(char::is_whitespace) {
        return Err(format!("{host:?} is not a valid host"));
    }
    let mut cmd = crate::git_cli::base_command(&program(), None);
    cmd.args(["-o", "ConnectTimeout=8", "-o", "ServerAliveInterval=5", "-o", "ServerAliveCountMax=2"]);
    // First contact records the host key; a changed key is still refused.
    cmd.args(["-o", "StrictHostKeyChecking=accept-new"]);
    if let Some(path) = control_path() {
        cmd.args(["-o", "ControlMaster=auto", "-o", &format!("ControlPath={path}"), "-o", "ControlPersist=120"]);
    }
    // Answers prompts while this call runs: remembered first, then the user.
    let done = Arc::new(AtomicBool::new(false));
    let listener = {
        let (creds, prompter, h, d) = (auth.credentials.clone(), auth.prompter.clone(), host.to_string(), done.clone());
        crate::git_cli::askpass_listener(
            move || d.load(Ordering::Relaxed),
            move |prompt| {
                let key = (h.clone(), prompt.to_string());
                if let Some(s) = creds.0.lock().get(&key) {
                    return Some(s.clone());
                }
                let s = prompter.as_ref()?(&h, prompt)?;
                if rememberable(prompt) {
                    creds.0.lock().insert(key, s.clone());
                }
                Some(s)
            },
        )
    };
    /// Stops the listener however this function returns.
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _stop = Stop(done);
    match listener {
        // One try per prompt: a wrong remembered password fails fast and is forgotten.
        Some((port, token)) if crate::git_cli::askpass_env(&mut cmd, port, &token) => cmd.args(["-o", "NumberOfPasswordPrompts=1"]),
        // Nothing can relay a prompt, so ssh must not try to show one.
        _ => cmd.args(["-o", "BatchMode=yes"]),
    };
    // The login shell may not be POSIX (fish), so the script runs under sh.
    cmd.arg("--").arg(host).arg(format!("sh -c {}", q(script)));
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not run ssh: {e}"))?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // Written on a thread so a full stdout pipe cannot deadlock us.
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("Permission denied") {
        auth.credentials.forget(host);
    }
    Err(err.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(str::to_string).unwrap_or_else(|| format!("ssh exited with {}", out.status)))
}

/// Split `out` into its first line (the stamp) and the rest.
fn split_stamp(out: &[u8]) -> Result<(String, &[u8]), String> {
    let nl = out.iter().position(|&b| b == b'\n').ok_or("unexpected reply from the server")?;
    Ok((String::from_utf8_lossy(&out[..nl]).trim().to_string(), &out[nl + 1..]))
}

/// The file's contents and stamp.
pub fn read(auth: &Auth, host: &str, path: &str) -> Result<(Vec<u8>, String), String> {
    let p = q(path);
    let out = run(auth, host, &format!("cksum < {p} && cat < {p}"), None)?;
    let (stamp, body) = split_stamp(&out)?;
    Ok((body.to_vec(), stamp))
}

/// Replace (or create) the file with `bytes`; returns the new stamp.
pub fn write(auth: &Auth, host: &str, path: &str, bytes: &[u8]) -> Result<String, String> {
    let p = q(path);
    let n = bytes.len();
    // `cat` sees end-of-input when the connection drops, so the size check
    // is what stops a partial upload reaching the target.
    let script =
        format!("t={p}.apex-$$; trap 'rm -f \"$t\"' EXIT HUP INT TERM; cat > \"$t\" && [ $(($(wc -c < \"$t\"))) -eq {n} ] && cat \"$t\" > {p} && cksum < {p}");
    let out = run(auth, host, &script, Some(bytes))?;
    Ok(split_stamp(&out)?.0)
}

/// The file's current stamp (no contents are transferred).
pub fn stamp(auth: &Auth, host: &str, path: &str) -> Result<String, String> {
    Ok(split_stamp(&run(auth, host, &format!("cksum < {}", q(path)), None)?)?.0)
}

/// The absolute form of `dir` (the home folder when empty) and its entries,
/// sorted, folders first and marked with a trailing `/`.
pub fn list(auth: &Auth, host: &str, dir: &str) -> Result<(String, Vec<String>), String> {
    let cd = if dir.is_empty() { "cd".to_string() } else { format!("cd -- {}", q(dir)) };
    // -L follows symlinks so a link to a folder is listed as one.
    let out = run(auth, host, &format!("{cd} && pwd && {{ ls -1ApL 2>/dev/null; true; }}"), None)?;
    let text = String::from_utf8_lossy(&out);
    let mut lines = text.lines();
    let abs = lines.next().unwrap_or("/").to_string();
    let mut entries: Vec<String> = lines.filter(|l| !l.is_empty()).map(str::to_string).collect();
    entries.sort_by_key(|e| (!e.ends_with('/'), e.to_lowercase()));
    Ok((abs, entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every operation against a fake `ssh` that runs the command locally,
    /// so the scripts themselves are exercised by a real `sh`.
    #[cfg(unix)]
    #[test]
    fn scripts_round_trip_through_sh() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let fake = d.path().join("ssh");
        std::fs::write(&fake, "#!/bin/sh\nfor last; do :; done\nexec sh -c \"$last\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: only this test reads APEX_SSH.
        unsafe { std::env::set_var("APEX_SSH", &fake) };

        let dir = d.path().join("it's here");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let file = dir.join("a.txt");
        let path = file.to_str().unwrap();
        std::fs::write(&file, "one\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();

        let a = Auth { credentials: Credentials::default(), prompter: None };
        let (body, s1) = read(&a, "h", path).unwrap();
        assert_eq!(body, b"one\n");
        assert_eq!(stamp(&a, "h", path).unwrap(), s1);

        let s2 = write(&a, "h", path, b"two\nlines\n").unwrap();
        assert_ne!(s1, s2);
        assert_eq!(std::fs::read(&file).unwrap(), b"two\nlines\n");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o640, "permissions kept");
        assert_eq!(stamp(&a, "h", path).unwrap(), s2);

        let (abs, entries) = list(&a, "h", dir.to_str().unwrap()).unwrap();
        assert_eq!(abs, dir.to_str().unwrap());
        assert_eq!(entries, vec!["sub/".to_string(), "a.txt".to_string()], "no temp file left behind");

        assert!(read(&a, "h", dir.join("missing").to_str().unwrap()).is_err());
        assert!(run(&a, "-oProxyCommand=x", "true", None).is_err());

        // A server that wants a password: the stand-in plays the askpass
        // helper itself, over the same loopback protocol.
        let pw = d.path().join("ssh-pw");
        std::fs::write(
            &pw,
            "#!/bin/bash\nexec 3<>/dev/tcp/127.0.0.1/$APEX_ASKPASS_PORT || exit 255\nprintf '%s\\nh'\\''s password: \\n' \"$APEX_ASKPASS_TOKEN\" >&3\nread -r line <&3\n[ \"$line\" = 'OK hunter2' ] || { echo 'Permission denied (password).' >&2; exit 255; }\nfor last; do :; done\nexec sh -c \"$last\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&pw, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: as above.
        unsafe { std::env::set_var("APEX_SSH", &pw) };
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = asked.clone();
        let answer = Arc::new(Mutex::new("wrong"));
        let given = answer.clone();
        let interactive = Auth {
            credentials: Credentials::default(),
            prompter: Some(Arc::new(move |host: &str, prompt: &str| {
                assert_eq!((host, prompt), ("h", "h's password:"));
                n.fetch_add(1, Ordering::SeqCst);
                Some(Secret::new(given.lock().as_bytes()))
            })),
        };
        let silent = Auth { credentials: interactive.credentials.clone(), prompter: None };
        assert!(stamp(&silent, "h", path).is_err(), "background calls never ask");
        assert!(stamp(&interactive, "h", path).unwrap_err().contains("Permission denied"));
        assert!(interactive.credentials.hosts().is_empty(), "a rejected password is forgotten");
        *answer.lock() = "hunter2";
        assert_eq!(stamp(&interactive, "h", path).unwrap(), s2);
        assert_eq!(stamp(&silent, "h", path).unwrap(), s2, "remembered for background calls");
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        interactive.credentials.forget("h");
        assert!(stamp(&silent, "h", path).is_err());
    }

    #[test]
    fn quoting() {
        assert_eq!(q("a b"), "'a b'");
        assert_eq!(q("it's"), "'it'\\''s'");
        assert_eq!(split_stamp(b"1 2\nbody").unwrap(), ("1 2".to_string(), &b"body"[..]));
    }
}
