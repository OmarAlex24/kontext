//! Sections of a Markdown file, its status (current, proposed or historical) and
//! the repo paths it cites. What depends on the repo is configurable:
//!
//! - Paths that are historical by where they live (`docs/work/`, snapshots)
//!   come from `historical = [...]` in each repo.
//! - The folders that a cited path starts with come from `roots` (by default,
//!   the top-level folders of the repo).
//! - An ADR is any file whose name starts with `ADR-NNNN`, not only the ones in
//!   `docs/01-decisions/`.
//!
//! The regular expressions imitate JavaScript's: `.` does not cross `\r`
//! (`[^\r\n\u2028\u2029]`), `\w`, `\d` and `\b` are ASCII.

use std::sync::LazyLock;

use globset::{GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// How much a doc describes the system as it is today.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Current,
    Proposed,
    Historical,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Current => "current",
            Status::Historical => "historical",
            Status::Proposed => "proposed",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Current => "current",
            Status::Historical => "historical",
            Status::Proposed => "proposed",
        }
    }

    pub fn parse(s: &str) -> Status {
        match s {
            "historical" => Status::Historical,
            "proposed" => Status::Proposed,
            _ => Status::Current,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusInfo {
    /// What the doc says ("Accepted", "canonical"…) or the rule that decided it.
    pub label: String,
    pub status: Status,
}

impl StatusInfo {
    fn new(label: &str, status: Status) -> StatusInfo {
        StatusInfo {
            label: label.to_string(),
            status,
        }
    }
}

/// A section: from its H1–H3 heading up to the next one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    /// The headings above it, from the outermost to `heading`.
    pub heading_path: Vec<String>,
    /// Lines, starting at 1 and including both ends.
    pub start_line: usize,
    pub end_line: usize,
    /// The text, including the heading line.
    pub body: String,
    /// Repo paths it cites (not checked for existence).
    pub refs: Vec<String>,
}

/// A file that has been read.
#[derive(Clone, Debug)]
pub struct Parsed {
    pub status: StatusInfo,
    pub read_when: String,
    pub sections: Vec<Section>,
    /// If it is an ADR: the ADRs it says it replaces entirely.
    pub supersedes: Vec<String>,
}

/// JavaScript's `.`: anything except a line terminator.
const DOT: &str = r"[^\r\n\x{2028}\x{2029}]";

fn re(s: &str) -> Regex {
    Regex::new(&s.replace("{DOT}", DOT)).expect("valid regular expression")
}

static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^(#{1,3})\s+({DOT}+?)\s*#*\s*$"));
static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(```|~~~)"));
static STATUS_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^\s*(?:>\s*)?(?:-\s*)?\*\*Status:\*\*\s*({DOT}+)$"));
static READ_WHEN_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^\s*(?:>\s*)?-\s*\*\*Read when:\*\*\s*({DOT}+)$"));
static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^#\s+({DOT}+?)\s*$"));
static ADR_FILE_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^ADR-([0-9]{4})"));
const STATUS_SCAN_LINES: usize = 60;

// The status and supersession patterns also match the Spanish words that the
// docs use, so they stay as they are.
static HISTORICAL_STATUS_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(supersed|deprecat|reject|retir|legacy|historic|snapshot|obsolet|archiv)")
});
static PARTIAL_SUPERSESSION_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^supersed(?-u:\w)*\s+(in\s+(part|its)|partially)(?-u:\b)"));
static PROPOSED_STATUS_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(propos|propuest|draft|borrador|roadmap)"));

static ADR_ID_RE: LazyLock<Regex> = LazyLock::new(|| re(r"ADR-([0-9]{4})"));
static ADR_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"\((?:\./)?(ADR-[0-9]{4}-[^)\s#]*?)\.md"));
static ADR_RANGE_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"ADR-([0-9]{4})\s+(?:a|to|–|-)\s+ADR-([0-9]{4})"));
static SUPERSEDES_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^\s*(?:>\s*)?(?:-\s*)?(?:\*\*)?(?:Fecha:[^.]*\.\s*)?(?:supersedes|reemplaza(?: a)?|sustituye(?: a)?)(?-u:\b)({DOT}*)$",
    )
});
static PARTIAL_SUPERSEDES_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)clarif|portion|parcial|in part|decision [0-9]|decisión [0-9]|§|incompatible")
});
static SENTENCE_END_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\.\s"));
const SUPERSESSION_SCAN_LINES: usize = 40;

static EXTERNAL_TARGET_RE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(#|/|[a-z][a-z0-9+.-]*:)"));
static MARKDOWN_LINK_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\]\(([^)\s]+)\)"));
static PLACEHOLDER_RE: LazyLock<Regex> = LazyLock::new(|| re(r"[<>*{}]|NNNN|\.\.\.|XXXX"));
static UNTRACKED_OUTPUT_RE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(^|/)(node_modules|dist|build|coverage|\.turbo)(/|$)"));
static TRIM_HASH_RE: LazyLock<Regex> = LazyLock::new(|| re(r"#{DOT}*$"));
static TRIM_LINE_RE: LazyLock<Regex> = LazyLock::new(|| re(r":[0-9]+(?:-[0-9]+)?$"));
static TRIM_PUNCT_RE: LazyLock<Regex> = LazyLock::new(|| re(r"[.,;:)\]]+$"));

/// The configurable parts of a repo: historical paths and top-level folders.
#[derive(Clone, Debug)]
pub struct Rules {
    historical: GlobSet,
    repo_path_re: Option<Regex>,
}

impl Rules {
    pub fn new(historical: &[String], roots: &[String]) -> anyhow::Result<Rules> {
        let mut b = GlobSetBuilder::new();
        for g in historical {
            b.add(
                globset::GlobBuilder::new(g)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| anyhow::anyhow!("historical = {g:?}: {e}"))?,
            );
        }
        let roots: Vec<String> = roots
            .iter()
            .map(|r| r.trim().trim_matches('/'))
            .filter(|r| !r.is_empty())
            .map(regex::escape)
            .collect();
        let repo_path_re = (!roots.is_empty()).then(|| {
            Regex::new(&format!(r"(?:{})/[A-Za-z0-9_@.\-/$\[\]]+", roots.join("|")))
                .expect("valid regular expression")
        });
        Ok(Rules {
            historical: b.build()?,
            repo_path_re,
        })
    }

    /// The rules of `examples/find-repo.toml`, plus the folders the unit
    /// tests cite (for the tests).
    pub fn example() -> Rules {
        Rules::new(
            &["docs/archive/**".into(), "docs/work/**".into()],
            &["apps", "db", "docs", "scripts", "services", "src"].map(String::from),
        )
        .unwrap()
    }
}

/// JavaScript's `trim()`: it also strips the BOM.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// The main word decides (`canonical with legacy notes` is still current;
/// `Superseded by ADR-0032` is not).
pub fn classify_status(label: &str) -> Status {
    let lower = label.to_lowercase();
    let lower = js_trim(lower.trim_start_matches(|c: char| !c.is_alphabetic()));
    if PARTIAL_SUPERSESSION_RE.is_match(lower) {
        return Status::Current;
    }
    if HISTORICAL_STATUS_RE.is_match(lower) {
        return Status::Historical;
    }
    if PROPOSED_STATUS_RE.is_match(lower) {
        return Status::Proposed;
    }
    Status::Current
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The ADR number of a file (`ADR-0102-x.md` → `0102`).
pub fn adr_number(path: &str) -> Option<String> {
    ADR_FILE_RE
        .captures(basename(path))
        .map(|c| c[1].to_string())
}

/// The name without `.md`.
fn stem(path: &str) -> &str {
    let b = basename(path);
    b.strip_suffix(".md").unwrap_or(b)
}

/// A set that remembers the order in which each item was added (JavaScript's
/// `Set`: deleting and adding again moves it to the end).
#[derive(Default)]
struct Ordered(Vec<String>);

impl Ordered {
    fn add(&mut self, s: &str) {
        if !self.0.iter().any(|x| x == s) {
            self.0.push(s.to_string());
        }
    }

    fn delete(&mut self, s: &str) {
        self.0.retain(|x| x != s);
    }
}

/// The ADRs a doc says it replaces entirely («Supersedes: ADR-0003»,
/// «Reemplaza a ADR-0031», «Sustituye … de ADR-0034 a ADR-0039»). Only counts
/// at the start of a line, and not if the replacement is partial.
pub fn superseded_adrs(lines: &[&str]) -> Vec<String> {
    let mut targets = Ordered::default();
    for line in lines.iter().take(SUPERSESSION_SCAN_LINES) {
        let Some(c) = SUPERSEDES_LINE_RE.captures(line) else {
            continue;
        };
        let rest = c.get(1).map(|m| m.as_str()).unwrap_or("");
        if PARTIAL_SUPERSEDES_RE.is_match(rest) {
            continue;
        }
        let sentence = SENTENCE_END_RE.split(rest).next().unwrap_or("");
        for m in ADR_ID_RE.captures_iter(sentence) {
            targets.add(&m[1]);
        }
        // Two ADRs can share a number; a link says which file it is.
        for m in ADR_LINK_RE.captures_iter(sentence) {
            let full = &m[1];
            targets.delete(full.get(4..8).unwrap_or(""));
            targets.add(full);
        }
        if let Some(r) = ADR_RANGE_RE.captures(sentence) {
            let a: u32 = r[1].parse().unwrap_or(0);
            let b: u32 = r[2].parse().unwrap_or(0);
            for id in a..=b {
                targets.add(&format!("{id:04}"));
            }
        }
    }
    targets.0
}

/// What is needed from each file for the second pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    pub path: String,
    pub status: StatusInfo,
    pub supersedes: Vec<String>,
}

/// Second pass: an ADR that still says `Accepted` is historical if a current
/// ADR says it replaces it entirely. `files` is in path order; the first one
/// that declares it wins.
pub fn apply_supersession(files: &mut [FileStatus]) {
    let mut superseders: Vec<(String, String)> = Vec::new();
    for f in files.iter() {
        let Some(declarer) = adr_number(&f.path) else {
            continue;
        };
        if f.status.status != Status::Current {
            continue;
        }
        for target in &f.supersedes {
            if *target != declarer
                && !target.starts_with(&format!("ADR-{declarer}-"))
                && !superseders.iter().any(|(t, _)| t == target)
            {
                superseders.push((target.clone(), declarer.clone()));
            }
        }
    }
    let by_of = |k: &str| {
        superseders
            .iter()
            .find(|(t, _)| t == k)
            .map(|(_, d)| d.clone())
    };
    for f in files.iter_mut() {
        let Some(id) = adr_number(&f.path) else {
            continue;
        };
        let by = by_of(stem(&f.path)).or_else(|| by_of(&id));
        if let Some(by) = by
            && f.status.status != Status::Historical
        {
            f.status = StatusInfo::new(&format!("superseded by ADR-{by}"), Status::Historical);
        }
    }
}

/// The status of a doc that nobody maintains: historical by its path, or what
/// its `Status:` line says, or current.
pub fn doc_status(path: &str, lines: &[&str], rules: &Rules) -> StatusInfo {
    if rules.historical.is_match(path) {
        return StatusInfo::new("historical path", Status::Historical);
    }
    for line in lines.iter().take(STATUS_SCAN_LINES) {
        if let Some(c) = STATUS_LINE_RE.captures(line) {
            let label = c[1].replace('*', "");
            let label = js_trim(&label);
            return StatusInfo::new(label, classify_status(label));
        }
    }
    if adr_number(path).is_some() {
        return StatusInfo::new("ADR without status", Status::Current);
    }
    StatusInfo::new("undeclared", Status::Current)
}

fn read_when_of(lines: &[&str]) -> String {
    for line in lines.iter().take(STATUS_SCAN_LINES) {
        if let Some(c) = READ_WHEN_RE.captures(line) {
            return js_trim(&c[1]).to_string();
        }
    }
    String::new()
}

fn trim_ref(r: &str) -> String {
    let r = TRIM_HASH_RE.replace(r, "");
    let r = TRIM_LINE_RE.replace(&r, "");
    TRIM_PUNCT_RE.replace(&r, "").into_owned()
}

/// Node's `posix.normalize` (relative paths).
fn posix_normalize(p: &str) -> String {
    let trailing = p.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let mut s = if out.is_empty() {
        ".".to_string()
    } else {
        out.join("/")
    };
    if trailing {
        s.push('/');
    }
    s
}

fn posix_dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => ".",
    }
}

/// Can a path start here? (JavaScript's `(?<![\w./-])`).
fn path_boundary(text: &str, at: usize) -> bool {
    match text[..at].chars().next_back() {
        None => true,
        Some(c) => !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')),
    }
}

/// The repo paths that a piece of Markdown cites: paths that start in a
/// top-level folder (in backticks or bare) and relative links, resolved from the
/// doc's folder. No templates (`<module>`, `NNNN`, globs).
pub fn extract_repo_refs(doc_path: &str, text: &str, rules: &Rules) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    if let Some(rx) = &rules.repo_path_re {
        let mut start = 0;
        while start <= text.len() {
            let Some(m) = rx.find_at(text, start) else {
                break;
            };
            if !path_boundary(text, m.start()) {
                // Like JavaScript's `lastIndex`: retry one character later.
                let step = text[m.start()..].chars().next().map_or(1, char::len_utf8);
                start = m.start() + step;
                continue;
            }
            start = m.end();
            let raw = m.as_str();
            let r = trim_ref(raw);
            let next: String = text[m.end()..]
                .chars()
                .next()
                .map(String::from)
                .unwrap_or_default();
            let prev = text[..m.start()].chars().next_back();
            // `[label](target)`: the label usually abbreviates the target, which
            // is checked below on its own.
            let is_link_label = prev == Some('[') && (raw.ends_with(']') || next == "]");
            if !is_link_label
                && r.contains('/')
                && !PLACEHOLDER_RE.is_match(&format!("{r}{next}"))
                && !UNTRACKED_OUTPUT_RE.is_match(&r)
            {
                refs.push(r.trim_end_matches('/').to_string());
            }
        }
    }
    let doc_dir = posix_dirname(doc_path);
    for c in MARKDOWN_LINK_RE.captures_iter(text) {
        let target = &c[1];
        if EXTERNAL_TARGET_RE.is_match(target)
            || PLACEHOLDER_RE.is_match(target)
            || UNTRACKED_OUTPUT_RE.is_match(target)
        {
            continue;
        }
        let joined = format!("{doc_dir}/{}", trim_ref(target));
        let resolved = posix_normalize(&joined);
        if !resolved.starts_with("..") && resolved != "." {
            refs.push(resolved.trim_end_matches('/').to_string());
        }
    }
    refs.sort();
    refs.dedup();
    refs
}

fn title_of(path: &str, lines: &[&str]) -> String {
    for line in lines {
        if let Some(c) = TITLE_RE.captures(line) {
            return c[1].to_string();
        }
    }
    stem(path).to_string()
}

/// Splits a Markdown file into H1–H3 sections, not counting `#` inside code
/// blocks.
pub fn parse_markdown(path: &str, text: &str, rules: &Rules) -> Parsed {
    let lines: Vec<&str> = text.split('\n').collect();
    let status = doc_status(path, &lines, rules);
    let read_when = read_when_of(&lines);
    let title = title_of(path, &lines);
    let mut sections = Vec::new();
    let mut stack: Vec<Option<String>> = Vec::new();
    let mut open = (title.clone(), vec![title], 1usize);
    let mut in_fence = false;

    let close = |sections: &mut Vec<Section>, open: &(String, Vec<String>, usize), end: usize| {
        if end < open.2 {
            return;
        }
        let body = lines[open.2 - 1..end].join("\n");
        if js_trim(&body).is_empty() {
            return;
        }
        sections.push(Section {
            refs: extract_repo_refs(path, &body, rules),
            body,
            heading: open.0.clone(),
            heading_path: open.1.clone(),
            start_line: open.2,
            end_line: end,
        });
    };

    for (i, line) in lines.iter().enumerate() {
        if FENCE_RE.is_match(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let Some(c) = HEADING_RE.captures(line) else {
            continue;
        };
        let n = i + 1;
        close(&mut sections, &open, n - 1);
        let depth = c[1].len();
        let heading = c[2].to_string();
        stack.resize(depth - 1, None);
        stack.push(Some(heading.clone()));
        open = (heading, stack.iter().flatten().cloned().collect(), n);
    }
    close(&mut sections, &open, lines.len());

    let supersedes = if adr_number(path).is_some() {
        let joined = sections
            .iter()
            .map(|s| s.body.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let ls: Vec<&str> = joined.split('\n').collect();
        superseded_adrs(&ls)
    } else {
        Vec::new()
    };
    Parsed {
        status,
        read_when,
        sections,
        supersedes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules::example()
    }

    const GUIDE: &str = "# Guide\nIntro that points to `services/api-core/src/app.ts`.\n\n## Geofences\n```sql\n# not a heading\n```\nGeofence text.\n### Detail\nMore detail.";

    #[test]
    fn splits_sections_with_line_ranges() {
        let p = parse_markdown("docs/guide.md", GUIDE, &rules());
        let got: Vec<(&str, usize, usize)> = p
            .sections
            .iter()
            .map(|s| (s.heading.as_str(), s.start_line, s.end_line))
            .collect();
        assert_eq!(
            got,
            vec![("Guide", 1, 3), ("Geofences", 4, 8), ("Detail", 9, 10)]
        );
        assert_eq!(
            p.sections[2].heading_path,
            vec!["Guide", "Geofences", "Detail"]
        );
        assert_eq!(p.sections[0].refs, vec!["services/api-core/src/app.ts"]);
    }

    #[test]
    fn preamble_is_named_after_the_file() {
        let p = parse_markdown("docs/untitled.md", "loose text\n## A\nb", &rules());
        assert_eq!(p.sections[0].heading, "untitled");
    }

    #[test]
    fn status_lines_and_paths() {
        let r = rules();
        let st = |path: &str, lines: &[&str]| doc_status(path, lines, &r).status;
        assert_eq!(
            st(
                "docs/01-decisions/ADR-0035-outbox.md",
                &["# ADR-0035", "**Status:** Superseded by ADR-0123"]
            ),
            Status::Historical
        );
        assert_eq!(
            st(
                "docs/01-decisions/ADR-0102-geo.md",
                &["**Status:** Accepted"]
            ),
            Status::Current
        );
        assert_eq!(
            st("docs/01-decisions/ADR-0112-x.md", &["**Status:** Proposed"]),
            Status::Proposed
        );
        assert_eq!(
            st("docs/x.md", &["> - **Status:** legacy draft"]),
            Status::Historical
        );
        assert_eq!(st("docs/work/plan.md", &[]), Status::Historical);
        assert_eq!(st("docs/archive/2023/checkout.md", &[]), Status::Historical);
        assert_eq!(
            doc_status("services/api-core/README.md", &["# api-core"], &r),
            StatusInfo::new("undeclared", Status::Current)
        );
        // A line with \r at the end does not count, as in JavaScript.
        assert_eq!(
            doc_status("docs/y.md", &["**Status:** Superseded\r"], &r).label,
            "undeclared"
        );
    }

    #[test]
    fn classify_by_primary_label() {
        for s in [
            "canonical",
            "canonical with legacy naming notes",
            "canonical | roadmap | legacy | draft",
            "Accepted (superseded in part by ADR-0098; Eve process)",
            "Superseded in its debit, price and offline-settlement",
            "Superseded in part by ADR-0095",
        ] {
            assert_eq!(classify_status(s), Status::Current, "{s}");
        }
        for s in [
            "Superseded by [ADR-0032](./x.md)",
            "Superseded — historical record only",
            "snapshot 2026-09-19 (historical note)",
        ] {
            assert_eq!(classify_status(s), Status::Historical, "{s}");
        }
        assert_eq!(
            classify_status("draft — becomes canonical when accepted"),
            Status::Proposed
        );
        assert_eq!(classify_status("🟡 Roadmap in progress"), Status::Proposed);
    }

    #[test]
    fn supersession_declarations() {
        // Spanish keywords, as the docs write them.
        assert_eq!(
            superseded_adrs(&["**Supersedes:** ADR-0003 (WorkOS)"]),
            vec!["0003"]
        );
        assert_eq!(
            superseded_adrs(&["> **Reemplaza a** ADR-0031"]),
            vec!["0031"]
        );
        assert_eq!(
            superseded_adrs(&[
                "Fecha: 2026-09-04. Sustituye para esta fase el transporte Kafka de ADR-0034 a ADR-0039. Esas decisiones conservan valor."
            ]),
            vec!["0034", "0039", "0035", "0036", "0037", "0038"]
        );
        assert!(superseded_adrs(&["> **Supersedes:** ADR-0001 Decision 11 (agent)"]).is_empty());
        assert!(superseded_adrs(&["> **Supersedes (clarifies):** ADR-0005 §x"]).is_empty());
        assert!(superseded_adrs(&["> [ADR-0093](./a.md) supersedes this ADR's memory"]).is_empty());
    }

    fn adr(n: &str, status: &str, extra: &[&str]) -> FileStatus {
        let path = format!("docs/01-decisions/ADR-{n}-x.md");
        let mut lines = vec![format!("# ADR-{n}"), format!("> **Status:** {status}")];
        lines.extend(extra.iter().map(|s| s.to_string()));
        lines.push(String::new());
        lines.push("body".into());
        let p = parse_markdown(&path, &lines.join("\n"), &rules());
        FileStatus {
            path,
            status: p.status,
            supersedes: p.supersedes,
        }
    }

    #[test]
    fn live_adr_retires_its_target() {
        let mut files = vec![
            adr("0034", "Accepted", &[]),
            adr(
                "0123",
                "Accepted",
                &["Sustituye la dirección de ADR-0034 a ADR-0035."],
            ),
        ];
        apply_supersession(&mut files);
        assert_eq!(
            files[0].status,
            StatusInfo::new("superseded by ADR-0123", Status::Historical)
        );
        assert_eq!(files[1].status.status, Status::Current);

        let mut proposed = vec![
            adr("0034", "Accepted", &[]),
            adr("0200", "Proposed", &["**Supersedes:** ADR-0034"]),
        ];
        apply_supersession(&mut proposed);
        assert_eq!(proposed[0].status.status, Status::Current);

        let f = |path: &str, text: &str| {
            let p = parse_markdown(path, text, &rules());
            FileStatus {
                path: path.into(),
                status: p.status,
                supersedes: p.supersedes,
            }
        };
        let mut linked = vec![
            f(
                "docs/01-decisions/ADR-0031-pricing.md",
                "# a\n> **Status:** Accepted\n",
            ),
            f(
                "docs/01-decisions/ADR-0031-worker.md",
                "# b\n> **Status:** Accepted\n",
            ),
            f(
                "docs/01-decisions/ADR-0033-x.md",
                "# c\n> **Status:** Accepted\n> **Supersedes:** [ADR-0031](./ADR-0031-pricing.md)\n",
            ),
        ];
        apply_supersession(&mut linked);
        assert_eq!(linked[0].status.status, Status::Historical);
        assert_eq!(linked[1].status.status, Status::Current);
    }

    #[test]
    fn repo_refs() {
        let r = rules();
        assert_eq!(
            extract_repo_refs(
                "docs/04-runtime/flow.md",
                "See `services/api-core/src/app.ts:12` and [ADR](../01-decisions/ADR-0001-x.md#d1).",
                &r
            ),
            vec![
                "docs/01-decisions/ADR-0001-x.md",
                "services/api-core/src/app.ts"
            ]
        );
        assert_eq!(
            extract_repo_refs(
                "docs/a.md",
                "[scripts/audit.ts](../services/api-core/scripts/audit.ts)",
                &r
            ),
            vec!["services/api-core/scripts/audit.ts"]
        );
        assert_eq!(
            extract_repo_refs(
                "docs/01-decisions/ADR-0002.md",
                "[a](ADR-0081-evals.md) [b](sub/x.md#h) [c](https://x.dev/y.md) [d](#anchor) [e](mailto:a@b.c) [f](/abs.md)",
                &r
            ),
            vec![
                "docs/01-decisions/ADR-0081-evals.md",
                "docs/01-decisions/sub/x.md"
            ]
        );
        assert!(
            extract_repo_refs(
                "docs/a.md",
                "`services/api-core/src/<module>/` and `db/migrations/NNNN_x.sql` and `apps/*/src`",
                &r
            )
            .is_empty()
        );
        // A path does not start inside another word.
        assert!(extract_repo_refs("docs/a.md", "my-docs/x and foo/apps/y", &r).is_empty());
    }

    #[test]
    fn posix_paths() {
        assert_eq!(posix_normalize("docs/../x.md"), "x.md");
        assert_eq!(posix_normalize("docs/../"), "./");
        assert_eq!(posix_normalize("./a/./b"), "a/b");
        assert_eq!(posix_dirname("README.md"), ".");
    }
}
