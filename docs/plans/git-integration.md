# Plan: Git and GitHub integration

Status: proposed (2026-10-12). Nothing here is implemented yet; the current
git support is listed in `FEATURE_MATRIX.md` under "Git".

## Where we start

`apex-project::git` (libgit2, vendored, no network features) already gives
repository discovery, status, blob access for index/HEAD, line hunks, file
history and unified diffs. `git_ui.rs` uses them for gutter marks, hunk
navigation and revert, compare-with tabs (plain unified text) and a status
palette. Nothing writes to the repository except Revert Hunk.

## Decisions

1. **Reads stay on libgit2; writes and network go through the `git` CLI.**
   libgit2 does not run hooks, does not sign commits (GPG/SSH), ignores
   commit templates and most credential helpers, and our build has no
   HTTPS/SSH transport. Running the user's `git` honours their config,
   hooks, signing, credential helpers and SSH agent with no new
   dependencies. Without `git` on `PATH` the read-only features keep working
   and the write commands explain what is missing.
2. **GitHub through the `gh` CLI**, plus URL-only features that need no
   network at all from Apex (Open on GitHub, copy permalink). No HTTP client,
   no OAuth flow and no token storage in Apex; `gh auth` owns credentials.
   The URL features also recognise GitLab, Bitbucket, Codeberg/Gitea and
   self-hosted remotes from `remote.origin.url`.
3. **Offline by default.** No network traffic unless the user runs Fetch,
   Pull, Push or a GitHub command. Optional `git_auto_fetch_minutes`
   (default 0 = off).
4. **Trust gate.** Running `git` in a repository executes its hooks and
   config-driven commands (`core.fsmonitor`, `core.sshCommand`, filters), so
   CLI operations use the existing workspace trust prompt from build tasks.
   Untrusted folders keep the read-only libgit2 features.
5. **Never lose work.** Discard, force-push, branch delete and stash drop
   are confirmed; destructive commands show what they will touch. Every CLI
   command and its output goes to a Git output log.

## Phases

Each phase ends with tests, docs (`docs/user-guide/git.md`), feature matrix
and changelog updates, and a commit.

### G1 — Command runner and staging

* `apex-project::git_cli`: run `git -C <workdir> …` on a worker thread,
  streamed output, cancellable, `GIT_TERMINAL_PROMPT=0`, `--porcelain`
  output parsed, progress lines (`--progress`) parsed for a status-bar
  indicator. Output log panel ("Git" tab in the bottom panel).
* Stage / unstage / discard: file, hunk and selected lines. Hunk and line
  staging build a patch from our hunks and apply it with
  `git apply --cached` (reverse for unstage).
* Status refresh after every command (re-uses the existing status scan).

### G2 — Source Control panel

* Sidebar tab beside the explorer: Conflicts, Staged, Changes, Untracked
  groups; per-file stage/unstage/discard/open/diff buttons and context menu;
  keyboard navigable; counts badge on the tab.
* Commit box: multi-line message with 50/72 rulers, Commit (Ctrl+Enter),
  Amend, Sign-off, "commit all" when nothing is staged (asks first),
  message history (Up in an empty box), commit template support via
  `git commit -F`.
* Commands in the palette and menus: Stage File, Unstage File, Commit,
  Amend Last Commit, Undo Last Commit (`reset --soft HEAD~1`, confirmed).

### G3 — Diff views

* New side-by-side diff widget built on the existing editor rendering:
  left/right panes with aligned filler rows, synchronised scrolling,
  intra-line (word) highlighting, syntax highlighting on both sides, folded
  unchanged regions with "show more", next/previous change, toggle
  side-by-side / inline (unified).
* The right side is editable when it is the working copy; the gutter has
  stage / unstage / revert-hunk buttons.
* Sources: working tree vs index, index vs HEAD, working tree vs any
  revision, two revisions, two files (`apex --diff` switches to this view),
  saved vs buffer.
* Commit view: header (author, date, message) and every changed file as a
  collapsible diff; opened from history, blame and the log.
* Alignment and word-diff logic lives in `apex-project` (no egui) with unit
  and property tests.

### G4 — Blame

* Toggle Blame (Ctrl+K Ctrl+G, currently unbound): annotation column
  with short id, author, relative date, shaded by age; adjacent lines from
  the same commit are grouped.
* Current-line blame as dim inline text after the caret line (setting,
  off by default) and in the status bar.
* Hover shows the full commit message; click opens the commit view;
  "Blame previous revision" walks back past a commit.
* libgit2 `blame_file` + `blame_buffer` so unsaved edits show as
  "not committed"; runs on a worker, cached per (HEAD, path), cancelled on
  tab switch. Large files: on demand only.

### G5 — Branches, history, stash, conflicts

* Branch picker (status bar click or palette): switch, create from here,
  rename, delete (confirmed), track remote branch; refuses to switch with
  conflicting local changes and offers stash.
* Repository log: list of commits with a lane graph, filter by text/author/
  path, opens the commit view. Line history for a selection (`git log -L`).
* Stash: push (with message, include untracked), list, apply, pop, drop.
* Merge conflicts: markers highlighted, inline actions Accept Ours / Theirs /
  Both, next/previous conflict, "Mark resolved" (stage). Merge/rebase in
  progress shown in the status bar with Continue / Abort.
* Not planned: interactive rebase editor, submodule management, LFS.

### G6 — Remotes

* Fetch, Pull (setting `git_pull_mode = "ff-only" | "rebase" | "merge"`,
  default ff-only), Push (sets upstream on first push after asking),
  Force Push with lease (confirmed), Sync (pull then push).
* Ahead/behind counts in the status bar (local data only; refreshed after
  fetch).
* Progress and cancel; authentication failures explained with the
  credential-helper / SSH-agent hint. Optional askpass: Apex sets
  `GIT_ASKPASS` to itself (`apex --askpass`) and shows a password dialog
  over IPC; nothing is stored.

### G7 — GitHub (and other forges)

* Without `gh`: Open File on Remote (current line or selection range,
  pinned to the commit), Copy Permalink, Open Commit / Branch on Remote,
  Compare branch on remote. Issue/PR references (`#123`, `owner/repo#123`)
  in commit messages and the commit view are links.
* With `gh` (detected; commands hidden otherwise):
  * Create Pull Request: title/body form pre-filled from commits, base
    branch, draft toggle; pushes first if needed.
  * Pull request for the current branch in the status bar (number, state,
    checks), refreshed on demand and after push.
  * Pull Requests palette: list, open in browser, check out
    (`gh pr checkout`), view diff in the commit-style view.
  * Checks: list check runs for the branch with links.
* Later (not in this plan): inline PR review comments, Actions logs,
  issue creation.

## Testing

* Unit tests create real repositories in temp dirs with the `git` CLI
  (skipped with a message when `git` is absent) and a bare repository as
  the "remote", so fetch/pull/push are tested fully offline.
* Diff alignment, word diff and patch generation for hunk/line staging:
  unit tests plus a proptest that applying the generated patch with
  `git apply --cached` gives the expected index content.
* `gh` features tested against a fake `gh` script selected with `APEX_GH`
  (records arguments, returns canned JSON). Real GitHub is checked by hand
  and recorded in `PROGRESS.md` as such.
* Xvfb scripted runs: stage + commit from the panel, side-by-side diff
  screenshot, blame toggle.
* Fuzz target for the porcelain/log parsers.

## Rough size

| Phase | Effort |
|---|---|
| G1 runner + staging | small |
| G2 Source Control panel | medium |
| G3 side-by-side diff | large (the main UI work) |
| G4 blame | medium |
| G5 branches/log/stash/conflicts | large |
| G6 remotes | medium |
| G7 GitHub/forges | medium |

Suggested order: G1 → G2 → G3 → G4 → G6 → G7 → G5 (remotes and GitHub are
more useful day to day than the log graph and conflict tools).
