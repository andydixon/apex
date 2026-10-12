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
command() { key ctrl+shift+p; sleep 0.6; xdotool type --delay 60 "$1"; sleep 0.8; key Return; sleep 1.8; }
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

# History editor: replace the author of every commit, apply, check git.
git -C "$REPO" add -A && git -C "$REPO" commit -q -m "Work in progress"
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
xdotool mousemove 410 457 click 1; sleep 5
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
