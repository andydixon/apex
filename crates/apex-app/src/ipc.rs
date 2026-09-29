//! Single-instance behaviour: a second `apex` invocation hands its paths to
//! the running editor instead of starting another one.
//!
//! The running instance listens on a loopback TCP port and records
//! `{port, token, pid}` in `instance.json` inside the per-user runtime
//! directory (owner-only permissions on Unix). A client must present the
//! token; requests are one JSON line, replies are `ok` and, for `--wait`,
//! `done` once every requested file has been closed. Nothing is reachable
//! from other machines or, on Unix, other users.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use apex_ui::OpenRequest;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Instance {
    port: u16,
    token: String,
    pid: u32,
}

/// What a second invocation asks the running instance to do.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Request {
    #[serde(default)]
    pub token: String,
    pub files: Vec<(PathBuf, Option<usize>, Option<usize>)>,
    pub folders: Vec<PathBuf>,
    pub diff: Option<(PathBuf, PathBuf)>,
    pub wait: bool,
}

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

fn instance_file() -> PathBuf {
    apex_project::paths::runtime_dir().join("instance.json")
}

/// Clients waiting for their files to close: (remaining paths, connection).
static WAITERS: Mutex<Vec<(Vec<PathBuf>, TcpStream)>> = Mutex::new(Vec::new());

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Hand `req` to a running instance. `Ok(true)` when it accepted the request
/// (and, with `wait`, when the files have since been closed); `Ok(false)` when
/// no instance is running.
pub fn try_send(mut req: Request) -> std::io::Result<bool> {
    let path = instance_file();
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let Ok(inst) = serde_json::from_str::<Instance>(&src) else {
        let _ = std::fs::remove_file(&path);
        return Ok(false);
    };
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, inst.port));
    let mut stream = match TcpStream::connect_timeout(&addr, Duration::from_secs(1)) {
        Ok(s) => s,
        Err(_) => {
            // Stale file from an instance that did not exit cleanly.
            let _ = std::fs::remove_file(&path);
            return Ok(false);
        }
    };
    req.token = inst.token;
    for (p, _, _) in &mut req.files {
        *p = canonical(p);
    }
    for p in &mut req.folders {
        *p = canonical(p);
    }
    let wait = req.wait;
    let body = serde_json::to_string(&req).map_err(std::io::Error::other)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(body.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim() != "ok" {
        // A different (older/newer) instance that rejected us: start our own.
        return Ok(false);
    }
    if wait {
        reader.get_ref().set_read_timeout(None)?;
        line.clear();
        // Blocks until "done" or the instance exits (EOF).
        let _ = reader.read_line(&mut line);
    }
    Ok(true)
}

fn random_token() -> String {
    let s = RandomState::new();
    let mut out = String::new();
    for salt in 0..4u64 {
        let v = s.hash_one((std::process::id(), salt, std::time::SystemTime::now()));
        out.push_str(&format!("{v:016x}"));
    }
    out
}

/// Start listening for other invocations. Returns the instance file path.
pub fn serve() -> std::io::Result<PathBuf> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    let dir = apex_project::paths::runtime_dir();
    apex_project::paths::ensure_private_dir(&dir)?;
    let path = instance_file();
    let body = serde_json::to_string(&Instance { port, token: token.clone(), pid: std::process::id() }).map_err(std::io::Error::other)?;
    apex_project::fileio::save_atomic(&path, body.as_bytes())?;
    std::thread::Builder::new().name("apex-ipc".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let token = token.clone();
            std::thread::spawn(move || handle(stream, &token));
        }
    })?;
    Ok(path)
}

fn handle(stream: TcpStream, token: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let Ok(req) = serde_json::from_str::<Request>(&line) else { return };
    // Constant-time comparison: a wrong token must look like a dropped connection.
    if req.token.len() != token.len() || req.token.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) != 0 {
        return;
    }
    let mut stream = reader.into_inner().into_inner();
    let open = OpenRequest { files: req.files.clone(), folders: req.folders.clone(), diff: req.diff.clone() };
    crate::open_requests::push_request(open);
    if stream.write_all(b"ok\n").is_err() {
        return;
    }
    if req.wait {
        let paths: Vec<PathBuf> = req.files.iter().map(|(p, _, _)| canonical(p)).collect();
        if paths.is_empty() {
            let _ = stream.write_all(b"done\n");
            return;
        }
        if let Ok(mut w) = WAITERS.lock() {
            w.push((paths, stream));
        }
    }
}

/// Called by the editor when a file's last view closes.
pub fn view_closed(path: &Path) {
    let closed = canonical(path);
    let Ok(mut waiters) = WAITERS.lock() else { return };
    waiters.retain_mut(|(paths, stream)| {
        paths.retain(|p| *p != closed);
        if paths.is_empty() {
            let _ = stream.write_all(b"done\n");
            false
        } else {
            true
        }
    });
}

/// Remove the instance file and release any waiting clients.
pub fn shutdown() {
    let _ = std::fs::remove_file(instance_file());
    if let Ok(mut w) = WAITERS.lock() {
        for (_, mut stream) in w.drain(..) {
            let _ = stream.write_all(b"done\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_token_and_wait() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: tests in this module run single-threaded with respect to this variable.
        unsafe { std::env::set_var("APEX_RUNTIME_DIR", dir.path()) };
        let rx = crate::open_requests::init();
        let inst = serve().unwrap();
        assert!(inst.exists());
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x").unwrap();
        // A wrong token is ignored.
        let mut bad: Instance = serde_json::from_str(&std::fs::read_to_string(&inst).unwrap()).unwrap();
        bad.token = "nope".repeat(16);
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, bad.port));
        let mut s = TcpStream::connect(addr).unwrap();
        let body = serde_json::to_string(&Request { token: bad.token, files: vec![(f.clone(), None, None)], ..Default::default() }).unwrap();
        s.write_all(body.as_bytes()).unwrap();
        s.write_all(b"\n").unwrap();
        let mut line = String::new();
        let _ = BufReader::new(s).read_line(&mut line);
        assert_eq!(line, "");
        assert!(rx.try_recv().is_err());
        // A real request arrives in the inbox; `wait` resolves when the file closes.
        let f2 = f.clone();
        let t = std::thread::spawn(move || try_send(Request { files: vec![(f2, Some(3), None)], wait: true, ..Default::default() }).unwrap());
        let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(req.files[0].1, Some(3));
        std::thread::sleep(Duration::from_millis(100));
        view_closed(&f);
        assert!(t.join().unwrap());
        shutdown();
        assert!(!inst.exists());
        assert!(!try_send(Request::default()).unwrap());
    }
}
