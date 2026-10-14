//! Editing files on other machines over SSH (`apex_project::remote`).
//!
//! A remote document has no local `path`; its host and remote path live in
//! `RemoteState::docs`, and its tab is titled `name (host)`. Every transfer
//! runs on a worker thread and reports back through `RemoteState::rx`, which
//! `remote_poll` drains each frame, so a slow or dead connection never
//! freezes the editor.
//!
//! Failures never open a dialog. A failed save leaves the edits in the
//! buffer (and in crash recovery), marks the tab offline and retries on its
//! own once the server answers again, unless the file changed there in the
//! meantime, in which case the user chooses: Save overwrites, Revert takes
//! the server's version. Every few seconds each remote file's checksum is
//! compared with the one last seen: clean buffers reload silently.
//!
//! Open and Save As use an in-app browser (`Dialog::RemoteBrowser`) that
//! lists folders on the server. Its Open Folder button adds the folder to
//! the REMOTE section at the top of the sidebar (`show_remote_folders`), a
//! tree listed over ssh as folders are expanded.
//!
//! Password and passphrase prompts from ssh appear as `Dialog::Askpass`
//! (over the browser, which comes back afterwards), but only for something
//! the user just did: background checks and automatic retries never ask. The
//! answer is kept in `RemoteState::credentials` (locked memory) for as long
//! as a file or folder from that server is open or the browser shows it, then wiped
//! and the shared connection closed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use apex_edit::{DocId, Document, ops};
use apex_project::remote::{Auth, Credentials};
use apex_project::secret::Secret;
use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::app::{ApexApp, Dialog};
use crate::view::ViewId;

/// How often each remote file is checked for changes on the server.
const CHECK_EVERY: Duration = Duration::from_secs(5);

/// Where a remote document lives and what is known about the copy there.
pub(crate) struct RemoteDoc {
    pub(crate) host: String,
    pub(crate) path: String,
    /// Checksum at the last read or write; `None` before the first save.
    stamp: Option<String>,
    /// The last transfer or check failed.
    offline: bool,
    /// A write is in flight.
    saving: bool,
    /// A save failed (or was asked for mid-write) and should run when possible.
    save_pending: bool,
    /// A stamp check is in flight.
    checking: bool,
    /// The user has been told the server copy changed under their edits.
    told_changed: bool,
}

/// A finished transfer, sent from a worker thread.
enum Reply {
    Opened {
        host: String,
        path: String,
        result: Result<(Vec<u8>, String), String>,
    },
    Saved {
        doc: DocId,
        rev: u64,
        then: Option<Box<Dialog>>,
        result: Result<String, String>,
    },
    /// `force`: an explicit Revert, applied even over unsaved edits.
    Reloaded {
        doc: DocId,
        force: bool,
        encoding: Option<apex_buffer::Encoding>,
        result: Result<(Vec<u8>, String), String>,
    },
    Checked {
        doc: DocId,
        result: Result<String, String>,
    },
    Listed {
        result: Result<(String, Vec<String>), String>,
    },
    /// A folder in the sidebar's REMOTE section was listed.
    FolderListed {
        host: String,
        dir: String,
        result: Result<(String, Vec<String>), String>,
    },
    /// ssh wants a password for something the user started; the worker waits on `reply`.
    Prompt {
        host: String,
        prompt: String,
        reply: Sender<Option<Secret>>,
    },
}

pub struct RemoteState {
    pub(crate) docs: HashMap<DocId, RemoteDoc>,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
    /// Host and folder the browser last showed, to start there next time.
    last_host: String,
    last_dir: String,
    last_check: Option<Instant>,
    /// Passwords given this session, wiped once nothing uses their server.
    credentials: Credentials,
    /// Transfers started and not yet answered.
    inflight: usize,
    /// Folders shown in the sidebar's REMOTE section: host and tree.
    folders: Vec<(String, Node)>,
}

/// A file or folder in a remote folder tree.
pub(crate) struct Node {
    name: String,
    path: String,
    dir: bool,
    open: bool,
    loading: bool,
    /// Listed entries; `None` until the folder is first opened.
    kids: Option<Vec<Node>>,
    error: Option<String>,
}

impl Node {
    fn new(name: &str, path: String, dir: bool) -> Self {
        Self { name: name.to_string(), path, dir, open: false, loading: false, kids: None, error: None }
    }

    fn find_mut(&mut self, path: &str) -> Option<&mut Node> {
        if self.path == path {
            return Some(self);
        }
        self.kids.as_mut()?.iter_mut().find_map(|k| k.find_mut(path))
    }

    /// Replace the children with `entries` (folders end in `/`), keeping the
    /// state of those that were already there.
    fn set_kids(&mut self, entries: &[String]) {
        let mut old: HashMap<String, Node> = self.kids.take().unwrap_or_default().into_iter().map(|n| (n.path.clone(), n)).collect();
        let kids = entries
            .iter()
            .map(|e| {
                let (name, dir) = e.strip_suffix('/').map_or((e.as_str(), false), |d| (d, true));
                let path = join(&self.path, name);
                old.remove(&path).filter(|n| n.dir == dir).unwrap_or_else(|| Node::new(name, path, dir))
            })
            .collect();
        self.kids = Some(kids);
    }

    /// Paths of open folders whose contents are shown (for Refresh).
    fn open_dirs(&self, out: &mut Vec<String>) {
        if self.open && self.kids.is_some() {
            out.push(self.path.clone());
            self.kids.iter().flatten().for_each(|k| k.open_dirs(out));
        }
    }
}

/// What a click in the REMOTE section asks for.
enum FolderAct {
    List(String),
    Open(String),
    Refresh,
    Close,
}

impl Default for RemoteState {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self {
            docs: HashMap::new(),
            tx,
            rx,
            last_host: String::new(),
            last_dir: String::new(),
            last_check: None,
            credentials: Credentials::default(),
            inflight: 0,
            folders: Vec::new(),
        }
    }
}

/// The remote file browser's state (kept in `Dialog::RemoteBrowser`).
#[derive(Debug, Clone)]
pub(crate) struct Browser {
    host: String,
    /// Absolute folder being shown (as typed until the server answers).
    dir: String,
    entries: Vec<String>,
    loading: bool,
    error: Option<String>,
    /// Save mode: the view to save, the file name, and what to show afterwards.
    save: Option<(ViewId, Option<Box<Dialog>>)>,
    name: String,
    /// The chosen name exists; the next click replaces it.
    confirm_replace: bool,
    /// Put the caret in the host field on the first frame only.
    focus_host: bool,
}

/// `dir/name` without doubling the slash.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

/// The folder containing `path` (`/` for top-level entries).
fn parent(path: &str) -> String {
    match path.trim_end_matches('/').rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].to_string(),
    }
}

/// The final component of `path`.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl ApexApp {
    /// Run `job` on a worker thread and deliver its reply to `remote_poll`.
    /// Only an `interactive` job (one the user just started) may ask for a
    /// password; others use remembered ones or fail quietly.
    fn remote_spawn(&mut self, interactive: bool, job: impl FnOnce(&Auth) -> Reply + Send + 'static) {
        let tx = self.remote.tx.clone();
        let ctx = self.egui_ctx.clone();
        let prompter: Option<apex_project::remote::Prompter> = interactive.then(|| {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            std::sync::Arc::new(move |host: &str, prompt: &str| {
                let (reply, answer) = crossbeam_channel::bounded(1);
                tx.send(Reply::Prompt { host: host.to_string(), prompt: prompt.to_string(), reply }).ok()?;
                if let Some(c) = &ctx {
                    c.request_repaint();
                }
                answer.recv_timeout(Duration::from_secs(600)).ok().flatten()
            }) as apex_project::remote::Prompter
        });
        let auth = Auth { credentials: self.remote.credentials.clone(), prompter };
        self.remote.inflight += 1;
        std::thread::spawn(move || {
            let _ = tx.send(job(&auth));
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    /// Whether the view shows a remote document.
    pub(crate) fn is_remote_view(&self, vid: ViewId) -> bool {
        self.views.get(&vid).is_some_and(|v| self.remote.docs.contains_key(&v.doc))
    }

    /// The tab title for a remote document, flagging a lost connection.
    fn remote_retitle(&mut self, d: DocId) {
        let (Some(r), Some(doc)) = (self.remote.docs.get(&d), self.docs.get_mut(&d)) else { return };
        let flag = if r.offline { ", offline" } else { "" };
        doc.scratch_title = Some(format!("{} ({}{flag})", file_name(&r.path), r.host));
    }

    /// Show the remote browser: to open a file, or (with `save`) to save a view there.
    pub(crate) fn show_remote_browser(&mut self, save: Option<(ViewId, Option<Box<Dialog>>)>) {
        let (mut host, mut dir, mut name) = (self.remote.last_host.clone(), self.remote.last_dir.clone(), String::new());
        if let Some((vid, _)) = &save {
            let doc = self.views.get(vid).map(|v| v.doc);
            if let Some(r) = doc.and_then(|d| self.remote.docs.get(&d)) {
                (host, dir, name) = (r.host.clone(), parent(&r.path), file_name(&r.path).to_string());
            } else if let Some(d) = doc.and_then(|d| self.docs.get(&d)) {
                // Same suggestion as the local Save As: a file's own name, or untitled-2.txt.
                name = if d.untitled_index.is_some() { format!("{}.txt", d.title().to_lowercase().replace(' ', "-")) } else { d.title() };
            }
        }
        let loading = !host.is_empty();
        if loading {
            self.remote_list(&host, &dir);
        }
        self.dialog = Some(Dialog::RemoteBrowser(Box::new(Browser {
            host,
            dir,
            entries: Vec::new(),
            loading,
            error: None,
            save,
            name,
            confirm_replace: false,
            focus_host: true,
        })));
    }

    fn remote_list(&mut self, host: &str, dir: &str) {
        let (host, dir) = (host.to_string(), dir.to_string());
        self.remote_spawn(true, move |a| Reply::Listed { result: apex_project::remote::list(a, &host, &dir) });
    }

    /// Open `host:path` (or focus its tab if it is already open).
    pub(crate) fn remote_open(&mut self, host: &str, path: &str) {
        if let Some(v) = self.views.values().find(|v| self.remote.docs.get(&v.doc).is_some_and(|r| r.host == host && r.path == path)).map(|v| v.id) {
            self.focus_view(v);
            return;
        }
        self.set_status(format!("Opening {host}:{path}…"), false);
        let (host, path) = (host.to_string(), path.to_string());
        self.remote_spawn(true, move |a| {
            let result = apex_project::remote::read(a, &host, &path);
            Reply::Opened { host, path, result }
        });
    }

    /// Start saving the view's remote document; `then` is shown once it is
    /// safely written (the next step of a close sequence). Always `false`:
    /// the write finishes later. `interactive`: the user asked (may prompt).
    pub(crate) fn remote_save(&mut self, vid: ViewId, then: Option<Box<Dialog>>, interactive: bool) -> bool {
        let Some(did) = self.views.get(&vid).map(|v| v.doc) else { return false };
        if self.settings.trim_trailing_whitespace_on_save {
            self.run_edit(vid, ops::trim_trailing_whitespace);
        }
        if self.settings.ensure_final_newline_on_save {
            self.run_edit(vid, ops::ensure_final_newline);
        }
        let (Some(r), Some(doc)) = (self.remote.docs.get_mut(&did), self.docs.get(&did)) else { return false };
        if r.saving {
            // Writes to one file must not overlap; this one follows the current one.
            r.save_pending = true;
            return false;
        }
        let normalised = apex_buffer::line_ending::normalise(&doc.text(), doc.line_ending);
        let (bytes, lossy) = apex_buffer::encode(&normalised, doc.encoding);
        let encoding = doc.encoding;
        r.saving = true;
        r.save_pending = false;
        let (host, path, rev) = (r.host.clone(), r.path.clone(), doc.revision());
        if lossy {
            self.set_status(format!("Some characters could not be encoded as {encoding} and were replaced"), true);
        }
        self.remote_spawn(interactive, move |a| Reply::Saved { doc: did, rev, then, result: apex_project::remote::write(a, &host, &path, &bytes) });
        false
    }

    /// Re-read a remote document. `force` (Revert) replaces unsaved edits;
    /// otherwise the reload is dropped if the buffer was edited meanwhile.
    pub(crate) fn remote_reload(&mut self, did: DocId, force: bool, encoding: Option<apex_buffer::Encoding>) {
        let Some(r) = self.remote.docs.get(&did) else { return };
        let (host, path) = (r.host.clone(), r.path.clone());
        self.remote_spawn(force, move |a| Reply::Reloaded { doc: did, force, encoding, result: apex_project::remote::read(a, &host, &path) });
    }

    /// Decode a remote file the way `open_path` decodes a local one.
    fn remote_decode(&self, bytes: &[u8], forced: Option<apex_buffer::Encoding>) -> Option<apex_buffer::DecodeOutcome> {
        if forced.is_none() && apex_buffer::detect_binary(bytes) {
            return None;
        }
        let fallback = apex_buffer::Encoding::from_label(&self.settings.fallback_encoding).unwrap_or(apex_buffer::Encoding::Legacy(encoding_rs::WINDOWS_1252));
        Some(apex_buffer::decode(bytes, forced, fallback))
    }

    /// Called every frame: apply finished transfers and start change checks.
    pub(crate) fn remote_poll(&mut self) {
        self.remote.docs.retain(|d, _| self.docs.contains_key(d));
        while let Ok(reply) = self.remote.rx.try_recv() {
            if !matches!(reply, Reply::Prompt { .. }) {
                self.remote.inflight -= 1;
            }
            self.remote_reply(reply);
        }
        self.remote_forget_unused();
        if self.remote.docs.is_empty() || self.remote.last_check.is_some_and(|t| t.elapsed() < CHECK_EVERY) {
            return;
        }
        self.remote.last_check = Some(Instant::now());
        let due: Vec<(DocId, String, String)> = self
            .remote
            .docs
            .iter_mut()
            .filter(|(_, r)| !r.saving && !r.checking)
            .map(|(d, r)| {
                r.checking = true;
                (*d, r.host.clone(), r.path.clone())
            })
            .collect();
        for (doc, host, path) in due {
            self.remote_spawn(false, move |a| Reply::Checked { doc, result: apex_project::remote::stamp(a, &host, &path) });
        }
    }

    /// Wipe the passwords of servers that no open file or browser uses any
    /// more, and close their shared connections. Waits while transfers run:
    /// an Open in flight has not produced its tab yet.
    fn remote_forget_unused(&mut self) {
        if self.remote.inflight > 0 {
            return;
        }
        let browsing = match &self.dialog {
            Some(Dialog::RemoteBrowser(b)) => Some(b.host.trim().to_string()),
            _ => None,
        };
        for host in self.remote.credentials.hosts() {
            let used = self.remote.docs.values().any(|r| r.host == host) || self.remote.folders.iter().any(|(h, _)| *h == host);
            if browsing.as_deref() != Some(host.as_str()) && !used {
                self.remote.credentials.forget(&host);
                std::thread::spawn(move || apex_project::remote::disconnect(&host));
            }
        }
    }

    fn remote_reply(&mut self, reply: Reply) {
        match reply {
            Reply::Prompt { host, prompt, reply } => {
                // One prompt at a time; whatever dialog is open comes back afterwards.
                if matches!(self.dialog, Some(Dialog::Askpass { .. })) {
                    let _ = reply.send(None);
                    return;
                }
                let back = self.dialog.take().map(Box::new);
                self.open_askpass(prompt, reply, Some(host), back);
            }
            Reply::Opened { host, path, result } => {
                let decoded = match &result {
                    Ok((bytes, _)) => self.remote_decode(bytes, None),
                    Err(e) => return self.set_status(format!("Could not open {host}:{path}: {e}"), true),
                };
                let Some(dec) = decoded else { return self.set_status(format!("{host}:{path} is a binary file"), true) };
                let stamp = result.ok().map(|(_, s)| s);
                let stats = apex_buffer::line_ending::scan(&dec.text, 4 << 20);
                let mut doc = Document::from_text(&dec.text);
                doc.encoding = dec.encoding;
                doc.line_ending = stats.dominant(apex_buffer::LineEnding::native());
                doc.mixed_line_endings = stats.is_mixed();
                doc.had_decode_errors = dec.had_errors;
                if self.settings.detect_indentation {
                    doc.detect_indent_once(self.settings.indent());
                } else {
                    doc.indent = self.settings.indent();
                }
                let did = doc.id;
                self.set_status(format!("Opened {host}:{path}"), false);
                let remote = RemoteDoc { host, path, stamp, offline: false, saving: false, save_pending: false, checking: false, told_changed: false };
                self.remote.docs.insert(did, remote);
                self.remote_retitle(did);
                let v = self.add_document(doc);
                self.replace_pristine_untitled(v);
                self.focus_view(v);
            }
            Reply::Saved { doc, rev, then, result } => {
                let Some(r) = self.remote.docs.get_mut(&doc) else { return };
                r.saving = false;
                let place = format!("{}:{}", r.host, r.path);
                match result {
                    Ok(stamp) => {
                        r.stamp = Some(stamp);
                        r.offline = false;
                        r.told_changed = false;
                        let again = std::mem::take(&mut r.save_pending);
                        if let Some(d) = self.docs.get_mut(&doc)
                            && d.revision() == rev
                        {
                            d.mark_saved();
                            d.read_only = false;
                            self.forget_recovery(doc);
                        }
                        self.set_status(format!("Saved {place}"), false);
                        if let Some(t) = then {
                            self.dialog = Some(*t);
                        }
                        if again && let Some(v) = self.views.values().find(|v| v.doc == doc).map(|v| v.id) {
                            self.remote_save(v, None, false);
                        }
                    }
                    Err(e) => {
                        r.offline = true;
                        r.save_pending = true;
                        self.set_status(format!("Could not save {}: {e}; kept here, will retry when the server is back", file_name(&place)), true);
                    }
                }
                self.remote_retitle(doc);
            }
            Reply::Reloaded { doc, force, encoding, result } => {
                let place = self.remote.docs.get(&doc).map(|r| format!("{}:{}", r.host, r.path)).unwrap_or_default();
                let (bytes, stamp) = match result {
                    Ok(ok) => ok,
                    Err(e) => {
                        if let Some(r) = self.remote.docs.get_mut(&doc) {
                            r.offline = true;
                        }
                        self.remote_retitle(doc);
                        if force {
                            self.set_status(format!("Could not reload {place}: {e}"), true);
                        }
                        return;
                    }
                };
                let Some(dec) = self.remote_decode(&bytes, encoding) else { return self.set_status(format!("{place} is now a binary file"), true) };
                let Some(view) = self.views.values().find(|v| v.doc == doc) else { return };
                let sels = view.sels.clone();
                let Some(d) = self.docs.get_mut(&doc) else { return };
                if d.is_dirty() && !force {
                    return;
                }
                d.replace_all(&dec.text, &sels);
                d.encoding = dec.encoding;
                d.had_decode_errors = dec.had_errors;
                d.mark_clean();
                for v in self.views.values_mut().filter(|v| v.doc == doc) {
                    v.cache.invalidate();
                }
                if let Some(r) = self.remote.docs.get_mut(&doc) {
                    (r.stamp, r.offline, r.save_pending, r.told_changed) = (Some(stamp), false, false, false);
                }
                self.forget_recovery(doc);
                self.remote_retitle(doc);
                self.set_status(format!("Reloaded {place}"), false);
            }
            Reply::Checked { doc, result } => {
                let Some(r) = self.remote.docs.get_mut(&doc) else { return };
                r.checking = false;
                let Ok(now) = result else {
                    // Unreachable (or the file is gone): say so in the tab only.
                    r.offline = true;
                    return self.remote_retitle(doc);
                };
                r.offline = false;
                let changed = r.stamp.as_ref().is_some_and(|s| *s != now);
                let place = format!("{}:{}", r.host, r.path);
                let dirty = self.docs.get(&doc).is_some_and(|d| d.is_dirty());
                if changed && !dirty {
                    self.remote_reload(doc, false, None);
                } else if changed && !r.told_changed {
                    // Our unsaved edits versus someone else's: the user decides.
                    r.told_changed = true;
                    r.save_pending = false;
                    self.set_status(format!("{} changed on the server; Save overwrites it, Revert takes theirs", file_name(&place)), true);
                } else if !changed && r.save_pending && dirty {
                    // Back online with nothing changed there: finish the save that failed.
                    if let Some(v) = self.views.values().find(|v| v.doc == doc).map(|v| v.id) {
                        self.remote_save(v, None, false);
                    }
                }
                self.remote_retitle(doc);
            }
            Reply::FolderListed { host, dir, result } => {
                for (_, root) in self.remote.folders.iter_mut().filter(|(h, _)| *h == host) {
                    let Some(n) = root.find_mut(&dir) else { continue };
                    n.loading = false;
                    match &result {
                        Ok((abs, entries)) => {
                            // A folder typed as `~/x` or left empty becomes absolute here.
                            if n.path != *abs {
                                n.path = abs.clone();
                                n.kids = None;
                            }
                            n.set_kids(entries);
                            n.error = None;
                        }
                        Err(e) => n.error = Some(e.clone()),
                    }
                }
            }
            Reply::Listed { result } => {
                let Some(Dialog::RemoteBrowser(b)) = &mut self.dialog else { return };
                b.loading = false;
                match result {
                    Ok((dir, entries)) => {
                        (b.dir, b.entries, b.error) = (dir, entries, None);
                        (self.remote.last_host, self.remote.last_dir) = (b.host.clone(), b.dir.clone());
                    }
                    Err(e) => b.error = Some(e),
                }
            }
        }
    }

    /// Add `host:dir` to the sidebar's REMOTE section and list it.
    pub(crate) fn remote_open_folder(&mut self, host: &str, dir: &str) {
        self.settings.sidebar = true;
        if self.remote.folders.iter().any(|(h, n)| h == host && n.path == dir) {
            return;
        }
        let name = if dir.is_empty() { "~" } else { file_name(dir.trim_end_matches('/')) };
        let mut root = Node::new(if name.is_empty() { "/" } else { name }, dir.to_string(), true);
        (root.open, root.loading) = (true, true);
        self.remote.folders.push((host.to_string(), root));
        self.remote_list_folder(host, dir);
    }

    fn remote_list_folder(&mut self, host: &str, dir: &str) {
        let (host, dir) = (host.to_string(), dir.to_string());
        self.remote_spawn(true, move |a| {
            let result = apex_project::remote::list(a, &host, &dir);
            Reply::FolderListed { host, dir, result }
        });
    }

    /// Whether the sidebar has remote folders to show.
    pub(crate) fn has_remote_folders(&self) -> bool {
        !self.remote.folders.is_empty()
    }

    /// The sidebar's REMOTE section: one tree per folder opened from the
    /// remote browser. Clicking a file opens it; a root's context menu
    /// refreshes or closes it.
    pub(crate) fn show_remote_folders(&mut self, ui: &mut egui::Ui, max_height: f32, fill: bool) {
        let theme = self.theme.clone();
        let font = self.settings.ui_font_size;
        let active = self.active_ids().and_then(|(_, d)| self.remote.docs.get(&d)).map(|r| (r.host.clone(), r.path.clone()));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("REMOTE").size(font - 2.0).color(theme.ui.text_muted).strong());
        });
        let mut acts = Vec::new();
        let mut folders = std::mem::take(&mut self.remote.folders);
        egui::ScrollArea::both().id_salt("remote-folders").max_height(max_height).auto_shrink([false, !fill]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
            for (i, (host, root)) in folders.iter_mut().enumerate() {
                let active = active.as_ref().filter(|(h, _)| h == host).map(|(_, p)| p.as_str());
                let mut out = Vec::new();
                draw_node(ui, root, 0, host, &theme, font, active, &mut out);
                acts.extend(out.into_iter().map(|a| (i, a)));
            }
        });
        self.remote.folders = folders;
        for (i, act) in acts {
            let host = self.remote.folders[i].0.clone();
            match act {
                FolderAct::List(dir) => self.remote_list_folder(&host, &dir),
                FolderAct::Open(path) => self.remote_open(&host, &path),
                FolderAct::Refresh => {
                    let mut dirs = Vec::new();
                    self.remote.folders[i].1.open_dirs(&mut dirs);
                    for d in dirs {
                        self.remote_list_folder(&host, &d);
                    }
                }
                FolderAct::Close => {
                    self.remote.folders.remove(i);
                    // Indices after this one have shifted; the rest wait for the next frame.
                    return;
                }
            }
        }
    }

    /// Draw the remote browser (`Dialog::RemoteBrowser`).
    pub(crate) fn show_remote_browser_dialog(&mut self, ctx: &egui::Context, mut b: Browser) {
        enum Act {
            List(String),
            Open(String),
            OpenFolder,
            Save(String),
            Close,
        }
        let mut act = None;
        let r = egui::Modal::new(egui::Id::new("remote-browser")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading(if b.save.is_some() { "Save to Remote" } else { "Open Remote File" });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Host");
                let resp = ui.add(egui::TextEdit::singleline(&mut b.host).hint_text("user@server or an ~/.ssh/config alias").desired_width(340.0));
                if std::mem::take(&mut b.focus_host) {
                    resp.request_focus();
                }
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Connect").clicked() || enter) && !b.host.trim().is_empty() {
                    act = Some(Act::List(b.dir.clone()));
                }
            });
            ui.horizontal(|ui| {
                ui.label("Folder");
                let resp = ui.add(egui::TextEdit::singleline(&mut b.dir).hint_text("home folder").desired_width(340.0));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    act = Some(Act::List(b.dir.clone()));
                }
                if ui.button("Up").clicked() {
                    act = Some(Act::List(parent(&b.dir)));
                }
            });
            ui.add_space(4.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(300.0).min_scrolled_height(300.0).auto_shrink([false, false]).show(ui, |ui| {
                    if b.loading {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Connecting…");
                        });
                    } else if let Some(e) = &b.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    for e in &b.entries {
                        if ui.selectable_label(b.name == *e, e).clicked() {
                            act = Some(match (e.strip_suffix('/'), b.save.is_some()) {
                                (Some(d), _) => Act::List(join(&b.dir, d)),
                                (None, false) => Act::Open(join(&b.dir, e)),
                                (None, true) => {
                                    b.name = e.clone();
                                    b.confirm_replace = false;
                                    continue;
                                }
                            });
                        }
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if b.save.is_some() {
                    ui.label("Name");
                    let resp = ui.add(egui::TextEdit::singleline(&mut b.name).desired_width(260.0));
                    if resp.changed() {
                        b.confirm_replace = false;
                    }
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let label = if b.confirm_replace { "Replace" } else { "Save" };
                    let ok = !b.host.trim().is_empty() && !b.name.trim().is_empty() && !b.name.contains('/') && !b.loading;
                    if ui.add_enabled(ok, egui::Button::new(label)).clicked() || (enter && ok) {
                        if b.entries.contains(&b.name) && !b.confirm_replace {
                            b.confirm_replace = true;
                        } else {
                            act = Some(Act::Save(join(&b.dir, b.name.trim())));
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    act = Some(Act::Close);
                }
                if b.save.is_none() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ok = !b.host.trim().is_empty() && !b.loading;
                        if ui.add_enabled(ok, egui::Button::new("Open Folder")).on_hover_text("Show this folder in the sidebar").clicked() {
                            act = Some(Act::OpenFolder);
                        }
                    });
                }
            });
            if b.confirm_replace {
                ui.label(format!("{} already exists. Click Replace to overwrite it.", b.name));
            }
        });
        if r.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            act = Some(Act::Close);
        }
        let host = b.host.trim().to_string();
        match act {
            Some(Act::List(dir)) => {
                (b.host, b.dir, b.loading, b.error) = (host.clone(), dir.clone(), true, None);
                b.entries.clear();
                self.remote_list(&host, &dir);
            }
            Some(Act::Open(path)) => {
                self.dialog = None;
                return self.remote_open(&host, &path);
            }
            Some(Act::OpenFolder) => {
                self.dialog = None;
                return self.remote_open_folder(&host, b.dir.trim());
            }
            Some(Act::Save(path)) => {
                self.dialog = None;
                let Some((vid, then)) = b.save else { return };
                let Some(did) = self.views.get(&vid).map(|v| v.doc) else { return };
                if self.remote.docs.values().any(|r| r.host == host && r.path == path) && self.remote.docs.get(&did).is_none_or(|r| r.path != path) {
                    return self.set_status(format!("{host}:{path} is already open in another tab; close it first"), true);
                }
                // The document now belongs to the server; a local file it came from is left alone.
                let remote = RemoteDoc { host, path, stamp: None, offline: false, saving: false, save_pending: false, checking: false, told_changed: false };
                self.remote.docs.insert(did, remote);
                if let Some(d) = self.docs.get_mut(&did) {
                    (d.path, d.file_identity, d.read_only, d.untitled_index) = (None, None, false, None);
                }
                self.remote_retitle(did);
                self.remote_save(vid, then, true);
                return;
            }
            Some(Act::Close) => {
                self.dialog = None;
                return;
            }
            None => {}
        }
        self.dialog = Some(Dialog::RemoteBrowser(Box::new(b)));
    }
}

/// One row of a remote folder tree and, if open, its children.
#[allow(clippy::too_many_arguments)]
fn draw_node(
    ui: &mut egui::Ui,
    n: &mut Node,
    depth: usize,
    host: &str,
    theme: &crate::theme::Theme,
    font: f32,
    active: Option<&str>,
    out: &mut Vec<FolderAct>,
) {
    use egui::{pos2, vec2};
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(200.0), font + 8.0), egui::Sense::click());
    if active == Some(n.path.as_str()) || resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, theme.ui.hover);
    }
    let x = rect.min.x + 8.0 + depth as f32 * 14.0;
    if n.dir {
        let c = pos2(x + 4.0, rect.center().y);
        let pts = if n.open {
            vec![c + vec2(-4.0, -2.0), c + vec2(4.0, -2.0), c + vec2(0.0, 3.0)]
        } else {
            vec![c + vec2(-2.0, -4.0), c + vec2(3.0, 0.0), c + vec2(-2.0, 4.0)]
        };
        ui.painter().add(egui::Shape::convex_polygon(pts, theme.ui.text_muted, egui::Stroke::NONE));
    }
    let (label, colour, size) =
        if depth == 0 { (format!("{} ({host})", n.name.to_uppercase()), theme.ui.text_muted, font - 1.0) } else { (n.name.clone(), theme.ui.text, font) };
    ui.painter().text(pos2(x + 14.0, rect.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(size), colour);
    let resp = if depth == 0 { resp.on_hover_text(format!("{host}:{}", n.path)) } else { resp };
    if resp.clicked() {
        if !n.dir {
            out.push(FolderAct::Open(n.path.clone()));
        } else {
            n.open = !n.open;
            if n.open && n.kids.is_none() && !n.loading {
                n.loading = true;
                out.push(FolderAct::List(n.path.clone()));
            }
        }
    }
    if depth == 0 {
        resp.context_menu(|ui| {
            if ui.button("Refresh").clicked() {
                out.push(FolderAct::Refresh);
                ui.close();
            }
            if ui.button("Close Remote Folder").clicked() {
                out.push(FolderAct::Close);
                ui.close();
            }
        });
    }
    if !(n.dir && n.open) {
        return;
    }
    let pad = 8.0 + (depth + 1) as f32 * 14.0 + 14.0;
    if n.loading && n.kids.is_none() {
        ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.spinner();
        });
    }
    if let Some(e) = &n.error {
        ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.label(egui::RichText::new(e).color(theme.ui.error).size(font - 2.0));
        });
    }
    for k in n.kids.iter_mut().flatten() {
        draw_node(ui, k, depth + 1, host, theme, font, active, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/etc", "hosts"), "/etc/hosts");
        assert_eq!(parent("/etc/nginx"), "/etc");
        assert_eq!(parent("/etc"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(file_name("/etc/hosts"), "hosts");
    }

    #[test]
    fn folder_refresh_keeps_state() {
        let mut root = Node::new("srv", "/srv".into(), true);
        root.set_kids(&["www/".into(), "a.txt".into()]);
        let www = root.find_mut("/srv/www").unwrap();
        (www.open, www.kids) = (true, Some(Vec::new()));
        root.open = true;
        root.set_kids(&["www/".into(), "b.txt".into()]);
        let names: Vec<_> = root.kids.iter().flatten().map(|k| (k.path.as_str(), k.dir)).collect();
        assert_eq!(names, [("/srv/www", true), ("/srv/b.txt", false)]);
        let mut open = Vec::new();
        root.open_dirs(&mut open);
        assert_eq!(open, ["/srv", "/srv/www"], "the open subfolder survives a refresh");
    }
}
