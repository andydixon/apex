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

## Project files

**Project > Save Project** writes `<name>.apex-project.toml`:

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
