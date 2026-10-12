# Git

Apex reads repositories with an embedded libgit2, so the read-only features
below need no `git` program. Everything that changes a repository (staging,
committing, branches, fetch/pull/push) runs **your installed `git`**, so your
configuration, hooks, commit signing, credential helpers and SSH agent all
apply. Set `APEX_GIT` to use a particular `git` binary.

Because `git` runs the repository's hooks and configured programs, these
commands ask you to **trust the folder** first (the same prompt as build
tasks). Untrusted folders keep the read-only features.

Everything is in the **Git** menu and the command palette.

* **Gutter marks** next to the line numbers show added, modified and deleted
  lines relative to the index (or `HEAD`, see below). They update as you
  type and after each save; the minimap shows them too.
* **Status bar** shows the current branch (or a short commit id when
  detached). The sidebar marks changed, added and untracked files.
* **Goto > Next Change / Previous Change** jump between hunks.
* **Revert Hunk** (Edit menu or the gutter's right-click) restores the hunk
  under the caret from the index after confirming. This is the only write.
* **Show Hunk Diff** pops up the hunk as a unified diff.
* **Compare with Saved / with Index / with HEAD** opens a read-only unified
  diff tab with syntax highlighting. `apex --diff old new` does the same for
  two arbitrary files.
* **File History** lists the commits that touched the file; choosing one
  opens that version read-only.
* **Git Status** opens a palette of changed files; choose one to open it.

Settings: `git_gutter = false` turns the marks off. Large repositories are
scanned in the background and the status refreshes when the watcher sees
changes under `.git`.

## Source Control panel

The lower half of the sidebar shows the repository of the active file (a
drop-down chooses between several open repositories): the branch and how
far it is ahead of or behind its upstream, the commit box, and the changed
files grouped as **Merge conflicts**, **Staged changes** and **Changes**.
Drag the divider to resize it; `git_sidebar = false` (or **Git > Toggle
Source Control Panel**) hides it.

* Click a file to preview it, double-click to open its changes (staged:
  HEAD → index; unstaged: index → working tree). Hover a file for Stage (+),
  Unstage (−) and Discard (⟲); right-click for the full list.
* **Commit** (or Ctrl+Enter in the message box) commits the staged changes.
  With nothing staged it offers to commit all changes to tracked files.
  Up and Down in an empty message box recall earlier messages; the number
  beside Sign-off is the summary line's length (50 recommended, 72 at most).
* **Amend** rewrites the last commit with the staged changes (an empty
  message keeps the old one); **Sign-off** adds a `Signed-off-by` trailer.
* **Undo Last Commit…** (Git menu or the panel's … menu) removes the last
  commit after confirming, keeps its changes staged and puts its message
  back in the box.
* Your hooks run as usual; their output is in the GIT tab.

## Staging

* **Stage File / Unstage File** for the active file, **Stage All Changes /
  Unstage All Changes** for the repository.
* **Stage Change at Caret** stages just the hunk under the caret;
  **Stage Selected Lines** stages only the changed lines touched by your
  selections (several cursors work). Save the file first: staging uses the
  file on disk. Untracked files are added with intent-to-add first.
* **Discard File Changes…** restores the file from the index after
  confirming (for an untracked file it offers to delete it).
* Right-click in the editor for Stage Change / Stage Selected Lines /
  Revert Change.

## Blame

**Git > Toggle Blame** (Ctrl+K Ctrl+G) adds a column to the gutter with the
author and age of the commit that last changed each line, shaded so newer
changes stand out. Unsaved edits show as *Not committed*. Hover for the full
commit message; click to open the commit (header, message and patch) in a
read-only tab, as does **Git > Show Commit** for the caret line. With blame
on, the status bar shows the caret line's author, age and summary;
`git_blame_inline = true` also writes it after the caret line.

## Edit Commit History

**Git > Edit Commit History…** opens an editor for the metadata of every
commit in the workspace folder's repository (all branches and tags), newest
first. Nothing changes until you apply.

* Select a commit to edit its author and committer (name, email, date) and
  its message. Dates take `YYYY-MM-DD HH:MM[:SS]` with an optional
  `±HH:MM` offset (without one, your local time zone applies); **Calendar…**
  picks a day and time, **Now** sets the current time. Edited commits are
  marked with a dot; **Reset commit** undoes the edits to one commit.
* Search filters by hash, author, email or message. **Claude/Codex only**
  lists commits whose author, committer or message mentions Claude, Codex
  or Anthropic, and merges that brought such commits in.
* **Replace author…** sets a new author on every commit whose author name or
  email matches (any part, ignoring case, or exactly).
* **Rewrite dates…** gives every commit a random time on a day in a range,
  inside a daily window (for example 09:00–17:00), keeping the order of
  commits.
* **Remove co-author lines** drops `Co-authored-by:` trailers and
  `Claude-Session:` lines from every message on apply; Claude/Codex
  attribution blocks are removed whole.
* **Apply** lists the commits that will change and asks first. The
  repository must have no uncommitted changes. Apex tags the current HEAD
  as `apex-backup-<time>`, rewrites local branches and tags with
  `git filter-branch`, then removes filter-branch's `refs/original` and
  prunes. To go back: `git reset --hard apex-backup-<time>`.
* **Force push** (and **Push tags**) then pushes every branch to `origin`
  with `--force-with-lease`; you must type `FORCE` to confirm. Anyone who
  has the old history must re-clone or reset.

Rewriting history gives every changed commit, and every commit after it, a
new hash. Coordinate with anyone else using the repository first.

## Git output

Every command Apex runs, with its output, is shown in the **GIT** tab of the
bottom panel (**Git > Show Git Output**). Commands run one at a time in the
background; the status bar shows progress and **Git > Cancel Git Command**
stops the running one. Git never prompts on a terminal: commands run with
`GIT_TERMINAL_PROMPT=0` and detached from any terminal.
