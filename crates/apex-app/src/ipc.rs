//! Single-instance behaviour: a second `apex` invocation hands its paths to
//! the running editor instead of starting another one.
//!
//! The running instance listens on a loopback TCP port and records
//! `{port, token, pid}` in `instance.json` inside the per-user runtime
//! directory (owner-only permissions on Unix). A client must present the
//! token; requests are one JSON line, replies are `ok` and, for `--wait`,
//! `done` once every requested file has been closed. Nothing is reachable
//! from other machines or, on Unix, other users.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use apex_ui::OpenRequest;
use serde::{Deserialize, Serialize};

/// Contents of `instance.json`: how a later invocation reaches this one.
#[derive(Serialize, Deserialize)]
struct Instance {
    /// Loopback TCP port the listener is bound to.
    port: u16,
    /// Shared secret a client must echo in `Request::token`.
    token: String,
    /// Process id of the running editor; recorded for diagnostics, not read here.
    pid: u32,
}

/// What a second invocation asks the running instance to do.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Request {
    /// Copy of the instance token, filled in by `try_send` from `instance.json`.
    #[serde(default)]
    pub token: String,
    /// Files to open with optional 1-based line and column; canonicalised by
    /// `try_send` before sending.
    pub files: Vec<(PathBuf, Option<usize>, Option<usize>)>,
    /// Folders to add as workspace roots; canonicalised by `try_send`.
    pub folders: Vec<PathBuf>,
    /// Two files (old, new) to compare.
    pub diff: Option<(PathBuf, PathBuf)>,
    /// Keep the connection open until every file in `files` has been closed.
    pub wait: bool,
}

/// Cap on the request line a client may send, so a misbehaving local
/// process cannot make the listener buffer unbounded data.
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

/// Path of `instance.json` in the per-user runtime directory.
fn instance_file() -> PathBuf {
    apex_project::paths::runtime_dir().join("instance.json")
}

/// Clients waiting for their files to close: (remaining paths, connection).
static WAITERS: Mutex<Vec<(Vec<PathBuf>, TcpStream)>> = Mutex::new(Vec::new());

/// Canonical form of `p` so client and editor compare the same path
/// spelling; falls back to `p` unchanged if it does not exist or cannot be
/// resolved.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Hand `req` to a running instance. `Ok(true)` when it accepted the request
/// (and, with `wait`, when the files have since been closed); `Ok(false)` when
/// no instance is running.
///
/// An unreadable `instance.json` or a refused connection is treated as a
/// stale file and deleted. Fills in `req.token` and canonicalises the paths.
/// Errors are I/O failures reading the file or talking to the instance (each
/// step has a 5 s timeout, except the `--wait` read, which has none).
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

/// Start listening for other invocations. Returns the instance file path.
///
/// Binds an ephemeral loopback port, writes `instance.json` atomically after
/// making the runtime directory owner-only, and spawns an `apex-ipc` thread
/// that handles each connection on its own thread. The listener lives for
/// the rest of the process.
pub fn serve() -> std::io::Result<PathBuf> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let token = apex_project::token::random_token().ok_or_else(|| std::io::Error::other("no OS random source for the instance token"))?;
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

/// Serve one connection: read a single JSON request line (at most
/// `MAX_REQUEST_BYTES`, 5 s timeout), check the token, forward the request to
/// the editor's inbox and reply `ok`. With `wait`, the stream is parked in
/// `WAITERS` until `view_closed` or `shutdown` writes `done`. Any failure
/// closes the connection without a reply.
fn handle(stream: TcpStream, token: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let Ok(req) = serde_json::from_str::<Request>(&line) else { return };
    // Constant-time comparison: a wrong token must look like a dropped connection.
    if !apex_project::token::tokens_equal(&req.token, token) {
        return;
    }
    // Unwrap `BufReader` and `Take` to write the reply on the same socket.
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
///
/// Removes `path` (canonicalised) from every waiter and sends `done` to, then
/// drops, each waiter with nothing left to wait for.
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
///
/// Called once the window has closed. Waiters get `done` so their
/// `--wait` invocations exit.
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

    /// Pins down that a wrong token gets no reply and no request, that a valid
    /// `--wait` request reaches the inbox and returns once the file closes, and
    /// that shutdown removes the instance file.
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
