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
* **Compare with Saved / with Index / with HEAD** opens a side-by-side diff
  of the buffer against that version (see below). `apex --diff old new`
  compares two files the same way.
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

## Side-by-side diffs

**Open Changes** (double-click a file in the Source Control panel, or the
Git menu), **Open Staged Changes**, the Compare commands and
`apex --diff old new` show the two versions next to each other:

* Both sides have syntax colours; removed lines are tinted on the left,
  added lines on the right, and changed words more strongly. A strip
  between the sides links each change.
* When the right side is your file (unstaged changes, Compare with…,
  `--diff`), it is the real document: edit, undo and save there as usual.
* The strip's buttons act on one change: **>** replaces it with the left
  side (an undoable edit), **+** stages it (unstaged changes), **−**
  unstages it (staged changes). The left side refreshes after staging.
* The sides scroll together. **Next / Previous** in the header (or Goto >
  Next Change) jump between changes; **Unified** opens the same comparison
  as a unified diff.
* Closing the tab closes both sides; unsaved edits on the right are asked
  about first.

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

## Fetch, pull and push

Apex only talks to the network when you ask (or when
`git_auto_fetch_minutes` is above 0). Fetch, Pull and Push are on the
Source Control panel's branch line and in the Git menu; the status bar shows
how many commits there are to push and to pull.

* **Fetch** fetches every remote and prunes deleted branches.
* **Pull** follows `git_pull_mode`: `ff-only` (default) refuses to create a
  merge commit, `rebase` replays your commits, `merge` merges.
* **Push** pushes to the upstream branch. A branch without one is
  published (`push --set-upstream`) to `origin`, or the only remote, after
  asking.
* **Sync** pulls and then pushes. **Force Push (with Lease)…** replaces the
  upstream branch after confirming; it refuses if the remote changed since
  your last fetch.
* **Credentials**: your credential helper and SSH agent are used as usual.
  When git or ssh needs a password, passphrase or host-key answer, Apex
  shows a dialog (it acts as the askpass helper); the answer goes to git
  once and is never stored. Progress appears in the status bar; **Cancel
  Git Command** stops a slow transfer.

## GitHub and other code hosts

**Git > Remote** builds links from the repository's remote (GitHub, GitLab,
Bitbucket, Codeberg/Gitea; other hosts are treated like GitHub, which suits
GitHub Enterprise) and opens them in your browser. Apex itself makes no
network request for these.

* **Open File on Remote** / **Copy Permalink**: the active file at the
  current commit, with the selected lines highlighted. Apex warns when the
  commit is not pushed yet or the file has uncommitted changes.
* **Open Commit / Branch on Remote**, **Compare Branch on Remote** (the
  host's "new pull/merge request" page).

**Git > GitHub** uses the [GitHub CLI](https://cli.github.com/) (`gh`), which
holds your GitHub login; Apex stores no tokens. Install it and run
`gh auth login` once (`APEX_GH` selects a particular binary).

* **Create Pull Request…**: title and description filled in from the
  branch's commits, base branch (the remote's default), draft. Unpushed
  commits are pushed first; a new branch is published.
* The status bar shows the current branch's pull request, its checks and
  review state. It refreshes after you push or create a pull request, and
  on **Refresh Pull Request Status**; Apex does not poll GitHub.
* **Pull Requests…** lists open pull requests; choose one to check it out,
  show its diff, or open it in the browser.
* **Pull Request Checks…** lists the checks; Enter opens one's details.
* **Open Pull Request in Browser**.

## Branches, log and stash

* **Switch Branch…** (or click the branch name in the Source Control panel)
  lists local branches and remote ones without a local copy; choosing a
  remote branch creates a local branch tracking it. The first entry creates
  a new branch from the current commit. Git refuses to switch when your
  changes would be overwritten: stash them first.
* **Create / Rename / Delete Branch…** and **Merge Branch into Current…**
  (Git > Branches). Deleting refuses a branch with unmerged commits.
* **Show Log** opens every commit of every branch and tag with a lane
  graph, branch and tag labels, author and age. Type to filter (by message,
  author, id or ref); double-click or Enter shows a commit; right-click to
  copy its id, create a branch there, check it out (detached), cherry-pick
  it onto the current branch, revert it, or open it on the remote. The log
  refreshes after each command.
* **Line History of Selection** shows how the selected lines changed over
  time (`git log -L`).
* **Stash Changes…** stashes everything, including untracked files, with an
  optional message; **Stashes…** lists them with Apply, Pop, Show (as a
  diff) and Drop (confirmed).

## Merge conflicts

When a merge, rebase, cherry-pick or revert stops on conflicts, the status
bar and the Source Control panel say so, and the conflicted files are listed
under **Merge conflicts**.

* Conflict blocks are tinted: the current side (ours), the incoming side
  (theirs) and the marker lines differ. **Next / Previous Conflict** move
  between them.
* With the caret in a block, **Accept Current Change**, **Accept Incoming
  Change** or **Accept Both Changes** (Git > Conflicts or right-click)
  replace the block; each is an ordinary undoable edit.
* Save, then **Mark Resolved** to stage the file (it warns if markers are
  left). When nothing is conflicted, **Continue** in the panel (or
  Git > Conflicts) finishes the merge or moves the rebase on, keeping
  git's prepared message. **Abort** returns to where you started, after
  confirming.

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
