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

## Git output

Every command Apex runs, with its output, is shown in the **GIT** tab of the
bottom panel (**Git > Show Git Output**). Commands run one at a time in the
background; the status bar shows progress and **Git > Cancel Git Command**
stops the running one. Git never prompts on a terminal: commands run with
`GIT_TERMINAL_PROMPT=0` and detached from any terminal.
