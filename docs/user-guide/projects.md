# Projects, the sidebar and sessions

## Folders

**File > Open Folder** (`Ctrl+Shift+O`), dragging a folder onto the window,
`apex .` or the **+** button in the sidebar add a folder to the workspace.
Several folders can be open at once. The sidebar shows them as a lazy tree:
only expanded directories are read, so a 100,000-file tree opens instantly,
and the index that powers Goto Anything and Find in Files is built in the
background and kept up to date by a file watcher.

Right-click an entry for new file, new folder, rename, duplicate, delete
(to the system bin where available, with confirmation), copy path, reveal
in the file manager, find in folder. The filter box narrows the tree. Files
ignored by `.gitignore` and `exclude_patterns` are shown dimmed or hidden
(`show_hidden_files`). Git status is shown next to changed files.

## Remote files over SSH

**File > Open Remote…** opens a browser for a server: enter a host
(`user@server` or an alias from `~/.ssh/config`), press Connect, and pick a
file. Apex runs your own `ssh`, so keys, the agent, `ProxyJump` and the rest
of your SSH config apply. The server needs only `sh`, `cat`, `ls`,
`cksum` and `mktemp`. The first connection to a new server records its host key; a
changed key is refused.

**Open Folder** (bottom right of the browser) shows the current folder in
a **REMOTE** section at the top of the sidebar instead. Click a folder to
expand it (it is listed over SSH the first time) and a file to open it.
Right-click the folder's top row to **Refresh** it (new and removed files
on the server) or **Close Remote Folder**. With no local folder open, the
remote folders fill the sidebar. Remote folders are a file tree only: no
Find in Files, git, rename or delete, and they are not restored when Apex
restarts.

### Passwords and passphrases

Keys and the agent are tried first. If the server (or your key) needs a
password or passphrase, Apex asks in a dialog that appears over the browser.
Cancel, or a wrong answer, just shows "Permission denied" in the browser;
press Connect to try again.

Apex keeps the answer so you are not asked again, and so the background
checks and automatic retries described below keep working on their own:

* It lives only in memory, in pages locked so the system never writes them
  to swap (and, on Linux, left out of crash dumps). It is never written to
  disk, the session, recovery snapshots or logs.
* It is kept while a file or folder from that server is open or the
  browser is showing that server. When the last one closes, the memory is overwritten with
  zeros and released, and the shared SSH connection to the server is closed
  so nothing can reuse the login. Quitting Apex wipes it too.
* Only Open, Save, Save As, Revert and browsing can ask. The background
  checks and automatic retries never show a prompt: without a remembered
  password they just mark the tab `offline`.
* A remembered password the server rejects (changed, say) is forgotten at
  once, and the next thing you do asks again.
* One-time codes (two-factor prompts) are asked every time, never kept.

The text field you type into belongs to the GUI toolkit; Apex clears it and
its undo history when the dialog closes, but cannot guarantee that no copy
was left in ordinary memory while you typed. If that matters, prefer keys
with an agent.

A remote tab is titled `name (host)`. Save writes back to the server (the
file keeps its owner and permissions, and a dropped connection never leaves
it half-written); Save As and **File > Save to Remote…** pick a location in
the same browser; Revert reloads the server's copy. Every 5 seconds Apex
checks each remote file. A tab with no unsaved edits reloads quietly when
the file changes on the server. A tab with unsaved edits is left as it is,
with a status-bar note: Save overwrites the server's copy, Revert takes it.

If the connection drops, the tab title shows `offline` and nothing pops up.
A save that fails keeps the changes in the editor (and in crash recovery)
and runs again by itself once the server answers, unless the file changed
there in the meantime. Retries use the remembered password, if any; they
never ask for one. Remote tabs are not restored when Apex restarts.

## Project files

**Project > Save Project As...** opens a native Save panel and suggests
`<name>.apex-project`. Setting `native_file_dialogs = false` uses a path
prompt instead. The project file contains:

```toml
# Apex project file. Safe to commit: it holds no session state.
folders = ["."]

[settings]
tab_width = 2

[[tasks]]
name = "Check"
command = ["cargo", "check"]
problem_matcher = "rustc"
```

Opening the project file restores the folders and applies the `[settings]`
overrides on top of your own settings. Tasks from a project file run only
after you trust the workspace (see [Build tasks](build-tasks.md)).

## Sessions and recovery

When you quit, the open folders, tabs, selections, scroll positions, pane
layout, bookmarks and the window size are saved to the data directory and
restored on the next start (`restore_session`). Unsaved buffers are kept too.

Every few seconds (`recovery_interval_ms`) unsaved changes are snapshotted
to the data directory. After a crash or power cut the next start offers to
restore them; snapshots are deleted when the file is saved or the restore is
declined.

Files changed on disk by another program are detected (on window focus and
every two seconds): an unmodified buffer reloads silently; a modified one
asks whether to reload or keep your version, and saving over a file that
changed since it was loaded asks before overwriting. Files that disappear
are marked in the tab.
