//! Facts with fingerprints (phase 4): the fingerprint is taken from the latest
//! commit, the fact goes stale only when its file changes, it stays unchecked
//! if the repo is not on the machine, and duplicates are saved as a
//! confirmation.

mod common;

use common::{commit_file, git, with_epic};
use serde_json::Value;

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

const SCOUT: [(&str, &str); 1] = [("KONTEXT_ACTOR", "agent:scout")];

#[test]
fn fingerprint_staleness_and_views() {
    let (p, _g, _e) = with_epic();
    let repo = p.code_repo("billing");
    commit_file(&repo, "src/invoice.ts", "export const iva = 0.16;\n");
    commit_file(&repo, "src/other.ts", "export const x = 1;\n");

    // The fingerprint: repo, relative path, HEAD and blob.
    let out = p.ctx_in_ok(
        &repo.join("src"),
        &SCOUT,
        &[
            "--json",
            "fact",
            "The VAT rate is fixed in invoice.ts",
            "--path",
            "invoice.ts",
            "--epic",
            "payments",
        ],
    );
    let f = json(&out);
    let id = f["id"].as_str().unwrap().to_string();
    let fp = &f["fields"]["fingerprints"][0];
    assert_eq!(fp["repo"], "billing");
    assert_eq!(fp["path"], "src/invoice.ts");
    assert_eq!(fp["commit"], git(&repo, &["rev-parse", "HEAD"]));
    assert_eq!(
        fp["blob"],
        git(&repo, &["rev-parse", "HEAD:src/invoice.ts"])
    );

    // A file that is not in a commit: it is rejected.
    std::fs::write(repo.join("src/new.ts"), "x").unwrap();
    let out = p.ctx_in(&repo, &SCOUT, &["fact", "Other", "--path", "src/new.ts"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Make a commit or drop --path"), "{err}");
    std::fs::remove_file(repo.join("src/new.ts")).unwrap();

    // A commit that does not touch the file: it is still fresh.
    commit_file(&repo, "src/other.ts", "export const x = 2;\n");
    let show = p.ctx_in_ok(&repo, &[], &["show", &id]);
    assert!(show.contains("fresh: its paths have not changed"), "{show}");
    let next = p.ctx_in_ok(&repo, &[], &["brief", "payments", "--peek"]);
    assert!(next.contains(&format!("- {id} The VAT rate")), "{next}");

    // A commit that changes the file: it is stale, with the short HEAD.
    let head = commit_file(&repo, "src/invoice.ts", "export const iva = 0.08;\n");
    let short = &head[..7];
    let show = p.ctx_in_ok(&repo, &[], &["show", &id]);
    assert!(
        show.contains(&format!(
            "stale: src/invoice.ts (billing) changed at {short}"
        )),
        "{show}"
    );
    let alert = format!(
        "{id} went stale: src/invoice.ts changed at {short}; the next scout will read it again"
    );
    let standup = p.ctx_in_ok(&repo, &[], &["standup", "--peek"]);
    assert!(standup.contains(&alert), "{standup}");
    let brief = p.ctx_in_ok(&repo, &[], &["brief", "payments", "--peek"]);
    assert!(brief.contains(&alert), "{brief}");
    assert!(brief.contains("## Epic facts (0)"), "{brief}");
    // The pm sees no facts, not even the stale ones.
    let pm = p.ctx_in_ok(&repo, &[], &["resume", "--as", "pm", "--peek"]);
    assert!(!pm.contains(&id), "{pm}");
    // The brief json gives the freshness of each fact it lists: none.
    let bj = json(&p.ctx_in_ok(&repo, &[], &["--json", "brief", "payments", "--peek"]));
    assert_eq!(bj["facts"].as_array().unwrap().len(), 0);

    // It was stored in the index: the second read does not need git for that HEAD.
    let cached = p.ctx_in_ok(&repo, &[], &["show", &id]);
    assert!(cached.contains("stale"), "{cached}");

    // The scout reads again and writes the same fact: it replaces the stale one.
    let out = p.ctx_in_ok(
        &repo,
        &SCOUT,
        &[
            "fact",
            "The VAT rate is fixed in invoice.ts",
            "--path",
            "src/invoice.ts",
            "--epic",
            "payments",
        ],
    );
    assert!(
        out.contains(&format!("it replaces {id}, which had expired")),
        "{out}"
    );
    let brief = p.ctx_in_ok(&repo, &[], &["brief", "payments", "--peek"]);
    assert!(!brief.contains("stale"), "{brief}");
    assert!(brief.contains("## Epic facts (1)"), "{brief}");
}

#[test]
fn unchecked_when_repo_is_not_here() {
    let (p, _g, _e) = with_epic();
    let repo = p.code_repo("billing");
    commit_file(&repo, "src/invoice.ts", "a\n");
    let id = json(&p.ctx_in_ok(
        &repo,
        &SCOUT,
        &[
            "--json",
            "fact",
            "Invoice in invoice.ts",
            "--path",
            "src/invoice.ts",
            "--epic",
            "payments",
        ],
    ))["id"]
        .as_str()
        .unwrap()
        .to_string();
    commit_file(&repo, "src/invoice.ts", "b\n");

    // From outside the repo and without saying where it is: unchecked, not stale.
    let outside = p.dir.path();
    let brief = p.ctx_in_ok(outside, &[], &["brief", "payments", "--peek"]);
    assert!(
        brief.contains(&format!("- {id} [unchecked] Invoice")),
        "{brief}"
    );
    assert!(!brief.contains("stale"), "{brief}");
    let show = p.ctx_in_ok(outside, &[], &["show", &id]);
    assert!(
        show.contains("unchecked: billing is not on this machine"),
        "{show}"
    );

    // With the path in projects.toml it is checked from any folder.
    p.map_repo("billing", &repo);
    let brief = p.ctx_in_ok(outside, &[], &["brief", "payments", "--peek"]);
    assert!(
        brief.contains(&format!("{id} went stale: src/invoice.ts")),
        "{brief}"
    );
}

#[test]
fn duplicates_become_confirmations() {
    let p = common::Project::new();
    let repo = p.code_repo("billing");
    commit_file(&repo, "src/invoice.ts", "a\n");
    let first = json(&p.ctx_in_ok(
        &repo,
        &SCOUT,
        &[
            "--json",
            "fact",
            "Stripe charges in MXN",
            "--path",
            "src/invoice.ts",
        ],
    ));
    let id = first["id"].as_str().unwrap();

    // Same normalized title (case, accents and punctuation aside).
    let out = p.ctx_in_ok(
        &repo,
        &[("KONTEXT_ACTOR", "agent:scout-2")],
        &["fact", "  stripe charges in mxn. "],
    );
    assert!(
        out.contains(&format!("{id} was already there (same title)")),
        "{out}"
    );

    // Same fingerprints with another title.
    let out = p.ctx_in_ok(
        &repo,
        &SCOUT,
        &[
            "--json",
            "fact",
            "Charges are in pesos",
            "--path",
            "src/invoice.ts",
        ],
    );
    let c = json(&out);
    assert_eq!(c["kind"], "confirm");
    assert_eq!(c["refs"][0], id);
    assert_eq!(c["fields"]["reason"], "same fingerprints");

    // There is still a single fact, with its two confirmations in the history.
    let state = p.state();
    let facts: Vec<&str> = state["status"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with("F-"))
        .map(String::as_str)
        .collect();
    assert_eq!(facts, vec![id]);
    let show = p.ctx_in_ok(&repo, &[], &["show", id]);
    assert_eq!(show.matches("confirmation by agent:").count(), 2, "{show}");

    // No fingerprints and another title: a new fact.
    let out = p.ctx_in_ok(&repo, &SCOUT, &["fact", "Another different fact"]);
    assert!(out.contains("saved the fact"), "{out}");
}

#[test]
fn repo_flag_and_remote_matching() {
    let p = common::Project::new();
    let repo = p.dir.path().join("checkout-with-other-name");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:Acme/Billing.git"],
    );
    commit_file(&repo, "lib/a.rs", "fn a() {}\n");
    p.add_repo_toml("[[repos]]\nname = \"billing\"\nremote = \"github.com/acme/billing\"\n");

    // The remote decides the name even if the folder has a different name.
    let f = json(&p.ctx_in_ok(
        &repo,
        &SCOUT,
        &["--json", "fact", "a exists", "--path", "lib/a.rs"],
    ));
    assert_eq!(f["fields"]["fingerprints"][0]["repo"], "billing");

    // --repo from another folder: the path is relative to the repo.
    p.map_repo("billing", &repo);
    let f = json(&p.ctx_in_ok(
        p.dir.path(),
        &SCOUT,
        &[
            "--json", "fact", "b", "--repo", "billing", "--path", "lib/a.rs",
        ],
    ));
    assert_eq!(f["kind"], "confirm");
    let out = p.ctx_in(
        p.dir.path(),
        &SCOUT,
        &["fact", "c", "--repo", "nothing", "--path", "x"],
    );
    assert_eq!(out.status.code(), Some(2));
}
