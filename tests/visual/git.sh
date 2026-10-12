#!/usr/bin/env bash
# Scripted Git integration run: drives the real editor under Xvfb against a
# throw-away repository (with a bare "remote") and checks the repository
# state with git after each step. Screenshots go to $OUT for review.
#
# Usage: tests/visual/git.sh [output-dir]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$ROOT/target/git-e2e}"
rm -rf "${OUT:?}" && mkdir -p "$OUT"
BIN="$ROOT/target/debug/apex"
[ -x "$BIN" ] || cargo build -q --bin apex
export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}
export WGPU_BACKEND=vulkan
export APEX_CONFIG_DIR="$OUT/config" APEX_DATA_DIR="$OUT/data" APEX_RUNTIME_DIR="$OUT/run" APEX_LOG=warn
export DISPLAY=:${GIT_E2E_DISPLAY:-79}
export GIT_AUTHOR_NAME=Tester GIT_AUTHOR_EMAIL=t@example.com GIT_COMMITTER_NAME=Tester GIT_COMMITTER_EMAIL=t@example.com

REPO="$OUT/repo"
REMOTE="$OUT/remote.git"
git init -q --bare -b main "$REMOTE"
git init -q -b main "$REPO"
git -C "$REPO" config commit.gpgsign false
seq -f 'line %g' 1 20 > "$REPO/f.txt"
git -C "$REPO" add f.txt && git -C "$REPO" commit -q -m "first commit"
git -C "$REPO" remote add origin "$REMOTE"
git -C "$REPO" push -q -u origin main
# Look like GitHub to Apex while git really talks to the local bare repository.
git -C "$REPO" remote set-url origin https://github.com/tester/repo.git
git -C "$REPO" config url."$REMOTE".insteadOf https://github.com/tester/repo.git
# A stand-in for the GitHub CLI: records its arguments, returns canned JSON.
GH="$OUT/fake-gh"
cat > "$GH" <<'SH'
#!/bin/sh
echo "$*" >> "$(dirname "$0")/gh.log"
case "$1 $2" in
  "--version "*) echo "gh version 2.99.0" ;;
  "pr list") echo '[{"number":7,"title":"Faster search","headRefName":"feature/fast-search","url":"https://github.com/tester/repo/pull/7","isDraft":false,"state":"OPEN","author":{"login":"ada"}}]' ;;
  "pr view") echo '{"number":7,"title":"Faster search","headRefName":"feature/fast-search","url":"https://github.com/tester/repo/pull/7","isDraft":false,"state":"OPEN","author":{"login":"ada"},"reviewDecision":"APPROVED","statusCheckRollup":[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://github.com/x"}]}' ;;
  "pr checkout") exit 0 ;;
  "pr create") cat > "$(dirname "$0")/pr-body.txt"; echo "https://github.com/tester/repo/pull/8" ;;
  "pr diff") printf 'diff --git a/x b/x\n+added\n' ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
SH
chmod +x "$GH"
export APEX_GH="$GH"
# Two separate changes: line 2 and line 15.
sed -i 's/^line 2$/LINE TWO/; s/^line 15$/LINE FIFTEEN/' "$REPO/f.txt"
mkdir -p "$APEX_DATA_DIR" && printf '["%s"]' "$REPO" > "$APEX_DATA_DIR/trusted.json"

Xvfb "$DISPLAY" -screen 0 1280x800x24 >/dev/null 2>&1 &
XPID=$!
APID=""
trap 'kill $APID 2>/dev/null; kill $XPID 2>/dev/null' EXIT
sleep 1
"$BIN" --new-window "$REPO" "$REPO/f.txt" > "$OUT/app.log" 2>&1 &
APID=$!
shot() { import -window root "$OUT/$1.png"; }
key() { xdotool key --delay 60 "$@"; sleep 0.3; }
type_text() { xdotool type --delay 30 "$1"; sleep 0.3; }
command() { key ctrl+shift+p; sleep 1; xdotool type --delay 80 "$1"; sleep 1; key Return; sleep 2; }
fail() { echo "FAIL: $*"; shot failure; exit 1; }
pass() { echo "ok - $*"; }

WID=""
for _ in $(seq 1 60); do
  WID=$( (xdotool search --onlyvisible --name "Apex" 2>/dev/null || true) | head -1)
  [ -n "$WID" ] && break
  sleep 0.5
done
[ -n "$WID" ] || fail "window did not appear"
sleep 2
xdotool windowfocus --sync "$WID" 2>/dev/null || { sleep 1; xdotool windowfocus --sync "$WID"; }
xdotool mousemove 700 300 click 1; sleep 0.3

# G1: stage the change at the caret (line 2) only.
key ctrl+Home Down
command "Stage Change at Caret"
staged=$(git -C "$REPO" diff --cached --name-only)
[ "$staged" = "f.txt" ] || fail "nothing staged"
git -C "$REPO" diff --cached | grep -q '^+LINE TWO' || fail "line 2 not staged"
git -C "$REPO" diff --cached | grep -q 'FIFTEEN' && fail "line 15 staged too"
git -C "$REPO" diff | grep -q '^+LINE FIFTEEN' || fail "line 15 should remain unstaged"
pass "stage change at caret"

# Unstage the file, then stage the selected line 15 only.
command "Unstage File"
[ -z "$(git -C "$REPO" diff --cached --name-only)" ] || fail "unstage file"
pass "unstage file"
key ctrl+g; type_text "15"; key Return
command "Stage Selected Lines"
git -C "$REPO" diff --cached | grep -q '^+LINE FIFTEEN' || fail "line 15 not staged"
git -C "$REPO" diff --cached | grep -q 'LINE TWO' && fail "line 2 staged by selection"
pass "stage selected lines"

command "Show Git Output"
shot g1-git-output

# G2: commit the staged line from the Source Control panel's message box.
before=$(git -C "$REPO" rev-list --count HEAD)
command "Commit"            # empty message: focuses the commit box
type_text "Shout line fifteen"; sleep 0.7
key ctrl+Return
sleep 2
[ "$(git -C "$REPO" log -1 --format=%s)" = "Shout line fifteen" ] || fail "commit message"
[ "$(git -C "$REPO" rev-list --count HEAD)" = $((before + 1)) ] || fail "commit count"
git -C "$REPO" diff | grep -q '^+LINE TWO' || fail "unstaged change was committed"
pass "commit from the panel"
shot g2-after-commit
command "Commit"
type_text "Shout line 15"
command "Amend Last Commit"
[ "$(git -C "$REPO" log -1 --format=%s)" = "Shout line 15" ] || fail "amend message"
[ "$(git -C "$REPO" rev-list --count HEAD)" = $((before + 1)) ] || fail "amend created a commit"
pass "amend"
# G4: blame column, then click line 1's annotation to open its commit.
xdotool mousemove 700 300 click 1; sleep 0.3
key ctrl+k ctrl+g
sleep 3
shot g4-blame
xdotool mousemove 300 62 click 1; sleep 1.5
title=$(xdotool getwindowname "$WID")
case "$title" in *commit*) pass "blame opens the commit ($title)";; *) fail "commit view not opened: $title";; esac
shot g4-commit-view
key ctrl+w
# G3: side-by-side changes of f.txt; stage the line-2 change from the strip.
xdotool mousemove 700 300 click 1; sleep 0.3
command "Open Changes"
sleep 1.5
shot g3-side-by-side
xdotool mousemove 770 107 click 1; sleep 2.5
git -C "$REPO" diff --cached | grep -q '^+LINE TWO' || fail "stage from the diff view"
pass "side-by-side diff: stage a change from the strip"
shot g3-after-stage
key ctrl+w; sleep 0.5

# G6: push the local commits, then pull a commit made in another clone.
command "Push"
sleep 2
[ "$(git -C "$REMOTE" log -1 --format=%s main)" = "$(git -C "$REPO" log -1 --format=%s)" ] || fail "push"
pass "push"
OTHER="$OUT/other"
git clone -q "$REMOTE" "$OTHER"
echo other > "$OTHER/other.txt"
git -C "$OTHER" add other.txt && git -C "$OTHER" commit -q -m "From another clone" && git -C "$OTHER" push -q
command "Pull"
sleep 2
[ "$(git -C "$REPO" log -1 --format=%s)" = "From another clone" ] || fail "pull"
pass "pull (fast-forward)"

# G7: GitHub through the (fake) GitHub CLI.
command "Refresh Pull Request Status"
sleep 1
grep -q "^pr view --json" "$OUT/gh.log" || fail "pr status not requested"
shot g7-pr-status
command "Pull Requests…"
sleep 4
shot g7-pr-list
key Return; sleep 2      # pull request #7 -> its actions
key Return; sleep 1.5    # "Check out"
grep -q "^pr checkout 7" "$OUT/gh.log" || fail "pr checkout"
pass "pull request list and check out"
git -C "$REPO" switch -q -c feature/fast-search
echo fast > "$REPO/search.txt" && git -C "$REPO" add search.txt && git -C "$REPO" commit -q -m "Make search faster" -m "Uses an index."
sleep 5
command "Create Pull Request"
sleep 1
shot g7-create-pr
xdotool mousemove 387 548 click 1; sleep 4
grep -q "^pr create --title Make search faster --body-file - --base main --head feature/fast-search" "$OUT/gh.log" || fail "pr create arguments"
[ "$(cat "$OUT/pr-body.txt")" = "Uses an index." ] || fail "pr body"
git -C "$REMOTE" rev-parse -q --verify refs/heads/feature/fast-search >/dev/null || fail "branch not published before creating the PR"
pass "create pull request (published the branch first)"
git -C "$REPO" switch -q main
sleep 5

# G5: log, branches, stash, a merge conflict resolved in the editor.
command "Show Log"
sleep 3
shot g5-log
key Escape; sleep 0.3
xdotool mousemove 700 300 click 1; sleep 0.3
command "Switch Branch…"
type_text "fast-search"; sleep 0.5; key Return; sleep 2.5
[ "$(git -C "$REPO" branch --show-current)" = "feature/fast-search" ] || fail "switch branch"
command "Switch Branch…"
type_text "main"; sleep 0.5; key Return; sleep 2.5
[ "$(git -C "$REPO" branch --show-current)" = "main" ] || fail "switch back"
pass "switch branch"
command "Create Branch…"
type_text "topic/e2e"; key Return; sleep 2.5
[ "$(git -C "$REPO" branch --show-current)" = "topic/e2e" ] || fail "create branch"
git -C "$REPO" switch -q main; sleep 4
pass "create branch"
echo scratch > "$REPO/scratch.txt"; sleep 1
command "Stash Changes…"
type_text "e2e stash"; key Return; sleep 2.5
git -C "$REPO" stash list | grep -q "e2e stash" || fail "stash"
[ ! -e "$REPO/scratch.txt" ] || fail "stash kept the untracked file"
command "Stashes…"
sleep 0.5; key Return; sleep 1; key Return; sleep 2.5   # stash@{0} -> Apply
[ -e "$REPO/scratch.txt" ] || fail "stash apply"
git -C "$REPO" stash drop -q && rm -f "$REPO/scratch.txt"
pass "stash and apply"
printf 'one\ntwo\n' > "$REPO/c.txt" && git -C "$REPO" add c.txt && git -C "$REPO" commit -q -m "add c"
git -C "$REPO" switch -q -c clash && printf 'CLASH ONE\ntwo\n' > "$REPO/c.txt" && git -C "$REPO" commit -q -am "clash"
git -C "$REPO" switch -q main && printf 'MAIN ONE\ntwo\n' > "$REPO/c.txt" && git -C "$REPO" commit -q -am "main one"
sleep 5
command "Merge Branch into Current…"
type_text "clash"; sleep 0.5; key Return; sleep 3
grep -q '^<<<<<<<' "$REPO/c.txt" || fail "merge did not conflict"
xdotool mousemove 89 101 click 1; sleep 2   # c.txt in the folder tree
xdotool mousemove 700 300 click 1; sleep 0.3
key ctrl+Home; sleep 0.3
shot g5-conflict
command "Accept Incoming Change"
key ctrl+s; sleep 1
[ "$(cat "$REPO/c.txt")" = "$(printf 'CLASH ONE\ntwo')" ] || fail "accept incoming: $(cat "$REPO/c.txt")"
command "Mark Resolved"
sleep 4
command "Continue Merge/Rebase"
sleep 3
[ "$(git -C "$REPO" log -1 --format=%P | wc -w)" = 2 ] || fail "merge not concluded"
pass "merge conflict resolved in the editor and continued"
key ctrl+w; sleep 0.5

# History editor: replace the author of every commit, apply, check git.
git -C "$REPO" add -A && git -C "$REPO" commit -q --allow-empty -m "Work in progress"
command "Edit Commit History"
sleep 2
shot h1-history-editor
xdotool mousemove 517 100 click 1; sleep 1
shot h2-replace-author
xdotool mousemove 637 375 click 1; type_text "tester"
xdotool mousemove 637 398 click 1; type_text "Renamed Person"
xdotool mousemove 637 421 click 1; type_text "renamed@example.com"
xdotool mousemove 460 467 click 1; sleep 1
xdotool mousemove 107 125 click 1; sleep 1
shot h3-confirm
key Return; sleep 5
authors=$(git -C "$REPO" log --branches --format=%an | sort -u)
[ "$authors" = "Renamed Person" ] || fail "history rewrite: authors are $authors"
tag=$(git -C "$REPO" tag -l 'apex-backup-*' | head -1)
[ -n "$tag" ] || fail "no backup tag"
[ "$(git -C "$REPO" log -1 --format=%an "$tag")" = "Tester" ] || fail "backup tag was rewritten"
[ "$(git -C "$REPO" log --format=%ae -1)" = "renamed@example.com" ] || fail "author email"
pass "history editor rewrites authors (backup tag kept)"
shot h4-after-rewrite
echo new > "$REPO/new.txt"
sleep 5
xdotool mousemove 150 306; sleep 1
shot g2-panel-hover
grep -q "fatal\|panicked" "$OUT/app.log" && fail "errors in app log"
echo "all git checks passed"
