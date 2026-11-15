//! `kontext handoff` (from the planning chat to a new PM chat) and
//! `kontext capture here --replace` (the new chat takes over the capture).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Project, git};
use serde_json::Value;

const PM: &[(&str, &str)] = &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")];
const HEADER: &str =
    "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n";

struct Setup {
    p: Project,
    repo: PathBuf,
    feature: String,
}

/// A plan with one feature, and a repo with `.pm/`: P1 placed in the
/// feature, P2 not placed, one message never audited.
fn setup() -> Setup {
    let p = Project::new();
    p.kontext_env_ok(PM, &["charter", "--objective", "Sell online"]);
    let goal = p.id_of(PM, &["goal", "add", "Take payments"]);
    p.kontext_env_ok(
        PM,
        &["epic", "add", "Payments", "--goal", &goal, "--name", "pay"],
    );
    let feature = p.id_of(
        PM,
        &[
            "feature",
            "add",
            "Refunds",
            "--epic",
            "pay",
            "--acceptance",
            "a refund shows in the order",
            "--why",
            "customers ask by email",
        ],
    );
    p.kontext_env_ok(
        PM,
        &[
            "task",
            "add",
            "Refund endpoint",
            "--feature",
            &feature,
            "--acceptance",
            "POST /refunds",
        ],
    );
    let repo = p.dir.path().join("repo");
    fs::create_dir_all(repo.join(".pm/dumps")).unwrap();
    git(&repo, &["init", "-q"]);
    // The project's repo: its .kontext.toml names it (`kontext install`).
    fs::write(repo.join(".kontext.toml"), "project = \"demo\"\n").unwrap();
    fs::write(
        repo.join(".pm/dumps/2026-11-12.md"),
        "## 2026-11-12 10:00\n\n**User:**\n\nrefunds without email\n\nand a dark mode\n",
    )
    .unwrap();
    let s = Setup { p, repo, feature };
    s.items("| P2 | dark mode | dumps/2026-11-12.md:7 | | | |\n");
    s
}

impl Setup {
    fn items(&self, extra: &str) {
        fs::write(
            self.repo.join(".pm/items.md"),
            format!(
                "# Ideas\n\n{HEADER}| P1 | refunds without email | dumps/2026-11-12.md:5 | pending | {} | |\n{extra}",
                self.feature
            ),
        )
        .unwrap();
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = self.p.kontext_in(dir, PM, args);
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn handoff_json(&self) -> (i32, Value) {
        let (code, out, err) = self.run(&self.repo, &["--json", "handoff"]);
        let v = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}\n{err}"));
        (code, v)
    }

    fn mark_audited(&self, now: &str) {
        let out = self
            .p
            .cmd(&["ideas", "--mark-audited"])
            .current_dir(&self.repo)
            .env("KONTEXT_NOW", now)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn sign(&self) {
        let v: Value = serde_json::from_str(&self.p.kontext_in_ok(
            &self.repo,
            PM,
            &["--json", "scope", "summary"],
        ))
        .unwrap();
        let h = v["hash"].as_str().unwrap().to_string();
        self.p.kontext_in_ok(
            &self.repo,
            PM,
            &[
                "scope",
                "sign",
                "--hash",
                &h,
                "--approved-by",
                "omar",
                "--said",
                "sí, adelante",
            ],
        );
    }

    fn ledger_len(&self) -> usize {
        self.p.entries("omar@test").len()
    }
}

/// Refuses, naming every condition that fails, until the plan is signed,
/// nothing is "Not placed" and every message was audited; then prints the
/// opening prompt. It never writes to the ledger.
#[test]
fn handoff_refuses_for_each_reason_then_prints_the_prompt() {
    let s = setup();
    let before = s.ledger_len();

    // Nothing done: all three, at once.
    let (code, _, err) = s.run(&s.repo, &["handoff"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("not ready to hand demo off"), "{err}");
    assert!(err.contains("The plan is not signed"), "{err}");
    assert!(
        err.contains("1 idea of the user's is not placed: P2 \"and a dark mode\""),
        "{err}"
    );
    // That reason names the commands that give an idea a destination.
    for cmd in [
        "kontext feature add",
        "kontext task add",
        "kontext decide",
        "`where` cell",
    ] {
        assert!(err.contains(cmd), "{cmd}: {err}");
    }
    assert!(
        err.contains("1 message of the user's was not seen by any audit"),
        "{err}"
    );
    let (code, v) = s.handoff_json();
    assert_eq!(code, 2);
    assert_eq!(v["ready"], false);
    assert_eq!(v["missing"].as_array().unwrap().len(), 3);
    assert!(v.get("prompt").is_none());

    // From a folder without .pm/: that is a reason too.
    let (code, _, err) = s.run(s.p.dir.path(), &["handoff"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("No .pm/items.md found in a repo of demo from here"),
        "{err}"
    );

    // Placed, audited and signed: ready.
    s.items("| P2 | dark mode | dumps/2026-11-12.md:7 | discarded | | not now |\n");
    s.mark_audited("2026-11-12T11:00:00Z");
    let (code, _, err) = s.run(&s.repo, &["handoff"]);
    assert_eq!(code, 2);
    assert!(err.contains("The plan is not signed"), "{err}");
    assert!(!err.contains("not placed"), "{err}");
    assert!(!err.contains("audit"), "{err}");
    s.sign();
    let signed = s.ledger_len();
    let (code, out, err) = s.run(&s.repo, &["handoff"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Ready to hand demo off: the plan is signed (S-"),
        "{out}"
    );
    assert!(out.contains("----- 8< -----"), "{out}");
    assert!(out.contains("You are the PM of the project demo"), "{out}");
    assert!(
        out.contains("Do not ask for a summary of that chat"),
        "{out}"
    );
    assert!(
        out.contains("`kontext resume --as pm`, then `kontext tree`"),
        "{out}"
    );
    assert!(out.contains("\"Starting after a handoff\""), "{out}");
    // Capture is off in this project: the prompt says so, no --replace.
    assert!(out.contains("Capture is off in this project"), "{out}");
    assert!(!out.contains("--replace"), "{out}");
    let (code, v) = s.handoff_json();
    assert_eq!(code, 0);
    assert_eq!(v["ready"], true);
    assert!(v["signed"].as_str().unwrap().starts_with("S-"));
    assert!(v["prompt"].as_str().unwrap().contains("kontext tree"));

    // Each reason alone holds it again: a new idea not placed…
    s.items(
        "| P2 | dark mode | dumps/2026-11-12.md:7 | discarded | | not now |\n\
         | P3 | export | dumps/2026-11-12.md:5 | | | |\n",
    );
    let (code, v) = s.handoff_json();
    assert_eq!(code, 2);
    let missing: Vec<&str> = v["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(missing[0].contains("not placed: P3"), "{missing:?}");
    s.items("| P2 | dark mode | dumps/2026-11-12.md:7 | discarded | | not now |\n");
    // …and a message after the last audit.
    fs::write(
        s.repo.join(".pm/dumps/2026-11-13.md"),
        "## 2026-11-13 09:00\n\n**User:**\n\nalso invoices\n",
    )
    .unwrap();
    let (code, _, err) = s.run(&s.repo, &["handoff"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("1 message of the user's was not seen since the audit of Nov 12"),
        "{err}"
    );
    assert!(!err.contains("not signed"), "{err}");
    s.mark_audited("2026-11-13T10:00:00Z");
    let (code, _, err) = s.run(&s.repo, &["handoff"]);
    assert_eq!(code, 0, "{err}");

    // Read-only: only the signature was written.
    assert_eq!(s.ledger_len(), signed);
    assert_eq!(signed, before + 1);
}

/// A scope change waiting for the owner does not hold the hand-off: it is a
/// note for the new PM.
#[test]
fn a_pending_scope_change_is_a_note_not_a_refusal() {
    let s = setup();
    s.items("| P2 | dark mode | dumps/2026-11-12.md:7 | discarded | | not now |\n");
    s.mark_audited("2026-11-12T11:00:00Z");
    s.sign();
    s.p.kontext_env_ok(
        PM,
        &[
            "feature",
            "add",
            "Invoices",
            "--epic",
            "pay",
            "--acceptance",
            "a PDF per order",
        ],
    );
    let (code, out, err) = s.run(&s.repo, &["handoff"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("Note: Scope change waiting for the owner's signature"),
        "{out}"
    );
}

/// With capture on, the prompt tells the new chat to take it over, and
/// `capture here --replace` leaves it the only conversation captured.
#[test]
fn capture_here_replace_leaves_only_the_new_chat() {
    let s = setup();
    s.p.add_repo_toml("[capture]\nenabled = true\n");
    s.items("| P2 | dark mode | dumps/2026-11-12.md:7 | discarded | | not now |\n");
    s.mark_audited("2026-11-12T11:00:00Z");
    s.sign();
    s.p.kontext_in_ok(&s.repo, PM, &["capture", "here", "--session", "plan-1"]);
    s.p.kontext_in_ok(&s.repo, PM, &["capture", "here", "--session", "plan-2"]);

    let (code, out, err) = s.run(&s.repo, &["handoff"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("1. Run `kontext capture here --replace`"),
        "{out}"
    );
    assert!(
        out.contains("Named for capture on this machine now: plan-1, plan-2"),
        "{out}"
    );
    assert!(out.contains("`kontext capture forget --all` here"), "{out}");
    let (_, v) = s.handoff_json();
    assert_eq!(v["named"].as_array().unwrap().len(), 2);

    let out = s.p.kontext_in_ok(
        &s.repo,
        PM,
        &["capture", "here", "--session", "pm-new", "--replace"],
    );
    assert!(
        out.contains("Named conversation pm-new (from --session) for the capture."),
        "{out}"
    );
    assert!(
        out.contains("Forgot 2 other named conversations (plan-1, plan-2)"),
        "{out}"
    );
    let v: Value =
        serde_json::from_str(&s.p.kontext_in_ok(&s.repo, PM, &["--json", "capture", "list"]))
            .unwrap();
    let named: Vec<&str> = v["named"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(named, vec!["pm-new"]);

    // Again: nothing else to forget, it stays named.
    let v: Value = serde_json::from_str(&s.p.kontext_in_ok(
        &s.repo,
        PM,
        &[
            "--json",
            "capture",
            "here",
            "--session",
            "pm-new",
            "--replace",
        ],
    ))
    .unwrap();
    assert_eq!(v["added"], false);
    assert_eq!(v["forgotten"].as_array().unwrap().len(), 0);
    assert_eq!(v["named"].as_array().unwrap().len(), 1);
    // Without --replace the output has no "forgotten" key, as before.
    let v: Value = serde_json::from_str(&s.p.kontext_in_ok(
        &s.repo,
        PM,
        &["--json", "capture", "here", "--session", "pm-2"],
    ))
    .unwrap();
    assert!(v.get("forgotten").is_none());
}

/// A `.pm/` counts only in a repo of the project. Another git folder with
/// an empty `items.md` (or one whose .kontext.toml names another project)
/// vouches for nothing: `handoff` refuses there, and the summary signed there
/// says the ideas were not checked, with no folder in it. A linked worktree
/// of the project's repo reads the main checkout's `.pm/`.
#[test]
fn a_pm_folder_counts_only_in_a_repo_of_the_project() {
    let s = setup();
    s.items("| P2 | dark mode | dumps/2026-11-12.md:7 | discarded | | not now |\n");
    s.mark_audited("2026-11-12T11:00:00Z");
    s.items("| P2 | dark mode | dumps/2026-11-12.md:7 | | | |\n");
    let other = s.p.dir.path().join("other");
    fs::create_dir_all(other.join(".pm")).unwrap();
    git(&other, &["init", "-q"]);
    fs::write(other.join(".pm/items.md"), format!("# Ideas\n\n{HEADER}")).unwrap();
    for link in [None, Some("project = \"elsewhere\"\n")] {
        if let Some(l) = link {
            fs::write(other.join(".kontext.toml"), l).unwrap();
        }
        let (code, _, err) = s.run(&other, &["handoff"]);
        assert_eq!(code, 2, "{err}");
        assert!(
            err.contains("No .pm/items.md found in a repo of demo from here"),
            "{err}"
        );
        assert!(!err.contains("Ready"), "{err}");
        let (_, sum, _) = s.run(&other, &["scope", "summary"]);
        assert!(
            sum.contains(
                "Ideas not checked: no .pm/items.md found in a repo of this project where this ran."
            ),
            "{sum}"
        );
        assert!(!sum.contains("Not placed: 0"), "{sum}");
        assert!(!sum.contains(&other.display().to_string()), "{sum}");
        // Its .pm/ is seen but does not count: the summary warns out of the
        // signed text, and the signature is refused.
        let (_, out, err) = s.run(&other, &["--json", "scope", "summary"]);
        assert!(
            err.contains("is in a repo that is not demo's, so it does not count"),
            "{err}"
        );
        let h = serde_json::from_str::<Value>(&out).unwrap()["hash"]
            .as_str()
            .unwrap()
            .to_string();
        let before = s.ledger_len();
        let (code, _, err) = s.run(
            &other,
            &[
                "scope",
                "sign",
                "--hash",
                &h,
                "--approved-by",
                "omar",
                "--said",
                "sí",
            ],
        );
        assert_ne!(code, 0, "{err}");
        assert!(
            err.contains("items.md is in a repo that is not demo's"),
            "{err}"
        );
        assert_eq!(s.ledger_len(), before);
    }
    // From the repo itself, the unplaced idea is seen.
    let (_, sum, _) = s.run(&s.repo, &["scope", "summary"]);
    assert!(sum.contains("Not placed: 1 of your ideas"), "{sum}");
    // A linked worktree of the repo: the main checkout's .pm/ and .kontext.toml.
    git(&s.repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
    let wt = s.p.dir.path().join("wt");
    git(&s.repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let (code, _, err) = s.run(&wt, &["handoff"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("P2 \"and a dark mode\""), "{err}");
    // A folder with no .pm/ at all still signs "Ideas not checked".
    s.p.kontext_env_ok(
        PM,
        &[
            "feature",
            "add",
            "Invoices",
            "--epic",
            "pay",
            "--acceptance",
            "a PDF",
            "--why",
            "accounting asks",
        ],
    );
    let bare = s.p.dir.path().join("bare");
    fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q"]);
    let (_, out, err) = s.run(&bare, &["--json", "scope", "summary"]);
    assert!(err.is_empty(), "{err}");
    let h = serde_json::from_str::<Value>(&out).unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, _, err) = s.run(
        &bare,
        &[
            "scope",
            "sign",
            "--hash",
            &h,
            "--approved-by",
            "omar",
            "--said",
            "sí",
        ],
    );
    assert_eq!(code, 0, "{err}");
}

/// Under `require_signatures`, the reason names the key and a command that
/// signs there; the plain `scope sign` it printed before was refused.
#[test]
fn under_require_signatures_the_reason_names_the_key_and_a_command_that_works() {
    let s = setup();
    let toml = s.p.home.join("project.toml");
    let text = fs::read_to_string(&toml)
        .unwrap()
        .replace("require_signatures = false", "require_signatures = true");
    fs::write(&toml, text).unwrap();
    let (code, _, err) = s.run(&s.repo, &["handoff"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("This project requires signatures"), "{err}");
    assert!(
        err.contains(
            "`kontext --sign scope sign --hash <hash> --approved-by <owner> --said \"<their words>\"`"
        ),
        "{err}"
    );
    assert!(err.contains("~/.ssh/id_ed25519.pub"), "{err}");
    assert!(err.contains("(omar)"), "{err}");
}
