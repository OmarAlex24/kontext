//! `kontext handoff`: from the planning chat to a new PM chat (DESIGN §12.4).
//!
//! The plan is made with one conversation and run by another. The new PM
//! starts from the ledger (`kontext resume --as pm`, `kontext tree`), never
//! from a summary of the planning chat, so the hand-off waits until nothing
//! the planning chat knows is only in that chat:
//! - the plan is signed (a `scope` entry counts);
//! - no idea of the user's is "Not placed" (`kontext ideas`);
//! - no message of the user's is unaudited (`kontext ideas --mark-audited`
//!   and the reader saw them all).
//!
//! It writes nothing. Ready, it prints the opening prompt for the new chat and
//! how the capture moves to it (`kontext capture here --replace`).

use serde::Serialize;

use crate::capture::Named;
use crate::ideas::month_day;
use crate::scope::{self, IdeasCheck};
use crate::state::State;
use crate::views::one_line;

/// What `kontext handoff` found.
#[derive(Serialize, Clone, Debug)]
pub struct Handoff {
    pub project: String,
    /// All three conditions hold.
    pub ready: bool,
    /// What is missing, one line each (empty when ready).
    pub missing: Vec<String>,
    /// The signed plan's `S` id, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed: Option<String>,
    /// Worth knowing, never blocking (a scope change waiting, the capture).
    pub notes: Vec<String>,
    /// The opening prompt for the new PM chat (only when ready).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Whether `[capture] enabled` is on.
    pub capture_enabled: bool,
    /// The conversations named for capture on this machine now: the new chat's
    /// `kontext capture here --replace` forgets them.
    pub named: Vec<Named>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The three conditions, each with what to do about it.
fn missing(st: &State, ideas: &IdeasCheck) -> Vec<String> {
    let mut out = Vec::new();
    if !scope::plan_signed(st) {
        out.push(
            "The plan is not signed. Show the owner `kontext scope summary` verbatim and, on their \
             yes, sign it with `kontext scope sign --hash <hash> --approved-by <owner> --said \
             \"<their words>\"`."
                .into(),
        );
    }
    match ideas {
        IdeasCheck::NotFound(dir) => out.push(format!(
            "No .pm/items.md found from {dir}, so the user's ideas and messages can't be checked. \
             Run `kontext handoff` from the repo that has .pm/ (where the planning chat runs)."
        )),
        IdeasCheck::Read(i) => {
            if !i.not_placed.is_empty() {
                let list: Vec<String> = i
                    .not_placed
                    .iter()
                    .map(|np| {
                        format!(
                            "{} \"{}\"",
                            np.id,
                            one_line(np.quote.as_deref().unwrap_or(&np.idea), 80)
                        )
                    })
                    .collect();
                out.push(format!(
                    "{} not placed: {}. Give each one a feature, task or decision in \
                     .pm/items.md, or ask the user whether to discard it.",
                    plural(
                        i.not_placed.len(),
                        "idea of the user's is",
                        "ideas of the user's are"
                    ),
                    list.join("; ")
                ));
            }
            if i.unaudited.count > 0 {
                let since = match &i.unaudited.since {
                    Some(s) => {
                        format!("since the audit of {}", month_day(s.get(..10).unwrap_or(s)))
                    }
                    None => "by any audit".into(),
                };
                out.push(format!(
                    "{} not seen {since}. Run the coverage audit (`kontext ideas \
                     --mark-audited`, then the kontext-pm-reader, as the kontext-pm skill says) \
                     and place what it finds.",
                    plural(
                        i.unaudited.count,
                        "message of the user's was",
                        "messages of the user's were"
                    )
                ));
            }
        }
    }
    out
}

/// The opening prompt for the new PM chat. It names no summary of the
/// planning chat on purpose: the new PM reads the ledger.
pub fn prompt(project: &str, signed: &str, signed_on: &str, capture: bool) -> String {
    let capture_step = if capture {
        "1. Run `kontext capture here --replace`. From now on this conversation is the one whose \
         messages kontext saves to .pm/dumps, and the planning chat's no longer are."
    } else {
        "1. Capture is off in this project: save the user's messages to .pm/dumps yourself, as the \
         kontext-pm skill says."
    };
    format!(
        "You are the PM of the project {project} from this message on: the conversation the user \
         talks to. The plan was made in another chat and the user signed it ({signed}, \
         {signed_on}). Do not ask for a summary of that chat: everything that counts is in \
         kontext.\n\
         \n\
         {capture_step}\n\
         2. Read the project from the ledger: `kontext resume --as pm`, then `kontext tree`.\n\
         3. Follow the kontext-pm skill (\"Starting after a handoff\") and kontext-orchestrate from \
         wave 1. Inside the signed scope you decide alone; a change of scope goes to the user.\n\
         4. Tell the user, in at most five lines, where the project stands and what you will \
         launch first, and wait for their go.\n"
    )
}

/// Checks the three conditions; ready, builds the prompt.
pub fn check(
    st: &State,
    project: &str,
    ideas: &IdeasCheck,
    capture_enabled: bool,
    named: Vec<Named>,
) -> Handoff {
    let missing = missing(st, ideas);
    let ready = missing.is_empty();
    let last = scope::last(st);
    let mut notes = Vec::new();
    if let Some(p) = scope::pending_line(st) {
        notes.push(format!(
            "{p} The new PM shows it to the owner; it does not hold the hand-off."
        ));
    }
    if capture_enabled {
        if named.is_empty() {
            notes.push(
                "No conversation is named for capture on this machine: once the new chat runs \
                 `kontext capture here --replace`, it is the only one captured."
                    .into(),
            );
        } else {
            let ids: Vec<&str> = named.iter().map(|n| n.id.as_str()).collect();
            notes.push(format!(
                "Named for capture on this machine now: {}. The new chat's `kontext capture here \
                 --replace` forgets them, so this chat stops being captured then. If the new \
                 chat runs on another machine, run `kontext capture forget --all` here.",
                ids.join(", ")
            ));
        }
    }
    let prompt = match (ready, last) {
        (true, Some(s)) => Some(prompt(project, &s.id, &scope::day(&s.hlc), capture_enabled)),
        _ => None,
    };
    Handoff {
        project: project.to_string(),
        ready,
        missing,
        signed: last.map(|s| s.id.clone()),
        notes,
        prompt,
        capture_enabled,
        named,
    }
}

impl Handoff {
    /// Not ready: what is missing, one line each (the refusal's text).
    pub fn missing_text(&self) -> String {
        let mut l = vec![format!(
            "not ready to hand {} off to a new PM chat. Missing:",
            self.project
        )];
        l.extend(self.missing.iter().map(|m| format!("- {m}")));
        l.join("\n")
    }

    /// Ready: the prompt to paste and what happens to the capture.
    pub fn text(&self) -> String {
        let mut l =
            vec![
            format!(
                "Ready to hand {} off: the plan is signed ({}), no idea of the user's is left \
                 unplaced and every message of theirs was audited.",
                self.project,
                self.signed.as_deref().unwrap_or("?"),
            ),
            String::new(),
            "Open a new chat for the PM, in the checkout the PM works in, and paste this as its \
             first message:"
                .into(),
            String::new(),
            "----- 8< -----".into(),
            self.prompt.clone().unwrap_or_default().trim_end().to_string(),
            "----- 8< -----".into(),
            String::new(),
            "Then stop typing in this chat: the new one carries on from the ledger.".into(),
        ];
        l.extend(self.notes.iter().map(|n| format!("Note: {n}")));
        l.join("\n") + "\n"
    }
}
