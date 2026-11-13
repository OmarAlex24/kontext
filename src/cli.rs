//! Commands. Phase 1: `init`, `verify`, `reindex`, `import --from-v0` and the
//! debugging commands (`debug append`, `debug claim`, `debug state`). Phase 2:
//! charter, goals, sign, decisions, questions, log, views, `recall`, `hook` and
//! `install`. Phase 3: epics, tasks, `assign`, `release`, `reopen`, `next`,
//! `brief`, `done` and `fact`. Outside the phases: `doctor` and `measure`, which
//! never open the project (see their modules).
//!
//! Exit codes: 0 ok, 1 error, 2 refused (nothing was written because a rule
//! isn't met), 3 stale context in `done`.

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::app;
use crate::changelog;
use crate::config::{self, Machine, Project};
use crate::distill;
use crate::facts;
use crate::find;
use crate::hook;
use crate::import_v0;
use crate::install;
use crate::model::{Actor, Entry, RawEntry};
use crate::recall;
use crate::repos::Repos;
use crate::store::{Refused, Store};
use crate::tasks::{self, DoneArgs, Stale, TaskArgs};
use crate::verify;
use crate::views;
use crate::write::{self, DecisionArgs};

#[derive(Parser, Debug)]
#[command(
    name = "kontext",
    bin_name = "kontext",
    version,
    about = "kontext: project memory for coding agents"
)]
pub struct Cli {
    /// Project to use (if not set: $KONTEXT_PROJECT, the .kontext.toml of the repo, or the only registered one)
    #[arg(long, global = true)]
    pub project: Option<String>,
    /// Who writes: human:<name> or agent:<name> (if not set: $KONTEXT_ACTOR, or agent:unknown)
    #[arg(long, global = true, value_name = "ACTOR")]
    pub actor: Option<String>,
    /// Role of the agent that writes (if not set: $KONTEXT_ROLE). In `install`, the default_role
    /// of the .kontext.toml that gets created (pm if not given)
    #[arg(long, global = true, value_name = "ROLE")]
    pub role: Option<String>,
    /// Model of the agent that writes (if not set: $KONTEXT_MODEL)
    #[arg(long, global = true, value_name = "MODEL")]
    pub model: Option<String>,
    /// JSON output (for agents)
    #[arg(long, global = true)]
    pub json: bool,
    /// Signs with SSH whatever gets signed (sign, decide, accept, reject, supersede), even if
    /// projects.toml has no [signing] key (uses ~/.ssh/id_ed25519.pub)
    #[arg(long, global = true)]
    pub sign: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// What `decide` and `propose` have in common.
#[derive(clap::Args, Debug)]
pub struct DecisionFlags {
    /// Title of the decision
    pub title: String,
    /// Why
    #[arg(long, default_value = "")]
    pub why: String,
    /// Detail
    #[arg(long, default_value = "")]
    pub body: String,
    /// Tags, separated by commas (the first one groups in `resume`)
    #[arg(long, value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Current decision that this one replaces (can be repeated)
    #[arg(long, value_name = "ID")]
    pub supersedes: Vec<String>,
    /// Epic it belongs to
    #[arg(long)]
    pub epic: Option<String>,
    /// Command that checks the decision (exits with 0 if it holds)
    #[arg(long, value_name = "CMD")]
    pub guard_cmd: Option<String>,
    /// Image (png, jpg, webp) or .html page to attach, for example the phase 0 mockup.
    /// It is stored in media/ in the kontext-home (can be repeated)
    #[arg(long = "attach", value_name = "FILE")]
    pub attach: Vec<String>,
    /// The screen of each attachment, in order (default: the file name without extension)
    #[arg(long = "screen", value_name = "NAME")]
    pub screen: Vec<String>,
}

impl DecisionFlags {
    fn args(&self) -> DecisionArgs {
        DecisionArgs {
            title: self.title.clone(),
            why: self.why.clone(),
            body: self.body.clone(),
            tags: clean_list(&self.tags),
            supersedes: self.supersedes.clone(),
            epic: self.epic.clone(),
            guard_cmd: self.guard_cmd.clone(),
            attachments: Vec::new(),
        }
    }
}

fn clean_list(v: &[String]) -> Vec<String> {
    v.iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Subcommand, Debug)]
pub enum GoalCmd {
    /// Adds a goal to the current charter (left unsigned)
    Add {
        title: String,
        /// When it's considered met
        #[arg(long)]
        done_when: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum EpicCmd {
    /// Creates an epic: a group of tasks with a coordinator, linked to charter goals
    Add {
        title: String,
        /// Charter goal it serves (can be repeated; at least one)
        #[arg(long = "goal", value_name = "G", required = true)]
        goals: Vec<String>,
        /// Who coordinates it (default: epic:<name>)
        #[arg(long)]
        coordinator: Option<String>,
        /// Short name for epic:<name> (default: taken from the title)
        #[arg(long)]
        name: Option<String>,
        /// Epic that must close first: E-…, epic:<name> or <name>, even if it doesn't exist
        /// yet (can be repeated). `kontext plan` builds the waves from this
        #[arg(long = "after", value_name = "EPIC")]
        after: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum FeatureCmd {
    /// Adds a feature to an epic: what the human would see working. Its tasks hang from it
    /// (`task add --feature`). Only the pm or an owner. Without --goal, it serves the epic's goals
    Add {
        title: String,
        /// Epic: E-…, epic:<name> or <name>
        #[arg(long)]
        epic: String,
        /// What the human would see when it works
        #[arg(long)]
        acceptance: Option<String>,
        /// Goal it serves (can be repeated)
        #[arg(long = "goal", value_name = "G")]
        goals: Vec<String>,
        /// Detail
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum TaskCmd {
    /// Adds a task to an epic. Without --goal, it serves the epic's goals
    Add {
        title: String,
        /// Epic: E-…, epic:<name> or <name> (not needed with --feature)
        #[arg(long, required_unless_present = "feature")]
        epic: Option<String>,
        /// Feature it hangs from (U-…); its epic is the task's epic
        #[arg(long, value_name = "U")]
        feature: Option<String>,
        /// Goal it serves (can be repeated)
        #[arg(long = "goal", value_name = "G")]
        goals: Vec<String>,
        /// Task that must close first (can be repeated)
        #[arg(long = "depends-on", value_name = "T")]
        depends_on: Vec<String>,
        /// When it's considered done
        #[arg(long)]
        acceptance: Option<String>,
        /// Detail
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum InboxCmd {
    /// One line: how many, how many new, the oldest, and what the first one blocks (for hooks and prompts)
    Count,
    /// Signs a charter or a goal, or accepts a proposal, from the inbox
    Sign {
        /// The item's number, its key, or the id of its entry
        item: String,
    },
    /// Answers a question in the inbox (in a group: 2.1, 2.2… or its Q-id)
    Answer { item: String, text: String },
    /// Removes it from the inbox without resolving it, with its reason
    Dismiss {
        item: String,
        #[arg(long, default_value = "")]
        why: String,
    },
    /// Confirms what the distill noted from your conversations (`review`)
    Confirm { item: String },
}

#[derive(Subcommand, Debug)]
pub enum CaptureCmd {
    /// Names this conversation ($CLAUDE_CODE_SESSION_ID) for the capture hook: once at least one is
    /// named, only named conversations are captured, for this project on this machine
    Here {
        /// The conversation's session id, outside Claude Code (default: $CLAUDE_CODE_SESSION_ID)
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// The conversations named for capture on this machine
    List,
    /// Forgets a named conversation, or all of them; with none named, every pm conversation in a
    /// capturing checkout is captured again
    Forget {
        /// Its session id
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        id: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Creates a kontext-home and registers it in ~/.config/kontext/projects.toml
    Init {
        name: String,
        /// Where to create it (default: ~/kontext-home/<name>)
        #[arg(long)]
        home: Option<PathBuf>,
        /// Git remote for the kontext-home
        #[arg(long)]
        remote: Option<String>,
    },
    /// Brings in an existing kontext-home with git, registers it and builds the index
    Clone {
        /// Git URL of the kontext-home
        url: String,
        /// Where to put it (default: ~/kontext-home/<repo name>)
        #[arg(long)]
        home: Option<PathBuf>,
        /// Name to register it under (default: the one in project.toml)
        #[arg(long)]
        name: Option<String>,
    },
    /// Uploads what's ours (ledger and cursors) and brings in everyone else's: commit, pull --rebase and push of the kontext-home
    Sync {
        /// Also uploads the changes to project.toml and synonyms.toml
        #[arg(long)]
        include_config: bool,
    },
    /// Project, writer, remote, last sync, what's left to upload, index and chains
    Status,
    /// Checks the chain of each ledger file (and any signatures)
    Verify,
    /// Rebuilds the index from the ledger files
    Reindex,
    /// Imports a ledger
    Import {
        /// CTX_HOME folder of the v0 `ctx.py` prototype (the one with ledger/entries.jsonl)
        #[arg(long = "from-v0", value_name = "PATH")]
        from_v0: PathBuf,
        /// Imports even if the v0 chain is broken
        #[arg(long)]
        force: bool,
    },
    /// Writes a new charter (objective, what isn't done, when it's finished). An owner signs it with `kontext sign`
    Charter {
        /// The project's objective
        #[arg(long)]
        objective: String,
        /// Something that isn't going to be done (can be repeated)
        #[arg(long = "non-goal", value_name = "TEXT")]
        non_goals: Vec<String>,
        /// When it's considered finished
        #[arg(long)]
        done_when: Option<String>,
        /// Short title (default: the objective)
        #[arg(long)]
        title: Option<String>,
    },
    /// Goals of the charter
    Goal {
        #[command(subcommand)]
        cmd: GoalCmd,
    },
    /// Signs a charter or a goal (owners only)
    Sign {
        id: String,
        /// Owner who approved it (only for the pm)
        #[arg(long)]
        approved_by: Option<String>,
    },
    /// Records a decision (owners only, or the pm with --approved-by)
    Decide {
        #[command(flatten)]
        d: DecisionFlags,
        /// Owner who approved it (only for the pm)
        #[arg(long)]
        approved_by: Option<String>,
    },
    /// Proposes a decision for an owner to accept or reject (anyone can)
    Propose {
        #[command(flatten)]
        d: DecisionFlags,
    },
    /// Accepts a proposal (owners only, or the pm with --approved-by)
    Accept {
        id: String,
        /// Also writes a decision with the content of the proposal
        #[arg(long)]
        as_decision: bool,
        #[arg(long)]
        approved_by: Option<String>,
        #[arg(long, default_value = "")]
        why: String,
    },
    /// Rejects a proposal, with its reason (owners only, or the pm with --approved-by)
    Reject {
        id: String,
        #[arg(long)]
        why: String,
        #[arg(long)]
        approved_by: Option<String>,
    },
    /// Marks that <old> is replaced by <new> (charters, goals and decisions: owners only)
    Supersede {
        old: String,
        new: String,
        #[arg(long, default_value = "")]
        why: String,
        #[arg(long)]
        approved_by: Option<String>,
    },
    /// Asks a question of a reader: owner, pm, epic:<x> or a person
    Ask {
        title: String,
        /// owner, pm, epic:<x> or a person (with --task, by default the task's epic)
        #[arg(long)]
        to: Option<String>,
        /// Task the question comes from (its answer counts as new context in `done`)
        #[arg(long)]
        task: Option<String>,
        /// Feature the question is about (U-…)
        #[arg(long, value_name = "U")]
        feature: Option<String>,
        #[arg(long, default_value = "")]
        body: String,
        /// Epic of the question (default: the task's, or yours if you coordinate it)
        #[arg(long)]
        epic: Option<String>,
        /// The answer stops your work. Without this, if the human already has many pending items
        /// ([inbox] max_pending), your question is marked as not urgent
        #[arg(long)]
        blocking: bool,
        /// Records the question even if an open one already says the same (by default, it is added to that one)
        #[arg(long)]
        no_merge: bool,
    },
    /// Answers a question
    Answer { id: String, text: String },
    /// Writes a log entry
    Log {
        text: String,
        /// What comes next
        #[arg(long)]
        next: Option<String>,
        /// Task it belongs to
        #[arg(long)]
        task: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        /// Is the "where we are" paragraph for the human (2 or 3 sentences); `kontext report` takes the last one
        #[arg(long)]
        status: bool,
    },
    /// Your pending items: what waits for an action from you (sign, answer, review and problems), by impact.
    /// Viewing them doesn't close them
    Inbox {
        #[command(subcommand)]
        cmd: Option<InboxCmd>,
        /// All of them, complete (without this: the 5 with the most impact and "and N more")
        #[arg(long)]
        all: bool,
        /// Whose (default: your name: --actor human:<x>, $KONTEXT_ACTOR or git config user.name)
        #[arg(long = "as", value_name = "HUMAN")]
        reader: Option<String>,
        /// Everyone's: one section per human with theirs and a shared one (sign, review, questions
        /// to owners and problems). With `count`, one line per human
        #[arg(long, global = true)]
        everyone: bool,
    },
    /// The report for the human: where we are, what's theirs to do, the problems, how it looks, and progress.
    /// Doesn't write anything or close pending items
    Report {
        /// A single-file HTML page, with the screenshots inside
        #[arg(long)]
        html: bool,
        /// Where to write it (default: standard output)
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: Option<PathBuf>,
        /// For whom (default: your name, as in standup)
        #[arg(long = "as", value_name = "HUMAN")]
        reader: Option<String>,
    },
    /// The user's ideas in `.pm/items.md` (skill kontext-pm) and where each one went: its state
    /// comes from the ledger through the ids in `where`. Doesn't write anything, except with
    /// `--mark-audited`
    Ideas {
        /// Before a coverage audit: write `.pm/audited.json` with the time and how many messages
        /// each dump has, so the messages after it count as not audited
        #[arg(long)]
        mark_audited: bool,
    },
    /// Which conversations the capture hook writes from, on this machine: `here`, `list`, `forget`.
    /// Works with the capture off
    Capture {
        #[command(subcommand)]
        cmd: CaptureCmd,
    },
    /// Epics
    Epic {
        #[command(subcommand)]
        cmd: EpicCmd,
    },
    /// Features: what the human would see working, between an epic and its tasks
    Feature {
        #[command(subcommand)]
        cmd: FeatureCmd,
    },
    /// Tasks
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Assigns a task (the epic's coordinator, the pm or an owner)
    Assign {
        task: String,
        /// To whom: agent:<name> or <name>
        #[arg(long)]
        to: String,
    },
    /// Releases a task: it becomes ready again (whoever holds it, the coordinator, the pm or an owner)
    Release {
        task: String,
        #[arg(long, default_value = "")]
        why: String,
    },
    /// Reopens a closed task, or a validated feature
    Reopen {
        task: String,
        #[arg(long)]
        why: String,
    },
    /// The next task of an epic for an implementer or tester, with its assignment mark
    Next {
        /// Epic: E-…, epic:<name> or <name>
        #[arg(long)]
        epic: String,
        /// Also claims it (atomic on this machine; exits with 2 if none is left)
        #[arg(long)]
        claim: bool,
        /// Character budget (default: 9000)
        #[arg(long)]
        max_chars: Option<usize>,
    },
    /// The wave order of open epics: what runs in series and what in parallel. Exits with 1 if there are cycles or dependencies on epics that don't exist
    Plan,
    /// The coordinator's view of an epic, with its assignment mark. Advances the epic:<name> cursor
    Brief {
        /// Epic: E-…, epic:<name> or <name>
        epic: String,
        /// Character budget (default: [budgets] coordinator, or 9000)
        #[arg(long)]
        max_chars: Option<usize>,
        /// Shows without advancing the cursor
        #[arg(long)]
        peek: bool,
    },
    /// Closes a task with evidence, or validates a feature (U-…) whose tasks are all closed.
    /// Refuses (exits with 3) if anything that touches it came in after --since
    Done {
        task: String,
        /// cmd:"<command>"=<code>, file:<path>, pr:<url>, url:<url>, or a screenshot (screenshot.png,
        /// .jpg or .webp; stored in media/). Can be repeated
        #[arg(long, value_name = "SPEC")]
        evidence: Vec<String>,
        /// The screen of each screenshot, in order, for the "before and after" in the report
        /// (default: the file name without extension)
        #[arg(long = "screen", value_name = "NAME")]
        screen: Vec<String>,
        /// Evidence in JSON: {"type":"command","value":"…","exit_code":0} or a list
        #[arg(long, value_name = "JSON")]
        evidence_json: Vec<String>,
        /// Runs this command and closes only if it exits with 0 (stores the exit code and the last 20 lines)
        #[arg(long, value_name = "CMD")]
        run: Option<String>,
        /// The assignment mark that `kontext next` or `kontext brief` gave you (required for agents
        /// closing a task; optional for a feature)
        #[arg(long, value_name = "MARK")]
        since: Option<String>,
        #[arg(long, default_value = "")]
        why: String,
    },
    /// Stores a fact with the fingerprint of the files it came from: it expires only when that code changes
    Fact {
        text: String,
        /// File or folder it comes from (can be repeated; it has to be in a commit)
        #[arg(long = "path", value_name = "PATH")]
        paths: Vec<String>,
        /// [[repos]] repo that paths are resolved against (default: the one for the current folder)
        #[arg(long)]
        repo: Option<String>,
        /// Epic it belongs to (shown in its brief and in `next`)
        #[arg(long)]
        epic: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long, default_value = "")]
        body: String,
    },
    /// What happened since your last visit and what it expects from you. Advances your cursor
    Standup {
        /// Shows without advancing the cursor
        #[arg(long)]
        peek: bool,
        /// Reader (default: your name: --actor human:<x>, $KONTEXT_ACTOR or git config user.name)
        #[arg(long = "as", value_name = "READER")]
        reader: Option<String>,
    },
    /// The context of a role: charter, goals, decisions, what's open and what's new. Advances the role's cursor
    Resume {
        /// Role: pm, owner…
        #[arg(long = "as", value_name = "ROLE", default_value = "pm")]
        role: String,
        /// Character budget (default: [budgets] in project.toml, or 9000)
        #[arg(long)]
        max_chars: Option<usize>,
        /// Shows without advancing the cursor
        #[arg(long)]
        peek: bool,
    },
    /// A full entry with its history (accepts prefixes: D-7f3)
    Show { id: String },
    /// Searches the docs of the project's repos by topic and reads only the section that matters
    Find {
        /// Topic to search for
        query: Vec<String>,
        /// Prints a section: <path:line> (or <repo:path:line> with several repos)
        #[arg(long, value_name = "PATH:LINE")]
        show: Option<String>,
        /// Current docs that cite paths from the repo that no longer exist
        #[arg(long)]
        check: bool,
        /// Includes historical docs (replaced ADRs, snapshots)
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// With --check: baseline (`<n> <doc>` per line); fails only if a doc has more broken paths
        #[arg(long, value_name = "FILE")]
        baseline: Option<PathBuf>,
        /// With --check and --baseline: writes the baseline with the current state
        #[arg(long)]
        update_baseline: bool,
    },
    /// Proposes decisions, questions, facts and log entries from a Claude Code transcript (with [distill] command)
    Distill {
        /// The transcript (.jsonl); only what's new since the last time is read
        #[arg(long, value_name = "PATH", required_unless_present = "wait")]
        transcript: Option<PathBuf>,
        /// Shows what would be sent (without secrets), without calling the model or moving the offset
        #[arg(long)]
        dry_run: bool,
        /// At most this many chunks (0: all)
        #[arg(long, default_value_t = 0)]
        max_chunks: usize,
        /// Size of each chunk, in characters
        #[arg(long, default_value_t = crate::distill::DEFAULT_CHUNK_CHARS)]
        chunk_chars: usize,
        /// Doesn't distill: waits for the distills that the hooks started to finish
        #[arg(long, conflicts_with_all = ["transcript", "dry_run"])]
        wait: bool,
        /// With --wait, how long to wait at most (600s)
        #[arg(long, value_name = "DURATION", requires = "wait")]
        timeout: Option<String>,
    },
    /// Technical changelog: tasks closed per epic (with their evidence) and merged PRs (gh). Always comes out the same
    Changelog {
        /// From: YYYY-MM-DD, ISO-8601 or a git ref (its commit date)
        #[arg(long)]
        since: Option<String>,
        /// Until (inclusive): YYYY-MM-DD, ISO-8601 or a git ref
        #[arg(long)]
        until: Option<String>,
        /// md or json (--json is the same as --format json)
        #[arg(long, default_value = "md", value_parser = ["md", "json"])]
        format: String,
    },
    /// Searches the ledger and the Claude Code transcripts
    Recall {
        /// Words to search for (all of them have to match)
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Only from this date (YYYY-MM-DD or ISO-8601)
        #[arg(long)]
        since: Option<String>,
    },
    /// Local read-only page: the ledger, the cursors, the alerts and the dashboards
    Ui {
        /// Port (0: a free one)
        #[arg(long, default_value_t = crate::ui::DEFAULT_PORT)]
        port: u16,
        /// Address to listen on. Default 127.0.0.1; any other address that isn't loopback leaves the ledger open to the network
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Opens the page in the browser
        #[arg(long)]
        open: bool,
        /// Who is looking (for "Signed by you" and "For you"; default: as in standup)
        #[arg(long = "as", value_name = "READER")]
        reader: Option<String>,
    },
    /// Entry point for the Claude Code hooks (reads the hook's JSON from stdin; always exits with 0)
    Hook { event: String },
    /// Prepares the current repo: .kontext.toml, start hook, skill, agent files and the project's [[repos]]
    Install {
        /// Shows what would change, without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Also replaces the agent files you changed by hand
        #[arg(long)]
        force: bool,
        /// Only the skills (kontext, kontext-orchestrate, kontext-show, kontext-migrate and
        /// kontext-pm), in the repo's .claude/skills/, without the agent files; no project needed
        #[arg(long)]
        skills_only: bool,
        /// With --skills-only: in your user's skills (~/.claude/skills/, or $CLAUDE_CONFIG_DIR/skills/)
        /// instead of the repo's
        #[arg(long, requires = "skills_only")]
        global: bool,
        /// Only .kontext.toml, in this checkout: nothing under .claude/, no skills, no agent
        /// files, no [[repos]] change. In a linked git worktree, with --role pm (the default), it
        /// marks the worktree as the PM's (an untracked file)
        #[arg(long, conflicts_with_all = ["skills_only", "force", "local"])]
        link_only: bool,
        /// The hooks and the Bash(kontext:*) permission go in .claude/settings.local.json instead
        /// of .claude/settings.json, and kontext's own entries leave settings.json. Run it in the
        /// main checkout: Claude Code reads that file from the main checkout's root in every
        /// worktree
        #[arg(long, conflicts_with = "skills_only")]
        local: bool,
        /// Sets worktree_role in .kontext.toml: the role of sessions in linked git worktrees that
        /// have no .kontext.toml of their own
        #[arg(long, value_name = "ROLE", conflicts_with = "skills_only")]
        worktree_role: Option<String>,
    },
    /// Checks this installation: the binary, PATH, config and project, kontext-home, hooks,
    /// capture, skills and the hook log. Writes nothing; exits with 1 if anything is a problem
    Doctor,
    /// Numbers about Claude Code conversations from their transcripts: requests, tokens, what
    /// fills the history, tools and kontext commands. Never prints their text; needs no project
    Measure {
        /// Transcript files (.jsonl)
        #[arg(required = true, value_name = "TRANSCRIPT")]
        transcripts: Vec<PathBuf>,
        /// Only lines from this moment on (RFC 3339, inclusive)
        #[arg(long, value_name = "RFC3339")]
        since: Option<String>,
        /// Only lines up to this moment (RFC 3339, inclusive)
        #[arg(long, value_name = "RFC3339")]
        until: Option<String>,
    },
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        cmd: DebugCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum DebugCmd {
    /// Adds raw entries: one JSON object per line on stdin, each one with
    /// its own lock. Prints the id of each one.
    Append {
        /// Waits until this instant (ms since 1970) before starting
        #[arg(long)]
        start_at: Option<i64>,
    },
    /// Claims a task only if nobody holds it (exits with 2 if it already has an owner)
    Claim {
        task: String,
        #[arg(long)]
        start_at: Option<i64>,
    },
    /// Prints the derived state as JSON
    State,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            if let Some(r) = e.downcast_ref::<Refused>() {
                eprintln!("kontext: refused: {r}");
                return ExitCode::from(2);
            }
            if let Some(r) = e.downcast_ref::<Stale>() {
                eprintln!("kontext: refused: {r}");
                return ExitCode::from(3);
            }
            eprintln!("kontext: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// `kontext install --skills-only [--global]`: only the skills, in the current
/// repo or in your user's. It needs no project: it's how you get the
/// `kontext-migrate` skill before creating one.
fn install_skills(dry_run: bool, global: bool) -> Result<ExitCode> {
    let dir = if global {
        match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d).join("skills"),
            None => config::home_dir()?.join(".claude").join("skills"),
        }
    } else {
        install::repo_root(&std::env::current_dir()?)
            .join(".claude")
            .join("skills")
    };
    let plan = install::plan_skills(&dir)?;
    if plan.is_noop() {
        println!(
            "The skills are already up to date in {}; nothing to change.",
            dir.display()
        );
        return Ok(ExitCode::SUCCESS);
    }
    for c in plan.changes.iter().filter(|c| c.changed()) {
        let what = if c.before.is_some() { "changed" } else { "new" };
        println!("{} ({what})", c.path.display());
    }
    if dry_run {
        println!("Dry run (--dry-run): nothing was written.");
        return Ok(ExitCode::SUCCESS);
    }
    for p in plan.apply()? {
        println!("Wrote {}", p.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// Why the capture writes nothing for a session in `dir` with these settings
/// (the hook's own rules: [`config::role_in`] and [`crate::capture::target`]),
/// or none if it captures there.
fn capture_blocked_here(dir: &Path, settings: &crate::capture::Settings) -> Vec<String> {
    use crate::capture::Target;
    let mut why = Vec::new();
    let (role, from) = config::role_in(dir);
    if role != "pm" {
        why.push(format!(
            "sessions here have role {role} ({from}), and only pm conversations are captured"
        ));
    }
    match crate::capture::target(dir, settings.worktrees) {
        Target::NotGit => why.push("this folder is not in a git repo".into()),
        Target::Unmarked { fix, .. } => why.push(format!(
            "this is a linked git worktree not marked as the PM's (if the PM works here, {fix})"
        )),
        Target::NoMain => why.push(
            "this is a worktree of a bare repository, which has no main checkout's .pm/".into(),
        ),
        Target::Main(_) | Target::OwnWorktree(_) | Target::Marked(_) => {}
    }
    why
}

/// `kontext capture here|list|forget`: the conversations named for the
/// capture hook, in the kontext-home's `.kontext/` (local). It works with the
/// capture off, so a conversation can be named before turning it on.
fn capture_cmd(project: Option<&str>, json_out: bool, cmd: CaptureCmd) -> Result<ExitCode> {
    use crate::capture;
    let p = config::resolve_project(project)?;
    let settings = capture::Settings::of(&Project::load(&p.home)?);
    let file = capture::named_path(&p.home);
    let current = capture::session_from_env();
    let cwd = std::env::current_dir()?;
    let mut lines: Vec<String> = Vec::new();
    let mut out = json!({
        "project": p.name,
        "home": p.home,
        "file": file,
        "capture_enabled": settings.enabled,
        "current": current,
    });
    let list = match cmd {
        CaptureCmd::Here { session } => {
            let (id, source) = match session.map(|s| s.trim().to_string()) {
                Some(s) if !s.is_empty() => (s, "--session"),
                Some(_) => bail!("--session needs a session id"),
                None => match &current {
                    Some(s) => (s.clone(), "$CLAUDE_CODE_SESSION_ID"),
                    None => bail!(
                        "{} is not set, so this is not a Claude Code session (Claude Code sets \
                         it in Bash tool and hook subprocesses): pass the conversation's id \
                         with --session <id> (the session= of its lines in {})",
                        capture::SESSION_ENV,
                        config::internal_dir(&p.home).join("hooks.log").display()
                    ),
                },
            };
            let (added, list) = capture::name(&p.home, &id, &cwd)?;
            lines.push(if added {
                format!("Named conversation {id} (from {source}) for the capture.")
            } else {
                format!("Conversation {id} (from {source}) was already named.")
            });
            let why = capture_blocked_here(&cwd, &settings);
            for w in &why {
                lines.push(format!("Nothing is captured from it here yet: {w}."));
            }
            out["session"] = json!(id);
            out["added"] = json!(added);
            out["not_captured_here"] = json!(why);
            list
        }
        CaptureCmd::List => capture::named(&p.home)?,
        CaptureCmd::Forget { id, all } => {
            let (gone, list) = capture::forget(&p.home, if all { None } else { id.as_deref() })?;
            if let (Some(id), false) = (&id, all)
                && gone.is_empty()
            {
                return Err(Refused(format!(
                    "{id} is not a named conversation (`kontext capture list` shows them)"
                ))
                .into());
            }
            lines.push(match gone.len() {
                0 => "No conversation was named; nothing to forget.".to_string(),
                1 => format!("Forgot {}.", gone[0]),
                n => format!("Forgot {n} named conversations."),
            });
            out["forgotten"] = json!(gone);
            list
        }
    };
    out["named"] = json!(list);
    if json_out {
        print_json(&out)?;
        return Ok(ExitCode::SUCCESS);
    }
    lines.push(format!(
        "Project {}, kontext-home {}; the list is {} (this machine only).",
        p.name,
        p.home.display(),
        file.display()
    ));
    if list.is_empty() {
        lines.push(
            "No conversation is named: every pm conversation in a capturing checkout is captured."
                .into(),
        );
    } else {
        lines.push(format!(
            "Named ({}): only these conversations are captured.",
            list.len()
        ));
        for n in &list {
            let this = if current.as_deref() == Some(n.id.as_str()) {
                "  (this conversation)"
            } else {
                ""
            };
            lines.push(format!("  {}  named {} in {}{this}", n.id, n.at, n.dir));
        }
        lines.push(format!(
            "A conversation's id changes on /clear: run `{}` in it again.",
            capture::NAME_COMMAND
        ));
    }
    if !settings.enabled {
        lines.push(
            "Capture is off ([capture] enabled is not true in project.toml): nothing is captured \
             until it is on."
                .into(),
        );
    }
    for l in lines {
        println!("{l}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The options of `kontext install` (besides `--skills-only`).
struct InstallHow<'a> {
    dry_run: bool,
    force: bool,
    link_only: bool,
    local: bool,
    worktree_role: Option<&'a str>,
}

/// The path of this binary, quoted if needed (for the hooks).
fn exe_quoted() -> Result<String> {
    let exe = std::env::current_exe().context("could not find the path of kontext")?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    Ok(install::shell_quote(&exe.display().to_string()))
}

/// For `kontext install`: if this binary is in a cargo build directory, the
/// hooks record a path that `cargo clean` deletes. A note, never a refusal.
fn build_dir_note() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    let checkout = install::cargo_build_checkout(&exe)?;
    Some(format!(
        "Note: this kontext is {}, in a cargo build directory: the hooks point at a file that \
         `cargo clean` deletes. Install it with `cargo install --path {} --locked` and run \
         `kontext install` again with the installed binary. `kontext doctor` checks it.",
        exe.display(),
        checkout.display()
    ))
}

fn open(project: Option<&str>) -> Result<Store> {
    let p = config::resolve_project(project)?;
    let mut store = Store::open(&p.home, &p.writer)?;
    store.set_signer(signer(false)?);
    Ok(store)
}

/// The key to sign with on this machine: `[signing] key` from projects.toml,
/// or with `--sign` and no key configured, `~/.ssh/id_ed25519.pub`.
fn signer(forced: bool) -> Result<Option<crate::sign::Signer>> {
    let key = match Machine::load()?.signing_key()? {
        Some(k) => Some(k),
        None if forced => Some(config::home_dir()?.join(".ssh").join("id_ed25519.pub")),
        None => None,
    };
    Ok(key.map(|key| crate::sign::Signer { key }))
}

pub fn run(cli: Cli) -> Result<ExitCode> {
    let project = cli.project.as_deref();
    let cx = Ctx {
        project,
        actor: cli.actor.as_deref(),
        role: cli.role.as_deref(),
        model: cli.model.as_deref(),
        json: cli.json,
        sign: cli.sign,
    };
    match cli.cmd {
        // Neither opens the project: `doctor` diagnoses a broken setup, and
        // `measure` needs no kontext configuration at all.
        Cmd::Doctor => {
            let r = crate::doctor::run(project);
            if cx.json {
                print_json(&r.json())?;
            } else {
                print!("{}", r.text());
            }
            Ok(if r.problems() > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Cmd::Measure {
            transcripts,
            since,
            until,
        } => {
            let window = crate::measure::Window::parse(since.as_deref(), until.as_deref())?;
            let m = crate::measure::measure(&transcripts, window)?;
            if cx.json {
                print_json(&m.json())?;
            } else {
                print!("{}", m.text());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Hook { event } => {
            // By hand, in a terminal, there is no JSON on stdin: don't wait for it.
            use std::io::IsTerminal;
            let mut empty = std::io::empty();
            let mut stdin = std::io::stdin();
            let input: &mut dyn std::io::Read = if stdin.is_terminal() {
                &mut empty
            } else {
                &mut stdin
            };
            hook::run(&event, input, &mut std::io::stdout());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Charter {
            objective,
            non_goals,
            done_when,
            title,
        } => cx.written("the charter", |s, a| {
            write::charter(
                s,
                a,
                &objective,
                &clean_list(&non_goals),
                done_when.as_deref(),
                title.as_deref(),
            )
        }),
        Cmd::Goal {
            cmd: GoalCmd::Add { title, done_when },
        } => cx.written("the goal", |s, a| {
            write::goal_add(s, a, &title, done_when.as_deref())
        }),
        Cmd::Sign { id, approved_by } => cx.written("the signature", |s, a| {
            write::sign(s, a, &id, approved_by.as_deref())
        }),
        Cmd::Decide { d, approved_by } => cx.written("the decision", |s, a| {
            let mut args = d.args();
            // Without permission, `decide` refuses: don't store the attachment for nothing.
            if s.policy().check(a, approved_by.as_deref()).is_ok() {
                args.attachments = store_media(s, &d.attach, &d.screen, true)?;
            }
            write::decide(s, a, &args, approved_by.as_deref())
        }),
        Cmd::Propose { d } => cx.written("the proposal", |s, a| {
            let mut args = d.args();
            args.attachments = store_media(s, &d.attach, &d.screen, true)?;
            write::propose(s, a, &args)
        }),
        Cmd::Accept {
            id,
            as_decision,
            approved_by,
            why,
        } => cx.written("the acceptance", |s, a| {
            write::accept(s, a, &id, as_decision, approved_by.as_deref(), &why)
        }),
        Cmd::Reject {
            id,
            why,
            approved_by,
        } => cx.written("the rejection", |s, a| {
            write::reject(s, a, &id, &why, approved_by.as_deref())
        }),
        Cmd::Supersede {
            old,
            new,
            why,
            approved_by,
        } => cx.written("the replacement", |s, a| {
            write::supersede(s, a, &old, &new, &why, approved_by.as_deref())
        }),
        Cmd::Ask {
            title,
            to,
            task,
            feature,
            body,
            epic,
            blocking,
            no_merge,
        } => cx.ask(write::AskArgs {
            title,
            to,
            task,
            feature,
            body,
            epic,
            blocking,
            no_merge,
            max_pending: 0,
        }),
        Cmd::Epic {
            cmd:
                EpicCmd::Add {
                    title,
                    goals,
                    coordinator,
                    name,
                    after,
                },
        } => cx.written_warn("the epic", |s, a| {
            let w = tasks::epic_add(
                s,
                a,
                &title,
                &goals,
                coordinator.as_deref(),
                name.as_deref(),
                &after,
            )?;
            Ok((w.entries, w.warnings))
        }),
        Cmd::Feature {
            cmd:
                FeatureCmd::Add {
                    title,
                    epic,
                    acceptance,
                    goals,
                    body,
                    tags,
                },
        } => {
            let args = crate::features::FeatureArgs {
                title,
                epic,
                acceptance,
                goals,
                body,
                tags: clean_list(&tags),
            };
            cx.written_warn("the feature", |s, a| {
                let w = crate::features::add(s, a, &args)?;
                Ok((w.entries, w.warnings))
            })
        }
        Cmd::Task {
            cmd:
                TaskCmd::Add {
                    title,
                    epic,
                    feature,
                    goals,
                    depends_on,
                    acceptance,
                    body,
                    tags,
                },
        } => {
            let args = TaskArgs {
                title,
                epic,
                feature,
                goals,
                depends_on,
                acceptance,
                body,
                tags: clean_list(&tags),
            };
            cx.written_warn("the task", |s, a| {
                let w = tasks::task_add(s, a, &args)?;
                Ok((w.entries, w.warnings))
            })
        }
        Cmd::Assign { task, to } => {
            cx.written("the assignment", |s, a| tasks::assign(s, a, &task, &to))
        }
        Cmd::Release { task, why } => {
            cx.written("the release", |s, a| tasks::release(s, a, &task, &why))
        }
        Cmd::Reopen { task, why } => {
            cx.written("the reopening", |s, a| tasks::reopen(s, a, &task, &why))
        }
        Cmd::Next {
            epic,
            claim,
            max_chars,
        } => cx.next(&epic, claim, max_chars),
        Cmd::Brief {
            epic,
            max_chars,
            peek,
        } => cx.brief(&epic, max_chars, peek),
        Cmd::Plan => {
            let (mut store, _) = cx.open()?;
            let st = store.state()?;
            let plan = crate::plan::of_state(&st);
            if cx.json {
                print_json(&plan.json())?;
            } else {
                print!("{}", plan.text());
            }
            Ok(if plan.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Cmd::Done {
            task,
            evidence,
            screen,
            evidence_json,
            run,
            since,
            why,
        } => cx.done(
            task,
            &evidence,
            &screen,
            &evidence_json,
            run.as_deref(),
            since.as_deref(),
            why,
        ),
        Cmd::Fact {
            text,
            paths,
            repo,
            epic,
            tags,
            body,
        } => cx.fact(facts::FactArgs {
            text,
            body,
            epic,
            tags: clean_list(&tags),
            paths,
            repo,
        }),
        Cmd::Answer { id, text } => {
            cx.written("the answer", |s, a| write::answer(s, a, &id, &text))
        }
        Cmd::Log {
            text,
            next,
            task,
            tags,
            status,
        } => cx.written(
            if status {
                "the status paragraph"
            } else {
                "the log entry"
            },
            |s, a| {
                write::log(
                    s,
                    a,
                    &text,
                    next.as_deref(),
                    task.as_deref(),
                    &clean_list(&tags),
                    status,
                )
            },
        ),
        Cmd::Inbox {
            cmd,
            all,
            reader,
            everyone,
        } => {
            if everyone {
                cx.inbox_everyone(cmd, all, reader)
            } else {
                cx.inbox(cmd, all, reader)
            }
        }
        Cmd::Report {
            html,
            output,
            reader,
        } => cx.report(html, output, reader),
        Cmd::Ideas { mark_audited } => cx.ideas(mark_audited),
        Cmd::Capture { cmd } => capture_cmd(project, cx.json, cmd),
        Cmd::Standup { peek, reader } => cx.standup(peek, reader),
        Cmd::Resume {
            role,
            max_chars,
            peek,
        } => cx.resume(&role, max_chars, peek),
        Cmd::Show { id } => cx.show(&id),
        Cmd::Find {
            query,
            show,
            check,
            all,
            limit,
            baseline,
            update_baseline,
        } => cx.find(FindArgs {
            query: query.join(" "),
            show,
            check,
            all,
            limit,
            baseline,
            update_baseline,
        }),
        Cmd::Distill {
            transcript,
            dry_run,
            max_chunks,
            chunk_chars,
            wait,
            timeout,
        } => {
            // Launched by a hook: on exit, good or bad, its marker is removed.
            let _pending = distill::PendingGuard::from_env();
            let (mut store, project) = cx.open()?;
            if wait {
                let ms = match timeout.as_deref() {
                    Some(t) => crate::tasks::parse_duration_ms(t)
                        .map_err(|e| anyhow::anyhow!("--timeout: {e}"))?,
                    None => 600_000,
                };
                let r = distill::wait(
                    store.home(),
                    std::time::Duration::from_millis(ms.max(0) as u64),
                )?;
                if cx.json {
                    print_json(&json!({
                        "waited_ms": r.waited.as_millis() as u64,
                        "stale": r.stale,
                    }))?;
                } else {
                    println!(
                        "No distill is pending anymore (waited {:.1} s{}).",
                        r.waited.as_secs_f64(),
                        if r.stale > 0 {
                            format!("; removed {} markers from distills that died", r.stale)
                        } else {
                            String::new()
                        }
                    );
                }
                return Ok(ExitCode::SUCCESS);
            }
            let Some(transcript) = transcript else {
                anyhow::bail!("missing --transcript");
            };
            let opts = distill::Options {
                transcript,
                dry_run,
                max_chunks,
                chunk_chars,
            };
            // With --json, progress goes to stderr and stdout gets only the JSON.
            let mut out: Box<dyn Write> = if cx.json {
                Box::new(std::io::stderr())
            } else {
                Box::new(std::io::stdout())
            };
            match distill::distill(&mut store, &project, &opts, &mut out) {
                Ok(r) => {
                    if cx.json {
                        print_json(&json!({
                            "chunks": r.chunks,
                            "added": r.added.iter().map(|e| &e.id).collect::<Vec<_>>(),
                            "redactions": r.redactions,
                            "disabled": r.disabled,
                        }))?;
                    }
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    distill::log_line(store.home(), &format!("the distill failed: {e:#}"));
                    Err(e)
                }
            }
        }
        Cmd::Changelog {
            since,
            until,
            format,
        } => {
            let (mut store, project) = cx.open()?;
            let cwd = std::env::current_dir()?;
            let repos = Repos::load(&project, &cwd)?;
            let git_repo = repos
                .here()
                .map(|h| h.root.clone())
                .or_else(|| repos.available().into_iter().next().map(|(_, p)| p));
            let since = since
                .map(|s| changelog::parse_bound(&s, git_repo.as_deref(), false))
                .transpose()?;
            let until = until
                .map(|s| changelog::parse_bound(&s, git_repo.as_deref(), true))
                .transpose()?;
            let st = store.state()?;
            let c = changelog::build(&st, &project, since, until);
            if cx.json || format == "json" {
                print_json(&c.json())?;
            } else {
                print!("{}", c.markdown());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Recall { text, limit, since } => cx.recall(&text.join(" "), limit, since.as_deref()),
        // `--role` (global) is the default_role of the .kontext.toml.
        Cmd::Install {
            skills_only: true,
            dry_run,
            global,
            ..
        } => install_skills(dry_run, global),
        Cmd::Install {
            dry_run,
            force,
            link_only,
            local,
            worktree_role,
            ..
        } => cx.install(
            InstallHow {
                dry_run,
                force,
                link_only,
                local,
                worktree_role: worktree_role.as_deref(),
            },
            cx.role.unwrap_or("pm"),
        ),
        Cmd::Init { name, home, remote } => init(&name, home, remote.as_deref()),
        Cmd::Clone { url, home, name } => {
            let c = crate::sync::clone(&url, home, name.as_deref())?;
            if cx.json {
                print_json(&c)?;
            } else {
                print!("{}", c.text());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Sync { include_config } => {
            let (mut store, _) = cx.open()?;
            let r = crate::sync::sync(&mut store, include_config)?;
            if cx.json {
                print_json(&r)?;
            } else {
                print!("{}", r.text());
            }
            Ok(if r.ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Cmd::Ui {
            port,
            host,
            open,
            reader,
        } => {
            let p = config::resolve_project(project)?;
            let proj = Project::load(&p.home)?;
            let you = match reader
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty())
            {
                Some(r) => r,
                None => crate::ui::default_viewer(cx.actor, &proj, &p.home),
            };
            let src = crate::ui::Source {
                home: p.home,
                writer: p.writer,
                you,
                cwd: std::env::current_dir().ok(),
            };
            crate::ui::serve(src, &crate::ui::Options { host, port, open })?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Status => {
            let (mut store, project) = cx.open()?;
            let s = crate::sync::status(&mut store, &project)?;
            if cx.json {
                print_json(&s)?;
            } else {
                print!("{}", s.text());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Verify => {
            let p = config::resolve_project(project)?;
            let report = verify::run_with_media(&p.home)?;
            let mut store = Store::open(&p.home, &p.writer)?;
            let sigs = store.check_signatures()?;
            let bad: Vec<_> = sigs.iter().filter(|(_, v)| !v.is_valid()).collect();
            for pr in report
                .problems
                .iter()
                .chain(&report.media_problems)
                .chain(&report.newer)
            {
                println!("{pr}");
            }
            for (e, v) in &bad {
                if let crate::sign::Verdict::Invalid(reason) = v {
                    println!(
                        "ledger/{}.jsonl:{}: invalid signature on {} ({}): {reason}",
                        e.writer, e.n, e.id, e.kind
                    );
                }
            }
            println!("{}", report.summary());
            if !sigs.is_empty() {
                println!(
                    "Signatures: {} valid, {} invalid or missing",
                    sigs.len() - bad.len(),
                    bad.len()
                );
            }
            Ok(if report.ok() && bad.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Cmd::Reindex => {
            let mut store = open(project)?;
            store.index().rebuild()?;
            let n = store.index().count()?;
            println!("Index rebuilt: {n} entries");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Import { from_v0, force } => {
            let mut store = open(project)?;
            let r = import_v0::import(&mut store, &from_v0, force)?;
            if let Some(p) = &r.chain_problem {
                eprintln!(
                    "kontext: warning: the v0 chain is broken ({p}); imported because of --force"
                );
            }
            let kinds: Vec<String> = r
                .imported
                .iter()
                .map(|(k, n)| format!("{k}: {n}"))
                .collect();
            println!(
                "Imported {} of {} entries from {} into the file of {}{}",
                r.imported_total(),
                r.total,
                from_v0.display(),
                store.writer(),
                if kinds.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", kinds.join(", "))
                }
            );
            if r.already > 0 {
                println!("Already imported: {}", r.already);
            }
            if !r.unresolved.is_empty() {
                println!(
                    "v0 references that could not be translated: {}",
                    r.unresolved.join(", ")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Debug { cmd } => debug(project, cmd),
    }
}

/// What `kontext find` asks for.
struct FindArgs {
    query: String,
    show: Option<String>,
    check: bool,
    all: bool,
    limit: usize,
    baseline: Option<PathBuf>,
    update_baseline: bool,
}

/// The global options.
struct Ctx<'a> {
    project: Option<&'a str>,
    actor: Option<&'a str>,
    role: Option<&'a str>,
    model: Option<&'a str>,
    json: bool,
    sign: bool,
}

fn print_json(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

impl Ctx<'_> {
    /// Who writes: `--actor`, `--role` and `--model`, or the variables.
    fn who(&self) -> Result<Actor> {
        Actor::resolve_with(self.actor, self.role, self.model)
    }

    fn open(&self) -> Result<(Store, Project)> {
        let p = config::resolve_project(self.project)?;
        let project = Project::load(&p.home)?;
        let mut store = Store::open(&p.home, &p.writer)?;
        store.set_signer(signer(self.sign)?);
        Ok((store, project))
    }

    /// Runs a command that writes, and prints what was written.
    fn written(
        &self,
        what: &str,
        f: impl FnOnce(&mut Store, &Actor) -> Result<Vec<Entry>>,
    ) -> Result<ExitCode> {
        let actor = self.who()?;
        let (mut store, project) = self.open()?;
        if actor.is_human()
            && let Some(w) = config::identity_warning(&project, store.home(), &actor.name)
        {
            eprintln!("kontext: warning: {w}");
        }
        let written = f(&mut store, &actor)?;
        let Some(main) = written.last() else {
            bail!("nothing was written");
        };
        // In `accept --as-decision`, the main entry is the new decision.
        let main = written
            .iter()
            .find(|e| e.kind == "decision")
            .unwrap_or(main);
        if self.json {
            print_json(main)?;
            return Ok(ExitCode::SUCCESS);
        }
        let mut line = format!("{}: saved {what}", main.id);
        if actor.kind == "agent" && actor.name == "unknown" {
            line.push_str(" (as agent:unknown; set KONTEXT_ACTOR or use --actor)");
        }
        println!("{line}");
        for e in written.iter().filter(|e| e.id != main.id) {
            println!("{}: also saved ({})", e.id, views::kind_label(&e.kind));
        }
        match main.kind.as_str() {
            "charter" | "goal" => {
                println!("An owner still needs to sign it: kontext sign {}", main.id)
            }
            "proposal" => println!("Waiting for an owner to review it"),
            _ => {}
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Like [`Ctx::written`], with warnings (drift) afterwards.
    fn written_warn(
        &self,
        what: &str,
        f: impl FnOnce(&mut Store, &Actor) -> Result<(Vec<Entry>, Vec<String>)>,
    ) -> Result<ExitCode> {
        let mut warnings = Vec::new();
        let code = self.written(what, |s, a| {
            let (e, w) = f(s, a)?;
            warnings = w;
            Ok(e)
        })?;
        for w in warnings {
            if self.json {
                eprintln!("kontext: warning: {w}");
            } else {
                println!("Warning: {w}");
            }
        }
        Ok(code)
    }

    fn next(&self, epic: &str, claim: bool, max_chars: Option<usize>) -> Result<ExitCode> {
        let actor = self.who()?;
        let (mut store, project) = self.open()?;
        let n = app::next(&mut store, &project, &actor, epic, claim, max_chars)?;
        if self.json {
            print_json(&n.json)?;
        } else {
            print!("{}", n.text);
        }
        Ok(ExitCode::SUCCESS)
    }

    fn brief(&self, epic: &str, max_chars: Option<usize>, peek: bool) -> Result<ExitCode> {
        let (mut store, project) = self.open()?;
        let r = app::brief(&mut store, &project, epic, max_chars)?;
        if self.json {
            print_json(&r.view.json)?;
        } else {
            print!("{}", r.view.text);
        }
        if !peek {
            store.advance(&r.view.reader, &r.frontier)?;
        }
        Ok(ExitCode::SUCCESS)
    }

    #[allow(clippy::too_many_arguments)]
    fn done(
        &self,
        task: String,
        specs: &[String],
        screens: &[String],
        json_specs: &[String],
        run: Option<&str>,
        since: Option<&str>,
        why: String,
    ) -> Result<ExitCode> {
        let actor = self.who()?;
        let (mut store, _) = self.open()?;
        let mut evidence = Vec::new();
        for s in specs {
            evidence.push(tasks::parse_evidence(s)?);
        }
        for s in json_specs {
            evidence.extend(tasks::parse_evidence_json(s)?);
        }
        tasks::check_file_evidence(&evidence, &std::env::current_dir()?)?;
        let since = since
            .map(|s| {
                s.trim()
                    .trim_matches('`')
                    .parse::<crate::cursor::Frontier>()
            })
            .transpose()?;
        let mut args = DoneArgs {
            task,
            evidence,
            since,
            why,
        };
        let has_images = args
            .evidence
            .iter()
            .any(|e| e.get("type").and_then(serde_json::Value::as_str) == Some("image"));
        if has_images {
            // Before copying the screenshots: could it be closed?
            tasks::check_done(&store.state()?, &actor, &args)?;
            let paths: Vec<String> = args
                .evidence
                .iter()
                .filter_map(|e| e.get("path").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect();
            let stored = store_media(&store, &paths, screens, false)?;
            let mut it = stored.into_iter();
            for e in args.evidence.iter_mut() {
                if e.get("path").is_some()
                    && e.get("type").and_then(serde_json::Value::as_str) == Some("image")
                {
                    *e = it.next().expect("one per image");
                }
            }
        } else if !screens.is_empty() {
            bail!("--screen goes with a screenshot in --evidence (screenshot.png)");
        }
        if let Some(cmd) = run {
            // Before running the command: could it be closed?
            tasks::check_done(&store.state()?, &actor, &args)?;
            eprintln!("kontext: running {cmd:?}…");
            let r = tasks::run_command(cmd)?;
            if r.exit_code != 0 {
                return Err(Refused(format!(
                    "`{cmd}` exited with {}; the task stays open. Last lines:\n{}",
                    r.exit_code, r.excerpt
                ))
                .into());
            }
            args.evidence.insert(0, r.evidence());
        }
        let task_id = std::cell::RefCell::new(String::new());
        let code = self.written("the closing", |s, a| {
            let w = tasks::done(s, a, &args)?;
            if let Some(d) = w.first() {
                *task_id.borrow_mut() = d.refs[0].clone();
            }
            Ok(w)
        })?;
        if !self.json {
            let st = store.state()?;
            let id = task_id.into_inner();
            if st.kind_of(&id) == Some("feature") {
                println!("{id} validated.");
                return Ok(code);
            }
            let unblocked: Vec<String> = st
                .of_kind("task")
                .filter(|t| {
                    crate::state::string_list(t.fields.get("depends_on")).contains(&id)
                        && st.task_status(&t.id) == Some(tasks::TaskStatus::Ready)
                })
                .map(|t| format!("{} {}", t.id, views::one_line(&t.title, 80)))
                .collect();
            println!("{id} closed.");
            if !unblocked.is_empty() {
                println!("Now ready: {}", unblocked.join("; "));
            }
        }
        Ok(code)
    }

    /// `kontext ask`: the new question, or the one it was added to; and the brake warning.
    fn ask(&self, mut a: write::AskArgs) -> Result<ExitCode> {
        let actor = self.who()?;
        let (mut store, project) = self.open()?;
        a.max_pending = project.max_pending();
        match write::ask(&mut store, &actor, &a)? {
            write::Asked::Joined { question, record } => {
                if self.json {
                    print_json(&record)?;
                } else {
                    let st = store.state()?;
                    println!(
                        "{} already asks the same thing («{}», from {}, {}): your question was added to it ({}) instead of being repeated.",
                        question.id,
                        views::one_line(&question.title, 100),
                        question.actor.short(),
                        tasks::ago(st.now_ms, tasks::hlc_ms(&question.hlc)),
                        record.id
                    );
                    println!(
                        "You'll see the answer with `kontext show {}`. Meanwhile, keep going with what doesn't depend on it. \
                         If it really is a different question: `kontext ask … --no-merge`.",
                        question.id
                    );
                }
            }
            write::Asked::New { entries, brake } => {
                let q = entries.last().context("nothing was written")?;
                if self.json {
                    print_json(q)?;
                } else {
                    let mut line = format!("{}: saved the question", q.id);
                    if actor.kind == "agent" && actor.name == "unknown" {
                        line.push_str(" (as agent:unknown; set KONTEXT_ACTOR or use --actor)");
                    }
                    println!("{line}");
                }
                if let Some(n) = brake {
                    let msg = format!(
                        "the human has {n} unread pending items; does this block you? Your question was marked as not urgent. \
                         If it really stops your work, ask again with --blocking (it joins {}). \
                         Meanwhile, move on with what doesn't depend on them, and don't pile more questions on them",
                        q.id
                    );
                    if self.json {
                        eprintln!("kontext: warning: {msg}");
                    } else {
                        println!("Warning: {msg}.");
                    }
                }
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    /// `kontext inbox --everyone`: read-only, like `kontext inbox`.
    fn inbox_everyone(
        &self,
        cmd: Option<InboxCmd>,
        all: bool,
        reader: Option<String>,
    ) -> Result<ExitCode> {
        use crate::inbox;
        if reader.is_some() {
            bail!(
                "--everyone and --as don't go together: --everyone already shows each human's inbox; \
                 for one person's, use `kontext inbox --as <human>`"
            );
        }
        let count = match cmd {
            None => false,
            Some(InboxCmd::Count) => true,
            Some(_) => bail!(
                "--everyone is only for viewing (`kontext inbox --everyone` or `kontext inbox count \
                 --everyone`): to sign, answer, dismiss or confirm, use your `kontext inbox` without --everyone"
            ),
        };
        let (mut store, project) = self.open()?;
        let st = app::state(&mut store, &project)?;
        let people: Vec<String> = project.people.keys().cloned().collect();
        let ev = inbox::everyone(&st, &project.name, &people);
        match (count, self.json) {
            (false, false) => print!("{}", inbox::everyone_text(&ev, all)),
            (false, true) => print_json(&inbox::everyone_json(&ev, all))?,
            (true, false) => print!("{}", inbox::everyone_count(&ev)),
            (true, true) => print_json(&inbox::everyone_count_json(&ev))?,
        }
        Ok(ExitCode::SUCCESS)
    }

    /// `kontext inbox` and its actions.
    fn inbox(&self, cmd: Option<InboxCmd>, all: bool, reader: Option<String>) -> Result<ExitCode> {
        use crate::inbox::{self, Class};
        let explicit = reader.as_ref().is_some_and(|r| !r.trim().is_empty());
        let human = self.reader(reader)?;
        let (mut store, project) = self.open()?;
        let st = app::state(&mut store, &project)?;
        // An agent that looks at the inbox looks at the same human that the start
        // hook names (the first owner). Otherwise the hook said "has 9 pending items",
        // `kontext inbox` answered that there were none, and the agent ran it again.
        let human = match inbox::default_human(&st) {
            Some(h) if !explicit && !self.who()?.is_human() => h,
            _ => human,
        };
        let ib = inbox::build(&st, &project.name, &human);
        let wrong = |it: &inbox::Item, what: &str| -> anyhow::Error {
            Refused(format!(
                "pending item {} is in the \"{}\" group, not \"{what}\": {}",
                it.n,
                it.class.label().to_lowercase(),
                it.command
            ))
            .into()
        };
        let code = match cmd {
            None => {
                if self.json {
                    print_json(&inbox::json(&ib, all))?;
                } else {
                    print!("{}", inbox::text(&ib, all));
                }
                return Ok(ExitCode::SUCCESS);
            }
            Some(InboxCmd::Count) => {
                if self.json {
                    print_json(&json!({
                        "count": ib.items.len(),
                        "new": ib.new_count(),
                        "oldest": ib.oldest().map(|o| o.since.clone()),
                        "line": inbox::count_line(&ib),
                    }))?;
                } else {
                    println!("{}", inbox::count_line(&ib));
                }
                return Ok(ExitCode::SUCCESS);
            }
            Some(InboxCmd::Sign { item }) => {
                let (it, _) = inbox::find(&ib, &st, &item)?;
                if it.class != Class::Sign {
                    return Err(wrong(it, "sign"));
                }
                let id = it.key.clone();
                if st.kind_of(&id) == Some("proposal") {
                    self.written("the acceptance", |s, a| {
                        inbox::require_human(a)?;
                        write::accept(s, a, &id, false, None, "")
                    })?
                } else {
                    self.written("the signature", |s, a| {
                        inbox::require_human(a)?;
                        write::sign(s, a, &id, None)
                    })?
                }
            }
            Some(InboxCmd::Answer { item, text }) => {
                let (it, sub) = inbox::find(&ib, &st, &item)?;
                if it.class != Class::Answer {
                    return Err(wrong(it, "answer"));
                }
                let q = match (sub, it.subs.as_slice()) {
                    (Some(x), _) => x.id.clone(),
                    (None, [one]) => one.id.clone(),
                    (None, many) => {
                        let list: Vec<String> = many
                            .iter()
                            .enumerate()
                            .map(|(i, x)| format!("  {}.{} {} {}", it.n, i + 1, x.id, x.text))
                            .collect();
                        return Err(Refused(format!(
                            "pending item {} has {} questions: answer one at a time with \
                             `kontext inbox answer {}.1 \"…\"` (or its Q-id):\n{}",
                            it.n,
                            many.len(),
                            it.n,
                            list.join("\n")
                        ))
                        .into());
                    }
                };
                self.written("the answer", |s, a| {
                    inbox::require_human(a)?;
                    write::answer(s, a, &q, &text)
                })?
            }
            Some(InboxCmd::Dismiss { item, why }) => {
                let (it, sub) = inbox::find(&ib, &st, &item)?;
                let (it, sub) = (it.clone(), sub.cloned());
                self.written("the dismissal", |s, a| {
                    inbox::close(s, a, "dismiss", &it, sub.as_ref(), &why)
                })?
            }
            Some(InboxCmd::Confirm { item }) => {
                let (it, sub) = inbox::find(&ib, &st, &item)?;
                if it.class != Class::Review {
                    return Err(wrong(it, "review"));
                }
                let (it, sub) = (it.clone(), sub.cloned());
                self.written("the confirmation", |s, a| {
                    inbox::close(s, a, "confirm", &it, sub.as_ref(), "")
                })?
            }
        };
        if !self.json {
            let (mut store, project) = self.open()?;
            let st = app::state(&mut store, &project)?;
            let left = inbox::build(&st, &project.name, &human);
            println!(
                "{} See them with `kontext inbox`.",
                inbox::count_line(&left)
            );
        }
        Ok(code)
    }

    /// `kontext report [--html] [-o file]`.
    fn report(
        &self,
        html: bool,
        output: Option<PathBuf>,
        reader: Option<String>,
    ) -> Result<ExitCode> {
        let human = self.reader(reader)?;
        let (mut store, project) = self.open()?;
        let st = app::state(&mut store, &project)?;
        let mut r = crate::report::build(&st, &project, &human);
        // The user's ideas, from `.pm/` as `kontext ideas` finds it; without
        // them, the report has no "Your ideas".
        r.ideas = std::env::current_dir()
            .ok()
            .and_then(|cwd| crate::report::ideas_for(&st, &project, &cwd));
        let text = if html {
            let images = match &output {
                Some(out) => crate::report::Images::file(store.home(), out),
                None => crate::report::Images::stdout(store.home()),
            };
            crate::report::html(&r, &images)?
        } else {
            crate::report::markdown(&r)
        };
        match output {
            Some(out) => {
                fs::write(&out, text.as_bytes())
                    .with_context(|| format!("could not write {}", out.display()))?;
                println!(
                    "Report written to {} ({} pending items, {} problems, {} screenshots).",
                    out.display(),
                    r.inbox.items.len(),
                    r.problems.len(),
                    r.shots.len() + r.shots.iter().filter(|s| s.before.is_some()).count()
                );
            }
            None => print!("{text}"),
        }
        Ok(ExitCode::SUCCESS)
    }

    fn ideas(&self, mark_audited: bool) -> Result<ExitCode> {
        let (mut store, project) = self.open()?;
        let cwd = std::env::current_dir()?;
        let repos = Repos::load(&project, &cwd)?;
        let own = crate::capture::Settings::of(&project).worktrees;
        if mark_audited {
            return self.mark_audited(&repos, &cwd, own);
        }
        let Some(pm) = crate::ideas::locate(&repos, own) else {
            if self.json {
                let unaudited = crate::ideas::pm_dir(&cwd, own)
                    .map(|pm| crate::ideas::unaudited(&pm))
                    .unwrap_or_default();
                print_json(&json!({"project": project.name, "path": null, "total": 0,
                    "counts": crate::ideas::IdeaState::ALL.iter()
                        .map(|s| json!({"state": s, "count": 0})).collect::<Vec<_>>(),
                    "ideas": [], "warnings": [], "not_placed": [], "unaudited": unaudited}))?;
            } else {
                println!("{}", crate::ideas::NONE_TEXT);
            }
            return Ok(ExitCode::SUCCESS);
        };
        let st = app::state(&mut store, &project)?;
        let people: Vec<String> = project.people.keys().cloned().collect();
        let ideas = crate::ideas::load(&st, &project.name, &people, &pm)?;
        if self.json {
            print_json(&ideas)?;
        } else {
            print!("{}", crate::ideas::text(&ideas));
        }
        Ok(ExitCode::SUCCESS)
    }

    /// `kontext ideas --mark-audited`: the `.pm/` of the repo it runs in, or
    /// else the one `kontext ideas` reads.
    fn mark_audited(&self, repos: &Repos, cwd: &Path, own: bool) -> Result<ExitCode> {
        let Some(pm) = crate::ideas::pm_dir(cwd, own).or_else(|| crate::ideas::locate(repos, own))
        else {
            return Err(Refused(
                "--mark-audited needs a .pm/: run it in the code repo that has the dumps".into(),
            )
            .into());
        };
        let a = crate::ideas::mark_audited(&pm)?;
        let path = pm.join(crate::ideas::AUDITED_FILE);
        let total: usize = a.files.values().sum();
        if self.json {
            print_json(&json!({"path": path, "at": a.at, "files": a.files, "messages": total}))?;
        } else {
            println!(
                "Audit marked at {}: {total} messages in {} dumps ({}).",
                a.at,
                a.files.len(),
                path.display()
            );
        }
        Ok(ExitCode::SUCCESS)
    }

    /// The reader for `standup`: `--as`, the human from `--actor`/`KONTEXT_ACTOR`, or
    /// the git name.
    fn reader(&self, flag: Option<String>) -> Result<String> {
        if let Some(r) = flag.filter(|r| !r.trim().is_empty()) {
            return Ok(r.trim().to_string());
        }
        let actor = self.who()?;
        if actor.is_human() {
            return Ok(actor.name);
        }
        let p = config::resolve_project(self.project)?;
        Ok(config::human_name_in(&Project::load(&p.home)?, &p.home))
    }

    fn standup(&self, peek: bool, reader: Option<String>) -> Result<ExitCode> {
        let reader = self.reader(reader)?;
        let (mut store, project) = self.open()?;
        let r = app::standup(&mut store, &project, &reader)?;
        if self.json {
            print_json(&r.view)?;
        } else {
            print!("{}", r.view.markdown());
        }
        if !peek {
            store.advance(&reader, &r.frontier)?;
        }
        Ok(ExitCode::SUCCESS)
    }

    fn resume(&self, role: &str, max_chars: Option<usize>, peek: bool) -> Result<ExitCode> {
        let (mut store, project) = self.open()?;
        let r = app::resume(&mut store, &project, role, max_chars)?;
        if self.json {
            print_json(&r.view.json)?;
        } else {
            print!("{}", r.view.text);
        }
        if !peek {
            store.advance(role, &r.frontier)?;
        }
        Ok(ExitCode::SUCCESS)
    }

    fn fact(&self, args: facts::FactArgs) -> Result<ExitCode> {
        let actor = self.who()?;
        let (mut store, project) = self.open()?;
        let cwd = std::env::current_dir()?;
        let repos = Repos::load(&project, &cwd)?;
        let (fps, mut warnings) = facts::capture(&repos, args.repo.as_deref(), &cwd, &args.paths)?;
        // Freshness of the facts is computed before the lock.
        let freshness = app::state(&mut store, &project)?.freshness;
        let mut w = facts::write(&mut store, &actor, &args, fps, &freshness)?;
        warnings.append(&mut w.warnings);
        let main = w
            .entries
            .iter()
            .find(|e| matches!(e.kind.as_str(), "fact" | "confirm"))
            .context("nothing was written")?;
        if self.json {
            print_json(main)?;
        } else {
            match &w.outcome {
                facts::Outcome::New => println!("{}: saved the fact", main.id),
                facts::Outcome::Confirmed { of, reason } => println!(
                    "{of} was already there ({reason}): saved {} as a confirmation, not a new fact",
                    main.id
                ),
                facts::Outcome::Replaced { old } => println!(
                    "{}: saved the fact; it replaces {old}, which had expired",
                    main.id
                ),
            }
            if !w.fingerprints.is_empty() {
                let labels: Vec<String> = w.fingerprints.iter().map(|f| f.label()).collect();
                println!("Fingerprints: {}", labels.join("; "));
            }
            if actor.kind == "agent" && actor.name == "unknown" {
                println!("(as agent:unknown; set KONTEXT_ACTOR or use --actor)");
            }
        }
        for warn in warnings {
            if self.json {
                eprintln!("kontext: warning: {warn}");
            } else {
                println!("Warning: {warn}");
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    fn find(&self, a: FindArgs) -> Result<ExitCode> {
        let (mut store, project) = self.open()?;
        let cwd = std::env::current_dir()?;
        let repos = Repos::load(&project, &cwd)?;
        let local: Vec<find::LocalRepo> = repos
            .available()
            .into_iter()
            .map(|(cfg, root)| find::LocalRepo::new(cfg, root))
            .collect();
        if local.is_empty() {
            let names: Vec<&str> = project.repos.iter().map(|r| r.name.as_str()).collect();
            bail!(
                "no repo of the project is on this machine ({}). Run `kontext find` inside one, \
                 or tell it where each one is in projects.toml: [projects.{}.repos] <name> = \"<path>\"",
                if names.is_empty() {
                    "no [[repos]] in project.toml; `kontext install` adds the current repo"
                        .to_string()
                } else {
                    names.join(", ")
                },
                project.name
            );
        }
        let synonyms = find::load_synonyms(store.home())?;
        let corpus = find::Corpus::open(store.index(), local)?;
        if a.check {
            let broken = corpus.broken_refs()?;
            let counts = find::count_by_doc(&broken);
            if a.update_baseline {
                let Some(path) = &a.baseline else {
                    bail!("--update-baseline needs --baseline <file>");
                };
                fs::write(path, find::format_baseline(&counts))?;
                println!(
                    "Baseline written: {} broken paths in {} docs.",
                    broken.len(),
                    counts.len()
                );
                return Ok(ExitCode::SUCCESS);
            }
            let baseline = match &a.baseline {
                Some(p) => find::parse_baseline(&fs::read_to_string(p).unwrap_or_default()),
                None => Default::default(),
            };
            let (regressions, improved) = find::compare_baseline(&counts, &baseline);
            if self.json {
                print_json(&json!({
                    "broken": broken.iter().map(|b| json!({"doc": b.doc, "line": b.line, "ref": b.reference})).collect::<Vec<_>>(),
                    "regressions": regressions.iter().map(|(d, f, al)| json!({"doc": d, "found": f, "allowed": al})).collect::<Vec<_>>(),
                    "improved": improved,
                }))?;
            } else {
                for b in &broken {
                    println!("{}:{}  →  {}", b.doc, b.line, b.reference);
                }
                println!(
                    "\n{} paths cited by current docs no longer exist ({} docs).",
                    broken.len(),
                    counts.len()
                );
                if !regressions.is_empty() && a.baseline.is_some() {
                    eprintln!("\nDocs with more broken paths than the baseline:");
                    for (doc, found, allowed) in &regressions {
                        eprintln!("  - {doc}: {found} (allowed {allowed})");
                    }
                }
                if improved {
                    println!(
                        "Improved: run `kontext find --check --baseline <file> --update-baseline` to set the new ceiling."
                    );
                }
            }
            return Ok(if regressions.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        if let Some(target) = &a.show {
            let Some(h) = corpus.show(target)? else {
                bail!("no section in {target}");
            };
            let loc = corpus.location(&h.repo, &h.path, h.start_line, h.end_line);
            if self.json {
                print_json(&find::show_json(&h, &loc))?;
            } else {
                print!("{}", find::show_text(&h, &loc));
            }
            return Ok(ExitCode::SUCCESS);
        }
        if a.query.trim().is_empty() {
            bail!(
                "usage: kontext find <topic> [--all] [--limit N] [--json], kontext find --show <path:line>, or kontext find --check"
            );
        }
        let hits = corpus.search(&a.query, &synonyms, a.all, a.limit)?;
        let results = find::results(&corpus, &hits);
        if self.json {
            print_json(&results)?;
        } else {
            print!("{}", find::markdown(&results));
        }
        Ok(ExitCode::SUCCESS)
    }

    fn show(&self, id: &str) -> Result<ExitCode> {
        let (mut store, project) = self.open()?;
        let st = app::state(&mut store, &project)?;
        let id = st.resolve(id)?;
        if self.json {
            print_json(&views::show_json(&st, &id))?;
        } else {
            print!("{}", views::show(&st, &id));
        }
        Ok(ExitCode::SUCCESS)
    }

    fn recall(&self, query: &str, limit: usize, since: Option<&str>) -> Result<ExitCode> {
        let (mut store, _) = self.open()?;
        let st = store.state()?;
        let (hits, total) = recall::recall(store.index(), &st, query, limit, since)?;
        if self.json {
            print_json(&json!({"total": total, "hits": hits}))?;
        } else {
            print!("{}", recall::markdown(query, &hits, total));
        }
        Ok(ExitCode::SUCCESS)
    }

    fn install(&self, how: InstallHow, role: &str) -> Result<ExitCode> {
        let code = self.install_inner(&how, role)?;
        if !how.link_only
            && let Some(note) = build_dir_note()
        {
            println!("{note}");
        }
        Ok(code)
    }

    fn install_inner(&self, how: &InstallHow, role: &str) -> Result<ExitCode> {
        if let Some(r) = how.worktree_role
            && (r.trim().is_empty() || r.trim() != r)
        {
            bail!("invalid --worktree-role {r:?}: give a role name, such as agent");
        }
        let resolved = config::resolve_project(self.project)?;
        let cwd = std::env::current_dir()?;
        let repo = install::repo_root(&cwd);
        if repo == resolved.home {
            bail!(
                "{} is the kontext-home; run `kontext install` in the code repo",
                repo.display()
            );
        }
        let linked = matches!(crate::git::checkout(&repo), Some((_, true)));
        if linked && how.worktree_role.is_some() {
            bail!(
                "{} is a linked git worktree: worktree_role is set in the main checkout's \
                 .kontext.toml (a tracked one reaches the worktrees once it is committed); run \
                 this there",
                repo.display()
            );
        }
        if linked && how.local {
            let main = crate::git::main_checkout(&repo)
                .map(|m| format!(" ({})", m.display()))
                .unwrap_or_default();
            bail!(
                "{} is a linked git worktree: Claude Code reads .claude/settings.local.json from \
                 the main checkout's root in a worktree, not from the worktree; run `kontext \
                 install --local` in the main checkout{main}",
                repo.display()
            );
        }
        if linked
            && !how.link_only
            && let Some(f) = config::find_repo_link(&repo)?
            && f.kind == config::LinkKind::MainCheckout
        {
            bail!(
                "{} is a linked git worktree without a .kontext.toml of its own: kontext is \
                 installed in the main checkout ({}); run `kontext install` there. `{}` marks this \
                 worktree as the PM's (it writes only its .kontext.toml); `kontext install \
                 --skills-only` also works here.",
                repo.display(),
                f.path.parent().unwrap_or(&f.path).display(),
                crate::capture::MARK_COMMAND
            );
        }
        let plan = if how.link_only {
            install::plan_link_only(&repo, &resolved.name, role, how.worktree_role)?
        } else {
            let project = Project::load(&resolved.home)?;
            install::plan(&install::Options {
                repo: &repo,
                project_name: &resolved.name,
                home: &resolved.home,
                default_role: role,
                exe: exe_quoted()?,
                distill: distill::Settings::of(&project)?.enabled,
                capture: crate::capture::Settings::of(&project).enabled,
                force: how.force,
                local: how.local,
                worktree_role: how.worktree_role,
            })?
        };
        for k in &plan.kept {
            println!(
                "Kept {}: it already exists with different content (did you change it?). `kontext install --force` replaces it.",
                k.display()
            );
        }
        for n in &plan.notes {
            println!("{n}");
        }
        if plan.is_noop() {
            if how.link_only {
                println!(
                    "{} is already there; nothing to change.",
                    repo.join(".kontext.toml").display()
                );
            } else {
                println!(
                    "Everything is installed in {} for project {}; nothing to change.",
                    repo.display(),
                    resolved.name
                );
            }
            return Ok(ExitCode::SUCCESS);
        }
        print!("{}", plan.diffs());
        if how.dry_run {
            println!("Dry run (--dry-run): nothing was written.");
            return Ok(ExitCode::SUCCESS);
        }
        for p in plan.apply()? {
            println!("Wrote {}", p.display());
        }
        Ok(ExitCode::SUCCESS)
    }
}

/// Copies images (and, with `allow_html`, pages) to `media/` and returns what
/// goes into the ledger, with its screen (`screens` in order; if missing, the
/// file name).
fn store_media(
    store: &Store,
    paths: &[String],
    screens: &[String],
    allow_html: bool,
) -> Result<Vec<serde_json::Value>> {
    if paths.is_empty() {
        if !screens.is_empty() {
            bail!("--screen goes with an attached file (--attach or a screenshot in --evidence)");
        }
        return Ok(Vec::new());
    }
    if screens.len() > paths.len() {
        bail!(
            "there are {} --screen for {} files: one screen per file, in order",
            screens.len(),
            paths.len()
        );
    }
    let project = Project::load(store.home())?;
    let max = project.media_max_bytes()?;
    let cwd = std::env::current_dir()?;
    let mut out = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let stored = crate::media::store(store.home(), &cwd.join(p), max, allow_html)?;
        let screen = screens
            .get(i)
            .cloned()
            .unwrap_or_else(|| crate::media::default_screen(p));
        let kind = if stored.ext == "html" {
            "page"
        } else {
            "image"
        };
        out.push(stored.to_json(kind, Some(&screen)));
    }
    Ok(out)
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn init(name: &str, home: Option<PathBuf>, remote: Option<&str>) -> Result<ExitCode> {
    if !valid_name(name) {
        bail!("invalid name {name:?}: use letters, digits, - _ and .");
    }
    let machine = Machine::load()?;
    if machine.projects.contains_key(name) {
        bail!(
            "project {name:?} is already registered in {}",
            Machine::path()?.display()
        );
    }
    let writer = machine.writer()?;
    let home = match home {
        Some(h) => h,
        None => config::home_dir()?.join("kontext-home").join(name),
    };
    let home = if home.is_absolute() {
        home
    } else {
        std::env::current_dir()?.join(home)
    };
    if home.exists() && fs::read_dir(&home)?.next().is_some() {
        bail!("{} already exists and is not empty", home.display());
    }
    for d in ["ledger", "cursors"] {
        fs::create_dir_all(home.join(d))?;
        fs::write(home.join(d).join(".gitkeep"), "")?;
    }
    let home = fs::canonicalize(&home)?;
    fs::write(
        home.join("project.toml"),
        Project::template(name, &config::human_name()),
    )?;
    fs::write(
        home.join(".gitignore"),
        format!("{}/\n", config::INTERNAL_DIR),
    )?;
    git(&home, &["init", "-q"])?;
    if let Some(url) = remote {
        git(&home, &["remote", "add", "origin", url])?;
    }
    Machine::register(name, &home)?;
    Store::open(&home, &writer)?;
    println!("Project {name} created in {}", home.display());
    println!("Writer of this machine: {writer}");
    Ok(ExitCode::SUCCESS)
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .context("could not run git")?;
    if !st.success() {
        bail!("`git {}` failed in {}", args.join(" "), dir.display());
    }
    Ok(())
}

fn wait_until(start_at: Option<i64>) {
    if let Some(at) = start_at {
        let now = jiff::Timestamp::now().as_millisecond();
        if at > now {
            std::thread::sleep(std::time::Duration::from_millis((at - now) as u64));
        }
    }
}

fn debug(project: Option<&str>, cmd: DebugCmd) -> Result<ExitCode> {
    let mut store = open(project)?;
    match cmd {
        DebugCmd::Append { start_at } => {
            let lines: Vec<String> = std::io::stdin().lock().lines().collect::<Result<_, _>>()?;
            wait_until(start_at);
            let mut out = std::io::stdout().lock();
            for (i, line) in lines.iter().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let raw: RawEntry = serde_json::from_str(line)
                    .with_context(|| format!("line {} of stdin", i + 1))?;
                let e = store.append(raw)?;
                writeln!(out, "{}", e.id)?;
            }
        }
        DebugCmd::Claim { task, start_at } => {
            wait_until(start_at);
            let mut raw = RawEntry::new("claim", "");
            raw.refs = vec![task.clone()];
            let e = store.append_if(raw, |st| {
                if let Some(c) = st.claim_of(&task) {
                    return Err(Refused(format!(
                        "{task} is already held by {} ({})",
                        c.actor, c.record
                    ))
                    .into());
                }
                Ok(())
            })?;
            println!("{}", e.id);
        }
        DebugCmd::State => {
            let st = store.state()?;
            let out = json!({
                "entries": st.order.len(),
                "status": st.status,
                "tasks": st.tasks.iter().map(|(k, t)| (k.clone(), json!(t.status))).collect::<serde_json::Map<_, _>>(),
                "claims": st.claims,
                "superseded_by": st.superseded_by,
                "not_counting": st.not_counting,
                "alerts": st.alerts,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}
