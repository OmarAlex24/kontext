//! The report for the human (`kontext report [--html] [-o file]` and the
//! `/report` path of `kontext ui`).
//!
//! It is a fixed, deterministic page: it comes from the ledger with a template,
//! and the agent does not design it. The only free part is the "where we are"
//! paragraph, which the agent saves with `kontext log --status "…"` (the last
//! one counts).
//!
//! Contents, in this order and nothing else:
//! 1. a status line: the paragraph, with a light according to the pending
//!    items and the problems;
//! 2. **Your turn**: every pending item (not the problems), from most to least
//!    impact, each with the exact command that resolves it; the first 5 in
//!    view and the rest folded ("and N more");
//! 3. **Problems**: the active ones, each with why it matters;
//! 4. **Your ideas**, only when `.pm/items.md` has ideas (the caller reads
//!    them: [`ideas_for`]): what the user said and where each one went,
//!    done and decided ones included, with their evidence;
//! 5. **What it looks like**: the most recent screenshots per epic, with the
//!    before and after of the same screen if there is one;
//! 6. **Progress**: a bar per goal and the wave line.
//!
//! Finished tasks are not listed. Generating it writes nothing and closes no
//! pending items. Without ideas the page has no such section: its style and
//! script go in their own blocks ([`ideas_style`], [`ideas_script`]) only
//! when there are ideas.
//!
//! The page is read-only and asks nothing of the network: the CSS, the font
//! (Anybody, OFL), the dither patterns and a small script are inside. The
//! script copies commands, compares the before and after, enlarges screenshots
//! and dithers them into a `<canvas>`; without it the page still reads in full.
//! `kontext ui` serves it with a CSP that only lets that style and that script
//! run
//! ([`csp`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::config::Project;
use crate::ideas::{EvidenceKind, Idea, IdeaState, Ideas};
use crate::inbox::{self, Class, Inbox, Item};
use crate::model::Entry;
use crate::plan;
use crate::state::{State, Status, string_list};
use crate::tasks::{self, TaskStatus};
use crate::views::{field, one_line};
use crate::write::STATUS_TAG;

const STYLE_CSS: &str = include_str!("../templates/report/style.css");
const SCRIPT_JS: &str = include_str!("../templates/report/report.js");
const IDEAS_CSS: &str = include_str!("../templates/report/ideas.css");
const IDEAS_JS: &str = include_str!("../templates/report/ideas.js");
/// Anybody ExtraBold, subconjunto latino (Etcetera Type Co., SIL OFL 1.1;
/// the full license is in `templates/report/anybody-OFL.txt`).
const DISPLAY_WOFF2: &[u8] = include_bytes!("../templates/report/anybody-800.woff2");

/// How many pending items are in view under "Your turn" (the rest, folded).
pub const TOP: usize = 5;
/// How many screens per epic under "What it looks like", and how many in total.
const SCREENS_PER_EPIC: usize = 2;
const MAX_SHOTS: usize = 8;
/// The most that is embedded in a single file (the rest goes beside it).
pub const EMBED_MAX: u64 = 12 * 1024 * 1024;

/// The status light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    /// Something waits for the human and stops work.
    Red,
    /// There are pending items, but nothing is stopped by them.
    Amber,
    /// Nothing waits for the human.
    Green,
}

impl Light {
    pub fn class(self) -> &'static str {
        match self {
            Light::Red => "red",
            Light::Amber => "amber",
            Light::Green => "green",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Light::Red => "Work is stopped, waiting for you",
            Light::Amber => "Moving, and there are things for you",
            Light::Green => "Moving without you",
        }
    }
}

/// The "where we are" paragraph.
#[derive(Debug, Clone)]
pub struct StatusNote {
    pub text: String,
    pub hlc: String,
    pub by: String,
}

/// A screenshot: from a `done` (evidence) or from a decision (attachment).
#[derive(Debug, Clone)]
pub struct Capture {
    pub rel: String,
    pub name: String,
    pub hlc: String,
    /// "approved reference (D-…)" if it comes from a decision.
    pub reference: Option<String>,
}

/// A screen in the gallery: the latest screenshot and the one before it.
#[derive(Debug, Clone)]
pub struct Shot {
    /// The epic (id), or `None` if it belongs to the project.
    pub epic_id: Option<String>,
    pub epic: String,
    pub screen: String,
    pub now: Capture,
    pub before: Option<Capture>,
}

#[derive(Debug, Clone)]
pub struct GoalBar {
    pub id: String,
    pub title: String,
    pub signed: bool,
    pub closed: usize,
    pub total: usize,
}

/// An epic in the sidebar: its wave, its progress and whether anything in it
/// waits for the human.
#[derive(Debug, Clone)]
pub struct EpicRow {
    pub id: String,
    pub title: String,
    /// "Wave 1", "Closed" or "No wave".
    pub wave: String,
    pub closed: usize,
    pub total: usize,
    /// How many pending items (problems included) touch it.
    pub waiting: usize,
}

#[derive(Debug, Clone)]
pub struct WaveStep {
    pub label: String,
    pub epics: Vec<String>,
    pub current: bool,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub project: String,
    pub generated: String,
    pub status: Option<StatusNote>,
    pub light: Light,
    pub light_reason: String,
    pub inbox: Inbox,
    /// "Your turn", in view: the highest-impact items, without the problems.
    pub top: Vec<Item>,
    /// The rest of the pending items (without problems), folded under "and N more".
    pub rest: Vec<Item>,
    pub problems: Vec<Item>,
    pub shots: Vec<Shot>,
    pub goals: Vec<GoalBar>,
    pub waves: Vec<WaveStep>,
    pub epics: Vec<EpicRow>,
    /// "2 days ago": the human's last write.
    pub last_action: Option<String>,
    /// The user's ideas (`.pm/items.md`), set by the caller ([`ideas_for`]);
    /// `None`, or no ideas, and the report has no "Your ideas".
    pub ideas: Option<Ideas>,
    /// `[media] max_size`: the most an idea's screenshot may weigh.
    pub media_max: u64,
}

/// The latest status paragraph (`kontext log --status`).
pub fn status_note(st: &State) -> Option<StatusNote> {
    st.journal()
        .into_iter()
        .rev()
        .find(|j| j.tags.iter().any(|t| t == STATUS_TAG))
        .map(|j| StatusNote {
            text: j.title.trim().to_string(),
            hlc: j.hlc.clone(),
            by: format!("{}:{}", j.actor.kind, j.actor.name),
        })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn build(st: &State, project: &Project, human: &str) -> Report {
    let ib = inbox::build(st, &project.name, human);
    let blocking = ib.items.iter().filter(|i| i.score() != (0, 0)).count();
    let problems: Vec<Item> = ib.of_class(Class::Problem).into_iter().cloned().collect();
    let others: Vec<&Item> = ib
        .items
        .iter()
        .filter(|i| i.class != Class::Problem)
        .collect();
    let (light, light_reason) = if blocking > 0 {
        (
            Light::Red,
            format!(
                "{} work",
                plural(blocking, "pending item stops", "pending items stop")
            ),
        )
    } else if !ib.items.is_empty() {
        (
            Light::Amber,
            format!(
                "{}; nothing is stopped by them",
                plural(ib.items.len(), "pending item", "pending items")
            ),
        )
    } else {
        (
            Light::Green,
            "nothing waits for an action from you".to_string(),
        )
    };
    Report {
        project: project.name.clone(),
        generated: tasks::ms_text(st.now_ms),
        status: status_note(st),
        light,
        light_reason,
        top: others.iter().take(TOP).map(|i| (*i).clone()).collect(),
        rest: others.iter().skip(TOP).map(|i| (*i).clone()).collect(),
        problems,
        shots: shots(st),
        goals: goals(st),
        waves: waves(st),
        epics: epic_rows(st, &ib),
        last_action: ib.last_action_ago(),
        inbox: ib,
        ideas: None,
        media_max: project
            .media_max_bytes()
            .unwrap_or(crate::media::DEFAULT_MAX_BYTES),
    }
}

/// The ideas for the report, found as `kontext ideas` finds them (the git repo
/// of `cwd`, or else the first project repo on this machine). `None` when there
/// is no `.pm/items.md`, it has no ideas, or anything fails: the report never
/// fails because of them.
pub fn ideas_for(st: &State, project: &Project, cwd: &Path) -> Option<Ideas> {
    let repos = crate::repos::Repos::load(project, cwd).ok()?;
    let pm = crate::ideas::locate(&repos)?;
    let people: Vec<String> = project.people.keys().cloned().collect();
    crate::ideas::load(st, &project.name, &people, &pm)
        .ok()
        .filter(|i| i.total > 0)
}

impl Report {
    /// Whether the page carries "Your ideas" (and its style and script).
    pub fn has_ideas(&self) -> bool {
        self.shown_ideas().is_some()
    }

    /// The ideas to show, if there is at least one.
    fn shown_ideas(&self) -> Option<&Ideas> {
        self.ideas.as_ref().filter(|i| i.total > 0)
    }
}

/// The screenshots of each epic (and of the project), from the oldest to the
/// newest: those of the `done` entries that count and the attachments of the
/// decisions in force.
fn captures(st: &State) -> Vec<(Option<String>, String, Capture)> {
    let mut out = Vec::new();
    let mut push = |epic: Option<String>, e: &Entry, it: &Value, reference: Option<String>| {
        if it.get("type").and_then(Value::as_str) != Some("image") {
            return;
        }
        let rel = it.get("value").and_then(Value::as_str).unwrap_or_default();
        let name = it.get("name").and_then(Value::as_str).unwrap_or(rel);
        let screen = it
            .get("screen")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| crate::media::default_screen(name));
        out.push((
            epic,
            screen,
            Capture {
                rel: rel.to_string(),
                name: name.to_string(),
                hlc: e.hlc.clone(),
                reference,
            },
        ));
    };
    for id in &st.order {
        let Some(e) = st.get(id) else { continue };
        match e.kind.as_str() {
            "done" => {
                let Some(task) = e.refs.first() else { continue };
                if st.task(task).and_then(|t| t.done.as_deref()) != Some(e.id.as_str()) {
                    continue;
                }
                let epic = st
                    .get(task)
                    .and_then(|t| st.epic_of(t))
                    .map(|x| x.id.clone());
                for it in e
                    .fields
                    .get("evidence")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    push(epic.clone(), e, it, None);
                }
            }
            "decision" | "proposal" if st.is_law(&e.id) => {
                let epic = field(e, "epic").and_then(|n| st.find_epic(n).ok());
                for it in e
                    .fields
                    .get("attachments")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    push(
                        epic.clone(),
                        e,
                        it,
                        Some(format!("approved reference ({})", e.id)),
                    );
                }
            }
            _ => {}
        }
    }
    out
}

fn shots(st: &State) -> Vec<Shot> {
    // (epic, screen) → screenshots in order.
    let mut by_screen: BTreeMap<(Option<String>, String), Vec<Capture>> = BTreeMap::new();
    for (epic, screen, c) in captures(st) {
        by_screen.entry((epic, screen)).or_default().push(c);
    }
    // Per epic, its screens from the newest to the oldest.
    let mut by_epic: BTreeMap<Option<String>, Vec<Shot>> = BTreeMap::new();
    for ((epic, screen), caps) in by_screen {
        let n = caps.len();
        let now = caps[n - 1].clone();
        let before = (n > 1).then(|| caps[n - 2].clone());
        let title = epic
            .as_deref()
            .and_then(|e| st.get(e))
            .map(|e| one_line(&e.title, 60))
            .unwrap_or_else(|| "Project".to_string());
        by_epic.entry(epic.clone()).or_default().push(Shot {
            epic_id: epic,
            epic: title,
            screen,
            now,
            before,
        });
    }
    let mut groups: Vec<Vec<Shot>> = by_epic
        .into_values()
        .map(|mut v| {
            v.sort_by(|a, b| b.now.hlc.cmp(&a.now.hlc).then(a.screen.cmp(&b.screen)));
            v.truncate(SCREENS_PER_EPIC);
            v
        })
        .collect();
    groups.sort_by(|a, b| b[0].now.hlc.cmp(&a[0].now.hlc));
    let mut out: Vec<Shot> = groups.into_iter().flatten().collect();
    out.truncate(MAX_SHOTS);
    out
}

fn goals(st: &State) -> Vec<GoalBar> {
    st.goals()
        .into_iter()
        .map(|g| {
            let tasks: Vec<&Entry> = st
                .of_kind("task")
                .filter(|t| string_list(t.fields.get("goals")).contains(&g.id))
                .collect();
            GoalBar {
                id: g.id.clone(),
                title: one_line(&g.title, 90),
                signed: st.status_of(&g.id) == Some(Status::Signed),
                closed: tasks
                    .iter()
                    .filter(|t| st.task_status(&t.id) == Some(TaskStatus::Closed))
                    .count(),
                total: tasks.len(),
            }
        })
        .collect()
}

fn epic_rows(st: &State, ib: &Inbox) -> Vec<EpicRow> {
    let p = plan::of_state(st);
    let mut out = Vec::new();
    let mut push = |e: &plan::PlanEpic, wave: String| {
        out.push(EpicRow {
            id: e.id.clone(),
            title: one_line(&e.title, 60),
            wave,
            closed: e.closed_tasks,
            total: e.tasks,
            waiting: ib.items.iter().filter(|i| i.epics.contains(&e.id)).count(),
        })
    };
    for w in &p.waves {
        for e in &w.epics {
            push(e, format!("Wave {}", w.n));
        }
    }
    for b in &p.blocked {
        push(&b.epic, "No wave".into());
    }
    for e in &p.closed {
        push(e, "Closed".into());
    }
    out
}

fn waves(st: &State) -> Vec<WaveStep> {
    let p = plan::of_state(st);
    if p.waves.is_empty() && p.closed.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if !p.closed.is_empty() {
        out.push(WaveStep {
            label: "Closed".into(),
            epics: p.closed.iter().map(|e| e.title.clone()).collect(),
            current: false,
            done: true,
        });
    }
    for w in &p.waves {
        out.push(WaveStep {
            label: format!("Wave {}", w.n),
            epics: w.epics.iter().map(|e| e.title.clone()).collect(),
            current: w.n == 1,
            done: false,
        });
    }
    out
}

// ------------------------------------------------------------ images

/// How the screenshots reach the page.
pub enum Images {
    /// Embedded (data URI) up to `max_total`; the rest goes to a folder beside
    /// the file (`side`: the folder and how the page cites it), or is skipped if
    /// there is no file.
    Embed {
        home: PathBuf,
        max_total: u64,
        side: Option<(PathBuf, String)>,
    },
    /// By URL (`/media/<file>` of `kontext ui`).
    Url { prefix: String },
}

impl Images {
    /// For `-o <file>`: what does not fit goes to `<file>_media/`.
    pub fn file(home: &Path, out: &Path) -> Images {
        let stem = out
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "report".into());
        let dir_name = format!("{stem}_media");
        let dir = out
            .parent()
            .map(|p| p.join(&dir_name))
            .unwrap_or_else(|| PathBuf::from(&dir_name));
        Images::Embed {
            home: home.to_path_buf(),
            max_total: EMBED_MAX,
            side: Some((dir, dir_name)),
        }
    }

    pub fn stdout(home: &Path) -> Images {
        Images::Embed {
            home: home.to_path_buf(),
            max_total: EMBED_MAX,
            side: None,
        }
    }

    pub fn url(prefix: &str) -> Images {
        Images::Url {
            prefix: prefix.to_string(),
        }
    }

    /// The address of a screenshot, or `None` if it cannot be shown.
    fn src(&self, c: &Capture, used: &mut u64) -> Option<String> {
        let name = c.rel.strip_prefix("media/")?;
        let (_, ext) = crate::media::valid_media_name(name)?;
        match self {
            Images::Url { prefix } => Some(format!("{prefix}{name}")),
            Images::Embed {
                home,
                max_total,
                side,
            } => {
                let bytes = fs::read(home.join(&c.rel)).ok()?;
                let n = bytes.len() as u64;
                if *used + n <= *max_total {
                    *used += n;
                    return Some(format!(
                        "data:{};base64,{}",
                        crate::media::content_type(ext),
                        crate::media::base64(&bytes)
                    ));
                }
                let (dir, rel) = side.as_ref()?;
                fs::create_dir_all(dir).ok()?;
                fs::write(dir.join(name), &bytes).ok()?;
                Some(format!("{rel}/{name}"))
            }
        }
    }
}

// ------------------------------------------------------------ HTML

/// Escapes text for HTML (content and attributes).
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn ago(r: &Report, hlc: &str) -> String {
    tasks::ago(r.inbox.now_ms, tasks::hlc_ms(hlc))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The 4×4 Bayer matrix: a dot lights up at level `k` if its threshold is less
/// than `k` (from 0, nothing, to 16, everything).
const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// The dither patterns as masks: `--d1` … `--d15` are SVG tiles of 8×8 px (4×4
/// dots of 2 px) in base64, and `.m1` … `.m16` pick one for `var(--m)`. The
/// CSS sets the color, so the same pattern works in light and dark.
fn dither_css() -> String {
    let mut vars = String::from(":root{");
    let mut picks = String::new();
    for k in 1..16u8 {
        let mut d = String::new();
        for (y, row) in BAYER4.iter().enumerate() {
            for (x, t) in row.iter().enumerate() {
                if *t < k {
                    d.push_str(&format!("M{} {}h2v2h-2z", x * 2, y * 2));
                }
            }
        }
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\" \
             shape-rendering=\"crispEdges\"><path d=\"{d}\"/></svg>"
        );
        vars.push_str(&format!(
            "--d{k}:url(data:image/svg+xml;base64,{});",
            crate::media::base64(svg.as_bytes())
        ));
        picks.push_str(&format!(".m{k}{{--m:var(--d{k})}}"));
    }
    vars.push_str("--d16:linear-gradient(#000 0 0)}\n");
    picks.push_str(".m16{--m:var(--d16)}\n");
    vars + &picks
}

/// The page's inline style, as it goes in its `<style>`.
pub fn inline_style() -> String {
    format!(
        "/* Anybody ExtraBold: Copyright 2020 The Anybody Project Authors \
         (github.com/Etcetera-Type-Co/Anybody). Licensed under the SIL Open Font \
         License, Version 1.1 (scripts.sil.org/OFL). Subconjunto latino. */\n\
         @font-face{{font-family:\"Anybody\";font-style:normal;font-weight:800;\
         font-display:swap;src:url(data:font/woff2;base64,{}) format(\"woff2\")}}\n{}{STYLE_CSS}",
        crate::media::base64(DISPLAY_WOFF2),
        dither_css()
    )
}

/// The page's inline script, as it goes in its `<script>`.
pub fn inline_script() -> &'static str {
    SCRIPT_JS
}

/// The style of "Your ideas", in a second `<style>` (only with ideas).
pub fn ideas_style() -> &'static str {
    IDEAS_CSS
}

/// The script of "Your ideas", in a second `<script>` (only with ideas).
pub fn ideas_script() -> &'static str {
    IDEAS_JS
}

fn sha256_source(s: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "'sha256-{}'",
        crate::media::base64(Sha256::digest(s.as_bytes()).as_slice())
    )
}

/// The CSP with which `kontext ui` serves `/report`: only the page's style and
/// script run (by their hash), the images come from `/media/` or are embedded,
/// the font is embedded and it connects to nothing.
pub fn csp() -> String {
    csp_for(false)
}

/// [`csp`], plus the hashes of the style and script of "Your ideas" when the
/// page carries them.
pub fn csp_for(with_ideas: bool) -> String {
    let (script, style) = if with_ideas {
        (
            format!(
                "{} {}",
                sha256_source(inline_script()),
                sha256_source(ideas_script())
            ),
            format!(
                "{} {}",
                sha256_source(&inline_style()),
                sha256_source(ideas_style())
            ),
        )
    } else {
        (
            sha256_source(inline_script()),
            sha256_source(&inline_style()),
        )
    };
    format!(
        "default-src 'none'; script-src {script}; style-src {style}; img-src 'self' data:; \
         font-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// How dense an item's edge is: what stops work gets more the more it stops;
/// the rest, barely.
fn impact_level(it: &Item) -> u8 {
    let (epics, tasks) = it.score();
    if epics + tasks > 0 {
        (6 + 3 * epics + tasks).min(16) as u8
    } else if it.not_urgent {
        1
    } else {
        3
    }
}

fn cmd_html(cmd: &str, main: bool) -> String {
    format!(
        "<span class=\"cmdbox{}\"><code class=\"cmd\">{}</code></span>",
        if main { " main" } else { "" },
        esc(cmd)
    )
}

/// `data-epic`: the epics it touches, separated by spaces (to filter by).
fn epic_attr(ids: &[String]) -> String {
    format!(" data-epic=\"{}\"", esc(&ids.join(" ")))
}

fn item_html(r: &Report, it: &Item, h: &mut String) {
    let blocks = it.score() != (0, 0);
    let kind = it.class.label();
    h.push_str(&format!(
        "<li class=\"todo{} m{}\"{}>\n<p class=\"todo-head\"><span class=\"kind k-{}\">{kind}</span>{}</p>\n<h3>{}</h3>\n",
        if blocks { " blocks" } else { "" },
        impact_level(it),
        epic_attr(&it.epics),
        kind.to_lowercase(),
        if it.new {
            "<span class=\"fresh\">New</span>"
        } else {
            ""
        },
        esc(&it.title)
    ));
    let impact = it.impact_text();
    let mut meta = Vec::new();
    if !impact.is_empty() {
        meta.push(format!("<strong>{}</strong>", esc(&capitalize(&impact))));
    } else if it.not_urgent {
        meta.push("Not urgent".to_string());
    }
    if !it.waiting.is_empty() {
        meta.push(format!(
            "{} {}",
            if it.waiting.len() == 1 {
                "waits"
            } else {
                "wait"
            },
            esc(&it.waiting.join(", "))
        ));
    }
    if !it.since.is_empty() {
        meta.push(format!(
            "<span class=\"nw\">{}</span>",
            esc(&ago(r, &it.since))
        ));
    }
    h.push_str(&format!("<p class=\"meta\">{}</p>\n", meta.join(" · ")));
    let grouped = it.subs.len() > 1 || it.class == Class::Review;
    if grouped {
        let sub = |s: &inbox::Sub| {
            let cmd = if it.class == Class::Answer {
                cmd_html(&format!("kontext answer {} \"…\"", s.id), false)
            } else {
                String::new()
            };
            format!("<li><span>{}</span>{cmd}</li>\n", esc(&s.text))
        };
        h.push_str("<ul class=\"subs\">\n");
        for s in it.subs.iter().take(4) {
            h.push_str(&sub(s));
        }
        h.push_str("</ul>\n");
        if it.subs.len() > 4 {
            h.push_str(&format!(
                "<details class=\"fold fold-subs\"><summary><span class=\"closed\">And {} more</span>\
                 <span class=\"opened\">Show less</span></summary>\n<ul class=\"subs\">\n",
                it.subs.len() - 4
            ));
            for s in it.subs.iter().skip(4) {
                h.push_str(&sub(s));
            }
            h.push_str("</ul>\n</details>\n");
        }
    }
    if !(grouped && it.class == Class::Answer) {
        // The command that resolves it, with its button; the other way out
        // (reject, dismiss), on a discreet line below.
        h.push_str(&format!(
            "<div class=\"do\">{}</div>\n",
            cmd_html(&it.command, true)
        ));
        if let Some(a) = it.alt.as_deref() {
            h.push_str(&format!(
                "<p class=\"alt\"><span class=\"or\">or</span><span class=\"cmdbox mini\"><code class=\"cmd\">{}</code></span></p>\n",
                esc(a)
            ));
        }
    }
    h.push_str("</li>\n");
}

fn figure(r: &Report, c: &Capture, label: &str, img: &Images, used: &mut u64, h: &mut String) {
    let when = ago(r, &c.hlc);
    let when = format!("<span class=\"nw\">{}</span>", esc(&when));
    let note = match &c.reference {
        Some(x) => format!("{} · {when}", esc(x)),
        None => when,
    };
    h.push_str("<figure>\n");
    match img.src(c, used) {
        Some(s) => h.push_str(&format!(
            "<div class=\"frame\"><img src=\"{}\" alt=\"{}\" loading=\"lazy\" decoding=\"async\"></div>\n",
            esc(&s),
            esc(&format!("{label}: {}", c.name))
        )),
        None => h.push_str(
            "<div class=\"frame missing\"><p>Screenshot skipped: it does not fit in a single file. \
             Generate the report with <code>-o report.html</code> or open it in <code>kontext ui</code>.</p></div>\n",
        ),
    }
    h.push_str(&format!(
        "<figcaption><b>{}</b> {note}</figcaption>\n</figure>\n",
        esc(label)
    ));
}

/// A section heading: title, how many (if it applies) and what it is.
fn block_head(id: &str, title: &str, count: Option<usize>, about: &str, h: &mut String) {
    h.push_str(&format!(
        "<div class=\"block-head\"><h2 id=\"{id}\">{title}</h2>{}<p>{about}</p></div>\n",
        count
            .map(|n| format!("<span class=\"count\">{n}</span>"))
            .unwrap_or_default()
    ));
}

fn light_name(l: Light) -> &'static str {
    match l {
        Light::Red => "red",
        Light::Amber => "amber",
        Light::Green => "green",
    }
}

/// The small light (sidebar and top bar on a phone).
fn mini_signal(r: &Report) -> String {
    format!(
        "<span class=\"mini-signal\" role=\"img\" aria-label=\"Status light: {}\">\
         <i class=\"l-red\"></i><i class=\"l-amber\"></i><i class=\"l-green\"></i></span>",
        light_name(r.light)
    )
}

/// The sidebar: the project and its status, the sections with how many there
/// are in each, the epics (with their wave, their progress and whether anything
/// in them waits for you; the script turns them into filters) and how to
/// regenerate the page. On a phone, a bar on top with a button that opens it as
/// a panel.
fn sidebar_html(r: &Report, h: &mut String) {
    h.push_str(&format!(
        "<header class=\"bar-top\">{}<p class=\"bar-project\">{}</p>\
         <button type=\"button\" class=\"bar-btn js-only\" aria-expanded=\"false\" aria-controls=\"lateral\">Sections</button></header>\n",
        mini_signal(r),
        esc(&r.project)
    ));
    h.push_str(
        "<aside class=\"side\" id=\"lateral\" aria-label=\"Summary and sections\">\n\
<div class=\"side-in\">\n<div class=\"side-close js-only\"><button type=\"button\" class=\"side-x\">Close</button></div>\n",
    );
    h.push_str(&format!(
        "<div class=\"side-head\">\n<p class=\"side-project\">{}</p>\n<p class=\"side-state\">{}<span>{}</span></p>\n<p class=\"side-last\">{}</p>\n</div>\n",
        esc(&r.project),
        mini_signal(r),
        esc(r.light.label()),
        match &r.last_action {
            Some(a) => format!("Your last action: {}", esc(a)),
            None => "You haven't written in the ledger yet".to_string(),
        }
    ));
    let pending = r.top.len() + r.rest.len();
    let count = |n: usize| format!("<span class=\"n\">{n}</span>");
    h.push_str("<nav class=\"side-nav\" aria-label=\"Sections\">\n<ul>\n");
    for (i, (id, label, n)) in [
        ("your-turn", "Your turn", Some(pending)),
        ("problems", "Problems", Some(r.problems.len())),
        (
            "what-it-looks-like",
            "What it looks like",
            Some(r.shots.len()),
        ),
        ("progress", "Progress", None),
    ]
    .into_iter()
    .enumerate()
    {
        h.push_str(&format!(
            "<li><a href=\"#{id}\" data-nav=\"{id}\"><span>{label}</span>{}</a></li>\n",
            n.map(count).unwrap_or_default()
        ));
        // "Your ideas" goes right after "Your turn".
        if let (0, Some(ideas)) = (i, r.shown_ideas()) {
            h.push_str(&format!(
                "<li class=\"nav-ideas\"><a href=\"#your-ideas\" data-nav=\"your-ideas\"><span>Your ideas</span>{}</a></li>\n",
                count(ideas.total)
            ));
        }
    }
    h.push_str("</ul>\n</nav>\n");
    if !r.epics.is_empty() {
        h.push_str(
            "<section class=\"side-epics\" aria-labelledby=\"side-epics-title\">\n\
             <div class=\"side-epics-head\"><h2 id=\"side-epics-title\">Epics</h2>\
             <button type=\"button\" class=\"epic-all js-only\" aria-pressed=\"true\">All</button></div>\n\
             <ul class=\"epics\">\n",
        );
        for e in &r.epics {
            let count = if e.total == 0 {
                "no tasks".to_string()
            } else {
                format!("{} of {}", e.closed, e.total)
            };
            h.push_str(&format!(
                "<li class=\"epic{}\" data-epic-row=\"{}\"><div class=\"epic-in\">\
                 <span class=\"epic-name\">{}{}</span><span class=\"epic-wave\">{}</span>\
                 <span class=\"bar mini\"><progress max=\"{}\" value=\"{}\" aria-label=\"{}: {count}\">{count}</progress></span>\
                 <span class=\"epic-num\">{count}</span></div></li>\n",
                if e.waiting > 0 { " waits" } else { "" },
                esc(&e.id),
                if e.waiting > 0 {
                    format!(
                        "<span class=\"wait\" role=\"img\" aria-label=\"{}\"></span>",
                        if e.waiting == 1 {
                            "1 pending item waits for you".to_string()
                        } else {
                            format!("{} pending items wait for you", e.waiting)
                        }
                    )
                } else {
                    String::new()
                },
                esc(&e.title),
                esc(&e.wave),
                e.total.max(1),
                e.closed,
                esc(&e.title)
            ));
        }
        h.push_str("</ul>\n</section>\n");
    }
    h.push_str(&format!(
        "<div class=\"side-foot\"><p>To regenerate it</p>{}<p class=\"stamp\">At {}</p></div>\n</div>\n</aside>\n",
        "<span class=\"cmdbox mini\"><code class=\"cmd\">kontext report --html</code></span>",
        esc(&r.generated)
    ));
}

/// The full page.
pub fn html(r: &Report, img: &Images) -> Result<String> {
    let cls = r.light.class();
    let mut h = String::new();
    h.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    h.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    h.push_str(&format!(
        "<title>{} · where we are</title>\n<style>{}</style>\n",
        esc(&r.project),
        inline_style()
    ));
    if r.shown_ideas().is_some() {
        h.push_str(&format!("<style>{}</style>\n", ideas_style()));
    }
    h.push_str(&format!("</head>\n<body class=\"s-{cls}\">\n"));
    h.push_str("<div class=\"shell\">\n");
    sidebar_html(r, &mut h);
    h.push_str("<main class=\"page\" id=\"contenido\">\n");

    // 1. Status: the light over a dithered field in the status color, which
    // goes from dense to sparse; the text, over a plain sheet.
    h.push_str(&format!(
        "<section class=\"state s-{cls}\" aria-labelledby=\"state-title\">\n<div class=\"field\" aria-hidden=\"true\">"
    ));
    for k in (1..=16).rev() {
        h.push_str(&format!("<i class=\"m{k}\"></i>"));
    }
    h.push_str("<i class=\"m1 tail\"></i></div>\n");
    h.push_str(&format!(
        "<div class=\"signal\" role=\"img\" aria-label=\"Status light: {}\">\
         <span class=\"lamp l-red\"></span><span class=\"lamp l-amber\"></span><span class=\"lamp l-green\"></span></div>\n\
         <div class=\"state-body\">\n<h1 id=\"state-title\">{}</h1>\n<p class=\"why\">{}</p>\n",
        light_name(r.light),
        esc(r.light.label()),
        esc(&capitalize(&r.light_reason))
    ));
    match &r.status {
        Some(s) => h.push_str(&format!(
            "<p class=\"note\">{}</p>\n<p class=\"by\">{} · {}</p>\n",
            esc(&s.text),
            esc(&s.by),
            esc(&ago(r, &s.hlc))
        )),
        None => h.push_str(
            "<p class=\"note empty\">There is no status paragraph yet. The agent writes it with \
             <code>kontext log --status \"…\"</code>.</p>\n",
        ),
    }
    h.push_str("</div>\n</section>\n<div class=\"sections\">\n");

    // 2. Your turn.
    let pending = r.top.len() + r.rest.len();
    h.push_str(
        "<section class=\"block todo-block\" id=\"your-turn\" aria-labelledby=\"todo-title\">\n",
    );
    block_head(
        "todo-title",
        "Your turn",
        (pending > 0).then_some(pending),
        if pending == 0 {
            "Nothing waits for an action from you."
        } else {
            "The items that stop the most work come first. Seeing them here does not close them: only what you do counts."
        },
        &mut h,
    );
    if !r.top.is_empty() {
        h.push_str("<ol class=\"todos\">\n");
        for it in &r.top {
            item_html(r, it, &mut h);
        }
        h.push_str("</ol>\n");
        if !r.rest.is_empty() {
            h.push_str(&format!(
                "<details class=\"fold fold-todos\"><summary><span class=\"closed\">And {} more</span>\
                 <span class=\"opened\">Show less</span></summary>\n<ol class=\"todos\">\n",
                r.rest.len()
            ));
            for it in &r.rest {
                item_html(r, it, &mut h);
            }
            h.push_str("</ol>\n</details>\n");
        }
    }
    h.push_str("</section>\n");

    // 3. Problems.
    h.push_str("<section class=\"block problems-block\" id=\"problems\" aria-labelledby=\"problems-title\">\n");
    block_head(
        "problems-title",
        "Problems",
        (!r.problems.is_empty()).then_some(r.problems.len()),
        if r.problems.is_empty() {
            "None active."
        } else {
            "What went wrong and is still unresolved."
        },
        &mut h,
    );
    if !r.problems.is_empty() {
        h.push_str("<ul class=\"problems\">\n");
        for p in &r.problems {
            let impact = p.impact_text();
            h.push_str(&format!(
                "<li{}><h3>{}</h3><p>{}{}</p></li>\n",
                epic_attr(&p.epics),
                esc(&p.title),
                esc(&capitalize(p.why.as_deref().unwrap_or(""))),
                if impact.is_empty() {
                    String::new()
                } else {
                    format!(" · <strong>{}</strong>", esc(&impact))
                }
            ));
        }
        h.push_str("</ul>\n");
    }
    h.push_str("</section>\n");

    // 5. What it looks like, built before "Your ideas": its screenshots are
    // counted first against the page's budget, and the ideas' use what is left.
    let mut look = String::new();
    let mut used = 0u64;
    look.push_str(
        "<section class=\"block look-block\" id=\"what-it-looks-like\" aria-labelledby=\"look-title\">\n",
    );
    block_head(
        "look-title",
        "What it looks like",
        (!r.shots.is_empty()).then_some(r.shots.len()),
        if r.shots.is_empty() {
            "There are no screenshots yet. Each task with an interface closes with one: \
             <code>kontext done … --evidence screenshot.png</code>."
        } else {
            "The latest of each epic. <span class=\"js-only\">It is dithered: hover over it or \
             tap it to see it in color. </span>If the screen already had a screenshot, the \
             earlier one is shown to compare."
        },
        &mut look,
    );
    if !r.shots.is_empty() {
        look.push_str("<div class=\"shots\">\n");
        for s in &r.shots {
            look.push_str(&format!(
                "<article class=\"shot{}\"{}>\n<h3><span class=\"epic\">{}</span><span class=\"screen\">{}</span></h3>\n<div class=\"pair\">\n",
                if s.before.is_some() { " has-before" } else { "" },
                epic_attr(s.epic_id.as_slice()),
                esc(&s.epic),
                esc(&s.screen)
            ));
            if let Some(b) = &s.before {
                figure(r, b, "Before", img, &mut used, &mut look);
            }
            let label = if s.before.is_some() { "Now" } else { "Latest" };
            figure(r, &s.now, label, img, &mut used, &mut look);
            look.push_str("</div>\n</article>\n");
        }
        look.push_str("</div>\n");
    }
    look.push_str("</section>\n");

    // 4. Your ideas.
    if let Some(ideas) = r.shown_ideas() {
        let mut left = match img {
            Images::Embed { max_total, .. } => max_total.saturating_sub(used),
            Images::Url { .. } => EMBED_MAX,
        };
        ideas_html(ideas, r.media_max, &mut left, &mut h);
    }
    h.push_str(&look);

    // 6. Progress.
    h.push_str("<section class=\"block progress-block\" id=\"progress\" aria-labelledby=\"progress-title\">\n");
    block_head(
        "progress-title",
        "Progress",
        None,
        "Closed tasks per goal, and the waves: what is closed, what is now and what comes next.",
        &mut h,
    );
    if r.goals.is_empty() {
        h.push_str("<p class=\"empty\">No goals yet.</p>\n");
    } else {
        h.push_str("<ul class=\"bars\">\n");
        for g in &r.goals {
            let count = if g.total == 0 {
                "no tasks".to_string()
            } else {
                format!("{} of {}", g.closed, g.total)
            };
            h.push_str(&format!(
                "<li><p class=\"bar-label\"><span>{}{}</span><span class=\"num\">{count}</span></p>\
                 <div class=\"bar\"><progress max=\"{}\" value=\"{}\" aria-label=\"{}: {count}\">{count}</progress></div></li>\n",
                esc(&g.title),
                if g.signed {
                    ""
                } else {
                    " <span class=\"unsigned\">unsigned</span>"
                },
                g.total.max(1),
                g.closed,
                esc(&g.title)
            ));
        }
        h.push_str("</ul>\n");
    }
    if !r.waves.is_empty() {
        h.push_str("<ol class=\"waves\" aria-label=\"Waves\">\n");
        for w in &r.waves {
            let (class, extra) = if w.done {
                ("done", "")
            } else if w.current {
                ("current", " aria-current=\"step\"")
            } else {
                ("next", "")
            };
            h.push_str(&format!(
                "<li class=\"{class}\"{extra}><span class=\"wv\" aria-hidden=\"true\"></span><b>{}</b><span>{}</span></li>\n",
                esc(&w.label),
                esc(&w.epics.join(", "))
            ));
        }
        h.push_str("</ol>\n");
    }
    h.push_str("</section>\n</div>\n</main>\n</div>\n");
    h.push_str(&format!("<script>{}</script>\n", inline_script()));
    if r.shown_ideas().is_some() {
        h.push_str(&format!("<script>{}</script>\n", ideas_script()));
    }
    h.push_str("</body>\n</html>\n");
    Ok(h)
}

// ------------------------------------------------------------ your ideas

/// The class of a state's ink (`st-<key>`) and its `data-state`.
fn state_key(s: IdeaState) -> &'static str {
    match s {
        IdeaState::NoDestination => "nowhere",
        IdeaState::WaitingForYou => "waiting",
        IdeaState::Pending => "pending",
        IdeaState::InProgress => "progress",
        IdeaState::Decided => "decided",
        IdeaState::Done => "done",
        IdeaState::Discarded => "discarded",
    }
}

/// What a state means for the reader, in one line.
fn state_about(s: IdeaState) -> &'static str {
    match s {
        IdeaState::NoDestination => {
            "Nothing in the ledger covers these. Give each one a place, or drop it."
        }
        IdeaState::WaitingForYou => "Each one needs a yes, a no or an answer from you.",
        IdeaState::Pending => "They have a place and nobody has started.",
        IdeaState::InProgress => "Started and not finished yet.",
        IdeaState::Decided => "A decision in force settles them.",
        IdeaState::Done => "Closed, each with its evidence.",
        IdeaState::Discarded => "You dropped it.",
    }
}

/// `2026-11-07` → `7 Nov`.
fn day_month(date: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut it = date.split('-');
    let (_, m, d) = (it.next()?, it.next()?, it.next()?);
    let m: usize = m.parse().ok()?;
    let d: u32 = d.parse().ok()?;
    Some(format!("{d} {}", MONTHS.get(m.checked_sub(1)?)?))
}

/// The anchors of the rows (`idea-P1`): the id with only letters, digits, `-`
/// and `_`, and a number after it if two ideas would share one.
fn idea_anchors(ideas: &[&Idea]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    ideas
        .iter()
        .map(|i| {
            let base: String =
                i.id.chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect();
            let base = format!("idea-{base}");
            let mut a = base.clone();
            let mut n = 2;
            while !seen.insert(a.clone()) {
                a = format!("{base}-{n}");
                n += 1;
            }
            a
        })
        .collect()
}

/// An idea's screenshot (`shots/<file>`, relative to `pm`) as a data URI, or
/// `None` if it cannot go in a page that gets shared: an absolute path or one
/// with `..`, anything that does not resolve inside `.pm/shots/`, a file over
/// `max` bytes, or one that is not a PNG, JPG or WebP image (by its content).
/// `left` is what remains of the page's budget for embedded images: a file
/// that does not fit in it is skipped too, and one that goes in is subtracted
/// (its raw bytes, as the report's own screenshots count).
pub fn idea_image(pm: &Path, rel: &str, max: u64, left: &mut u64) -> Option<String> {
    let rel = rel.strip_prefix(".pm/").unwrap_or(rel);
    if rel.is_empty()
        || Path::new(rel).is_absolute()
        || rel.starts_with(['/', '\\'])
        || rel.split(['/', '\\']).any(|p| p == "..")
        || !crate::media::is_image_path(rel)
    {
        return None;
    }
    // From the canonical `.pm/`: a `shots/` that is a link out of it fails.
    let shots = pm.canonicalize().ok()?.join("shots");
    let file = pm.join(rel).canonicalize().ok()?;
    if !file.starts_with(&shots) {
        return None;
    }
    let meta = fs::metadata(&file).ok()?;
    if !meta.is_file() || meta.len() > max || meta.len() > *left {
        return None;
    }
    let bytes = fs::read(&file).ok()?;
    let n = bytes.len() as u64;
    if n > max || n > *left {
        return None;
    }
    let ext = crate::media::sniff(&bytes)?;
    *left -= n;
    Some(format!(
        "data:{};base64,{}",
        crate::media::content_type(ext),
        crate::media::base64(&bytes)
    ))
}

/// The ideas of each state that has any, in display order; inside a state,
/// by the date they were said (the undated last), then as in the table.
fn idea_groups(ideas: &Ideas) -> Vec<(IdeaState, Vec<&Idea>)> {
    IdeaState::ALL
        .iter()
        .filter_map(|s| {
            let mut g = ideas.of_state(*s);
            if g.is_empty() {
                return None;
            }
            g.sort_by(|a, b| match (&a.said, &b.said) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
            Some((*s, g))
        })
        .collect()
}

/// "Your ideas": the proof strip (one cell per idea, from the least settled to
/// the most), a filter per state, and a group per state with its rows.
fn ideas_html(ideas: &Ideas, media_max: u64, left: &mut u64, h: &mut String) {
    let pm = ideas
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let groups = idea_groups(ideas);
    let all: Vec<&Idea> = groups.iter().flat_map(|(_, g)| g.iter().copied()).collect();
    let anchors = idea_anchors(&all);
    let anchor_of = |i: &Idea| -> &str {
        let n = all.iter().position(|x| std::ptr::eq(*x, i)).unwrap_or(0);
        &anchors[n]
    };

    h.push_str(
        "<section class=\"block ideas-block\" id=\"your-ideas\" tabindex=\"-1\" aria-labelledby=\"ideas-title\">\n",
    );
    block_head(
        "ideas-title",
        "Your ideas",
        Some(ideas.total),
        "What you said, in your words, and where each one went.",
        h,
    );
    for w in &ideas.warnings {
        h.push_str(&format!("<p class=\"ideas-warn\">{}</p>\n", esc(w)));
    }

    // The proof strip.
    let label_at = |n: Option<&(IdeaState, Vec<&Idea>)>| n.map(|(s, _)| s.label()).unwrap_or("");
    h.push_str(&format!(
        "<ol class=\"proof\" aria-label=\"One cell per idea, from {} to {}\">\n",
        label_at(groups.first()),
        label_at(groups.last())
    ));
    for (s, g) in &groups {
        let label = capitalize(s.label());
        for i in g {
            h.push_str(&format!(
                "<li><a class=\"cell st-{}\" href=\"#{}\" title=\"{}: {}\"><span class=\"sr\">{}, {}</span></a></li>\n",
                state_key(*s),
                esc(anchor_of(i)),
                esc(&label),
                esc(&i.idea),
                esc(&i.idea),
                esc(&label)
            ));
        }
    }
    h.push_str("</ol>\n");

    // The filter: "All" and one button per state that has ideas.
    h.push_str("<div class=\"idea-filter\" role=\"group\" aria-label=\"Show only one state\">\n");
    h.push_str(&format!(
        "<button type=\"button\" class=\"st-btn\" data-state=\"\" aria-pressed=\"true\"><span>All</span><b>{}</b></button>\n",
        ideas.total
    ));
    for (s, g) in &groups {
        h.push_str(&format!(
            "<button type=\"button\" class=\"st-btn\" data-state=\"{k}\" aria-pressed=\"false\"><i class=\"chip st-{k}\" aria-hidden=\"true\"></i><span>{}</span><b>{}</b></button>\n",
            esc(&capitalize(s.label())),
            g.len(),
            k = state_key(*s)
        ));
    }
    h.push_str("</div>\n");

    // The groups.
    for (s, g) in &groups {
        let k = state_key(*s);
        h.push_str(&format!(
            "<div class=\"idea-group\" data-state=\"{k}\">\n<h3 class=\"group-head\"><i class=\"chip st-{k}\" aria-hidden=\"true\"></i><span>{}</span><b>{}</b><span class=\"about\">{}</span></h3>\n<ul class=\"ideas\">\n",
            esc(&capitalize(s.label())),
            g.len(),
            esc(state_about(*s))
        ));
        for i in g {
            idea_row(i, k, anchor_of(i), &pm, media_max, left, h);
        }
        h.push_str("</ul>\n</div>\n");
    }
    h.push_str("</section>\n");
}

/// Where an idea went, in one line: the `where` cell as written; if it only
/// has ids, what they are (`Q-1a2b3c · Which CSV columns?`); with nothing, a
/// fixed sentence for an idea with no destination.
fn went_text(i: &Idea) -> String {
    let only_ids = crate::ideas::id_tokens(&i.where_)
        .iter()
        .fold(i.where_.clone(), |w, t| w.replace(t.as_str(), ""))
        .chars()
        .all(|c| !c.is_alphanumeric());
    if i.where_.trim().is_empty() {
        if i.state == IdeaState::NoDestination {
            return "No decision, task or question covers it yet.".to_string();
        }
        return String::new();
    }
    if only_ids && !i.refs.is_empty() {
        return i
            .refs
            .iter()
            .map(|r| {
                format!(
                    "{} · {}",
                    r.cited.as_deref().unwrap_or(&r.id),
                    one_line(&r.title, 90)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
    }
    i.where_.clone()
}

fn idea_row(
    i: &Idea,
    key: &str,
    anchor: &str,
    pm: &Path,
    media_max: u64,
    left: &mut u64,
    h: &mut String,
) {
    // The first screenshot that can go in the page; the rest of the evidence,
    // and any screenshot that cannot, as chips of text.
    let mut shot: Option<(String, String)> = None;
    let mut chips: Vec<String> = Vec::new();
    for e in &i.evidence {
        if e.kind == EvidenceKind::Image
            && shot.is_none()
            && let Some(src) = idea_image(pm, &e.value, media_max, left)
        {
            let name = e.value.rsplit('/').next().unwrap_or(&e.value).to_string();
            shot = Some((src, name));
            continue;
        }
        chips.push(e.value.clone());
    }
    h.push_str(&format!(
        "<li class=\"idea st-{key}{}\" id=\"{}\">\n",
        if shot.is_some() { " has-shot" } else { "" },
        esc(anchor)
    ));
    match i.said.as_deref().and_then(|d| day_month(d).map(|t| (d, t))) {
        Some((iso, text)) => h.push_str(&format!(
            "<p class=\"said\"><time datetime=\"{}\">{}</time></p>\n",
            esc(iso),
            esc(&text)
        )),
        None => h.push_str("<p class=\"said\"></p>\n"),
    }
    h.push_str(&format!(
        "<div class=\"idea-body\"><h4>{}</h4>",
        esc(&i.idea)
    ));
    if let Some(q) = &i.quote {
        h.push_str(&format!(
            "<blockquote>\u{201c}{}\u{201d}</blockquote>",
            esc(q)
        ));
    }
    let went = went_text(i);
    if !went.trim().is_empty() {
        h.push_str(&format!("<p class=\"went\">{}</p>", esc(&went)));
    }
    for n in &i.notes {
        h.push_str(&format!(
            "<p class=\"idea-notes\">{}</p>",
            esc(&capitalize(n))
        ));
    }
    if !chips.is_empty() {
        h.push_str("<p class=\"proofs\">");
        for c in &chips {
            h.push_str(&format!("<span class=\"ev\">{}</span>", esc(c)));
        }
        h.push_str("</p>");
    }
    h.push_str("\n</div>\n");
    if let Some((src, name)) = shot {
        h.push_str(&format!(
            "<figure class=\"idea-shot\"><div class=\"frame\"><img src=\"{}\" alt=\"{}\" loading=\"lazy\"></div><figcaption>{}</figcaption></figure>\n",
            esc(&src),
            esc(&format!("Screenshot of {}: {name}", i.idea)),
            esc(&name)
        ));
    }
    h.push_str("</li>\n");
}

/// "Your ideas" in text: the summary, the ideas that are not settled, and
/// how many the settled states have.
fn ideas_markdown(ideas: &Ideas, l: &mut Vec<String>) {
    l.push(String::new());
    l.push("## Your ideas".into());
    l.push(crate::ideas::summary(ideas) + ".");
    for w in &ideas.warnings {
        l.push(format!("Warning: {w}"));
    }
    let open = [
        IdeaState::NoDestination,
        IdeaState::WaitingForYou,
        IdeaState::Pending,
        IdeaState::InProgress,
    ];
    for s in open {
        for i in ideas.of_state(s) {
            let went = if i.where_.trim().is_empty() {
                String::new()
            } else {
                format!(" → {}", one_line(&went_text(i), 120))
            };
            l.push(format!(
                "- {}: {} · {}{went}",
                capitalize(s.label()),
                i.id,
                one_line(&i.idea, 100)
            ));
        }
    }
    let settled: Vec<String> = [IdeaState::Decided, IdeaState::Done, IdeaState::Discarded]
        .into_iter()
        .filter_map(|s| {
            let n = ideas.count(s);
            (n > 0).then(|| format!("{} {n}", s.label()))
        })
        .collect();
    if settled.is_empty() {
        l.push("`kontext ideas` lists them all with their evidence.".into());
    } else {
        l.push(format!(
            "{}: `kontext ideas` lists them all with their evidence.",
            capitalize(&settled.join(" · "))
        ));
    }
}

// ------------------------------------------------------------ texto

/// The terminal version: the same, in Markdown.
pub fn markdown(r: &Report) -> String {
    let mut l = vec![format!("# Where we are in {}", r.project)];
    l.push(String::new());
    let dot = match r.light {
        Light::Red => "Red",
        Light::Amber => "Amber",
        Light::Green => "Green",
    };
    l.push(format!(
        "**{dot}: {}** ({}).",
        r.light.label(),
        r.light_reason
    ));
    match &r.status {
        Some(s) => l.push(format!("{} ({}, {})", s.text, s.by, ago(r, &s.hlc))),
        None => l.push("No status paragraph: `kontext log --status \"…\"`.".into()),
    }
    l.push(String::new());
    l.push("## Your turn".into());
    if r.top.is_empty() {
        l.push("Nothing waits for an action from you.".into());
    }
    for it in &r.top {
        let impact = it.impact_text();
        l.push(format!(
            "- {} · {}{} → `{}`",
            it.class.label(),
            it.title,
            if impact.is_empty() {
                String::new()
            } else {
                format!(" ({impact})")
            },
            it.command
        ));
    }
    if !r.rest.is_empty() {
        l.push(format!(
            "- … and {} more: `kontext inbox --all`",
            r.rest.len()
        ));
    }
    l.push(String::new());
    l.push("## Problems".into());
    if r.problems.is_empty() {
        l.push("None active.".into());
    }
    for p in &r.problems {
        l.push(format!(
            "- {}: {}",
            p.title,
            p.why.as_deref().unwrap_or_default()
        ));
    }
    if let Some(ideas) = r.shown_ideas() {
        ideas_markdown(ideas, &mut l);
    }
    l.push(String::new());
    l.push("## What it looks like".into());
    if r.shots.is_empty() {
        l.push("No screenshots yet.".into());
    }
    for s in &r.shots {
        let before = s
            .before
            .as_ref()
            .map(|b| format!(" (before: {})", b.rel))
            .unwrap_or_default();
        l.push(format!(
            "- {} · {}: {}{before}",
            s.epic, s.screen, s.now.rel
        ));
    }
    l.push(String::new());
    l.push("## Progress".into());
    for g in &r.goals {
        l.push(format!("- {}: {} of {} tasks", g.title, g.closed, g.total));
    }
    if !r.waves.is_empty() {
        let parts: Vec<String> = r
            .waves
            .iter()
            .map(|w| format!("{}: {}", w.label, w.epics.join(", ")))
            .collect();
        l.push(format!("- {}", parts.join(" → ")));
    }
    l.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Policy;

    #[test]
    fn idea_images_share_the_page_budget() {
        let dir = tempfile::tempdir().unwrap();
        let pm = dir.path().join(".pm");
        fs::create_dir_all(pm.join("shots")).unwrap();
        let shots = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/screenshots");
        let a = fs::read(shots.join("email.png")).unwrap();
        let b = fs::read(shots.join("payment-v1.png")).unwrap();
        fs::write(pm.join("shots/a.png"), &a).unwrap();
        fs::write(pm.join("shots/b.png"), &b).unwrap();
        let md = "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n\
                  | P1 | first | | pending | | shots/a.png |\n\
                  | P2 | second | | pending | | shots/b.png |\n";
        let st = State::build_at(vec![], &Policy::new(&["omar"]), 0);
        let ideas = crate::ideas::build(&st, "demo", &[], &pm, md);
        let render = |budget: u64| {
            let mut left = budget;
            let mut h = String::new();
            ideas_html(&ideas, u64::MAX, &mut left, &mut h);
            (h, left)
        };
        // Room for both: both go in, and the budget counts their raw bytes.
        let total = (a.len() + b.len()) as u64;
        let (h, left) = render(total + 10);
        assert_eq!(h.matches("data:image/png;base64,").count(), 2);
        assert_eq!(left, 10);
        // One byte short: the first goes in, the second is a chip.
        let (h, left) = render(total - 1);
        assert_eq!(h.matches("data:image/png;base64,").count(), 1);
        assert!(h.contains(&crate::media::base64(&a)));
        assert!(!h.contains(&crate::media::base64(&b)));
        assert!(h.contains("<span class=\"ev\">shots/b.png</span>"), "{h}");
        assert_eq!(left, b.len() as u64 - 1);
        // Nothing left: both are chips.
        let (h, _) = render(0);
        assert!(!h.contains("data:image/"));
        assert!(h.contains("<span class=\"ev\">shots/a.png</span>"));
    }
}
