//! `kontext changelog` (DESIGN §10): a deterministic technical changelog. The
//! tasks closed in the range (by the `hlc` of their `done`), grouped by epic
//! with their evidence, plus the merged PRs of each `[[repos]]` repo that is on
//! GitHub (via `gh pr list`), grouped by the type in their title (`feat: …`,
//! `fix: …`).
//!
//! Nothing depends on the time it runs: the same input gives the same bytes. If
//! `gh` is missing or fails, it notes that and comes out with only the tasks.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::config::Project;
use crate::git;
use crate::install::normalize_remote;
use crate::model::Entry;
use crate::state::State;
use crate::tasks::{TaskStatus, epic_name};

/// One end of the range, as text comparable with `hlc` and `mergedAt`
/// (`YYYY-MM-DDTHH:MM:SS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Bound {
    pub at: String,
    /// What was asked for (`2026-11-01`, `v1.2`).
    pub given: String,
}

fn iso(ts: jiff::Timestamp) -> String {
    ts.strftime("%Y-%m-%dT%H:%M:%S").to_string()
}

/// A date (`YYYY-MM-DD` or ISO-8601) or a git ref (its commit date in `repo`).
/// `end_of_day`: a lone date counts the whole day.
pub fn parse_bound(s: &str, repo: Option<&Path>, end_of_day: bool) -> Result<Bound> {
    let s = s.trim();
    if s.len() == 10
        && let Ok(d) = s.parse::<jiff::civil::Date>()
    {
        let mut ts = d.to_zoned(jiff::tz::TimeZone::UTC)?.timestamp();
        if end_of_day {
            ts = ts.checked_add(jiff::SignedDuration::from_secs(86_400 - 1))?;
        }
        return Ok(Bound {
            at: iso(ts),
            given: s.to_string(),
        });
    }
    if let Ok(ts) = s.parse::<jiff::Timestamp>() {
        return Ok(Bound {
            at: iso(ts),
            given: s.to_string(),
        });
    }
    let Some(repo) = repo else {
        bail!(
            "{s:?} is not a date (YYYY-MM-DD or ISO-8601), and there is no git repo here to read it as a ref"
        );
    };
    let Some(date) = git::line(repo, &["log", "-1", "--format=%cI", s, "--"]) else {
        bail!(
            "{s:?} is neither a date nor a git ref in {}",
            repo.display()
        );
    };
    let ts: jiff::Timestamp = date
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid commit date {date:?}: {e}"))?;
    Ok(Bound {
        at: iso(ts),
        given: s.to_string(),
    })
}

fn in_range(at: &str, since: Option<&Bound>, until: Option<&Bound>) -> bool {
    let at = at.get(..19).unwrap_or(at);
    since.is_none_or(|b| at >= b.at.as_str()) && until.is_none_or(|b| at <= b.at.as_str())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClosedTask {
    pub id: String,
    pub title: String,
    /// `hlc` of the `done`.
    pub closed: String,
    pub by: String,
    pub done: String,
    pub evidence: Vec<Value>,
    /// The links to PRs in its evidence.
    pub prs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EpicGroup {
    pub epic: Option<String>,
    pub name: Option<String>,
    pub title: String,
    pub tasks: Vec<ClosedTask>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    #[serde(rename = "mergedAt")]
    pub merged_at: String,
    pub author: String,
    /// `feat`, `fix`…, or empty if the title doesn't follow `<type>: <desc>`.
    pub kind: String,
    /// The title without the `<type>:`.
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct KindGroup {
    /// Empty if the titles don't follow `<type>: <desc>` ("other").
    pub kind: String,
    pub prs: Vec<Pr>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoPrs {
    pub repo: String,
    pub github: String,
    pub groups: Vec<KindGroup>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Changelog {
    pub project: String,
    pub since: Option<Bound>,
    pub until: Option<Bound>,
    pub epics: Vec<EpicGroup>,
    pub repos: Vec<RepoPrs>,
    pub notes: Vec<String>,
}

static PR_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https?://github\.com/[^/]+/[^/]+/pull/[0-9]+").unwrap());
static CONVENTIONAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z]+)(?:\([^)]*\))?!?:\s*(.+)$").unwrap());

/// The order of the types. The others come after, by name, and last come the
/// ones with no type.
const KINDS: &[&str] = &[
    "feat", "fix", "perf", "refactor", "docs", "test", "build", "ci", "chore", "style", "revert",
];

fn kind_rank(t: &str) -> (usize, String) {
    match KINDS.iter().position(|x| *x == t) {
        Some(i) => (i, String::new()),
        None if t.is_empty() => (usize::MAX, String::new()),
        None => (KINDS.len(), t.to_string()),
    }
}

/// `feat(api): thing` → (`feat`, `thing`).
pub fn split_kind(title: &str) -> (String, String) {
    match CONVENTIONAL.captures(title.trim()) {
        Some(c) => (c[1].to_lowercase(), c[2].trim().to_string()),
        None => (String::new(), title.trim().to_string()),
    }
}

/// The tasks closed in the range, by epic.
pub fn closed_tasks(st: &State, since: Option<&Bound>, until: Option<&Bound>) -> Vec<EpicGroup> {
    let mut by_epic: BTreeMap<Option<String>, Vec<ClosedTask>> = BTreeMap::new();
    for t in st.of_kind("task") {
        if st.task_status(&t.id) != Some(TaskStatus::Closed) {
            continue;
        }
        let Some(done) = st.task(&t.id).and_then(|i| i.done.clone()) else {
            continue;
        };
        let Some(d) = st.get(&done) else { continue };
        if !in_range(&d.hlc, since, until) {
            continue;
        }
        let evidence = match d.fields.get("evidence") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let prs = evidence
            .iter()
            .filter_map(|e| e.get("value").and_then(Value::as_str))
            .filter(|v| PR_URL.is_match(v))
            .map(str::to_string)
            .collect();
        by_epic
            .entry(st.epic_of(t).map(|e| e.id.clone()))
            .or_default()
            .push(ClosedTask {
                id: t.id.clone(),
                title: t.title.trim().to_string(),
                closed: d.hlc.clone(),
                by: d.actor.short(),
                done: d.id.clone(),
                evidence,
                prs,
            });
    }
    let pos = |id: &Option<String>| -> (usize, String) {
        match id {
            Some(i) => (
                st.order
                    .iter()
                    .position(|x| x == i)
                    .unwrap_or(usize::MAX - 1),
                i.clone(),
            ),
            None => (usize::MAX, String::new()),
        }
    };
    let mut groups: Vec<EpicGroup> = by_epic
        .into_iter()
        .map(|(epic, mut tasks)| {
            tasks.sort_by(|a, b| (&a.closed, &a.id).cmp(&(&b.closed, &b.id)));
            let e: Option<&Entry> = epic.as_ref().and_then(|i| st.get(i));
            EpicGroup {
                name: e.map(epic_name),
                title: e
                    .map(|e| e.title.trim().to_string())
                    .unwrap_or_else(|| "No epic".into()),
                epic,
                tasks,
            }
        })
        .collect();
    groups.sort_by_key(|g| pos(&g.epic));
    groups
}

/// `github.com/a/b` → `a/b`.
pub fn github_slug(remote: &str) -> Option<String> {
    let n = normalize_remote(remote);
    let rest = n
        .strip_prefix("github.com/")
        .or_else(|| n.strip_prefix("www.github.com/"))?;
    let mut parts = rest.split('/');
    let (Some(o), Some(r)) = (parts.next(), parts.next()) else {
        return None;
    };
    (!o.is_empty() && !r.is_empty()).then(|| format!("{o}/{r}"))
}

fn gh_bin() -> String {
    std::env::var("KONTEXT_GH_BIN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "gh".into())
}

/// The merged PRs of a GitHub repo in the range.
pub fn merged_prs(slug: &str, since: Option<&Bound>, until: Option<&Bound>) -> Result<Vec<Pr>> {
    let mut search = Vec::new();
    if let Some(s) = since {
        search.push(format!("merged:>={}", &s.at[..10]));
    }
    if let Some(u) = until {
        search.push(format!("merged:<={}", &u.at[..10]));
    }
    let mut cmd = Command::new(gh_bin());
    cmd.args(["pr", "list", "--repo", slug, "--state", "merged"]);
    if !search.is_empty() {
        cmd.args(["--search", &search.join(" ")]);
    }
    cmd.args([
        "--json",
        "number,title,url,mergedAt,author",
        "--limit",
        "1000",
    ]);
    let out = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("could not run {}: {e}", gh_bin()))?;
    if !out.status.success() {
        bail!(
            "{} exited with {}: {}",
            gh_bin(),
            out.status.code().unwrap_or(-1),
            crate::views::one_line(&String::from_utf8_lossy(&out.stderr), 200)
        );
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow::anyhow!("{} did not return JSON: {e}", gh_bin()))?;
    let Value::Array(items) = v else {
        bail!("{} did not return a list", gh_bin());
    };
    let mut prs: Vec<Pr> = items
        .iter()
        .filter_map(|p| {
            let merged_at = p.get("mergedAt")?.as_str()?.to_string();
            if !in_range(&merged_at, since, until) {
                return None;
            }
            let title = p.get("title")?.as_str()?.trim().to_string();
            let (kind, summary) = split_kind(&title);
            Some(Pr {
                number: p.get("number")?.as_u64()?,
                url: p
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                author: p
                    .get("author")
                    .and_then(|a| a.get("login"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                merged_at,
                title,
                kind,
                summary,
            })
        })
        .collect();
    prs.sort_by(|a, b| (&a.merged_at, a.number).cmp(&(&b.merged_at, b.number)));
    Ok(prs)
}

fn group_prs(prs: Vec<Pr>) -> Vec<KindGroup> {
    let mut groups: BTreeMap<(usize, String), KindGroup> = BTreeMap::new();
    for p in prs {
        groups
            .entry(kind_rank(&p.kind))
            .or_insert_with(|| KindGroup {
                kind: p.kind.clone(),
                prs: Vec::new(),
            })
            .prs
            .push(p);
    }
    groups.into_values().collect()
}

/// Builds the changelog.
pub fn build(
    st: &State,
    project: &Project,
    since: Option<Bound>,
    until: Option<Bound>,
) -> Changelog {
    let epics = closed_tasks(st, since.as_ref(), until.as_ref());
    let mut repos = Vec::new();
    let mut notes = Vec::new();
    for r in &project.repos {
        let Some(slug) = r.remote.as_deref().and_then(github_slug) else {
            continue;
        };
        match merged_prs(&slug, since.as_ref(), until.as_ref()) {
            Ok(prs) => repos.push(RepoPrs {
                repo: r.name.clone(),
                github: slug,
                groups: group_prs(prs),
            }),
            Err(e) => notes.push(format!(
                "Could not read the PRs of {} ({slug}): {e:#}. Only the tasks are included.",
                r.name
            )),
        }
    }
    Changelog {
        project: project.name.clone(),
        since,
        until,
        epics,
        repos,
        notes,
    }
}

fn evidence_line(e: &Value) -> String {
    let ty = e.get("type").and_then(Value::as_str).unwrap_or("?");
    let v = e.get("value").and_then(Value::as_str).unwrap_or("");
    match ty {
        "pr" => format!("**PR** {v}"),
        "url" if PR_URL.is_match(v) => format!("**PR** {v}"),
        "command" => match e.get("exit_code").and_then(Value::as_i64) {
            Some(n) => format!("command `{v}` (exited with {n})"),
            None => format!("command `{v}`"),
        },
        "file" => format!("file `{v}`"),
        other => format!("{other} {v}"),
    }
}

impl Changelog {
    pub fn markdown(&self) -> String {
        let mut l = vec![format!("# Technical changelog of {}", self.project)];
        let range = match (&self.since, &self.until) {
            (None, None) => "The whole ledger.".to_string(),
            (Some(s), None) => format!("Since {}.", s.given),
            (None, Some(u)) => format!("Until {}.", u.given),
            (Some(s), Some(u)) => format!("From {} to {}.", s.given, u.given),
        };
        l.push(String::new());
        l.push(range);
        let n: usize = self.epics.iter().map(|g| g.tasks.len()).sum();
        l.push(String::new());
        l.push(format!("## Closed tasks ({n})"));
        if n == 0 {
            l.push(String::new());
            l.push("None in the range.".into());
        }
        for g in &self.epics {
            l.push(String::new());
            match &g.epic {
                Some(id) => l.push(format!("### {} ({id})", g.title)),
                None => l.push(format!("### {}", g.title)),
            }
            for t in &g.tasks {
                l.push(format!(
                    "- {} {} (closed {} by {})",
                    t.id,
                    t.title,
                    crate::views::when(&t.closed),
                    t.by
                ));
                // PRs first.
                let mut ev: Vec<&Value> = t.evidence.iter().collect();
                ev.sort_by_key(|e| !evidence_line(e).starts_with("**PR**"));
                for e in ev {
                    l.push(format!("  - {}", evidence_line(e)));
                }
            }
        }
        let total: usize = self
            .repos
            .iter()
            .flat_map(|r| r.groups.iter())
            .map(|g| g.prs.len())
            .sum();
        if !self.repos.is_empty() {
            l.push(String::new());
            l.push(format!("## Merged PRs ({total})"));
            for r in &self.repos {
                l.push(String::new());
                l.push(format!("### {} ({})", r.repo, r.github));
                if r.groups.is_empty() {
                    l.push(String::new());
                    l.push("None in the range.".into());
                }
                for g in &r.groups {
                    l.push(String::new());
                    l.push(format!(
                        "#### {}",
                        if g.kind.is_empty() { "other" } else { &g.kind }
                    ));
                    for p in &g.prs {
                        let author = if p.author.is_empty() {
                            String::new()
                        } else {
                            format!(", {}", p.author)
                        };
                        l.push(format!(
                            "- #{} {} ({}{author}) {}",
                            p.number,
                            p.summary,
                            p.merged_at.get(..10).unwrap_or(&p.merged_at),
                            p.url
                        ));
                    }
                }
            }
        }
        if !self.notes.is_empty() {
            l.push(String::new());
            l.push("## Notes".into());
            for n in &self.notes {
                l.push(format!("- {n}"));
            }
        }
        l.join("\n") + "\n"
    }

    pub fn json(&self) -> Value {
        serde_json::to_value(self).expect("can be serialized")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_slugs() {
        assert_eq!(
            split_kind("feat(api): charge"),
            ("feat".into(), "charge".into())
        );
        assert_eq!(split_kind("Fix!: thing"), ("fix".into(), "thing".into()));
        assert_eq!(split_kind("No type"), (String::new(), "No type".into()));
        assert_eq!(
            github_slug("git@github.com:Acme/Billing.git").as_deref(),
            Some("Acme/Billing")
        );
        assert_eq!(github_slug("gitlab.com/a/b"), None);
        assert!(kind_rank("feat") < kind_rank("fix"));
        assert!(kind_rank("revert") < kind_rank("wip"));
        assert!(kind_rank("wip") < kind_rank(""));
    }

    #[test]
    fn bounds() {
        let b = parse_bound("2026-11-01", None, false).unwrap();
        assert_eq!(b.at, "2026-11-01T00:00:00");
        let b = parse_bound("2026-11-01", None, true).unwrap();
        assert_eq!(b.at, "2026-11-01T23:59:59");
        let b = parse_bound("2026-11-01T10:00:00Z", None, true).unwrap();
        assert_eq!(b.at, "2026-11-01T10:00:00");
        assert!(parse_bound("v1.0", None, false).is_err());
        assert!(in_range(
            "2026-11-01T23:59:59.500Z.0001",
            Some(&parse_bound("2026-11-01", None, false).unwrap()),
            Some(&parse_bound("2026-11-01", None, true).unwrap())
        ));
    }
}
