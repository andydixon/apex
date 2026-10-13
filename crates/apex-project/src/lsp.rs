//! A compiled-in Language Server Protocol client (JSON-RPC over stdio).
//!
//! Scope: initialise/shutdown, full-document synchronisation, diagnostics,
//! completion, hover, definition/declaration/implementation/references,
//! document and workspace symbols, signature help, rename, formatting and
//! code actions. Semantic tokens and inlay hints are not requested (documented
//! limitation). Positions are converted to UTF-16 by the caller using
//! `apex_buffer::position`.
//!
//! The server process is only started by explicit configuration and workspace
//! trust; nothing is downloaded or executed automatically.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How to launch one language server and which Apex languages it serves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ServerConfig {
    /// Program and arguments.
    pub command: Vec<String>,
    /// Language ids this server handles (Apex ids: "rust", "python", …).
    pub languages: Vec<String>,
    /// Sent as `initializationOptions` in the `initialize` request.
    pub initialization_options: Option<Value>,
    /// Sent via `workspace/didChangeConfiguration` once initialised, and returned for `workspace/configuration` requests.
    pub settings: Option<Value>,
    /// Extra environment variables for the server; `LD_*` and `DYLD_*` keys are ignored by `LspClient::start`.
    pub env: HashMap<String, String>,
}

/// Well-known servers offered when found on PATH (never downloaded).
pub fn known_servers() -> Vec<ServerConfig> {
    // `PATH` is not checked here; callers filter with `program_available`.
    let mk = |cmd: &[&str], langs: &[&str]| ServerConfig {
        command: cmd.iter().map(|s| s.to_string()).collect(),
        languages: langs.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    vec![
        mk(&["rust-analyzer"], &["rust"]),
        mk(&["pyright-langserver", "--stdio"], &["python"]),
        mk(&["pylsp"], &["python"]),
        mk(&["typescript-language-server", "--stdio"], &["typescript", "tsx", "javascript"]),
        mk(&["gopls"], &["go"]),
        mk(&["clangd"], &["c", "cpp"]),
        mk(&["lua-language-server"], &["lua"]),
        mk(&["bash-language-server", "start"], &["bash"]),
        mk(&["vscode-json-language-server", "--stdio"], &["json", "jsonc"]),
        mk(&["vscode-css-language-server", "--stdio"], &["css", "scss"]),
        mk(&["vscode-html-language-server", "--stdio"], &["html"]),
        mk(&["yaml-language-server", "--stdio"], &["yaml"]),
        mk(&["marksman", "server"], &["markdown"]),
        mk(&["taplo", "lsp", "stdio"], &["toml"]),
        mk(&["jdtls"], &["java"]),
        mk(&["kotlin-language-server"], &["kotlin"]),
        mk(&["sourcekit-lsp"], &["swift"]),
        mk(&["intelephense", "--stdio"], &["php"]),
        mk(&["solargraph", "stdio"], &["ruby"]),
        mk(&["omnisharp", "-lsp"], &["csharp"]),
    ]
}

/// True if the program can be found on PATH (or is an absolute path that exists).
pub fn program_available(program: &str) -> bool {
    let p = Path::new(program);
    // Absolute paths bypass the `PATH` search.
    if p.is_absolute() {
        return p.exists();
    }
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        // On Windows the bare name may lack an executable extension.
        candidate.is_file() || (cfg!(windows) && ["exe", "cmd", "bat"].iter().any(|e| candidate.with_extension(e).is_file()))
    })
}

// ----- protocol types ---------------------------------------------------------

/// LSP position: zero-based line and UTF-16 code-unit offset within the line.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// LSP half-open range from `start` to `end`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A range within a document identified by URI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Location {
    /// `file://` URI as sent by the server (see `path_for`).
    pub uri: String,
    pub range: Range,
}

/// One diagnostic from `textDocument/publishDiagnostics`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub range: Range,
    /// LSP severity: 1 error, 2 warning, 3 information, 4 hint; `None` if omitted.
    #[serde(default)]
    pub severity: Option<u8>,
    /// Server-specific diagnostic code (number or string).
    #[serde(default)]
    pub code: Option<Value>,
    /// Name of the tool that produced the diagnostic, e.g. `rustc`.
    #[serde(default)]
    pub source: Option<String>,
    pub message: String,
}

/// Replace `range` with `new_text`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// One completion candidate; only the fields Apex uses are decoded.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItem {
    pub label: String,
    /// LSP `CompletionItemKind` number; see `kind_label`.
    #[serde(default)]
    pub kind: Option<u8>,
    #[serde(default)]
    pub detail: Option<String>,
    /// Markdown/plain string or `MarkupContent`, kept raw.
    #[serde(default)]
    pub documentation: Option<Value>,
    #[serde(default)]
    pub insert_text: Option<String>,
    /// 2 means `insert_text` is a snippet; 1 or `None` means plain text.
    #[serde(default)]
    pub insert_text_format: Option<u8>,
    /// Raw `TextEdit` or `InsertReplaceEdit`; decoded by `insertion`.
    #[serde(default)]
    pub text_edit: Option<Value>,
    /// Edits elsewhere in the document (for example auto-imports).
    #[serde(default)]
    pub additional_text_edits: Vec<TextEdit>,
    #[serde(default)]
    pub sort_text: Option<String>,
    #[serde(default)]
    pub filter_text: Option<String>,
}

impl CompletionItem {
    /// Text to insert and, if the server gave a text edit, its range.
    pub fn insertion(&self) -> (String, Option<Range>) {
        if let Some(te) = &self.text_edit {
            let new_text = te.get("newText").and_then(|v| v.as_str()).unwrap_or(&self.label).to_string();
            // `InsertReplaceEdit` has no `range`; use its `replace` range.
            let range = te.get("range").or_else(|| te.get("replace")).and_then(|r| serde_json::from_value::<Range>(r.clone()).ok());
            return (new_text, range);
        }
        (self.insert_text.clone().unwrap_or_else(|| self.label.clone()), None)
    }

    /// Short human-readable name for the LSP `CompletionItemKind`, or `""` if unknown.
    pub fn kind_label(&self) -> &'static str {
        match self.kind.unwrap_or(0) {
            1 => "text",
            2 => "method",
            3 => "function",
            4 => "constructor",
            5 => "field",
            6 => "variable",
            7 => "class",
            8 => "interface",
            9 => "module",
            10 => "property",
            11 => "unit",
            12 => "value",
            13 => "enum",
            14 => "keyword",
            15 => "snippet",
            16 => "colour",
            17 => "file",
            18 => "reference",
            19 => "folder",
            20 => "enum member",
            21 => "constant",
            22 => "struct",
            23 => "event",
            24 => "operator",
            25 => "type parameter",
            _ => "",
        }
    }
}

/// Flattened document or workspace symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolInfo {
    /// Symbol name.
    pub name: String,
    /// LSP `SymbolKind` number; see `symbol_kind_label`.
    pub kind: u8,
    pub uri: String,
    /// Selection range for document symbols; full location range for workspace symbols.
    pub range: Range,
    /// Enclosing container name (workspace symbols only).
    pub container: Option<String>,
    /// Nesting level in a hierarchical document-symbol tree; 0 at top level.
    pub depth: u16,
}

/// Short human-readable name for an LSP `SymbolKind` number; `"symbol"` if unknown.
pub fn symbol_kind_label(kind: u8) -> &'static str {
    match kind {
        1 => "file",
        2 => "module",
        3 => "namespace",
        4 => "package",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "constructor",
        10 => "enum",
        11 => "interface",
        12 => "fn",
        13 => "variable",
        14 => "const",
        15 => "string",
        16 => "number",
        17 => "boolean",
        18 => "array",
        19 => "object",
        20 => "key",
        21 => "null",
        22 => "enum member",
        23 => "struct",
        24 => "event",
        25 => "operator",
        26 => "type parameter",
        _ => "symbol",
    }
}

/// A server-proposed multi-file edit, normalised from either `changes` or `documentChanges`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorkspaceEdit {
    /// uri → edits (document changes flattened in; version checks are up to the caller).
    pub changes: HashMap<String, Vec<TextEdit>>,
    /// Edits with a required document version, when the server supplied one.
    pub versions: HashMap<String, i64>,
    /// Resource operations (create/rename/delete) are reported, not applied.
    pub unsupported_operations: Vec<String>,
}

impl WorkspaceEdit {
    /// Decode an LSP `WorkspaceEdit`; malformed entries are skipped rather than failing the whole edit.
    pub fn parse(v: &Value) -> Self {
        let mut out = WorkspaceEdit::default();
        if let Some(changes) = v.get("changes").and_then(|c| c.as_object()) {
            for (uri, edits) in changes {
                if let Ok(edits) = serde_json::from_value::<Vec<TextEdit>>(edits.clone()) {
                    out.changes.entry(uri.clone()).or_default().extend(edits);
                }
            }
        }
        if let Some(dc) = v.get("documentChanges").and_then(|c| c.as_array()) {
            for change in dc {
                // Only resource operations (create/rename/delete) carry `kind`.
                if let Some(kind) = change.get("kind").and_then(|k| k.as_str()) {
                    out.unsupported_operations.push(kind.to_string());
                    continue;
                }
                let Some(uri) = change.pointer("/textDocument/uri").and_then(|u| u.as_str()) else { continue };
                if let Some(ver) = change.pointer("/textDocument/version").and_then(|v| v.as_i64()) {
                    out.versions.insert(uri.to_string(), ver);
                }
                if let Some(edits) = change.get("edits").and_then(|e| e.as_array()) {
                    for e in edits {
                        // Annotated edits carry the same fields plus annotationId.
                        if let Ok(te) = serde_json::from_value::<TextEdit>(e.clone()) {
                            out.changes.entry(uri.to_string()).or_default().push(te);
                        }
                    }
                }
            }
        }
        out
    }

    /// True when there are no text edits and no unsupported resource operations to report.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.unsupported_operations.is_empty()
    }
}

/// A code action or bare command offered by the server.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeAction {
    pub title: String,
    /// LSP `CodeActionKind`, e.g. `quickfix`.
    pub kind: Option<String>,
    pub edit: Option<WorkspaceEdit>,
    /// Raw `Command` to run via `workspace/executeCommand` (for a bare command, the whole item).
    pub command: Option<Value>,
    /// Server's hint that this is the preferred fix.
    pub is_preferred: bool,
}

// ----- transport -------------------------------------------------------------------

/// A decoded message from the server, produced by the reader thread.
#[derive(Debug)]
pub enum Incoming {
    /// Reply to one of our requests; `id` matches `LspClient::request`.
    Response {
        id: i64,
        result: Option<Value>,
        error: Option<Value>,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// Server-to-client request that needs a reply with the same `id`.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// The server's stdout closed, failed, or sent an oversized message.
    Exited,
}

/// Reader thread body: parses `Content-Length`-framed JSON-RPC messages from `stdout` and sends them on `tx`.
/// Sends `Incoming::Exited` and returns on EOF, a read error, or a frame above 64 MiB; returns quietly if `tx` is closed.
fn reader_thread(stdout: impl Read + Send + 'static, tx: Sender<Incoming>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut content_length: Option<usize> = None;
        let mut line = String::new();
        // Headers.
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Incoming::Exited);
                    return;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = tx.send(Incoming::Exited);
                    return;
                }
            }
            let t = line.trim_end();
            if t.is_empty() {
                break;
            }
            if let Some(v) = t.strip_prefix("Content-Length:") {
                content_length = v.trim().parse().ok();
            }
        }
        // A header block without `Content-Length` cannot be framed; skip it.
        let Some(len) = content_length else { continue };
        // Cap the allocation so a misbehaving server cannot make Apex reserve unbounded memory.
        if len > 64 * 1024 * 1024 {
            tracing::error!("LSP message of {len} bytes exceeds the limit; disconnecting");
            let _ = tx.send(Incoming::Exited);
            return;
        }
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).is_err() {
            let _ = tx.send(Incoming::Exited);
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&body) else {
            tracing::warn!("LSP: ignoring malformed JSON message");
            continue;
        };
        let msg = if let Some(method) = v.get("method").and_then(|m| m.as_str()).map(str::to_string) {
            let params = v.get("params").cloned().unwrap_or(Value::Null);
            match v.get("id") {
                // A non-null `id` alongside `method` makes it a request needing a reply.
                Some(id) if !id.is_null() => Incoming::Request { id: id.clone(), method, params },
                _ => Incoming::Notification { method, params },
            }
        } else if let Some(id) = v.get("id").and_then(|i| i.as_i64()) {
            // Apex only sends integer ids, so other ids cannot match a pending request.
            Incoming::Response { id, result: v.get("result").cloned(), error: v.get("error").cloned() }
        } else {
            continue;
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}

/// Writer thread body: frames each JSON body from `rx` with a `Content-Length` header and writes it to `stdin`.
/// Returns when the channel closes or a write fails (the server has gone).
fn writer_thread(mut stdin: impl Write + Send + 'static, rx: Receiver<String>) {
    for body in rx {
        if stdin.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes()).is_err()
            || stdin.write_all(body.as_bytes()).is_err()
            || stdin.flush().is_err()
        {
            return;
        }
    }
}

// ----- client ----------------------------------------------------------------------

/// Lifecycle of an `LspClient`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    /// `initialize` sent; document opens are queued until the reply.
    Starting,
    /// Initialised; requests and notifications flow normally.
    Running,
    /// `shutdown` sent; the process is killed if it does not answer within five seconds.
    ShuttingDown,
    /// Process has exited or been killed.
    Exited,
}

/// Why a request was sent, so the application can interpret the response.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestKind {
    Initialize,
    Shutdown,
    Completion {
        uri: String,
        version: i64,
    },
    Hover {
        uri: String,
    },
    Definition {
        uri: String,
    },
    Declaration {
        uri: String,
    },
    Implementation {
        uri: String,
    },
    References {
        uri: String,
    },
    DocumentSymbols {
        uri: String,
    },
    WorkspaceSymbols,
    SignatureHelp {
        uri: String,
    },
    Rename {
        uri: String,
    },
    Formatting {
        uri: String,
        version: i64,
    },
    CodeAction {
        uri: String,
    },
    ExecuteCommand,
    /// Any other method; not produced within this module.
    Other(String),
}

/// Events for the application.
#[derive(Debug)]
pub enum LspEvent {
    /// The `initialize` handshake completed and queued documents were opened.
    Ready,
    /// Diagnostics replace any previous set for `uri`.
    Diagnostics { uri: String, version: Option<i64>, diagnostics: Vec<Diagnostic> },
    /// Reply to a request other than `initialize`/`shutdown`; `result` is `Err` with the server's message on failure.
    Response { id: i64, kind: RequestKind, result: Result<Value, String> },
    /// Server asked to apply an edit; the application must answer with `respond_apply_edit`.
    ApplyEdit { id: Value, label: Option<String>, edit: WorkspaceEdit },
    /// `window/showMessage` (or `showMessageRequest`); `level` is the LSP `MessageType`, 1 error to 4 log.
    Message { level: u8, text: String },
    /// `window/logMessage` text, truncated to 500 characters.
    Log(String),
    /// `$/progress` title or message; empty when a progress sequence ends.
    Progress(String),
    /// The server stopped; `code` is its exit code when known.
    Exited { code: Option<i32> },
}

/// One running language-server process and its JSON-RPC session.
pub struct LspClient {
    /// Configuration the server was started with.
    pub config: ServerConfig,
    /// Workspace root; the server's working directory and `rootUri`.
    pub root: PathBuf,
    child: Child,
    /// Outgoing JSON bodies for the writer thread (bounded; see `send`).
    out: Sender<String>,
    /// Decoded messages from the reader thread.
    incoming: Receiver<Incoming>,
    /// Next JSON-RPC request id; ids start at 1 and are never reused within a session.
    next_id: i64,
    /// Outstanding requests by id, so responses can be routed by purpose.
    pending: HashMap<i64, RequestKind>,
    pub state: ClientState,
    /// `capabilities` from the `initialize` result; `Value::Null` until then.
    pub capabilities: Value,
    /// `serverInfo.name` from the `initialize` result, or empty.
    pub server_name: String,
    /// Open document URIs and their last sent version.
    open: HashMap<String, i64>,
    /// `(uri, language_id, text)` opened before initialisation finished; replayed on `Running`.
    queued_opens: Vec<(String, String, String)>,
    /// When `shutdown` was sent, for the kill timeout in `poll`.
    shutdown_sent: Option<Instant>,
    /// When the process was spawned.
    pub started: Instant,
    /// Restart counter; set to 0 here and not updated within this module.
    pub restarts: u32,
    /// Last 20 stderr lines (each up to 300 characters), filled by a background thread.
    stderr_tail: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
}

/// Convert a filesystem path to a `file://` URI, percent-encoding every byte outside the unreserved set plus `/` and `:`.
pub fn uri_for(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !p.starts_with('/') {
        out.push('/');
    }
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Inverse of `uri_for`: decode a `file://` URI to a path.
/// Returns `None` for other schemes, truncated or invalid percent escapes, or non-UTF-8 results.
pub fn path_for(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut it = rest.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let h = it.next()?;
            let l = it.next()?;
            let v = u8::from_str_radix(&format!("{}{}", h as char, l as char), 16).ok()?;
            bytes.push(v);
        } else {
            bytes.push(b);
        }
    }
    let s = String::from_utf8(bytes).ok()?;
    #[cfg(windows)]
    let s = s.strip_prefix('/').map(|x| x.replace('/', "\\")).unwrap_or(s);
    Some(PathBuf::from(s))
}

impl LspClient {
    /// Spawn the server in `root`, start its reader, writer and stderr threads, and send `initialize`.
    /// Errors if the command is empty, the process cannot be spawned, or a thread cannot be created.
    pub fn start(config: ServerConfig, root: &Path) -> Result<Self, String> {
        if config.command.is_empty() {
            return Err("no command".into());
        }
        let mut cmd = Command::new(&config.command[0]);
        cmd.args(&config.command[1..]).current_dir(root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in &config.env {
            // Skip loader variables so a project configuration cannot inject libraries into the server.
            if k.starts_with("LD_") || k.starts_with("DYLD_") {
                continue;
            }
            cmd.env(k, v);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Own process group so terminal signals aimed at Apex do not reach the server.
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().map_err(|e| format!("could not start {:?}: {e}", config.command[0]))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr_tail = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            std::thread::Builder::new()
                .name("apex-lsp-stderr".into())
                .spawn(move || {
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                        // Keep only a bounded tail so a chatty server cannot grow memory without limit.
                        if t.len() >= 20 {
                            t.pop_front();
                        }
                        t.push_back(line.chars().take(300).collect());
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        let (in_tx, in_rx) = unbounded();
        let (out_tx, out_rx) = bounded::<String>(1024);
        std::thread::Builder::new().name("apex-lsp-read".into()).spawn(move || reader_thread(stdout, in_tx)).map_err(|e| e.to_string())?;
        std::thread::Builder::new().name("apex-lsp-write".into()).spawn(move || writer_thread(stdin, out_rx)).map_err(|e| e.to_string())?;
        let mut c = Self {
            config,
            root: root.to_path_buf(),
            child,
            out: out_tx,
            incoming: in_rx,
            next_id: 1,
            pending: HashMap::new(),
            state: ClientState::Starting,
            capabilities: Value::Null,
            server_name: String::new(),
            open: HashMap::new(),
            queued_opens: Vec::new(),
            shutdown_sent: None,
            started: Instant::now(),
            restarts: 0,
            stderr_tail,
        };
        c.send_initialize();
        Ok(c)
    }

    /// Send the `initialize` request advertising the client capabilities Apex implements.
    fn send_initialize(&mut self) {
        let root_uri = uri_for(&self.root);
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "Apex", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}],
            "initializationOptions": self.config.initialization_options,
            "capabilities": {
                "textDocument": {
                    "synchronization": {"dynamicRegistration": false, "didSave": true},
                    "completion": {"completionItem": {"snippetSupport": true, "documentationFormat": ["markdown", "plaintext"]}, "contextSupport": false},
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "signatureHelp": {"signatureInformation": {"documentationFormat": ["markdown", "plaintext"], "parameterInformation": {"labelOffsetSupport": true}}},
                    "definition": {}, "declaration": {}, "implementation": {}, "references": {},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "rename": {"prepareSupport": false},
                    "formatting": {}, "rangeFormatting": {},
                    "codeAction": {"codeActionLiteralSupport": {"codeActionKind": {"valueSet": ["quickfix", "refactor", "source"]}}},
                    "publishDiagnostics": {"relatedInformation": false}
                },
                "workspace": {"applyEdit": true, "workspaceEdit": {"documentChanges": true}, "symbol": {}, "configuration": false, "workspaceFolders": true},
                "window": {"workDoneProgress": true}
            }
        });
        self.request(RequestKind::Initialize, "initialize", params);
    }

    /// Serialise and queue a message for the writer thread.
    fn send(&self, body: Value) {
        if let Ok(s) = serde_json::to_string(&body) {
            // Non-blocking: if the queue is full or the writer has gone, the message is dropped rather than stalling the UI.
            let _ = self.out.try_send(s);
        }
    }

    /// Send a JSON-RPC notification (no reply expected).
    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Send a request, remembering `kind` so `poll` can route the reply; returns the request id.
    pub fn request(&mut self, kind: RequestKind, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        tracing::debug!("LSP -> {method} #{id}");
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Ask the server to cancel request `id` (`$/cancelRequest`); its reply may still arrive.
    pub fn cancel(&self, id: i64) {
        self.notify("$/cancelRequest", json!({"id": id}));
    }

    /// Reply successfully to a server-to-client request.
    pub fn respond(&self, id: Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    /// Reply with a JSON-RPC error to a server-to-client request.
    pub fn respond_error(&self, id: Value, code: i64, message: &str) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
    }

    /// Answer a `workspace/applyEdit` request (see `LspEvent::ApplyEdit`).
    pub fn respond_apply_edit(&self, id: Value, applied: bool, reason: Option<&str>) {
        self.respond(id, json!({"applied": applied, "failureReason": reason}));
    }

    /// True once initialisation has finished and before shutdown.
    pub fn is_running(&self) -> bool {
        self.state == ClientState::Running
    }

    /// The last lines the server wrote to stderr (for failure reports).
    pub fn stderr_tail(&self) -> String {
        self.stderr_tail.lock().map(|t| t.iter().cloned().collect::<Vec<_>>().join("\n")).unwrap_or_default()
    }

    /// True if the server advertised `capability` (present and not `null` or `false`).
    pub fn supports(&self, capability: &str) -> bool {
        !matches!(self.capabilities.get(capability), None | Some(Value::Null) | Some(Value::Bool(false)))
    }

    // ----- document sync (full) --------------------------------------------------

    /// Send `textDocument/didOpen` at version 1, or queue it until the server is running.
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) {
        if self.state != ClientState::Running {
            self.queued_opens.push((uri.to_string(), language_id.to_string(), text.to_string()));
            return;
        }
        self.open.insert(uri.to_string(), 1);
        self.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": language_id, "version": 1, "text": text}}));
    }

    /// Send the full new text with an incremented version; returns that version.
    /// Returns `None` if the server is not running or `uri` is not open.
    pub fn did_change(&mut self, uri: &str, text: &str) -> Option<i64> {
        if self.state != ClientState::Running {
            return None;
        }
        let v = self.open.get_mut(uri)?;
        *v += 1;
        let version = *v;
        self.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]}));
        Some(version)
    }

    /// Send `textDocument/didSave` if `uri` is open.
    pub fn did_save(&self, uri: &str) {
        if self.open.contains_key(uri) {
            self.notify("textDocument/didSave", json!({"textDocument": {"uri": uri}}));
        }
    }

    /// Send `textDocument/didClose` if `uri` is open, and forget its version.
    pub fn did_close(&mut self, uri: &str) {
        if self.open.remove(uri).is_some() {
            self.notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}));
        }
    }

    /// True if `uri` has been opened with this server.
    pub fn is_open(&self, uri: &str) -> bool {
        self.open.contains_key(uri)
    }

    /// Last version sent for `uri`, if open.
    pub fn version(&self, uri: &str) -> Option<i64> {
        self.open.get(uri).copied()
    }

    // ----- requests ------------------------------------------------------------------

    /// `TextDocumentPositionParams` for `uri` at `pos`.
    fn td_pos(uri: &str, pos: Position) -> Value {
        json!({"textDocument": {"uri": uri}, "position": pos})
    }

    /// Request completions at `pos`; `trigger` is the typed trigger character, if any. Returns the request id.
    /// The document version is recorded so stale replies can be discarded.
    pub fn completion(&mut self, uri: &str, pos: Position, trigger: Option<char>) -> i64 {
        let version = self.version(uri).unwrap_or(0);
        let mut params = Self::td_pos(uri, pos);
        if let Some(c) = trigger {
            params["context"] = json!({"triggerKind": 2, "triggerCharacter": c.to_string()});
        } else {
            params["context"] = json!({"triggerKind": 1});
        }
        self.request(RequestKind::Completion { uri: uri.to_string(), version }, "textDocument/completion", params)
    }

    /// Request hover information at `pos`; returns the request id.
    pub fn hover(&mut self, uri: &str, pos: Position) -> i64 {
        self.request(RequestKind::Hover { uri: uri.to_string() }, "textDocument/hover", Self::td_pos(uri, pos))
    }

    /// Request go-to-definition at `pos`; returns the request id.
    pub fn definition(&mut self, uri: &str, pos: Position) -> i64 {
        self.request(RequestKind::Definition { uri: uri.to_string() }, "textDocument/definition", Self::td_pos(uri, pos))
    }

    /// Request go-to-declaration at `pos`; returns the request id.
    pub fn declaration(&mut self, uri: &str, pos: Position) -> i64 {
        self.request(RequestKind::Declaration { uri: uri.to_string() }, "textDocument/declaration", Self::td_pos(uri, pos))
    }

    /// Request go-to-implementation at `pos`; returns the request id.
    pub fn implementation(&mut self, uri: &str, pos: Position) -> i64 {
        self.request(RequestKind::Implementation { uri: uri.to_string() }, "textDocument/implementation", Self::td_pos(uri, pos))
    }

    /// Request all references at `pos`, including the declaration; returns the request id.
    pub fn references(&mut self, uri: &str, pos: Position) -> i64 {
        let mut p = Self::td_pos(uri, pos);
        p["context"] = json!({"includeDeclaration": true});
        self.request(RequestKind::References { uri: uri.to_string() }, "textDocument/references", p)
    }

    /// Request the document's symbols; returns the request id.
    pub fn document_symbols(&mut self, uri: &str) -> i64 {
        self.request(RequestKind::DocumentSymbols { uri: uri.to_string() }, "textDocument/documentSymbol", json!({"textDocument": {"uri": uri}}))
    }

    /// Request workspace symbols matching `query`; returns the request id.
    pub fn workspace_symbols(&mut self, query: &str) -> i64 {
        self.request(RequestKind::WorkspaceSymbols, "workspace/symbol", json!({"query": query}))
    }

    /// Request signature help at `pos`; returns the request id.
    pub fn signature_help(&mut self, uri: &str, pos: Position) -> i64 {
        self.request(RequestKind::SignatureHelp { uri: uri.to_string() }, "textDocument/signatureHelp", Self::td_pos(uri, pos))
    }

    /// Request a rename of the symbol at `pos` to `new_name`; returns the request id.
    pub fn rename(&mut self, uri: &str, pos: Position, new_name: &str) -> i64 {
        let mut p = Self::td_pos(uri, pos);
        p["newName"] = json!(new_name);
        self.request(RequestKind::Rename { uri: uri.to_string() }, "textDocument/rename", p)
    }

    /// Request whole-document formatting; returns the request id.
    /// The document version is recorded so edits for a changed document can be discarded.
    pub fn formatting(&mut self, uri: &str, tab_size: u32, insert_spaces: bool) -> i64 {
        let version = self.version(uri).unwrap_or(0);
        self.request(
            RequestKind::Formatting { uri: uri.to_string(), version },
            "textDocument/formatting",
            json!({"textDocument": {"uri": uri}, "options": {"tabSize": tab_size, "insertSpaces": insert_spaces, "trimTrailingWhitespace": true}}),
        )
    }

    /// Request code actions for `range` with the diagnostics that overlap it; returns the request id.
    pub fn code_actions(&mut self, uri: &str, range: Range, diagnostics: &[Diagnostic]) -> i64 {
        self.request(
            RequestKind::CodeAction { uri: uri.to_string() },
            "textDocument/codeAction",
            json!({"textDocument": {"uri": uri}, "range": range, "context": {"diagnostics": diagnostics}}),
        )
    }

    /// Ask the server to run `command` (`workspace/executeCommand`); returns the request id.
    pub fn execute_command(&mut self, command: &str, arguments: Value) -> i64 {
        self.request(RequestKind::ExecuteCommand, "workspace/executeCommand", json!({"command": command, "arguments": arguments}))
    }

    // ----- lifecycle --------------------------------------------------------------------

    /// Begin a graceful shutdown by sending `shutdown`; idempotent.
    /// `poll` sends `exit` on the reply, or kills the process after five seconds.
    pub fn shutdown(&mut self) {
        if matches!(self.state, ClientState::ShuttingDown | ClientState::Exited) {
            return;
        }
        self.state = ClientState::ShuttingDown;
        self.shutdown_sent = Some(Instant::now());
        self.request(RequestKind::Shutdown, "shutdown", Value::Null);
    }

    /// Kill the process if it ignores shutdown.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.state = ClientState::Exited;
    }

    /// Drain incoming messages into application events.
    /// Non-blocking. Also answers some server requests directly and may kill the process on shutdown timeout.
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut events = Vec::new();
        if self.state == ClientState::ShuttingDown && self.shutdown_sent.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) {
            self.kill();
            events.push(LspEvent::Exited { code: None });
            return events;
        }
        while let Ok(msg) = self.incoming.try_recv() {
            match msg {
                Incoming::Exited => {
                    // Give the stderr reader a moment to catch the final lines.
                    std::thread::sleep(Duration::from_millis(30));
                    let code = self.child.try_wait().ok().flatten().and_then(|s| s.code());
                    self.state = ClientState::Exited;
                    events.push(LspEvent::Exited { code });
                    break;
                }
                Incoming::Response { id, result, error } => {
                    tracing::debug!("LSP <- response #{id} ok={} err={}", result.is_some(), error.is_some());
                    // Replies to unknown ids (e.g. after cancellation) are dropped.
                    let Some(kind) = self.pending.remove(&id) else { continue };
                    match kind {
                        RequestKind::Initialize => {
                            if let Some(err) = error {
                                events.push(LspEvent::Message {
                                    level: 1,
                                    text: format!(
                                        "language server failed to initialise: {}",
                                        err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error")
                                    ),
                                });
                                self.shutdown();
                                continue;
                            }
                            let r = result.unwrap_or(Value::Null);
                            self.capabilities = r.get("capabilities").cloned().unwrap_or(Value::Null);
                            self.server_name = r.pointer("/serverInfo/name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                            // Handshake order matters: `initialized`, then configuration, then queued opens.
                            self.notify("initialized", json!({}));
                            if let Some(settings) = &self.config.settings {
                                self.notify("workspace/didChangeConfiguration", json!({"settings": settings}));
                            }
                            self.state = ClientState::Running;
                            for (uri, lang, text) in std::mem::take(&mut self.queued_opens) {
                                self.did_open(&uri, &lang, &text);
                            }
                            events.push(LspEvent::Ready);
                        }
                        RequestKind::Shutdown => {
                            self.notify("exit", Value::Null);
                            // Brief pause so the server can act on `exit` before it is killed.
                            std::thread::sleep(Duration::from_millis(50));
                            self.kill();
                            events.push(LspEvent::Exited { code: None });
                        }
                        kind => {
                            let res = match (result, error) {
                                (_, Some(e)) => Err(e.get("message").and_then(|m| m.as_str()).unwrap_or("request failed").to_string()),
                                (Some(r), None) => Ok(r),
                                (None, None) => Ok(Value::Null),
                            };
                            events.push(LspEvent::Response { id, kind, result: res });
                        }
                    }
                }
                Incoming::Notification { method, params } => match method.as_str() {
                    "textDocument/publishDiagnostics" => {
                        let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("").to_string();
                        let version = params.get("version").and_then(|v| v.as_i64());
                        let diagnostics =
                            params.get("diagnostics").cloned().and_then(|d| serde_json::from_value::<Vec<Diagnostic>>(d).ok()).unwrap_or_default();
                        events.push(LspEvent::Diagnostics { uri, version, diagnostics });
                    }
                    "window/showMessage" => {
                        let level = params.get("type").and_then(|t| t.as_u64()).unwrap_or(3) as u8;
                        let text = params.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
                        events.push(LspEvent::Message { level, text });
                    }
                    "window/logMessage" => {
                        if let Some(m) = params.get("message").and_then(|m| m.as_str()) {
                            events.push(LspEvent::Log(m.chars().take(500).collect()));
                        }
                    }
                    "$/progress" => {
                        if let Some(title) = params.pointer("/value/title").or_else(|| params.pointer("/value/message")).and_then(|t| t.as_str()) {
                            let kind = params.pointer("/value/kind").and_then(|k| k.as_str()).unwrap_or("");
                            events.push(LspEvent::Progress(if kind == "end" { String::new() } else { title.to_string() }));
                        }
                    }
                    _ => {}
                },
                Incoming::Request { id, method, params } => match method.as_str() {
                    "workspace/applyEdit" => {
                        let label = params.get("label").and_then(|l| l.as_str()).map(str::to_string);
                        let edit = params.get("edit").map(WorkspaceEdit::parse).unwrap_or_default();
                        events.push(LspEvent::ApplyEdit { id, label, edit });
                    }
                    // Accepted but ignored: Apex does not use dynamic registration.
                    "window/workDoneProgress/create" | "client/registerCapability" | "client/unregisterCapability" => self.respond(id, Value::Null),
                    "workspace/configuration" => {
                        // Same settings object for every requested section.
                        let n = params.get("items").and_then(|i| i.as_array()).map(|a| a.len()).unwrap_or(1);
                        let settings = self.config.settings.clone().unwrap_or(Value::Null);
                        self.respond(id, Value::Array(vec![settings; n]));
                    }
                    "workspace/workspaceFolders" => {
                        let uri = uri_for(&self.root);
                        self.respond(id, json!([{"uri": uri, "name": self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()}]));
                    }
                    "window/showMessageRequest" => {
                        // Shown as information only; Apex offers no choice, so the reply is `null`.
                        let text = params.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
                        events.push(LspEvent::Message { level: 3, text });
                        self.respond(id, Value::Null);
                    }
                    // JSON-RPC "method not found".
                    _ => self.respond_error(id, -32601, "method not supported by Apex"),
                },
            }
        }
        events
    }
}

impl Drop for LspClient {
    /// Best-effort cleanup: send `exit` and kill the process so no server outlives its client.
    fn drop(&mut self) {
        if self.state != ClientState::Exited {
            self.notify("exit", Value::Null);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Extract readable text from a hover `contents` value (MarkupContent, MarkedString or arrays).
pub fn hover_text(contents: &Value) -> String {
    match contents {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(hover_text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n"),
        Value::Object(o) => {
            if let Some(v) = o.get("value").and_then(|v| v.as_str()) {
                match o.get("language").and_then(|l| l.as_str()) {
                    Some(lang) => format!("```{lang}\n{v}\n```"),
                    None => v.to_string(),
                }
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// Flatten `textDocument/documentSymbol` or `workspace/symbol` results.
pub fn parse_symbols(v: &Value, default_uri: &str) -> Vec<SymbolInfo> {
    let mut out = Vec::new();
    fn walk(v: &Value, uri: &str, depth: u16, out: &mut Vec<SymbolInfo>) {
        let Some(items) = v.as_array() else { return };
        for item in items {
            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let kind = item.get("kind").and_then(|k| k.as_u64()).unwrap_or(0) as u8;
            if let Some(loc) = item.get("location") {
                // SymbolInformation
                let u = loc.get("uri").and_then(|u| u.as_str()).unwrap_or(uri).to_string();
                let range = loc.get("range").and_then(|r| serde_json::from_value::<Range>(r.clone()).ok()).unwrap_or_default();
                let container = item.get("containerName").and_then(|c| c.as_str()).map(str::to_string);
                out.push(SymbolInfo { name, kind, uri: u, range, container, depth });
            } else {
                // DocumentSymbol
                let range =
                    item.get("selectionRange").or_else(|| item.get("range")).and_then(|r| serde_json::from_value::<Range>(r.clone()).ok()).unwrap_or_default();
                out.push(SymbolInfo { name, kind, uri: uri.to_string(), range, container: None, depth });
                if let Some(children) = item.get("children") {
                    walk(children, uri, depth + 1, out);
                }
            }
        }
    }
    walk(v, default_uri, 0, &mut out);
    out
}

/// Parse a definition/references result into locations (handles Location, Location[], LocationLink[]).
pub fn parse_locations(v: &Value) -> Vec<Location> {
    let one = |item: &Value| -> Option<Location> {
        if let Some(uri) = item.get("targetUri").and_then(|u| u.as_str()) {
            let range = item.get("targetSelectionRange").or_else(|| item.get("targetRange")).and_then(|r| serde_json::from_value::<Range>(r.clone()).ok())?;
            return Some(Location { uri: uri.to_string(), range });
        }
        serde_json::from_value::<Location>(item.clone()).ok()
    };
    match v {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        Value::Null => Vec::new(),
        other => one(other).into_iter().collect(),
    }
}

/// Decode a `textDocument/codeAction` result into `CodeAction`s; items without a title are skipped.
pub fn parse_code_actions(v: &Value) -> Vec<CodeAction> {
    let Some(items) = v.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| {
            let title = item.get("title").and_then(|t| t.as_str())?.to_string();
            if item.get("edit").is_none() && item.get("command").is_some() && item.get("kind").is_none() && item.get("arguments").is_some() {
                // A bare Command.
                return Some(CodeAction { title, kind: None, edit: None, command: Some(item.clone()), is_preferred: false });
            }
            Some(CodeAction {
                title,
                kind: item.get("kind").and_then(|k| k.as_str()).map(str::to_string),
                edit: item.get("edit").map(WorkspaceEdit::parse),
                command: item.get("command").cloned(),
                is_preferred: item.get("isPreferred").and_then(|p| p.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trip() {
        let p = Path::new("/home/me/my project/ü.rs");
        let u = uri_for(p);
        assert_eq!(u, "file:///home/me/my%20project/%C3%BC.rs");
        assert_eq!(path_for(&u).unwrap(), p);
        assert!(path_for("http://x").is_none());
    }

    #[test]
    fn parses_workspace_edit_forms() {
        let v = json!({"changes": {"file:///a.rs": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "newText": "x"}]}});
        let e = WorkspaceEdit::parse(&v);
        assert_eq!(e.changes["file:///a.rs"].len(), 1);
        let v = json!({"documentChanges": [
            {"textDocument": {"uri": "file:///b.rs", "version": 7}, "edits": [{"range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0}}, "newText": "y"}]},
            {"kind": "create", "uri": "file:///c.rs"}
        ]});
        let e = WorkspaceEdit::parse(&v);
        assert_eq!(e.versions["file:///b.rs"], 7);
        assert_eq!(e.unsupported_operations, vec!["create"]);
    }

    #[test]
    fn parses_symbols_locations_hover() {
        let v = json!([{"name": "S", "kind": 23, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 0}}, "selectionRange": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 8}}, "children": [{"name": "m", "kind": 6, "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 5}}}]}]);
        let s = parse_symbols(&v, "file:///x");
        assert_eq!(s.len(), 2);
        assert_eq!((s[1].name.as_str(), s[1].depth), ("m", 1));
        let locs = parse_locations(
            &json!([{"targetUri": "file:///y", "targetRange": {"start": {"line": 3, "character": 1}, "end": {"line": 3, "character": 4}}, "targetSelectionRange": {"start": {"line": 3, "character": 2}, "end": {"line": 3, "character": 3}}}]),
        );
        assert_eq!(locs[0].range.start.character, 2);
        assert_eq!(hover_text(&json!({"kind": "markdown", "value": "**x**"})), "**x**");
        assert_eq!(hover_text(&json!([{"language": "rust", "value": "fn a()"}, "doc"])), "```rust\nfn a()\n```\n\ndoc");
    }

    /// Drives the client against a fake server implemented with a shell pipeline is
    /// fragile; instead, exercise the framing with an in-process echo of the protocol.
    #[cfg(unix)]
    #[test]
    fn talks_to_a_minimal_server() {
        // A tiny LSP "server" in Python that answers initialize, publishes a diagnostic
        // on didOpen and handles shutdown. Skipped when python3 is unavailable.
        if !program_available("python3") {
            return;
        }
        let script = r#"
import sys, json
def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line: return None
        line = line.decode().strip()
        if not line: break
        k, v = line.split(':', 1); headers[k.strip()] = v.strip()
    n = int(headers['Content-Length'])
    return json.loads(sys.stdin.buffer.read(n))
def send(msg):
    b = json.dumps(msg).encode()
    sys.stdout.buffer.write(b'Content-Length: %d\r\n\r\n' % len(b) + b); sys.stdout.buffer.flush()
while True:
    m = read()
    if m is None: break
    if m.get('method') == 'initialize':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'capabilities': {'hoverProvider': True}, 'serverInfo': {'name': 'fake'}}})
    elif m.get('method') == 'textDocument/didOpen':
        uri = m['params']['textDocument']['uri']
        send({'jsonrpc': '2.0', 'method': 'textDocument/publishDiagnostics', 'params': {'uri': uri, 'diagnostics': [{'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 2}}, 'severity': 1, 'message': 'bad'}]}})
    elif m.get('method') == 'textDocument/hover':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': {'contents': 'hovered'}})
    elif m.get('method') == 'shutdown':
        send({'jsonrpc': '2.0', 'id': m['id'], 'result': None})
    elif m.get('method') == 'exit':
        break
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake_lsp.py");
        std::fs::write(&path, script).unwrap();
        let cfg = ServerConfig { command: vec!["python3".into(), path.display().to_string()], languages: vec!["rust".into()], ..Default::default() };
        let mut c = LspClient::start(cfg, dir.path()).unwrap();
        c.did_open("file:///x.rs", "rust", "ab");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ready = false;
        let mut diags = None;
        let mut hover = None;
        let mut hover_sent = false;
        while Instant::now() < deadline && (diags.is_none() || hover.is_none()) {
            for ev in c.poll() {
                match ev {
                    LspEvent::Ready => ready = true,
                    LspEvent::Diagnostics { diagnostics, .. } => diags = Some(diagnostics),
                    LspEvent::Response { kind: RequestKind::Hover { .. }, result, .. } => hover = Some(result),
                    _ => {}
                }
            }
            if ready && !hover_sent {
                c.hover("file:///x.rs", Position { line: 0, character: 1 });
                hover_sent = true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready);
        assert!(c.supports("hoverProvider"));
        assert_eq!(c.server_name, "fake");
        assert_eq!(diags.unwrap()[0].message, "bad");
        assert_eq!(hover_text(hover.unwrap().unwrap().get("contents").unwrap()), "hovered");
        c.shutdown();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        while Instant::now() < deadline && !exited {
            for ev in c.poll() {
                if matches!(ev, LspEvent::Exited { .. }) {
                    exited = true;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(exited);
    }
}
