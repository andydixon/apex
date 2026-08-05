//! The built-in language registry and detection.

use std::path::Path;

/// A declarative fallback highlighter for languages without a usable grammar.
pub struct SimpleSyntax {
    pub keywords: &'static [&'static str],
    pub case_insensitive: bool,
    pub line_comment: Option<&'static str>,
    pub strings: &'static [char],
    /// Lines starting with this (after whitespace) are headings/sections.
    pub section_prefix: Option<char>,
}

/// A tree-sitter grammar and its queries.
pub struct Grammar {
    pub language: fn() -> tree_sitter::Language,
    pub highlights: &'static str,
    pub injections: Option<&'static str>,
    pub tags: Option<&'static str>,
}

pub struct LanguageDef {
    /// Stable identifier (used in settings, overrides and injections).
    pub id: &'static str,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub filenames: &'static [&'static str],
    /// Interpreter names matched against a `#!` line.
    pub shebangs: &'static [&'static str],
    pub line_comment: Option<&'static str>,
    pub block_comment: Option<(&'static str, &'static str)>,
    pub grammar: Option<Grammar>,
    /// Fallback highlighter when `grammar` is `None` (documented in the user guide).
    pub simple: Option<SimpleSyntax>,
    /// Node kinds that produce fold regions in addition to bracketed blocks.
    pub fold_kinds: &'static [&'static str],
}

macro_rules! lang_fn {
    ($name:ident, $e:expr) => {
        fn $name() -> tree_sitter::Language {
            tree_sitter::Language::new($e)
        }
    };
}

lang_fn!(l_rust, tree_sitter_rust::LANGUAGE);
lang_fn!(l_python, tree_sitter_python::LANGUAGE);
lang_fn!(l_js, tree_sitter_javascript::LANGUAGE);
lang_fn!(l_ts, tree_sitter_typescript::LANGUAGE_TYPESCRIPT);
lang_fn!(l_tsx, tree_sitter_typescript::LANGUAGE_TSX);
lang_fn!(l_json, tree_sitter_json::LANGUAGE);
lang_fn!(l_toml, tree_sitter_toml_ng::LANGUAGE);
lang_fn!(l_md, tree_sitter_md::LANGUAGE);
lang_fn!(l_md_inline, tree_sitter_md::INLINE_LANGUAGE);
lang_fn!(l_bash, tree_sitter_bash::LANGUAGE);
lang_fn!(l_c, tree_sitter_c::LANGUAGE);
lang_fn!(l_cpp, tree_sitter_cpp::LANGUAGE);
lang_fn!(l_go, tree_sitter_go::LANGUAGE);
lang_fn!(l_html, tree_sitter_html::LANGUAGE);
lang_fn!(l_css, tree_sitter_css::LANGUAGE);
lang_fn!(l_yaml, tree_sitter_yaml::LANGUAGE);
lang_fn!(l_csharp, tree_sitter_c_sharp::LANGUAGE);
lang_fn!(l_java, tree_sitter_java::LANGUAGE);
lang_fn!(l_kotlin, tree_sitter_kotlin_ng::LANGUAGE);
lang_fn!(l_swift, tree_sitter_swift::LANGUAGE);
lang_fn!(l_php, tree_sitter_php::LANGUAGE_PHP);
lang_fn!(l_ruby, tree_sitter_ruby::LANGUAGE);
lang_fn!(l_lua, tree_sitter_lua::LANGUAGE);
lang_fn!(l_powershell, tree_sitter_powershell::LANGUAGE);
lang_fn!(l_xml, tree_sitter_xml::LANGUAGE_XML);
lang_fn!(l_sql, tree_sitter_sequel::LANGUAGE);
lang_fn!(l_diff, tree_sitter_diff::LANGUAGE);
lang_fn!(l_ini, tree_sitter_ini::LANGUAGE);
lang_fn!(l_make, tree_sitter_make::LANGUAGE);
lang_fn!(l_cmake, tree_sitter_cmake::LANGUAGE);
lang_fn!(l_regex, tree_sitter_regex::LANGUAGE);
fn l_scss() -> tree_sitter::Language {
    tree_sitter_scss::language()
}

const BRACES: &[&str] = &[];

macro_rules! def {
    ($id:literal, $name:literal, ext: [$($ext:literal),*], files: [$($f:literal),*], shebang: [$($sb:literal),*], line: $lc:expr, block: $bc:expr, grammar: $g:expr, fold: $fold:expr) => {
        LanguageDef { id: $id, name: $name, extensions: &[$($ext),*], filenames: &[$($f),*], shebangs: &[$($sb),*], line_comment: $lc, block_comment: $bc, grammar: $g, simple: None, fold_kinds: $fold }
    };
    ($id:literal, $name:literal, ext: [$($ext:literal),*], files: [$($f:literal),*], shebang: [$($sb:literal),*], line: $lc:expr, block: $bc:expr, simple: $simple:expr) => {
        LanguageDef { id: $id, name: $name, extensions: &[$($ext),*], filenames: &[$($f),*], shebangs: &[$($sb),*], line_comment: $lc, block_comment: $bc, grammar: None, simple: Some($simple), fold_kinds: BRACES }
    };
}

static LANGUAGES: &[LanguageDef] = &[
    def!("plain", "Plain Text", ext: ["txt", "text", "log"], files: [], shebang: [], line: None, block: None, grammar: None, fold: BRACES),
    def!("rust", "Rust", ext: ["rs"], files: [], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_rust, highlights: tree_sitter_rust::HIGHLIGHTS_QUERY, injections: Some(tree_sitter_rust::INJECTIONS_QUERY), tags: Some(tree_sitter_rust::TAGS_QUERY) }),
        fold: &["block", "declaration_list", "field_declaration_list", "enum_variant_list", "match_block", "use_list", "arguments", "block_comment", "line_comment"]),
    def!("python", "Python", ext: ["py", "pyw", "pyi"], files: ["SConstruct", "SConscript"], shebang: ["python", "python3"], line: Some("# "), block: Some(("\"\"\"", "\"\"\"")),
        grammar: Some(Grammar { language: l_python, highlights: tree_sitter_python::HIGHLIGHTS_QUERY, injections: None, tags: Some(tree_sitter_python::TAGS_QUERY) }),
        fold: &["function_definition", "class_definition", "if_statement", "for_statement", "while_statement", "with_statement", "try_statement", "block", "dictionary", "list", "argument_list", "string"]),
    def!("javascript", "JavaScript", ext: ["js", "mjs", "cjs", "jsx"], files: [], shebang: ["node"], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_js, highlights: tree_sitter_javascript::HIGHLIGHT_QUERY, injections: Some(tree_sitter_javascript::INJECTIONS_QUERY), tags: Some(tree_sitter_javascript::TAGS_QUERY) }),
        fold: &["statement_block", "class_body", "object", "array", "arguments", "jsx_element", "template_string", "comment", "switch_body"]),
    def!("typescript", "TypeScript", ext: ["ts", "mts", "cts"], files: [], shebang: ["ts-node", "deno"], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_ts, highlights: tree_sitter_typescript::HIGHLIGHTS_QUERY, injections: None, tags: Some(tree_sitter_typescript::TAGS_QUERY) }),
        fold: &["statement_block", "class_body", "object", "array", "arguments", "interface_body", "object_type", "enum_body", "template_string", "comment", "switch_body"]),
    def!("tsx", "TSX", ext: ["tsx"], files: [], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_tsx, highlights: tree_sitter_typescript::HIGHLIGHTS_QUERY, injections: None, tags: Some(tree_sitter_typescript::TAGS_QUERY) }),
        fold: &["statement_block", "class_body", "object", "array", "arguments", "interface_body", "object_type", "jsx_element", "template_string", "comment"]),
    def!("json", "JSON", ext: ["json", "jsonl", "geojson", "webmanifest"], files: [".babelrc", ".eslintrc", "composer.lock"], shebang: [], line: None, block: None,
        grammar: Some(Grammar { language: l_json, highlights: tree_sitter_json::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["object", "array"]),
    def!("jsonc", "JSON with Comments", ext: ["jsonc", "json5"], files: ["tsconfig.json", "jsconfig.json", ".vscode/settings.json"], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_json, highlights: tree_sitter_json::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["object", "array"]),
    def!("toml", "TOML", ext: ["toml"], files: ["Cargo.lock", "Pipfile", "poetry.lock"], shebang: [], line: Some("# "), block: None,
        grammar: Some(Grammar { language: l_toml, highlights: tree_sitter_toml_ng::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["table", "table_array_element", "array", "inline_table"]),
    def!("yaml", "YAML", ext: ["yaml", "yml"], files: [".clang-format", ".clang-tidy"], shebang: [], line: Some("# "), block: None,
        grammar: Some(Grammar { language: l_yaml, highlights: tree_sitter_yaml::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["block_mapping_pair", "block_sequence_item", "block_node", "flow_mapping", "flow_sequence"]),
    def!("markdown", "Markdown", ext: ["md", "markdown", "mdown", "mkd"], files: ["README", "CHANGELOG", "CONTRIBUTING"], shebang: [], line: None, block: Some(("<!--", "-->")),
        grammar: Some(Grammar { language: l_md, highlights: tree_sitter_md::HIGHLIGHT_QUERY_BLOCK, injections: Some(tree_sitter_md::INJECTION_QUERY_BLOCK), tags: None }),
        fold: &["section", "fenced_code_block", "list", "block_quote", "html_block"]),
    def!("markdown_inline", "Markdown Inline", ext: [], files: [], shebang: [], line: None, block: None,
        grammar: Some(Grammar { language: l_md_inline, highlights: tree_sitter_md::HIGHLIGHT_QUERY_INLINE, injections: Some(tree_sitter_md::INJECTION_QUERY_INLINE), tags: None }), fold: BRACES),
    def!("bash", "Shell Script", ext: ["sh", "bash", "zsh", "ksh", "bashrc", "zshrc", "profile"], files: [".bashrc", ".zshrc", ".profile", ".bash_profile", ".bash_aliases", "PKGBUILD", ".envrc"], shebang: ["sh", "bash", "zsh", "ksh", "dash"], line: Some("# "), block: None,
        grammar: Some(Grammar { language: l_bash, highlights: tree_sitter_bash::HIGHLIGHT_QUERY, injections: None, tags: None }), fold: &["function_definition", "compound_statement", "if_statement", "for_statement", "while_statement", "case_statement", "heredoc_body"]),
    def!("c", "C", ext: ["c", "h"], files: [], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_c, highlights: tree_sitter_c::HIGHLIGHT_QUERY, injections: None, tags: Some(tree_sitter_c::TAGS_QUERY) }), fold: &["compound_statement", "field_declaration_list", "enumerator_list", "initializer_list", "argument_list", "comment", "preproc_if", "preproc_ifdef"]),
    def!("cpp", "C++", ext: ["cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "inl", "ipp", "cu", "cuh"], files: [], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_cpp, highlights: tree_sitter_cpp::HIGHLIGHT_QUERY, injections: None, tags: Some(tree_sitter_cpp::TAGS_QUERY) }), fold: &["compound_statement", "field_declaration_list", "enumerator_list", "initializer_list", "argument_list", "comment", "preproc_if", "preproc_ifdef", "declaration_list"]),
    def!("csharp", "C#", ext: ["cs", "csx"], files: [], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_csharp, highlights: tree_sitter_c_sharp::HIGHLIGHTS_QUERY, injections: None, tags: Some(tree_sitter_c_sharp::TAGS_QUERY) }), fold: &["block", "declaration_list", "enum_member_declaration_list", "switch_body", "initializer_expression", "comment"]),
    def!("java", "Java", ext: ["java"], files: [], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_java, highlights: tree_sitter_java::HIGHLIGHTS_QUERY, injections: None, tags: Some(tree_sitter_java::TAGS_QUERY) }), fold: &["block", "class_body", "interface_body", "enum_body", "array_initializer", "switch_block", "block_comment"]),
    def!("kotlin", "Kotlin", ext: ["kt", "kts"], files: [], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_kotlin, highlights: include_str!("../../../assets/grammars/kotlin/highlights.scm"), injections: None, tags: None }), fold: &["class_body", "function_body", "block", "enum_class_body", "lambda_literal", "when_expression", "block_comment"]),
    def!("swift", "Swift", ext: ["swift"], files: ["Package.swift"], shebang: ["swift"], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_swift, highlights: tree_sitter_swift::HIGHLIGHTS_QUERY, injections: Some(tree_sitter_swift::INJECTIONS_QUERY), tags: Some(tree_sitter_swift::TAGS_QUERY) }), fold: &["class_body", "function_body", "enum_class_body", "protocol_body", "statements", "lambda_literal", "multiline_comment"]),
    def!("go", "Go", ext: ["go"], files: ["go.mod", "go.sum"], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_go, highlights: tree_sitter_go::HIGHLIGHTS_QUERY, injections: None, tags: Some(tree_sitter_go::TAGS_QUERY) }), fold: &["block", "field_declaration_list", "literal_value", "import_spec_list", "const_declaration", "var_declaration", "comment", "argument_list", "interface_type"]),
    def!("php", "PHP", ext: ["php", "phtml", "php5", "phps"], files: [], shebang: ["php"], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_php, highlights: tree_sitter_php::HIGHLIGHTS_QUERY, injections: Some(tree_sitter_php::INJECTIONS_QUERY), tags: Some(tree_sitter_php::TAGS_QUERY) }), fold: &["compound_statement", "declaration_list", "enum_declaration_list", "array_creation_expression", "arguments", "comment"]),
    def!("ruby", "Ruby", ext: ["rb", "rake", "gemspec", "ru"], files: ["Gemfile", "Rakefile", "Guardfile", "Vagrantfile", "Brewfile"], shebang: ["ruby"], line: Some("# "), block: Some(("=begin", "=end")),
        grammar: Some(Grammar { language: l_ruby, highlights: tree_sitter_ruby::HIGHLIGHTS_QUERY, injections: None, tags: Some(tree_sitter_ruby::TAGS_QUERY) }), fold: &["method", "class", "module", "do_block", "block", "if", "unless", "while", "case", "begin", "hash", "array", "singleton_method"]),
    def!("lua", "Lua", ext: ["lua"], files: [".luacheckrc"], shebang: ["lua", "luajit"], line: Some("-- "), block: Some(("--[[", "]]")),
        grammar: Some(Grammar { language: l_lua, highlights: tree_sitter_lua::HIGHLIGHTS_QUERY, injections: Some(tree_sitter_lua::INJECTIONS_QUERY), tags: Some(tree_sitter_lua::TAGS_QUERY) }), fold: &["function_definition", "function_declaration", "block", "table_constructor", "if_statement", "for_statement", "while_statement", "do_statement", "comment"]),
    def!("powershell", "PowerShell", ext: ["ps1", "psm1", "psd1"], files: [], shebang: ["pwsh", "powershell"], line: Some("# "), block: Some(("<#", "#>")),
        grammar: Some(Grammar { language: l_powershell, highlights: tree_sitter_powershell::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["script_block", "statement_block", "function_statement", "hash_literal_expression", "comment"]),
    def!("html", "HTML", ext: ["html", "htm", "xhtml", "vue", "svelte"], files: [], shebang: [], line: None, block: Some(("<!--", "-->")),
        grammar: Some(Grammar { language: l_html, highlights: tree_sitter_html::HIGHLIGHTS_QUERY, injections: Some(tree_sitter_html::INJECTIONS_QUERY), tags: None }), fold: &["element", "script_element", "style_element", "comment"]),
    def!("css", "CSS", ext: ["css"], files: [], shebang: [], line: None, block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_css, highlights: tree_sitter_css::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["block", "rule_set", "media_statement", "keyframes_statement", "comment"]),
    def!("scss", "SCSS", ext: ["scss"], files: [], shebang: [], line: Some("// "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_scss, highlights: tree_sitter_scss::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["block", "rule_set", "media_statement", "mixin_statement", "comment"]),
    def!("xml", "XML", ext: ["xml", "xsd", "xsl", "xslt", "svg", "plist", "csproj", "fsproj", "props", "targets", "nuspec", "resx", "wxs", "xaml", "ui", "pom"], files: [], shebang: [], line: None, block: Some(("<!--", "-->")),
        grammar: Some(Grammar { language: l_xml, highlights: tree_sitter_xml::XML_HIGHLIGHT_QUERY, injections: None, tags: None }), fold: &["element", "Comment", "CDSect"]),
    def!("sql", "SQL", ext: ["sql", "psql", "mysql", "ddl", "dml"], files: [], shebang: [], line: Some("-- "), block: Some(("/*", "*/")),
        grammar: Some(Grammar { language: l_sql, highlights: tree_sitter_sequel::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["statement", "column_definitions", "select", "from", "where", "comment"]),
    // The only crates.io Dockerfile grammar targets an old tree-sitter ABI, so
    // Dockerfiles use the declarative fallback highlighter.
    def!("dockerfile", "Dockerfile", ext: ["dockerfile", "containerfile"], files: ["Dockerfile", "Containerfile", "dockerfile"], shebang: [], line: Some("# "), block: None,
        simple: SimpleSyntax { keywords: &["FROM", "RUN", "CMD", "LABEL", "MAINTAINER", "EXPOSE", "ENV", "ADD", "COPY", "ENTRYPOINT", "VOLUME", "USER", "WORKDIR", "ARG", "ONBUILD", "STOPSIGNAL", "HEALTHCHECK", "SHELL", "AS"], case_insensitive: true, line_comment: Some("#"), strings: &['"', '\''], section_prefix: None }),
    def!("diff", "Diff", ext: ["diff", "patch", "rej"], files: [], shebang: [], line: None, block: None,
        grammar: Some(Grammar { language: l_diff, highlights: tree_sitter_diff::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["block", "hunk"]),
    def!("ini", "INI / Config", ext: ["ini", "cfg", "conf", "desktop", "service", "editorconfig", "gitconfig", "properties", "env", "flake8", "pylintrc", "npmrc"], files: [".gitconfig", ".gitmodules", ".editorconfig", ".npmrc", "setup.cfg", "tox.ini", ".flake8", ".pylintrc", ".coveragerc", ".env", "php.ini", "my.cnf"], shebang: [], line: Some("# "), block: None,
        grammar: Some(Grammar { language: l_ini, highlights: tree_sitter_ini::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["section"]),
    def!("make", "Makefile", ext: ["mk", "mak", "make"], files: ["Makefile", "makefile", "GNUmakefile", "Makefile.am", "Makefile.in"], shebang: ["make"], line: Some("# "), block: None,
        grammar: Some(Grammar { language: l_make, highlights: tree_sitter_make::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["rule", "define_directive", "conditional"]),
    def!("cmake", "CMake", ext: ["cmake"], files: ["CMakeLists.txt"], shebang: [], line: Some("# "), block: Some(("#[[", "]]")),
        grammar: Some(Grammar { language: l_cmake, highlights: tree_sitter_cmake::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: &["if_condition", "foreach_loop", "function_def", "macro_def", "while_loop", "bracket_comment"]),
    def!("regex", "Regular Expression", ext: [], files: [], shebang: [], line: None, block: None,
        grammar: Some(Grammar { language: l_regex, highlights: tree_sitter_regex::HIGHLIGHTS_QUERY, injections: None, tags: None }), fold: BRACES),
    def!("gitignore", "Git Ignore", ext: ["gitignore", "ignore", "dockerignore", "npmignore"], files: [".gitignore", ".dockerignore", ".npmignore", ".ignore", ".gitattributes"], shebang: [], line: Some("# "), block: None,
        simple: SimpleSyntax { keywords: &[], case_insensitive: false, line_comment: Some("#"), strings: &[], section_prefix: None }),
    def!("requirements", "Requirements", ext: [], files: ["requirements.txt", "requirements-dev.txt", "constraints.txt"], shebang: [], line: Some("# "), block: None,
        simple: SimpleSyntax { keywords: &[], case_insensitive: false, line_comment: Some("#"), strings: &[], section_prefix: None }),
];

/// All registered languages, in presentation order.
pub fn languages() -> &'static [LanguageDef] {
    LANGUAGES
}

/// Find a language by id, name (case-insensitive) or common alias.
pub fn lookup(id: &str) -> Option<&'static LanguageDef> {
    let id = id.trim();
    let lower = id.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "js" | "node" => "javascript",
        "ts" => "typescript",
        "py" | "python3" => "python",
        "rs" => "rust",
        "sh" | "shell" | "zsh" | "shellscript" => "bash",
        "c++" | "cxx" | "cc" => "cpp",
        "cs" | "c#" => "csharp",
        "md" => "markdown",
        "yml" => "yaml",
        "rb" => "ruby",
        "kt" => "kotlin",
        "ps1" | "pwsh" => "powershell",
        "text" | "txt" | "plaintext" => "plain",
        "jsx" => "javascript",
        "html5" | "xhtml" => "html",
        "docker" => "dockerfile",
        "makefile" => "make",
        "golang" => "go",
        other => other,
    };
    LANGUAGES.iter().find(|l| l.id == alias || l.name.eq_ignore_ascii_case(id)).or_else(|| LANGUAGES.iter().find(|l| l.extensions.contains(&alias)))
}

/// Detect a language from path, shebang and content heuristics.
pub fn detect(path: Option<&Path>, first_line: &str) -> &'static LanguageDef {
    if let Some(p) = path {
        let file_name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let lower_name = file_name.to_ascii_lowercase();
        // Exact filename matches first (Makefile, Dockerfile, .gitconfig…).
        for l in LANGUAGES {
            if l.filenames.iter().any(|f| f.eq_ignore_ascii_case(&file_name)) {
                return l;
            }
        }
        // Dockerfile.dev, Makefile.linux and friends.
        if lower_name.starts_with("dockerfile") || lower_name.starts_with("containerfile") {
            return lookup("dockerfile").expect("registered");
        }
        if lower_name.starts_with("makefile") {
            return lookup("make").expect("registered");
        }
        if let Some(ext) = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) {
            for l in LANGUAGES {
                if l.extensions.contains(&ext.as_str()) {
                    return l;
                }
            }
        }
        // Dotfiles like `.bashrc` have no extension in the Path sense.
        if let Some(rest) = lower_name.strip_prefix('.') {
            for l in LANGUAGES {
                if l.extensions.contains(&rest) {
                    return l;
                }
            }
        }
    }
    if let Some(l) = detect_shebang(first_line) {
        return l;
    }
    detect_content(first_line)
}

fn detect_shebang(first_line: &str) -> Option<&'static LanguageDef> {
    let line = first_line.strip_prefix("#!")?;
    let mut parts = line.split_whitespace();
    let mut interp = parts.next()?.rsplit('/').next()?.to_string();
    if interp == "env" {
        // `#!/usr/bin/env -S python3 -u` → skip flags.
        interp = parts.find(|p| !p.starts_with('-'))?.to_string();
    }
    let base: String = interp.chars().take_while(|c| !c.is_ascii_digit() && *c != '.').collect();
    LANGUAGES.iter().find(|l| l.shebangs.iter().any(|s| *s == interp || *s == base))
}

fn detect_content(first_line: &str) -> &'static LanguageDef {
    let t = first_line.trim_start();
    let id = if t.starts_with("<?xml") {
        "xml"
    } else if t.starts_with("<!DOCTYPE html") || t.starts_with("<html") {
        "html"
    } else if t.starts_with("diff --git") || t.starts_with("--- a/") || t.starts_with("Index: ") {
        "diff"
    } else if t.starts_with('{') || t.starts_with('[') {
        "json"
    } else if t.starts_with("<?php") {
        "php"
    } else {
        "plain"
    };
    lookup(id).expect("registered")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_name_extension_shebang_and_content() {
        assert_eq!(detect(Some(Path::new("src/main.rs")), "").id, "rust");
        assert_eq!(detect(Some(Path::new("Makefile")), "").id, "make");
        assert_eq!(detect(Some(Path::new("Dockerfile.dev")), "").id, "dockerfile");
        assert_eq!(detect(Some(Path::new(".bashrc")), "").id, "bash");
        assert_eq!(detect(Some(Path::new("tsconfig.json")), "").id, "jsonc");
        assert_eq!(detect(Some(Path::new("run")), "#!/usr/bin/env -S python3 -u").id, "python");
        assert_eq!(detect(Some(Path::new("run")), "#!/bin/bash").id, "bash");
        assert_eq!(detect(None, "<?xml version=\"1.0\"?>").id, "xml");
        assert_eq!(detect(None, "diff --git a/x b/x").id, "diff");
        assert_eq!(detect(Some(Path::new("notes")), "hello").id, "plain");
        assert_eq!(lookup("JS").map(|l| l.id), Some("javascript"));
        assert_eq!(lookup("C++").map(|l| l.id), Some("cpp"));
        assert!(lookup("nonexistent").is_none());
    }

    #[test]
    fn all_grammars_load_and_queries_compile() {
        for l in languages() {
            if let Some(g) = &l.grammar {
                let lang = (g.language)();
                let q = tree_sitter::Query::new(&lang, g.highlights);
                assert!(q.is_ok(), "{}: highlights query failed: {:?}", l.id, q.err());
                if let Some(inj) = g.injections {
                    assert!(tree_sitter::Query::new(&lang, inj).is_ok(), "{}: injections query failed", l.id);
                }
                if let Some(tags) = g.tags {
                    assert!(tree_sitter::Query::new(&lang, tags).is_ok(), "{}: tags query failed", l.id);
                }
            }
        }
    }

    #[test]
    fn ids_unique() {
        let mut seen = std::collections::HashSet::new();
        for l in languages() {
            assert!(seen.insert(l.id), "duplicate {}", l.id);
        }
    }
}
