//! The command registry: every user-invocable action has a stable identifier,
//! a human-readable name and a category. Menus, key bindings, the command
//! palette and macros all dispatch through [`Command`].

use serde::{Deserialize, Serialize};

macro_rules! commands {
    ($( $variant:ident => $id:literal, $name:literal, $cat:ident ; )*) => {
        /// A built-in command. Arguments, where needed, are carried in [`CommandArgs`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Command { $( $variant, )* }

        impl Command {
            pub const ALL: &'static [Command] = &[ $( Command::$variant, )* ];

            /// Stable identifier used in key maps, macros and the palette.
            pub const fn id(self) -> &'static str {
                match self { $( Command::$variant => $id, )* }
            }

            pub const fn name(self) -> &'static str {
                match self { $( Command::$variant => $name, )* }
            }

            pub const fn category(self) -> Category {
                match self { $( Command::$variant => Category::$cat, )* }
            }

            pub fn from_id(id: &str) -> Option<Command> {
                match id { $( $id => Some(Command::$variant), )* _ => None }
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    File,
    Edit,
    Selection,
    Find,
    View,
    Goto,
    Tools,
    Project,
    Git,
    Window,
    Help,
}

impl Category {
    pub const fn label(self) -> &'static str {
        match self {
            Self::File => "File",
            Self::Edit => "Edit",
            Self::Selection => "Selection",
            Self::Find => "Find",
            Self::View => "View",
            Self::Goto => "Goto",
            Self::Tools => "Tools",
            Self::Project => "Project",
            Self::Git => "Git",
            Self::Window => "Window",
            Self::Help => "Help",
        }
    }
}

commands! {
    // File
    NewFile => "file.new", "New File", File;
    OpenFile => "file.open", "Open File…", File;
    OpenFolder => "file.open_folder", "Open Folder…", File;
    OpenFolderByPath => "file.open_folder_path", "Open Folder by Path…", File;
    OpenRecent => "file.open_recent", "Open Recent…", File;
    ReopenClosed => "file.reopen_closed", "Reopen Closed File", File;
    Save => "file.save", "Save", File;
    SaveAs => "file.save_as", "Save As…", File;
    SaveAll => "file.save_all", "Save All", File;
    Revert => "file.revert", "Revert File", File;
    RenameFile => "file.rename", "Rename File…", File;
    DeleteFile => "file.delete", "Delete File…", File;
    DuplicateFile => "file.duplicate", "Duplicate File", File;
    CloseTab => "file.close", "Close File", File;
    CloseOtherTabs => "file.close_others", "Close Other Files", File;
    CloseTabsToRight => "file.close_to_right", "Close Files to the Right", File;
    CloseAllTabs => "file.close_all", "Close All Files", File;
    CloseWindow => "window.close", "Close Window", File;
    NewWindow => "window.new", "New Window", File;
    Quit => "app.quit", "Quit", File;
    CopyFilePath => "file.copy_path", "Copy File Path", File;
    RevealInFileManager => "file.reveal", "Reveal in File Manager", File;
    ReloadFromDisk => "file.reload", "Reload from Disk", File;
    SetEncoding => "file.set_encoding", "Save with Encoding…", File;
    ReloadWithEncoding => "file.reload_with_encoding", "Reload with Encoding…", File;
    SetLineEnding => "file.set_line_ending", "Set Line Endings…", File;
    SetLanguage => "file.set_language", "Set Syntax…", File;
    Print => "file.print", "Print…", File;

    // Edit
    Undo => "edit.undo", "Undo", Edit;
    Redo => "edit.redo", "Redo", Edit;
    Cut => "edit.cut", "Cut", Edit;
    Copy => "edit.copy", "Copy", Edit;
    Paste => "edit.paste", "Paste", Edit;
    PasteAndIndent => "edit.paste_indent", "Paste and Indent", Edit;
    PasteFromHistory => "edit.paste_history", "Paste from History…", Edit;
    DuplicateLine => "edit.duplicate", "Duplicate Line / Selection", Edit;
    DeleteLine => "edit.delete_line", "Delete Line", Edit;
    MoveLineUp => "edit.move_line_up", "Move Line Up", Edit;
    MoveLineDown => "edit.move_line_down", "Move Line Down", Edit;
    JoinLines => "edit.join_lines", "Join Lines", Edit;
    SplitLines => "edit.split_lines", "Split Selection into Lines", Edit;
    TransposeChars => "edit.transpose", "Transpose Characters", Edit;
    SortLines => "edit.sort_lines", "Sort Lines", Edit;
    SortLinesCaseInsensitive => "edit.sort_lines_ci", "Sort Lines (Case Insensitive)", Edit;
    SortLinesReverse => "edit.sort_lines_reverse", "Sort Lines (Reverse)", Edit;
    UniqueLines => "edit.unique_lines", "Remove Duplicate Lines", Edit;
    ReverseLines => "edit.reverse_lines", "Reverse Lines", Edit;
    ShuffleLines => "edit.shuffle_lines", "Shuffle Lines", Edit;
    TrimTrailingWhitespace => "edit.trim_whitespace", "Trim Trailing Whitespace", Edit;
    ConvertIndentToSpaces => "edit.indent_to_spaces", "Convert Indentation to Spaces", Edit;
    ConvertIndentToTabs => "edit.indent_to_tabs", "Convert Indentation to Tabs", Edit;
    Reindent => "edit.reindent", "Reindent Lines", Edit;
    ToggleComment => "edit.toggle_comment", "Toggle Line Comment", Edit;
    ToggleBlockComment => "edit.toggle_block_comment", "Toggle Block Comment", Edit;
    InsertDateTime => "edit.insert_datetime", "Insert Date and Time", Edit;
    InsertSequence => "edit.insert_sequence", "Insert Number Sequence…", Edit;
    UpperCase => "edit.upper_case", "Upper Case", Edit;
    LowerCase => "edit.lower_case", "Lower Case", Edit;
    TitleCase => "edit.title_case", "Title Case", Edit;
    SwapCase => "edit.swap_case", "Swap Case", Edit;
    SnakeCase => "edit.snake_case", "snake_case", Edit;
    CamelCase => "edit.camel_case", "camelCase", Edit;
    KebabCase => "edit.kebab_case", "kebab-case", Edit;
    Indent => "edit.indent", "Indent", Edit;
    Outdent => "edit.outdent", "Outdent", Edit;
    InsertTab => "edit.tab", "Tab", Edit;
    InsertNewline => "edit.newline", "New Line", Edit;
    InsertNewlineBelow => "edit.newline_below", "Insert Line Below", Edit;
    InsertNewlineAbove => "edit.newline_above", "Insert Line Above", Edit;
    DeleteBackward => "edit.backspace", "Backspace", Edit;
    DeleteForward => "edit.delete", "Delete", Edit;
    DeleteWordBackward => "edit.delete_word_backward", "Delete Word Backward", Edit;
    DeleteWordForward => "edit.delete_word_forward", "Delete Word Forward", Edit;
    DeleteToLineStart => "edit.delete_to_line_start", "Delete to Start of Line", Edit;
    DeleteToLineEnd => "edit.delete_to_line_end", "Delete to End of Line", Edit;
    ToggleOverwrite => "edit.toggle_overwrite", "Toggle Overwrite Mode", Edit;
    HardWrap => "edit.hard_wrap", "Wrap Paragraph at Ruler", Edit;
    SurroundWith => "edit.surround", "Surround Selection With…", Edit;
    ToggleBookmark => "edit.toggle_bookmark", "Toggle Bookmark", Edit;
    NextBookmark => "edit.next_bookmark", "Next Bookmark", Edit;
    PrevBookmark => "edit.prev_bookmark", "Previous Bookmark", Edit;
    ClearBookmarks => "edit.clear_bookmarks", "Clear Bookmarks", Edit;
    SetMark => "edit.set_mark", "Set Named Mark…", Edit;
    GotoMark => "edit.goto_mark", "Go to Named Mark…", Edit;
    InsertSnippet => "edit.insert_snippet", "Insert Snippet…", Edit;
    TriggerCompletion => "edit.complete", "Autocomplete", Edit;
    NormaliseUnicode => "edit.normalise_unicode", "Normalise Unicode (NFC)", Edit;
    RecordMacro => "macro.record", "Start/Stop Recording Macro", Tools;
    PlayMacro => "macro.play", "Replay Macro", Tools;
    SaveMacro => "macro.save", "Save Macro…", Tools;
    RunNamedMacro => "macro.run", "Run Saved Macro…", Tools;
    FormatDocument => "edit.format", "Format Document", Edit;

    // Cursor movement
    MoveLeft => "cursor.left", "Move Left", Selection;
    MoveRight => "cursor.right", "Move Right", Selection;
    MoveUp => "cursor.up", "Move Up", Selection;
    MoveDown => "cursor.down", "Move Down", Selection;
    MoveWordLeft => "cursor.word_left", "Move Word Left", Selection;
    MoveWordRight => "cursor.word_right", "Move Word Right", Selection;
    MoveSubwordLeft => "cursor.subword_left", "Move Subword Left", Selection;
    MoveSubwordRight => "cursor.subword_right", "Move Subword Right", Selection;
    MoveLineStart => "cursor.line_start", "Move to Start of Line", Selection;
    MoveLineEnd => "cursor.line_end", "Move to End of Line", Selection;
    MoveDocStart => "cursor.doc_start", "Move to Start of Document", Selection;
    MoveDocEnd => "cursor.doc_end", "Move to End of Document", Selection;
    MovePageUp => "cursor.page_up", "Page Up", Selection;
    MovePageDown => "cursor.page_down", "Page Down", Selection;
    MoveParagraphUp => "cursor.paragraph_up", "Move to Previous Paragraph", Selection;
    MoveParagraphDown => "cursor.paragraph_down", "Move to Next Paragraph", Selection;
    MoveToBracket => "cursor.bracket", "Jump to Matching Bracket", Selection;
    SelectLeft => "select.left", "Select Left", Selection;
    SelectRight => "select.right", "Select Right", Selection;
    SelectUp => "select.up", "Select Up", Selection;
    SelectDown => "select.down", "Select Down", Selection;
    SelectWordLeft => "select.word_left", "Select Word Left", Selection;
    SelectWordRight => "select.word_right", "Select Word Right", Selection;
    SelectSubwordLeft => "select.subword_left", "Select Subword Left", Selection;
    SelectSubwordRight => "select.subword_right", "Select Subword Right", Selection;
    SelectLineStart => "select.line_start", "Select to Start of Line", Selection;
    SelectLineEnd => "select.line_end", "Select to End of Line", Selection;
    SelectDocStart => "select.doc_start", "Select to Start of Document", Selection;
    SelectDocEnd => "select.doc_end", "Select to End of Document", Selection;
    SelectPageUp => "select.page_up", "Select Page Up", Selection;
    SelectPageDown => "select.page_down", "Select Page Down", Selection;
    SelectParagraphUp => "select.paragraph_up", "Select to Previous Paragraph", Selection;
    SelectParagraphDown => "select.paragraph_down", "Select to Next Paragraph", Selection;
    SelectToBracket => "select.bracket", "Select to Matching Bracket", Selection;
    SelectAll => "select.all", "Select All", Selection;
    SelectLine => "select.line", "Expand Selection to Line", Selection;
    SelectWord => "select.word", "Expand Selection to Word", Selection;
    SelectScope => "select.scope", "Expand Selection to Scope", Selection;
    SelectBrackets => "select.brackets", "Expand Selection to Brackets", Selection;
    SelectIndentation => "select.indentation", "Expand Selection to Indentation", Selection;
    SelectParagraph => "select.paragraph", "Expand Selection to Paragraph", Selection;
    ShrinkSelection => "select.shrink", "Shrink Selection", Selection;
    SwapAnchor => "select.swap_anchor", "Swap Selection Anchor and Head", Selection;
    AddCursorAbove => "select.add_cursor_above", "Add Cursor Above", Selection;
    AddCursorBelow => "select.add_cursor_below", "Add Cursor Below", Selection;
    AddNextOccurrence => "select.add_next", "Quick Add Next", Selection;
    SkipNextOccurrence => "select.skip_next", "Quick Skip Next", Selection;
    AddAllOccurrences => "select.add_all", "Quick Find All", Selection;
    UndoSelection => "select.undo", "Undo Last Selection", Selection;
    SingleSelection => "select.single", "Single Selection", Selection;
    SplitSelectionIntoLines => "select.split_lines", "Split into Lines", Selection;
    ColumnSelectUp => "select.column_up", "Column Select Up", Selection;
    ColumnSelectDown => "select.column_down", "Column Select Down", Selection;
    SelectNextCursor => "select.rotate_next", "Rotate Primary Cursor Forward", Selection;
    SelectPrevCursor => "select.rotate_prev", "Rotate Primary Cursor Backward", Selection;
    InvertSelection => "select.invert", "Invert Selection", Selection;

    // Find
    Find => "find.show", "Find…", Find;
    Replace => "find.replace", "Replace…", Find;
    FindNext => "find.next", "Find Next", Find;
    FindPrev => "find.prev", "Find Previous", Find;
    FindUnderCursor => "find.under_cursor", "Find Word Under Cursor", Find;
    FindInFiles => "find.in_files", "Find in Files…", Find;
    ReplaceNext => "find.replace_next", "Replace Next", Find;
    ReplaceAll => "find.replace_all", "Replace All", Find;
    ToggleRegex => "find.toggle_regex", "Toggle Regular Expression", Find;
    ToggleCaseSensitive => "find.toggle_case", "Toggle Case Sensitive", Find;
    ToggleWholeWord => "find.toggle_word", "Toggle Whole Word", Find;
    UseSelectionForFind => "find.use_selection", "Use Selection for Find", Find;
    FindInSelection => "find.in_selection", "Find in Selection", Find;
    CloseFind => "find.close", "Close Find", Find;

    // View
    ToggleSidebar => "view.toggle_sidebar", "Toggle Sidebar", View;
    ToggleMinimap => "view.toggle_minimap", "Toggle Minimap", View;
    ToggleLineNumbers => "view.toggle_line_numbers", "Toggle Line Numbers", View;
    ToggleRelativeLineNumbers => "view.toggle_relative_line_numbers", "Toggle Relative Line Numbers", View;
    ToggleWordWrap => "view.toggle_word_wrap", "Toggle Word Wrap", View;
    ToggleWhitespace => "view.toggle_whitespace", "Toggle Whitespace Rendering", View;
    ToggleIndentGuides => "view.toggle_indent_guides", "Toggle Indent Guides", View;
    ToggleStatusBar => "view.toggle_status_bar", "Toggle Status Bar", View;
    ToggleTabs => "view.toggle_tabs", "Toggle Tab Bar", View;
    ToggleMenuBar => "view.toggle_menu_bar", "Toggle Menu Bar", View;
    TogglePanel => "view.toggle_panel", "Toggle Bottom Panel", View;
    ToggleFullscreen => "view.fullscreen", "Toggle Full Screen", View;
    ToggleDistractionFree => "view.distraction_free", "Toggle Distraction-Free Mode", View;
    ToggleZenMode => "view.zen", "Toggle Zen Mode", View;
    ToggleLowDistraction => "view.low_distraction", "Toggle Low-Distraction Mode", View;
    ToggleTypewriter => "view.typewriter", "Toggle Typewriter Scrolling", View;
    ZoomIn => "view.zoom_in", "Increase Font Size", View;
    ZoomOut => "view.zoom_out", "Decrease Font Size", View;
    ZoomReset => "view.zoom_reset", "Reset Font Size", View;
    UiScaleUp => "view.ui_scale_up", "Increase Interface Scale", View;
    UiScaleDown => "view.ui_scale_down", "Decrease Interface Scale", View;
    SelectTheme => "view.theme", "Select Colour Scheme…", View;
    ToggleLightDark => "view.toggle_light_dark", "Toggle Light/Dark Theme", View;
    FoldCurrent => "fold.current", "Fold", View;
    UnfoldCurrent => "fold.unfold", "Unfold", View;
    FoldAll => "fold.all", "Fold All", View;
    UnfoldAll => "fold.unfold_all", "Unfold All", View;
    FoldLevel1 => "fold.level1", "Fold Level 1", View;
    FoldLevel2 => "fold.level2", "Fold Level 2", View;
    FoldLevel3 => "fold.level3", "Fold Level 3", View;
    FoldComments => "fold.comments", "Fold All Comments", View;
    ToggleFold => "fold.toggle", "Toggle Fold", View;
    ScrollLineUp => "view.scroll_up", "Scroll Line Up", View;
    ScrollLineDown => "view.scroll_down", "Scroll Line Down", View;
    ScrollToCursor => "view.scroll_to_cursor", "Scroll to Cursor", View;
    CentreCursor => "view.centre_cursor", "Centre Cursor in View", View;
    SplitVertical => "pane.split_vertical", "Split Pane Right", View;
    SplitHorizontal => "pane.split_horizontal", "Split Pane Down", View;
    LayoutSingle => "layout.single", "Layout: Single", View;
    LayoutColumns2 => "layout.columns2", "Layout: Two Columns", View;
    LayoutColumns3 => "layout.columns3", "Layout: Three Columns", View;
    LayoutRows2 => "layout.rows2", "Layout: Two Rows", View;
    LayoutGrid => "layout.grid", "Layout: 2×2 Grid", View;
    ClosePane => "pane.close", "Close Pane", View;
    FocusNextPane => "pane.focus_next", "Focus Next Pane", View;
    FocusPrevPane => "pane.focus_prev", "Focus Previous Pane", View;
    FocusPane1 => "pane.focus1", "Focus Pane 1", View;
    FocusPane2 => "pane.focus2", "Focus Pane 2", View;
    FocusPane3 => "pane.focus3", "Focus Pane 3", View;
    FocusPane4 => "pane.focus4", "Focus Pane 4", View;
    MoveTabToNextPane => "pane.move_tab_next", "Move File to Next Pane", View;
    MoveTabToPrevPane => "pane.move_tab_prev", "Move File to Previous Pane", View;
    DuplicateView => "pane.duplicate_view", "Open View in Next Pane", View;
    ToggleSpellCheck => "view.spell_check", "Toggle Spell Check", View;
    ShowOutline => "view.outline", "Toggle Document Outline", View;
    ToggleMutedSyntax => "view.muted_syntax", "Toggle Muted Syntax Colours", View;

    // Goto
    CommandPalette => "goto.commands", "Command Palette…", Goto;
    GotoAnything => "goto.anything", "Goto Anything…", Goto;
    GotoLine => "goto.line", "Go to Line…", Goto;
    GotoSymbol => "goto.symbol", "Go to Symbol…", Goto;
    GotoSymbolInProject => "goto.project_symbol", "Go to Symbol in Project…", Goto;
    GotoDefinition => "goto.definition", "Go to Definition", Goto;
    GotoReferences => "goto.references", "Go to References", Goto;
    GotoDeclaration => "goto.declaration", "Go to Declaration", Goto;
    GotoImplementation => "goto.implementation", "Go to Implementation", Goto;
    NavigateBack => "goto.back", "Jump Back", Goto;
    NavigateForward => "goto.forward", "Jump Forward", Goto;
    NextTab => "tab.next", "Next File", Goto;
    PrevTab => "tab.prev", "Previous File", Goto;
    Tab1 => "tab.1", "Switch to File 1", Goto;
    Tab2 => "tab.2", "Switch to File 2", Goto;
    Tab3 => "tab.3", "Switch to File 3", Goto;
    Tab4 => "tab.4", "Switch to File 4", Goto;
    Tab5 => "tab.5", "Switch to File 5", Goto;
    Tab6 => "tab.6", "Switch to File 6", Goto;
    Tab7 => "tab.7", "Switch to File 7", Goto;
    Tab8 => "tab.8", "Switch to File 8", Goto;
    Tab9 => "tab.9", "Switch to File 9 (Last)", Goto;
    MoveTabLeft => "tab.move_left", "Move File Left", Goto;
    MoveTabRight => "tab.move_right", "Move File Right", Goto;
    PinTab => "tab.pin", "Pin / Unpin File", Goto;
    NextDiagnostic => "goto.next_diagnostic", "Next Problem", Goto;
    PrevDiagnostic => "goto.prev_diagnostic", "Previous Problem", Goto;
    NextChange => "git.next_hunk", "Next Change", Goto;
    PrevChange => "git.prev_hunk", "Previous Change", Goto;
    RevertHunk => "git.revert_hunk", "Revert Change…", Git;
    ShowHunkDiff => "git.show_hunk", "Show Inline Diff", Git;
    CompareWithSaved => "git.compare_saved", "Compare with Saved", Git;
    CompareWithHead => "git.compare_head", "Compare with HEAD", Git;
    CompareWithIndex => "git.compare_index", "Compare with Index", Git;
    FileHistory => "git.file_history", "File History", Git;
    GitStatusPanel => "git.status", "Git Status", Git;
    GitStageFile => "git.stage_file", "Stage File", Git;
    GitUnstageFile => "git.unstage_file", "Unstage File", Git;
    GitStageHunk => "git.stage_hunk", "Stage Change at Caret", Git;
    GitStageLines => "git.stage_lines", "Stage Selected Lines", Git;
    GitStageAll => "git.stage_all", "Stage All Changes", Git;
    GitUnstageAll => "git.unstage_all", "Unstage All Changes", Git;
    GitDiscardFile => "git.discard_file", "Discard File Changes…", Git;
    GitShowOutput => "git.output", "Show Git Output", Git;
    GitToggleSourceControl => "git.toggle_panel", "Toggle Source Control Panel", Git;
    GitCommit => "git.commit", "Commit", Git;
    GitCommitAmend => "git.commit_amend", "Amend Last Commit", Git;
    GitUndoCommit => "git.undo_commit", "Undo Last Commit…", Git;
    GitOpenChanges => "git.open_changes", "Open Changes", Git;
    GitOpenStagedChanges => "git.open_staged_changes", "Open Staged Changes", Git;
    GitToggleBlame => "git.toggle_blame", "Toggle Blame", Git;
    GitShowCommit => "git.show_commit", "Show Commit", Git;
    GitHistoryEditor => "git.edit_history", "Edit Commit History…", Git;
    GitFetch => "git.fetch", "Fetch", Git;
    GitPull => "git.pull", "Pull", Git;
    GitPush => "git.push", "Push", Git;
    GitForcePush => "git.force_push", "Force Push (with Lease)…", Git;
    GitSync => "git.sync", "Sync (Pull then Push)", Git;
    GitOpenOnRemote => "git.open_on_remote", "Open File on Remote", Git;
    GitCopyPermalink => "git.copy_permalink", "Copy Permalink", Git;
    GitOpenCommitOnRemote => "git.open_commit_on_remote", "Open Commit on Remote", Git;
    GitOpenBranchOnRemote => "git.open_branch_on_remote", "Open Branch on Remote", Git;
    GitCompareOnRemote => "git.compare_on_remote", "Compare Branch on Remote", Git;
    GhCreatePr => "github.create_pr", "Create Pull Request…", Git;
    GhPrStatus => "github.pr_status", "Refresh Pull Request Status", Git;
    GhOpenPr => "github.open_pr", "Open Pull Request in Browser", Git;
    GhListPrs => "github.list_prs", "Pull Requests…", Git;
    GhChecks => "github.checks", "Pull Request Checks…", Git;
    GhPrActions => "github.pr_actions", "Pull Request Actions", Git;
    GhCheckoutPr => "github.checkout_pr", "Check Out Pull Request", Git;
    GhPrDiff => "github.pr_diff", "Show Pull Request Diff", Git;
    GhOpenUrl => "github.open_url", "Open Link", Git;
    GhSubmitPr => "github.submit_pr", "Submit Pull Request", Git;
    GitCancel => "git.cancel", "Cancel Git Command", Git;

    // Tools
    Settings => "tools.settings", "Settings…", Tools;
    OpenSettingsFile => "tools.settings_file", "Open Settings File", Tools;
    OpenKeymapFile => "tools.keymap_file", "Open Key Bindings File", Tools;
    KeyboardShortcuts => "tools.shortcuts", "Keyboard Shortcuts…", Tools;
    BuildSelect => "build.select", "Select Build Task…", Tools;
    Build => "build.run", "Build", Tools;
    BuildCancel => "build.cancel", "Cancel Build", Tools;
    BuildRerun => "build.rerun", "Rerun Last Build", Tools;
    ShowOutput => "panel.output", "Show Output Panel", Tools;
    ShowProblems => "panel.problems", "Show Problems", Tools;
    LspRestart => "lsp.restart", "Restart Language Server", Tools;
    LspRename => "lsp.rename", "Rename Symbol…", Tools;
    LspCodeAction => "lsp.code_action", "Code Actions…", Tools;
    LspHover => "lsp.hover", "Show Hover Information", Tools;
    LspSignatureHelp => "lsp.signature", "Show Signature Help", Tools;
    LspToggle => "lsp.toggle", "Toggle Language Server", Tools;
    ToggleVimMode => "tools.vim", "Toggle Vim Mode", Tools;
    ShowRecovery => "tools.recovery", "Recover Unsaved Files…", Tools;
    ShowClipboardHistory => "tools.clipboard_history", "Clipboard History…", Tools;
    WordCount => "tools.word_count", "Word Count", Tools;

    // Project
    AddFolder => "project.add_folder", "Add Folder to Workspace…", Project;
    RemoveFolder => "project.remove_folder", "Remove Folder from Workspace", Project;
    SaveProject => "project.save", "Save Project As…", Project;
    OpenProject => "project.open", "Open Project…", Project;
    RecentProjects => "project.recent", "Recent Projects…", Project;
    CloseProject => "project.close", "Close Workspace", Project;
    RefreshExplorer => "project.refresh", "Refresh Folders", Project;
    RevealActiveFile => "project.reveal_active", "Reveal Active File in Sidebar", Project;
    FocusSidebar => "project.focus_sidebar", "Focus Sidebar", Project;
    NewFileInFolder => "project.new_file", "New File in Folder…", Project;
    NewFolder => "project.new_folder", "New Folder…", Project;
    TrustWorkspace => "project.trust", "Trust Workspace", Project;
    OpenProjectSettings => "project.settings", "Open Project Settings", Project;

    // Help
    About => "help.about", "About Apex", Help;
    OpenUserGuide => "help.guide", "User Guide", Help;
    ShowKeyboardCheatSheet => "help.cheatsheet", "Keyboard Cheat Sheet", Help;
}

impl Command {
    /// Whether the command only makes sense with an editor view focused.
    pub fn needs_editor(self) -> bool {
        matches!(self.category(), Category::Edit | Category::Selection)
            || matches!(
                self,
                Command::Save
                    | Command::SaveAs
                    | Command::Revert
                    | Command::CloseTab
                    | Command::GotoLine
                    | Command::GotoSymbol
                    | Command::FindNext
                    | Command::FindPrev
                    | Command::Find
                    | Command::Replace
            )
    }

    /// Commands that a macro recording should capture.
    pub fn is_recordable(self) -> bool {
        matches!(self.category(), Category::Edit | Category::Selection | Category::Find)
            && !matches!(self, Command::RecordMacro | Command::PlayMacro | Command::SaveMacro | Command::RunNamedMacro)
    }
}

/// A command together with its optional argument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invocation {
    pub command: Command,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
}

impl From<Command> for Invocation {
    fn from(command: Command) -> Self {
        Self { command, arg: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for c in Command::ALL {
            assert!(seen.insert(c.id()), "duplicate id {}", c.id());
            assert_eq!(Command::from_id(c.id()), Some(*c));
            assert!(!c.name().is_empty());
        }
    }
}
