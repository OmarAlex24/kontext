//! What the commands and the hook share: building a view from the store, with
//! its cursor.

use anyhow::Result;
use serde_json::Value;

use crate::board::{self, BriefOpts, NextOpts};
use crate::config::Project;
use crate::cursor::{self, Frontier};
use crate::facts;
use crate::model::{Actor, Entry};
use crate::repos::Repos;
use crate::state::State;
use crate::store::{Refused, Store};
use crate::tasks;
use crate::verify;
use crate::views::{self, DEFAULT_BUDGET, ResumeOpts, Standup};

/// `2026-11-03 18:22 UTC`: `KONTEXT_NOW` (for tests) or the clock, like
/// [`tasks::now_ms`], so two views generated a minute apart in a test match.
pub fn now_text() -> String {
    jiff::Timestamp::from_millisecond(tasks::now_ms())
        .unwrap_or_else(|_| jiff::Timestamp::now())
        .strftime("%Y-%m-%d %H:%M UTC")
        .to_string()
}

/// The budget of a role: `[budgets]` in `project.toml`, or 9,000.
pub fn budget(project: &Project, role: &str) -> usize {
    project
        .budgets
        .get(role)
        .map(|b| *b as usize)
        .unwrap_or(DEFAULT_BUDGET)
}

/// A built view and the frontier it reached (to advance the cursor by exactly
/// what was shown).
pub struct Rendered<T> {
    pub view: T,
    pub frontier: Frontier,
}

/// The state with the currency of the facts (phase 4): compares their
/// fingerprints with the local checkouts of the project's repos. If something
/// fails (git, `projects.toml`), the facts are left unchecked; the view is
/// still produced.
pub fn state(store: &mut Store, project: &Project) -> Result<State> {
    let mut st = store.state()?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| store.home().to_path_buf());
    if let Ok(repos) = Repos::load(project, &cwd) {
        facts::annotate(&mut st, &repos, Some(store.index()));
    }
    Ok(st)
}

/// The state, the frontier at that same moment, and what is new for `reader`.
fn snapshot(
    store: &mut Store,
    project: &Project,
    reader: &str,
) -> Result<(State, Frontier, Vec<Entry>)> {
    let st = state(store, project)?;
    let frontier = Frontier(store.index().frontier()?);
    let since = store.cursor(reader)?;
    let new: Vec<Entry> = cursor::new_since(st.entries.values(), &since)
        .into_iter()
        .cloned()
        .collect();
    Ok((st, frontier, new))
}

/// Who writes, according to the environment (`$KONTEXT_ACTOR`), if set: the
/// hook and the agent share the Claude Code environment.
fn env_writer() -> Option<String> {
    std::env::var("KONTEXT_ACTOR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|_| Actor::from_env().ok())
        .map(|a| a.short())
}

pub struct Resume {
    pub text: String,
    pub json: Value,
}

/// `kontext resume --as <role>`.
pub fn resume(
    store: &mut Store,
    project: &Project,
    role: &str,
    max_chars: Option<usize>,
) -> Result<Rendered<Resume>> {
    let (st, frontier, new) = snapshot(store, project, role)?;
    let report = verify::run(store.home())?;
    let opts = ResumeOpts {
        project: project.name.clone(),
        role: role.to_string(),
        max_chars: max_chars.unwrap_or_else(|| budget(project, role)),
        generated: now_text(),
        chain_problem: report.problems.first().map(ToString::to_string),
        entries: st.entries.len(),
        writer: env_writer(),
        distill: crate::distill::active(project),
    };
    Ok(Rendered {
        view: Resume {
            text: views::resume(&st, &new, &opts),
            json: views::resume_json(&st, &new, &opts),
        },
        frontier,
    })
}

/// `kontext standup` for `reader`.
pub fn standup(store: &mut Store, project: &Project, reader: &str) -> Result<Rendered<Standup>> {
    let (st, frontier, new) = snapshot(store, project, reader)?;
    let since = cursor::last_advanced(store.home(), reader)
        .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string());
    Ok(Rendered {
        view: views::standup(&st, &new, &project.name, reader, since),
        frontier,
    })
}

/// The `pointer` mode of the hook: three lines at most.
pub fn pointer(store: &mut Store, project: &Project, role: &str) -> Result<String> {
    let (st, _, new) = snapshot(store, project, role)?;
    let questions = st
        .open_questions()
        .into_iter()
        .filter(|q| views::question_is_for(&st, q, role))
        .count();
    let updates = if new.len() == 1 {
        "1 update".to_string()
    } else {
        format!("{} updates", new.len())
    };
    let questions = if questions == 1 {
        "1 question".to_string()
    } else {
        format!("{questions} questions")
    };
    let write = if crate::distill::active(project) {
        format!(". {}", views::DISTILL_HINT)
    } else {
        "; the `kontext` skill explains what else to check and how to write.".into()
    };
    Ok(format!(
        "Project {} in kontext: there are {updates} and {questions} for you ({role}).\n\
         Run `kontext resume --as {role}` to see the charter, the decisions in force, what the \
         user said and what's new{write}\n\
         What the user said applies even if unsigned (the signature is for the charter); what \
         was superseded no longer applies. {}\n",
        project.name,
        views::invoke_hint(env_writer().as_deref())
    ))
}

/// What entered the ledger since `since` (the frontier from the last time the
/// hook gave context to a session), for the hook on a `resume`. With `epic`,
/// only what touches that epic or the project's rules (as the `brief` does).
/// Returns how many entries there are and the text (`None` if there is
/// nothing to say).
pub fn session_delta(
    store: &mut Store,
    project: &Project,
    role: &str,
    epic: Option<&str>,
    since: &Frontier,
) -> Result<(usize, Option<String>)> {
    let st = state(store, project)?;
    let mut new: Vec<Entry> = cursor::new_since(st.entries.values(), since)
        .into_iter()
        .cloned()
        .collect();
    if let Some(epic) = epic {
        let id = st.find_epic(epic)?;
        let e = st.get(&id).expect("the epic exists");
        new.retain(|x| board::concerns_epic(&st, x, e));
    }
    let report = verify::run(store.home())?;
    let opts = views::DeltaOpts {
        role: role.to_string(),
        chain_problem: report.problems.first().map(ToString::to_string),
    };
    Ok((new.len(), views::session_delta(&st, &new, &opts)))
}

pub struct Brief {
    pub text: String,
    pub json: Value,
    /// The reader: `epic:<name>`.
    pub reader: String,
}

/// `kontext brief <epic>`: the coordinator's view, with what is new for
/// `epic:<name>` and the assignment frontier at that moment.
pub fn brief(
    store: &mut Store,
    project: &Project,
    epic: &str,
    max_chars: Option<usize>,
) -> Result<Rendered<Brief>> {
    let st = store.state()?;
    let epic = st.find_epic(epic)?;
    let reader = format!("epic:{}", tasks::epic_name(st.get(&epic).unwrap()));
    let (st, frontier, new) = snapshot(store, project, &reader)?;
    let opts = BriefOpts {
        project: project.name.clone(),
        epic,
        max_chars: max_chars.unwrap_or_else(|| budget(project, "coordinator")),
        generated: now_text(),
        frontier: frontier.clone(),
    };
    Ok(Rendered {
        view: Brief {
            text: board::brief(&st, &new, &opts),
            json: board::brief_json(&st, &new, &opts),
            reader,
        },
        frontier,
    })
}

pub struct Next {
    pub text: String,
    pub json: Value,
}

/// `kontext next --epic <epic> [--claim] [--max-chars]` (9,000 by default).
/// With no tasks to hand out, a [`Refused`] with the blocked ones and what
/// blocks them.
pub fn next(
    store: &mut Store,
    project: &Project,
    actor: &Actor,
    epic: &str,
    claim: bool,
    max_chars: Option<usize>,
) -> Result<Next> {
    let (task, claimed) = if claim {
        let (task, written) = tasks::next_claim(store, actor, epic)?;
        (task, written.map(|e| e.id))
    } else {
        let st = store.state()?;
        let epic = st.find_epic(epic)?;
        match tasks::pick(&st, &epic, actor) {
            Some((t, _)) => (t, None),
            None => return Err(Refused(tasks::nothing_ready(&st, &epic, actor)).into()),
        }
    };
    let st = state(store, project)?;
    let frontier = Frontier(store.index().frontier()?);
    let opts = NextOpts {
        project: project.name.clone(),
        frontier,
        claimed,
        actor: format!("{}:{}", actor.kind, actor.name),
        max_chars: max_chars.unwrap_or(DEFAULT_BUDGET),
    };
    Ok(Next {
        text: board::next(&st, &task, &opts),
        json: board::next_json(&st, &task, &opts),
    })
}
