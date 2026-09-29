# Git

Apex reads repositories with an embedded libgit2; no `git` binary is needed
and nothing is ever written to the repository except the explicit hunk
revert below.

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

Not provided: committing, staging, branching, pushing. Use the command line
or a dedicated client; Apex notices the results.
