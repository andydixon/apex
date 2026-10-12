//! History editor back end: load every commit's metadata, stage edits
//! (author, committer, dates, message), bulk operations, and rewrite history
//! with `git filter-branch` after making a backup tag.
//!
//! Rewriting changes every descendant's hash. The apply step refuses to run
//! with uncommitted changes, always tags the old HEAD first, and pushes only
//! when asked (with `--force-with-lease`).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::git_cli::JobCtx;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommitRecord {
    pub hash: String,
    pub short_hash: String,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: String,
    pub refs: Vec<String>,
    pub parents: Vec<String>,
    pub message: String,
}

impl CommitRecord {
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("").trim()
    }
}

/// Trailing newlines removed, CRLF folded: how messages are compared.
pub fn normalise_message(m: &str) -> String {
    m.replace("\r\n", "\n").trim_end_matches('\n').to_string()
}

const LOG_FORMAT: &str = "--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI%x1f%D%x1f%P%x1f%B%x1e";

/// Parse `git log --all --reverse` output in [`LOG_FORMAT`] (oldest first).
pub fn parse_log(out: &str) -> Result<Vec<CommitRecord>, String> {
    let mut commits = Vec::new();
    for raw in out.split('\x1e') {
        let raw = raw.trim_start_matches(['\n', '\r']);
        if raw.trim().is_empty() {
            continue;
        }
        let p: Vec<&str> = raw.splitn(11, '\x1f').collect();
        if p.len() != 11 {
            return Err(format!("could not parse commit record {:?}", raw.chars().take(60).collect::<String>()));
        }
        let split = |s: &str, sep: char| s.split(sep).map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect::<Vec<_>>();
        commits.push(CommitRecord {
            hash: p[0].to_string(),
            short_hash: p[1].to_string(),
            author_name: p[2].to_string(),
            author_email: p[3].to_string(),
            author_date: p[4].to_string(),
            committer_name: p[5].to_string(),
            committer_email: p[6].to_string(),
            committer_date: p[7].to_string(),
            refs: split(p[8], ','),
            parents: split(p[9], ' '),
            message: normalise_message(p[10]),
        });
    }
    Ok(commits)
}

/// Repository facts shown above the editor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoFacts {
    pub clean: bool,
    pub branch: Option<String>,
    pub origin: Option<String>,
    /// Local UTC offset in minutes (from `git var`), for "now".
    pub local_offset: i32,
}

/// Load facts and every commit reachable from any ref (oldest first).
pub fn load(c: &JobCtx) -> Result<(RepoFacts, Vec<CommitRecord>), String> {
    let clean = c.git_quiet(&["status", "--porcelain"], None)?.trim().is_empty();
    let branch = c.git_quiet(&["symbolic-ref", "--quiet", "--short", "HEAD"], None).ok().map(|s| s.trim().to_string());
    let origin = c.git_quiet(&["remote", "get-url", "origin"], None).ok().map(|s| s.trim().to_string());
    let ident = c.git_quiet(&["var", "GIT_COMMITTER_IDENT"], None).unwrap_or_default();
    let local_offset = ident.split_whitespace().last().and_then(parse_offset).unwrap_or(0);
    let log = match c.git_quiet(&["log", "--all", "--topo-order", "--reverse", "--date=iso-strict", LOG_FORMAT], None) {
        Ok(l) => l,
        // A repository without commits has nothing to edit.
        Err(e) if e.contains("does not have any commits") => String::new(),
        Err(e) => return Err(e),
    };
    Ok((RepoFacts { clean, branch, origin, local_offset }, parse_log(&log)?))
}

// ----- dates ---------------------------------------------------------------

/// A wall-clock date with an optional UTC offset (none = local time, which
/// git interprets when the date is applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitDate {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub offset: Option<i32>,
}

fn parse_offset(s: &str) -> Option<i32> {
    if s == "Z" {
        return Some(0);
    }
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: i32 = digits[..2].parse().ok()?;
    let m: i32 = digits[2..].parse().ok()?;
    (h <= 23 && m <= 59).then_some(sign * (h * 60 + m))
}

pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 for a civil date (proleptic Gregorian).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

impl GitDate {
    /// ISO 8601 (`2024-05-01T09:30:00+01:00`, `Z`, `+0100`), with `T` or a
    /// space, seconds optional, offset optional (= local time).
    pub fn parse(input: &str) -> Option<GitDate> {
        let s = input.trim();
        let (date, rest) = s.split_at_checked(10)?;
        let mut dp = date.split('-');
        let year: i64 = dp.next()?.parse().ok()?;
        let month: u32 = dp.next()?.parse().ok()?;
        let day: u32 = dp.next()?.parse().ok()?;
        if dp.next().is_some() || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) || !(1000..=9999).contains(&year) {
            return None;
        }
        let rest = rest.strip_prefix(['T', ' '])?;
        // Split the time from the offset at the first sign/Z after the time.
        let (time, off) = match rest.find(['+', '-', 'Z', ' ']) {
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => (rest, ""),
        };
        let tp: Vec<&str> = time.split(':').collect();
        if !(2..=3).contains(&tp.len()) || tp.iter().any(|x| x.len() != 2) {
            return None;
        }
        let hour: u32 = tp[0].parse().ok()?;
        let minute: u32 = tp[1].parse().ok()?;
        let second: u32 = tp.get(2).map(|x| x.parse().ok()).unwrap_or(Some(0))?;
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        let offset = if off.is_empty() { None } else { Some(parse_offset(off)?) };
        Some(GitDate { year, month, day, hour, minute, second, offset })
    }

    pub fn format(&self) -> String {
        let base = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", self.year, self.month, self.day, self.hour, self.minute, self.second);
        match self.offset {
            Some(o) => format!("{base}{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, o.abs() / 60, o.abs() % 60),
            None => base,
        }
    }

    pub fn from_unix(t: i64, offset: i32) -> GitDate {
        let local = t + offset as i64 * 60;
        let (year, month, day) = civil_from_days(local.div_euclid(86_400));
        let s = local.rem_euclid(86_400) as u32;
        GitDate { year, month, day, hour: s / 3600, minute: s % 3600 / 60, second: s % 60, offset: Some(offset) }
    }

    /// Seconds since the epoch, taking a missing offset as `local_offset`.
    pub fn to_unix(&self, local_offset: i32) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400 + (self.hour * 3600 + self.minute * 60 + self.second) as i64
            - self.offset.unwrap_or(local_offset) as i64 * 60
    }
}

/// Normalised form for comparison (unparsable dates compare as text).
pub fn normalise_date(s: &str) -> String {
    GitDate::parse(s).map(|d| d.format()).unwrap_or_else(|| s.trim().to_string())
}

pub fn now(local_offset: i32) -> GitDate {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    GitDate::from_unix(t, local_offset)
}

// ----- edits ---------------------------------------------------------------

/// Whether an edited record differs from the original in any editable field.
pub fn changed(orig: &CommitRecord, draft: &CommitRecord) -> bool {
    orig.author_name != draft.author_name
        || orig.author_email != draft.author_email
        || normalise_date(&orig.author_date) != normalise_date(&draft.author_date)
        || orig.committer_name != draft.committer_name
        || orig.committer_email != draft.committer_email
        || normalise_date(&orig.committer_date) != normalise_date(&draft.committer_date)
        || normalise_message(&orig.message) != normalise_message(&draft.message)
}

/// Check a draft before applying.
pub fn validate(d: &CommitRecord) -> Result<(), String> {
    if d.author_name.trim().is_empty() || d.author_email.trim().is_empty() {
        return Err(format!("{}: author name and email are required", d.short_hash));
    }
    if d.committer_name.trim().is_empty() || d.committer_email.trim().is_empty() {
        return Err(format!("{}: committer name and email are required", d.short_hash));
    }
    for (what, v) in [("author", &d.author_date), ("committer", &d.committer_date)] {
        if GitDate::parse(v).is_none() {
            return Err(format!("{}: invalid {what} date “{v}” (use YYYY-MM-DD HH:MM[:SS][±HH:MM])", d.short_hash));
        }
    }
    Ok(())
}

/// Replace the author of every draft whose name or email matches `query`
/// (case-insensitive; substring unless `exact`). Returns the count.
pub fn replace_author(drafts: &mut [CommitRecord], query: &str, name: &str, email: &str, exact: bool) -> Result<usize, String> {
    let (query, name, email) = (query.trim(), name.trim(), email.trim());
    if query.is_empty() {
        return Err("enter the author to search for".into());
    }
    if name.is_empty() || email.is_empty() {
        return Err("the replacement name and email are required".into());
    }
    let q = query.to_lowercase();
    let hit = |v: &str| if exact { v.to_lowercase() == q } else { v.to_lowercase().contains(&q) };
    let mut n = 0;
    for d in drafts.iter_mut() {
        if hit(&d.author_name) || hit(&d.author_email) {
            d.author_name = name.to_string();
            d.author_email = email.to_string();
            n += 1;
        }
    }
    Ok(n)
}

fn parse_day(s: &str) -> Option<(i64, u32, u32)> {
    let d = GitDate::parse(&format!("{}T00:00", s.trim()))?;
    Some((d.year, d.month, d.day))
}

fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 24 && m <= 59 && h * 60 + m <= 1440).then_some(h * 3600 + m * 60)
}

/// Give every commit (`drafts` oldest first) the same author and committer
/// time: a random day in `[start, end]` at a random time in the daily window
/// `[from, to)`, in local time. The random instants are sorted, so times
/// never go backwards. `rand(n)` returns a value in `0..n`.
pub fn spread_dates(drafts: &mut [CommitRecord], start: &str, end: &str, from: &str, to: &str, mut rand: impl FnMut(u64) -> u64) -> Result<usize, String> {
    let s = parse_day(start).ok_or("start date must be YYYY-MM-DD")?;
    let e = parse_day(end).ok_or("end date must be YYYY-MM-DD")?;
    let (s_days, e_days) = (days_from_civil(s.0, s.1, s.2), days_from_civil(e.0, e.1, e.2));
    if e_days < s_days {
        return Err("the end date is before the start date".into());
    }
    let f = parse_hm(from).ok_or("window start must be HH:MM")?;
    let t = parse_hm(to).ok_or("window end must be HH:MM")?;
    if t <= f {
        return Err("the window must end after it starts".into());
    }
    let days = (e_days - s_days + 1) as u64;
    let mut stamps: Vec<i64> = (0..drafts.len()).map(|_| (s_days + rand(days) as i64) * 86_400 + f as i64 + rand((t - f) as u64) as i64).collect();
    stamps.sort_unstable();
    for (d, st) in drafts.iter_mut().zip(stamps) {
        // Wall-clock time without an offset: git applies the local zone.
        let g = GitDate { offset: None, ..GitDate::from_unix(st, 0) };
        d.author_date = g.format();
        d.committer_date = d.author_date.clone();
    }
    Ok(drafts.len())
}

fn co_author_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^\s*co-authored(?:-|\s+)by\s*:").expect("valid regex"))
}

fn ai_attribution_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^\s*co-authored(?:-|\s+)by\s*:.*\b(claude|codex)\b").expect("valid regex"))
}

/// Drop `Co-authored-by:` trailers and `Claude-Session:` lines. A Claude or
/// Codex co-author line also takes the non-blank lines that follow it (its
/// attribution block). Returns the message and the number of lines removed.
pub fn remove_co_authors(message: &str) -> (String, usize) {
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = 0;
    let mut in_block = false;
    let text = message.replace("\r\n", "\n");
    for line in text.split('\n') {
        if ai_attribution_re().is_match(line) {
            in_block = true;
        } else if in_block && !line.trim().is_empty() {
        } else if co_author_re().is_match(line) || line.to_lowercase().contains("claude-session:") {
            in_block = false;
        } else {
            in_block = false;
            kept.push(line);
            continue;
        }
        removed += 1;
    }
    // Collapse runs of blank lines left behind.
    let mut out = String::new();
    let mut blanks = 0;
    for l in kept {
        if l.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(l);
        out.push('\n');
    }
    (normalise_message(&out), removed)
}

/// Commits whose author, committer or message mentions Claude, Codex or
/// Anthropic, plus merges whose merged-in side contains such a commit.
pub fn ai_commits(commits: &[CommitRecord]) -> HashSet<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"(?i)\b(claude|codex|anthropic)\b").expect("valid regex"));
    let by_hash: HashMap<&str, &CommitRecord> = commits.iter().map(|c| (c.hash.as_str(), c)).collect();
    let mut ai: HashSet<String> = commits
        .iter()
        .filter(|c| [&c.author_name, &c.author_email, &c.committer_name, &c.committer_email, &c.message].iter().any(|s| re.is_match(s)))
        .map(|c| c.hash.clone())
        .collect();
    let ancestors = |start: &[String], skip: &HashSet<String>| -> HashSet<String> {
        let mut seen = HashSet::new();
        let mut stack: Vec<String> = start.to_vec();
        while let Some(h) = stack.pop() {
            if skip.contains(&h) || !seen.insert(h.clone()) {
                continue;
            }
            if let Some(c) = by_hash.get(h.as_str()) {
                stack.extend(c.parents.iter().cloned());
            }
        }
        seen
    };
    // ponytail: an ancestor walk per merge, O(commits × merges); memoise if
    // very large repositories become slow.
    for c in commits {
        if c.parents.len() < 2 || ai.contains(&c.hash) {
            continue;
        }
        let base = ancestors(&c.parents[..1], &HashSet::new());
        if ancestors(&c.parents[1..], &base).iter().any(|h| ai.contains(h)) {
            ai.insert(c.hash.clone());
        }
    }
    ai
}

// ----- apply ---------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ApplyOptions {
    pub remove_co_authors: bool,
    pub force_push: bool,
    pub push_tags: bool,
}

fn sh_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\"'\"'"))
}

/// The commits that will actually change (after co-author removal).
pub fn planned(originals: &[CommitRecord], drafts: &HashMap<String, CommitRecord>, strip_co_authors: bool) -> Vec<CommitRecord> {
    originals
        .iter()
        .filter_map(|o| {
            let mut d = drafts.get(&o.hash).cloned().unwrap_or_else(|| o.clone());
            if strip_co_authors {
                d.message = remove_co_authors(&d.message).0;
            }
            changed(o, &d).then_some(d)
        })
        .collect()
}

/// Rewrite history so each commit in `changes` gets its edited metadata.
/// Returns a summary for the status bar.
pub fn apply(c: &JobCtx, originals: &[CommitRecord], changes: &[CommitRecord], opts: &ApplyOptions) -> Result<String, String> {
    if !c.git_quiet(&["status", "--porcelain"], None)?.trim().is_empty() {
        return Err("the repository has uncommitted changes; commit or stash them first".into());
    }
    for d in changes {
        validate(d)?;
    }
    let mut summary = Vec::new();
    if !changes.is_empty() {
        let orig: HashMap<&str, &CommitRecord> = originals.iter().map(|o| (o.hash.as_str(), o)).collect();
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // filter-branch rewrites every tag pointing at a rewritten commit, so
        // the backup is held in a private ref during the rewrite and turned
        // into the tag afterwards.
        let backup = format!("apex-backup-{stamp}");
        let hold = format!("refs/apex-backup/{stamp}");
        c.git(&["update-ref", &hold, "HEAD"], None)?;
        let dir = std::env::temp_dir().join(format!("apex-rewrite-{}-{stamp}", std::process::id()));
        crate::paths::ensure_private_dir(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let result = (|| -> Result<String, String> {
            let mut env = String::from("case \"$GIT_COMMIT\" in\n");
            let mut msg = String::from("case \"$GIT_COMMIT\" in\n");
            for d in changes {
                env.push_str(&format!("{})\n", d.hash));
                for (k, v) in [
                    ("GIT_AUTHOR_NAME", d.author_name.trim()),
                    ("GIT_AUTHOR_EMAIL", d.author_email.trim()),
                    ("GIT_AUTHOR_DATE", &normalise_date(&d.author_date)),
                    ("GIT_COMMITTER_NAME", d.committer_name.trim()),
                    ("GIT_COMMITTER_EMAIL", d.committer_email.trim()),
                    ("GIT_COMMITTER_DATE", &normalise_date(&d.committer_date)),
                ] {
                    env.push_str(&format!("export {k}={}\n", sh_quote(v)));
                }
                env.push_str(";;\n");
                if orig.get(d.hash.as_str()).is_none_or(|o| normalise_message(&o.message) != normalise_message(&d.message)) {
                    let file = dir.join(format!("{}.txt", d.hash));
                    std::fs::write(&file, &d.message).map_err(|e| e.to_string())?;
                    msg.push_str(&format!("{})\ncat {}\n;;\n", d.hash, sh_quote(&file.to_string_lossy().replace('\\', "/"))));
                }
            }
            env.push_str("esac\n");
            msg.push_str("*)\ncat\n;;\nesac\n");
            let env_path = dir.join("env-filter.sh");
            let msg_path = dir.join("msg-filter.sh");
            std::fs::write(&env_path, env).map_err(|e| e.to_string())?;
            std::fs::write(&msg_path, msg).map_err(|e| e.to_string())?;
            let dot = |p: &Path| format!(". {}", sh_quote(&p.to_string_lossy().replace('\\', "/")));
            let (env_cmd, msg_cmd) = (dot(&env_path), dot(&msg_path));
            // Local branches and tags only: remote-tracking refs must keep the
            // remote's state (or --force-with-lease would refuse), and backup
            // tags must keep the old history. filter-branch ignores --exclude,
            // so the refs are listed explicitly.
            // ponytail: refs/stash and notes are left as they are.
            let refs_out = c.git_quiet(&["for-each-ref", "--format=%(refname)", "refs/heads/", "refs/tags/"], None)?;
            let refs: Vec<&str> = refs_out.lines().map(str::trim).filter(|r| !r.is_empty() && !r.starts_with("refs/tags/apex-backup-")).collect();
            if refs.is_empty() {
                return Err("there are no branches to rewrite".into());
            }
            let mut args = vec!["filter-branch", "-f", "--env-filter", &env_cmd, "--msg-filter", &msg_cmd, "--tag-name-filter", "cat", "--"];
            args.extend(refs);
            let out = c.git(&args, None)?;
            if !out.contains(" was rewritten") {
                return Err("git rewrote no refs; nothing changed".into());
            }
            Ok(out)
        })();
        let _ = std::fs::remove_dir_all(&dir);
        c.git(&["tag", &backup, &hold], None)?;
        c.git(&["update-ref", "-d", &hold], None)?;
        result.map_err(|e| format!("{e} (backup tag {backup} points at the old history)"))?;
        // filter-branch keeps refs/original/*; drop them so old objects can go.
        let originals = c.git_quiet(&["for-each-ref", "--format=%(refname)", "refs/original/"], None)?;
        for r in originals.lines().filter(|l| !l.trim().is_empty()) {
            c.git(&["update-ref", "-d", r.trim()], None)?;
        }
        if let Err(e) = c.git(&["reflog", "expire", "--expire=now", "--all"], None).and_then(|_| c.git(&["gc", "--prune=now", "--quiet"], None)) {
            c.log(format!("[cleanup warning: {e}]"));
        }
        summary.push(format!("Rewrote {} commit{}; backup tag {backup}", changes.len(), if changes.len() == 1 { "" } else { "s" }));
    }
    if opts.force_push {
        c.git_quiet(&["remote", "get-url", "origin"], None).map_err(|_| "there is no origin remote to push to".to_string())?;
        c.git(&["push", "--force-with-lease", "--progress", "origin", "--all"], None)?;
        if opts.push_tags {
            c.git(&["push", "--force-with-lease", "--progress", "origin", "--tags"], None)?;
        }
        summary.push("force-pushed to origin".into());
    }
    if summary.is_empty() {
        return Err("nothing to apply".into());
    }
    Ok(summary.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(hash: &str, parents: &[&str], author: &str, msg: &str) -> CommitRecord {
        CommitRecord {
            hash: hash.into(),
            short_hash: hash.into(),
            author_name: author.into(),
            author_email: format!("{}@example.com", author.to_lowercase()),
            author_date: "2024-01-01T10:00:00+00:00".into(),
            committer_name: author.into(),
            committer_email: "c@example.com".into(),
            committer_date: "2024-01-01T10:00:00+00:00".into(),
            refs: vec![],
            parents: parents.iter().map(|p| p.to_string()).collect(),
            message: msg.into(),
        }
    }

    #[test]
    fn dates_parse_format_and_round_trip() {
        let d = GitDate::parse("2024-05-01T09:30:00+01:00").unwrap();
        assert_eq!(d.format(), "2024-05-01T09:30:00+01:00");
        assert_eq!(GitDate::parse("2024-05-01 09:30").unwrap().format(), "2024-05-01T09:30:00");
        assert_eq!(GitDate::parse("2024-05-01 09:30:05 -0500").unwrap().format(), "2024-05-01T09:30:05-05:00");
        assert_eq!(GitDate::parse("2024-05-01T09:30:00Z").unwrap().offset, Some(0));
        for bad in ["2024-02-30 10:00", "2024-13-01 10:00", "2024-05-01", "yesterday", "2024-05-01 25:00", "2024-05-01T10:00+1"] {
            assert!(GitDate::parse(bad).is_none(), "{bad}");
        }
        assert_eq!(normalise_date("2024-05-01 09:30:00+01:00"), normalise_date("2024-05-01T09:30:00+0100"));
        let t = d.to_unix(0);
        assert_eq!(GitDate::from_unix(t, 60), d);
        assert_eq!(civil_from_days(days_from_civil(2000, 2, 29)), (2000, 2, 29));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
    }

    #[test]
    fn parses_log_records() {
        let out = "aaa\x1fa\x1fAda\x1fada@x\x1f2024-01-01T10:00:00+00:00\x1fAda\x1fada@x\x1f2024-01-01T10:00:00+00:00\x1fHEAD -> main, tag: v1\x1f\x1ffirst\n\nbody\n\x1e\nbbb\x1fb\x1fBo\x1fbo@x\x1f2024-01-02T10:00:00+00:00\x1fBo\x1fbo@x\x1f2024-01-02T10:00:00+00:00\x1f\x1faaa\x1fsecond\n\x1e";
        let c = parse_log(out).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].refs, vec!["HEAD -> main", "tag: v1"]);
        assert_eq!(c[0].message, "first\n\nbody");
        assert_eq!(c[1].hash, "bbb");
        assert_eq!(c[1].parents, vec!["aaa"]);
        assert_eq!(c[1].subject(), "second");
        assert!(parse_log("garbage\x1e").is_err());
    }

    #[test]
    fn bulk_author_and_date_spread() {
        let mut d = vec![rec("1", &[], "Alice", "a"), rec("2", &["1"], "Bob", "b"), rec("3", &["2"], "alicia", "c")];
        assert_eq!(replace_author(&mut d, "ALIC", "Al", "al@x", false).unwrap(), 2);
        assert_eq!(d[2].author_name, "Al");
        assert_eq!(replace_author(&mut d, "bob", "B", "b@x", true).unwrap(), 1);
        assert!(replace_author(&mut d, "", "x", "y", true).is_err());
        let mut seed = 7u64;
        let rand = move |n: u64| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 33) % n
        };
        assert_eq!(spread_dates(&mut d, "2024-03-01", "2024-03-03", "08:00", "12:00", rand).unwrap(), 3);
        let times: Vec<GitDate> = d.iter().map(|c| GitDate::parse(&c.author_date).unwrap()).collect();
        for w in times.windows(2) {
            assert!(w[0].to_unix(0) <= w[1].to_unix(0));
        }
        for t in &times {
            assert!(t.offset.is_none() && t.year == 2024 && t.month == 3 && (1..=3).contains(&t.day) && (8..12).contains(&t.hour));
        }
        assert_eq!(d[0].committer_date, d[0].author_date);
        assert!(spread_dates(&mut d, "2024-03-03", "2024-03-01", "08:00", "12:00", |_| 0).is_err());
        assert!(spread_dates(&mut d, "2024-03-01", "2024-03-01", "12:00", "08:00", |_| 0).is_err());
    }

    #[test]
    fn co_author_removal() {
        let m = "Fix it\n\nBody.\n\nCo-authored-by: Bob <b@x>\nCo-Authored-By: Claude <noreply@anthropic.com>\n🤖 Generated with Claude Code\nhttps://claude.ai/code\n\nClaude-Session: abc\n";
        let (out, n) = remove_co_authors(m);
        assert_eq!(out, "Fix it\n\nBody.");
        assert_eq!(n, 5);
        assert_eq!(remove_co_authors("plain\n"), ("plain".to_string(), 0));
    }

    #[test]
    fn ai_commit_detection_includes_merges() {
        let commits = vec![
            rec("a", &[], "Ada", "root"),
            rec("b", &["a"], "Ada", "main work"),
            rec("c", &["a"], "Ada", "feature\n\nCo-Authored-By: Claude <noreply@anthropic.com>"),
            rec("d", &["b", "c"], "Ada", "Merge feature"),
            rec("e", &["d"], "Ada", "after"),
            rec("f", &["e", "b"], "Ada", "Merge main again"),
        ];
        let ai = ai_commits(&commits);
        let mut v: Vec<&str> = ai.iter().map(String::as_str).collect();
        v.sort();
        assert_eq!(v, vec!["c", "d"]);
    }

    #[test]
    fn applies_edits_with_real_git() {
        use crate::git_cli::{JobEvent, available, start_job};
        if !available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_path_buf();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git").current_dir(&p).args(args).output().unwrap();
            assert!(o.status.success(), "{args:?} {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).into_owned()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Orig"]);
        git(&["config", "user.email", "orig@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        for (f, m) in [("a", "one"), ("b", "two\n\nCo-Authored-By: Claude <noreply@anthropic.com>")] {
            std::fs::write(p.join(f), f).unwrap();
            git(&["add", f]);
            git(&["commit", "-q", "-m", m]);
        }
        type Body = Box<dyn FnOnce(&JobCtx) -> Result<String, String> + Send>;
        let run = |f: Body| {
            let job = start_job("t", p.clone(), f);
            loop {
                match job.rx.recv_timeout(std::time::Duration::from_secs(60)).expect("job finishes") {
                    JobEvent::Finished(r) => return r,
                    _ => continue,
                }
            }
        };
        let (tx, rx) = std::sync::mpsc::channel();
        run(Box::new(move |c| {
            tx.send(load(c)).unwrap();
            Ok(String::new())
        }))
        .unwrap();
        let (facts, commits) = rx.recv().unwrap().unwrap();
        assert!(facts.clean);
        assert_eq!(facts.branch.as_deref(), Some("main"));
        assert_eq!(commits.len(), 2);
        let mut first = commits[0].clone();
        first.author_name = "New Name".into();
        first.author_date = "2020-02-03 04:05:06 +0000".into();
        first.message = "one, edited".into();
        let drafts: HashMap<String, CommitRecord> = [(first.hash.clone(), first)].into_iter().collect();
        let changes = planned(&commits, &drafts, true);
        assert_eq!(changes.len(), 2, "edited commit plus co-author removal");
        let originals = commits.clone();
        let msg = run(Box::new(move |c| apply(c, &originals, &changes, &ApplyOptions::default()))).unwrap();
        assert!(msg.starts_with("Rewrote 2 commits; backup tag apex-backup-"), "{msg}");
        let log = git(&["log", "--reverse", "--format=%an|%aI|%B%x1e"]);
        let entries: Vec<&str> = log.split('\x1e').map(str::trim).filter(|s| !s.is_empty()).collect();
        assert_eq!(entries[0], "New Name|2020-02-03T04:05:06Z|one, edited");
        assert_eq!(entries[1], "Orig|".to_string() + &commits[1].author_date + "|two");
        let tag = git(&["tag"]).trim().to_string();
        assert!(tag.starts_with("apex-backup-"));
        assert_eq!(git(&["log", "--reverse", "--format=%an|%s", &tag]).trim(), "Orig|one\nOrig|two", "backup tag must keep the old history");
        assert!(git(&["for-each-ref", "refs/original/"]).trim().is_empty());
        // Dirty trees are refused.
        std::fs::write(p.join("a"), "changed").unwrap();
        let originals = commits.clone();
        let err = run(Box::new(move |c| apply(c, &originals, &[], &ApplyOptions::default()))).unwrap_err();
        assert!(err.contains("uncommitted"), "{err}");
    }
}
