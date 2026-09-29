# Search and replace

## In the file (`Ctrl+F`, `Ctrl+H`)

The find bar appears above the status bar. Matches are highlighted in the
text, the scrollbar and the minimap, and the bar shows "3 of 17". `Enter` /
`Shift+Enter` and `F3` / `Shift+F3` step through matches; `Escape` returns
to the editor at the current match.

| Keys | Option |
|---|---|
| `Alt+R` | Regular expression (Rust `regex` syntax; errors are shown in the bar instead of searching) |
| `Alt+C` | Case sensitive |
| `Alt+W` | Whole word |
| `Ctrl+E` | Use the selection as the search text |
| `Ctrl+F3` | Find the word under the caret |
| **Find in Selection** | Restrict the search to the selected range |

Replace (`Ctrl+H`) adds a replacement field: `Ctrl+Shift+H` replaces the
current match and moves on, `Ctrl+Alt+Enter` replaces all in one undo step.
Replacements support `$1` and `${name}` groups in regex mode and preserve
case when the search is case-insensitive (`word` → `Word` stays capitalised).
**Find All** turns every match into a cursor.

Searches are incremental; on very large files they are bounded so the
editor stays responsive, and the bar says when the count is capped.

## Across the project (`Ctrl+Shift+F`)

**Find in Files** searches the open folders in the background using the
same syntax plus include/exclude globs (`*.rs, !target/**`). Results are
grouped by file with context lines; click a result to open it at the match,
or use the results panel's **Next/Previous** entries. Binary files and the
`exclude_patterns` from settings are skipped, and `.gitignore` is respected.

**Replace in Files** previews the count per file and asks for confirmation.
Every changed file is written atomically; files open in the editor are
updated in place and keep their undo history.
