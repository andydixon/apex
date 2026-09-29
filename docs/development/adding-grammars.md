# Adding a built-in syntax grammar

Grammars are compiled into `apex-syntax`; nothing is loaded from disk at
run time. Adding a language is four edits and a test run.

1. **Add the grammar crate** to `crates/apex-syntax/Cargo.toml`. Use a
   crate that targets tree-sitter 0.27 (`tree-sitter-<lang>`); check its
   licence is MIT/Apache-2.0 compatible with AGPL-3.0-or-later and note it in
   `CONTRIBUTING.md`'s dependency list.

2. **Declare the language function** near the top of
   `crates/apex-syntax/src/language.rs`:

   ```rust
   lang_fn!(l_zig, tree_sitter_zig::LANGUAGE);
   ```

3. **Add an entry** to the `LANGUAGES` table in the same file with the
   `def!` macro. The fields are: id, display name, extensions, exact file
   names, shebang programs, line comment, block comment, grammar (queries)
   and the node kinds that fold:

   ```rust
   def!("zig", "Zig", ext: ["zig"], files: [], shebang: [], line: Some("// "), block: None,
       grammar: Some(Grammar {
           language: l_zig,
           highlights: tree_sitter_zig::HIGHLIGHTS_QUERY,
           injections: None,
           tags: None, // symbols (Goto Symbol) come from TAGS_QUERY when the crate has one
       }),
       fold: &["block", "struct_declaration"]),
   ```

   If the crate ships no highlight query, write one in
   `assets/grammars/<lang>/highlights.scm` and reference it with
   `include_str!` (Kotlin is the example). Capture names must be the
   standard tree-sitter ones (`keyword`, `string`, `comment`, `function`,
   `type`, `variable.parameter`, …); themes map those scopes to colours, so
   no theme changes are needed. Only captures that exist in the grammar's
   `node-types.json` are allowed; the test in step 4 catches the rest.

   Languages without a usable grammar can use the declarative fallback
   (second `def!` form) as `Dockerfile` does:

   ```rust
   def!("ini", "INI", ext: ["ini", "cfg"], files: [], shebang: [], line: Some("; "), block: None,
       simple: SimpleSyntax { keywords: &[], case_insensitive: true, line_comment: Some(";"), strings: &['"'], section_prefix: Some('[') }),
   ```

4. **Run the tests**: `cargo test -p apex-syntax`.
   `all_grammars_load_and_queries_compile` compiles every query against its
   grammar, so a capture that does not exist fails here rather than at run
   time; `ids_unique` guards the table. Add a case to
   `detects_by_name_extension_shebang_and_content` for unusual file names,
   then open a sample file in the editor to check colours and folding.

Injections (code fences in Markdown, `<script>` in HTML) are declared by
the grammar's `INJECTIONS_QUERY`; the highlighter supports one level of
injection and looks the injected language up by the name in the query
(`#set! injection.language "javascript"`), so the id in `LANGUAGES` must
match what the query uses.

Finally add the language to the table in `FEATURE_MATRIX.md`.
