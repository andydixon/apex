//! Code hosts: web links for a repository's remote (GitHub, GitLab,
//! Bitbucket, Gitea/Codeberg; unknown hosts are treated like GitHub, which
//! covers GitHub Enterprise), and parsing of the GitHub CLI's JSON output.
//! Building links needs no network; only the `gh` commands talk to GitHub.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    GitLab,
    Bitbucket,
    Gitea,
}

/// `https://host/owner/repo` for a remote, and which forge it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebRepo {
    pub base: String,
    pub forge: Forge,
}

/// Parse `git@host:owner/repo.git`, `ssh://git@host:22/owner/repo.git`,
/// `https://user@host/owner/repo(.git)` and similar.
pub fn web_repo(remote_url: &str) -> Option<WebRepo> {
    let url = remote_url.trim();
    let (host, path) = if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
        // scheme://[user@]host[:port]/path
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
        let host = host.split(':').next()?;
        (host.to_string(), path.to_string())
    } else {
        // scp-like: [user@]host:path
        let (left, path) = url.split_once(':')?;
        if left.contains('/') {
            return None; // a local path
        }
        let host = left.rsplit_once('@').map(|(_, h)| h).unwrap_or(left);
        (host.to_string(), path.trim_start_matches('/').to_string())
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    if host.is_empty() || path.split('/').filter(|s| !s.is_empty()).count() < 2 {
        return None;
    }
    let h = host.to_lowercase();
    let forge = if h.contains("gitlab") {
        Forge::GitLab
    } else if h.contains("bitbucket") {
        Forge::Bitbucket
    } else if h.contains("codeberg") || h.contains("gitea") || h.contains("forgejo") {
        Forge::Gitea
    } else {
        Forge::GitHub
    };
    Some(WebRepo { base: format!("https://{host}/{path}"), forge })
}

fn encode_path(p: &str) -> String {
    p.split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

impl WebRepo {
    /// Link to `path` at `rev`, highlighting one-based `lines` if given.
    pub fn file_url(&self, rev: &str, path: &str, lines: Option<(usize, usize)>) -> String {
        let p = encode_path(path);
        let (prefix, anchor) = match self.forge {
            Forge::GitHub => (format!("{}/blob/{rev}/{p}", self.base), lines.map(|(a, b)| if a == b { format!("#L{a}") } else { format!("#L{a}-L{b}") })),
            Forge::GitLab => (format!("{}/-/blob/{rev}/{p}", self.base), lines.map(|(a, b)| if a == b { format!("#L{a}") } else { format!("#L{a}-{b}") })),
            Forge::Bitbucket => {
                (format!("{}/src/{rev}/{p}", self.base), lines.map(|(a, b)| if a == b { format!("#lines-{a}") } else { format!("#lines-{a}:{b}") }))
            }
            Forge::Gitea => (format!("{}/src/commit/{rev}/{p}", self.base), lines.map(|(a, b)| if a == b { format!("#L{a}") } else { format!("#L{a}-L{b}") })),
        };
        prefix + &anchor.unwrap_or_default()
    }

    pub fn commit_url(&self, id: &str) -> String {
        match self.forge {
            Forge::GitHub | Forge::Gitea => format!("{}/commit/{id}", self.base),
            Forge::GitLab => format!("{}/-/commit/{id}", self.base),
            Forge::Bitbucket => format!("{}/commits/{id}", self.base),
        }
    }

    pub fn branch_url(&self, branch: &str) -> String {
        let b = encode_path(branch);
        match self.forge {
            Forge::GitHub => format!("{}/tree/{b}", self.base),
            Forge::GitLab => format!("{}/-/tree/{b}", self.base),
            Forge::Bitbucket => format!("{}/src/{b}", self.base),
            Forge::Gitea => format!("{}/src/branch/{b}", self.base),
        }
    }

    /// Page for proposing `branch` as a pull/merge request.
    pub fn compare_url(&self, branch: &str) -> String {
        let b = encode_path(branch);
        match self.forge {
            Forge::GitHub => format!("{}/compare/{b}?expand=1", self.base),
            Forge::GitLab => format!("{}/-/merge_requests/new?merge_request%5Bsource_branch%5D={b}", self.base),
            Forge::Bitbucket => format!("{}/pull-requests/new?source={b}", self.base),
            Forge::Gitea => format!("{}/compare/{b}", self.base),
        }
    }
}

// ----- GitHub CLI JSON ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub head_ref_name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub author: Option<Author>,
    #[serde(default)]
    pub review_decision: Option<String>,
    #[serde(default)]
    pub status_check_rollup: Vec<CheckRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub login: String,
}

/// One entry of `statusCheckRollup` (check runs and commit statuses).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckRun {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub context: String,
    /// Check runs: QUEUED / IN_PROGRESS / COMPLETED.
    #[serde(default)]
    pub status: String,
    /// Check runs: SUCCESS / FAILURE / NEUTRAL / SKIPPED / CANCELLED / ...
    #[serde(default)]
    pub conclusion: String,
    /// Commit statuses: SUCCESS / FAILURE / PENDING / ERROR.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub details_url: String,
    #[serde(default)]
    pub target_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    Failed,
    Pending,
    Skipped,
}

impl CheckRun {
    pub fn label(&self) -> &str {
        if self.name.is_empty() { &self.context } else { &self.name }
    }
    pub fn link(&self) -> &str {
        if self.details_url.is_empty() { &self.target_url } else { &self.details_url }
    }
    pub fn outcome(&self) -> CheckState {
        let v = if self.conclusion.is_empty() { self.state.as_str() } else { self.conclusion.as_str() };
        match v {
            "SUCCESS" => CheckState::Passed,
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => CheckState::Failed,
            "NEUTRAL" | "SKIPPED" | "STALE" => CheckState::Skipped,
            _ => CheckState::Pending,
        }
    }
}

impl PrSummary {
    /// "PR #12 open · checks passing" style summary for the status bar.
    pub fn status_label(&self) -> String {
        let state = if self.is_draft { "draft".to_string() } else { self.state.to_lowercase() };
        let mut s = format!("PR #{} {state}", self.number);
        let runs = &self.status_check_rollup;
        if !runs.is_empty() {
            let failed = runs.iter().filter(|r| r.outcome() == CheckState::Failed).count();
            let pending = runs.iter().filter(|r| r.outcome() == CheckState::Pending).count();
            s.push_str(&if failed > 0 {
                format!(", {failed} check{} failing", if failed == 1 { "" } else { "s" })
            } else if pending > 0 {
                format!(", {pending} check{} running", if pending == 1 { "" } else { "s" })
            } else {
                ", checks passing".to_string()
            });
        }
        match self.review_decision.as_deref() {
            Some("APPROVED") => s.push_str(", approved"),
            Some("CHANGES_REQUESTED") => s.push_str(", changes requested"),
            _ => {}
        }
        s
    }
}

pub const PR_FIELDS: &str = "number,title,headRefName,url,isDraft,state,author,reviewDecision,statusCheckRollup";
pub const PR_LIST_FIELDS: &str = "number,title,headRefName,url,isDraft,state,author";

pub fn parse_pr(json: &str) -> Result<PrSummary, String> {
    serde_json::from_str(json).map_err(|e| format!("unexpected output from gh: {e}"))
}

pub fn parse_pr_list(json: &str) -> Result<Vec<PrSummary>, String> {
    serde_json::from_str(json).map_err(|e| format!("unexpected output from gh: {e}"))
}

/// Default title and body for a new pull request from the commits on the
/// branch (oldest first): one commit gives its subject and body; several
/// give the branch name as the title and the subjects as a list.
pub fn pr_template(branch: &str, commits: &[(String, String)]) -> (String, String) {
    match commits {
        [(subject, body)] => (subject.clone(), body.trim().to_string()),
        _ => {
            let words = branch.rsplit('/').next().unwrap_or(branch).replace(['-', '_'], " ");
            let mut title: String = words.trim().to_string();
            if let Some(first) = title.get(..1) {
                title = first.to_uppercase() + &title[1..];
            }
            let body = commits.iter().map(|(s, _)| format!("- {s}")).collect::<Vec<_>>().join("\n");
            (title, body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls() {
        let gh = |u| web_repo(u).map(|w| w.base);
        assert_eq!(gh("git@github.com:andydixon/apex.git").as_deref(), Some("https://github.com/andydixon/apex"));
        assert_eq!(gh("https://github.com/andydixon/apex").as_deref(), Some("https://github.com/andydixon/apex"));
        assert_eq!(gh("https://token@github.com/a/b.git/").as_deref(), Some("https://github.com/a/b"));
        assert_eq!(gh("ssh://git@gitlab.example.com:2222/group/sub/proj.git").as_deref(), Some("https://gitlab.example.com/group/sub/proj"));
        assert_eq!(web_repo("git@gitlab.com:g/p.git").unwrap().forge, Forge::GitLab);
        assert_eq!(web_repo("https://bitbucket.org/t/r.git").unwrap().forge, Forge::Bitbucket);
        assert_eq!(web_repo("https://codeberg.org/u/r").unwrap().forge, Forge::Gitea);
        assert!(web_repo("/srv/git/repo.git").is_none());
        assert!(web_repo("../other").is_none());
        assert!(web_repo("https://github.com/onlyowner").is_none());
    }

    #[test]
    fn links_per_forge() {
        let r = |u| web_repo(u).unwrap();
        let g = r("git@github.com:o/r.git");
        assert_eq!(g.file_url("abc", "src/a b.rs", Some((3, 7))), "https://github.com/o/r/blob/abc/src/a%20b.rs#L3-L7");
        assert_eq!(g.file_url("abc", "x.rs", Some((3, 3))), "https://github.com/o/r/blob/abc/x.rs#L3");
        assert_eq!(g.commit_url("abc"), "https://github.com/o/r/commit/abc");
        assert_eq!(g.compare_url("feat/x"), "https://github.com/o/r/compare/feat/x?expand=1");
        let l = r("git@gitlab.com:g/p.git");
        assert_eq!(l.file_url("abc", "x.rs", Some((1, 2))), "https://gitlab.com/g/p/-/blob/abc/x.rs#L1-2");
        assert_eq!(l.branch_url("main"), "https://gitlab.com/g/p/-/tree/main");
        let b = r("https://bitbucket.org/t/r.git");
        assert_eq!(b.file_url("abc", "x.rs", Some((1, 2))), "https://bitbucket.org/t/r/src/abc/x.rs#lines-1:2");
        let c = r("https://codeberg.org/u/r");
        assert_eq!(c.file_url("abc", "x.rs", None), "https://codeberg.org/u/r/src/commit/abc/x.rs");
    }

    #[test]
    fn gh_json_and_labels() {
        let pr = parse_pr(
            r#"{"number":12,"title":"Add x","headRefName":"feat","url":"https://github.com/o/r/pull/12","isDraft":false,"state":"OPEN",
               "author":{"login":"ada"},"reviewDecision":"APPROVED",
               "statusCheckRollup":[{"__typename":"CheckRun","name":"ci","status":"COMPLETED","conclusion":"SUCCESS","detailsUrl":"https://x"},
                                    {"__typename":"StatusContext","context":"lint","state":"PENDING","targetUrl":"https://y"}]}"#,
        )
        .unwrap();
        assert_eq!(pr.status_label(), "PR #12 open, 1 check running, approved");
        assert_eq!(pr.status_check_rollup[1].label(), "lint");
        assert_eq!(pr.status_check_rollup[1].link(), "https://y");
        let list = parse_pr_list(r#"[{"number":3,"title":"T","headRefName":"b","url":"u","isDraft":true,"state":"OPEN","author":{"login":"x"}}]"#).unwrap();
        assert_eq!(list[0].status_label(), "PR #3 draft");
        assert!(parse_pr("not json").is_err());
    }

    #[test]
    fn pr_templates() {
        assert_eq!(pr_template("x", &[("Fix it".into(), "Because.\n".into())]), ("Fix it".into(), "Because.".into()));
        let (t, b) = pr_template("feature/fast-search", &[("a".into(), String::new()), ("b".into(), String::new())]);
        assert_eq!(t, "Fast search");
        assert_eq!(b, "- a\n- b");
    }
}
