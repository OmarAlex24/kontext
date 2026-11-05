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
//! 4. **What it looks like**: the most recent screenshots per epic, with the
//!    before and after of the same screen if there is one;
//! 5. **Progress**: a bar per goal and the wave line.
//!
//! There is no history of what is done. Generating it writes nothing and
//! closes no pending items.
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
use crate::inbox::{self, Class, Inbox, Item};
use crate::model::Entry;
use crate::plan;
use crate::state::{State, Status, string_list};
use crate::tasks::{self, TaskStatus};
use crate::views::{field, one_line};
use crate::write::is_status_tag;

const STYLE_CSS: &str = include_str!("../templates/report/style.css");
const SCRIPT_JS: &str = include_str!("../templates/report/report.js");
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
}

/// The latest status paragraph (`kontext log --status`).
pub fn status_note(st: &State) -> Option<StatusNote> {
    st.journal()
        .into_iter()
        .rev()
        .find(|j| j.tags.iter().any(|t| is_status_tag(t)))
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
                "{} trabajo",
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
    format!(
        "default-src 'none'; script-src {}; style-src {}; img-src 'self' data:; \
         font-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        sha256_source(inline_script()),
        sha256_source(&inline_style())
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
    for (id, label, n) in [
        ("your-turn", "Your turn", Some(pending)),
        ("problems", "Problems", Some(r.problems.len())),
        (
            "what-it-looks-like",
            "What it looks like",
            Some(r.shots.len()),
        ),
        ("progress", "Progress", None),
    ] {
        h.push_str(&format!(
            "<li><a href=\"#{id}\" data-nav=\"{id}\"><span>{label}</span>{}</a></li>\n",
            n.map(count).unwrap_or_default()
        ));
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
        "<title>{} · where we are</title>\n<style>{}</style>\n</head>\n<body class=\"s-{cls}\">\n",
        esc(&r.project),
        inline_style()
    ));
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

    // 4. What it looks like.
    h.push_str(
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
        &mut h,
    );
    if !r.shots.is_empty() {
        let mut used = 0u64;
        h.push_str("<div class=\"shots\">\n");
        for s in &r.shots {
            h.push_str(&format!(
                "<article class=\"shot{}\"{}>\n<h3><span class=\"epic\">{}</span><span class=\"screen\">{}</span></h3>\n<div class=\"pair\">\n",
                if s.before.is_some() { " has-before" } else { "" },
                epic_attr(s.epic_id.as_slice()),
                esc(&s.epic),
                esc(&s.screen)
            ));
            if let Some(b) = &s.before {
                figure(r, b, "Before", img, &mut used, &mut h);
            }
            let label = if s.before.is_some() { "Now" } else { "Latest" };
            figure(r, &s.now, label, img, &mut used, &mut h);
            h.push_str("</div>\n</article>\n");
        }
        h.push_str("</div>\n");
    }
    h.push_str("</section>\n");

    // 5. Progress.
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
    h.push_str("</body>\n</html>\n");
    Ok(h)
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
