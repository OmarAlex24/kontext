//! `kontext find`: sections, doc status, synonyms, `--show`, `--check`, the
//! incremental index and golden queries over a fixture of docs.

mod common;

use std::path::Path;

use common::{Project, commit_file, git};
use serde_json::Value;

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

fn locations(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| r["location"].as_str().unwrap().to_string())
        .collect()
}

/// A small repo with docs in several states.
fn docs_repo(p: &Project) -> std::path::PathBuf {
    let repo = p.code_repo("app");
    commit_file(
        &repo,
        "docs/adr/ADR-0001-geocercas.md",
        "# ADR-0001 Geocercas\n**Status:** Accepted\n\nEvaluamos la geofence por posición en `src/geo.rs`.\n\n## Detalle\nOtra cosa.\n",
    );
    commit_file(
        &repo,
        "docs/adr/ADR-0002-outbox.md",
        "# ADR-0002 Outbox\n**Status:** Superseded by ADR-0003\n\nGeocercas vía outbox.\n",
    );
    commit_file(
        &repo,
        "docs/adr/ADR-0003-colas.md",
        "# ADR-0003 Colas\n**Status:** Accepted\n**Supersedes:** ADR-0004\n\nUna cola sin outbox. Ver `src/gone.rs`.\n",
    );
    commit_file(
        &repo,
        "docs/adr/ADR-0004-viejo.md",
        "# ADR-0004 Viejo\n**Status:** Accepted\n\nOutbox viejo.\n",
    );
    commit_file(
        &repo,
        "docs/work/plan.md",
        "# Plan\n\nGeocercas en el plan viejo.\n",
    );
    commit_file(&repo, "src/geo.rs", "// ver ADR-0001\nfn geo() {}\n");
    commit_file(
        &repo,
        "node_modules/x/README.md",
        "# Geocercas de una dependencia\n",
    );
    p.add_repo_toml("historical = [\"docs/work/**\"]\n");
    repo
}

#[test]
fn search_status_show_and_check() {
    let p = Project::new();
    let repo = docs_repo(&p);
    let find = |args: &[&str]| -> Value {
        let mut a = vec!["--json", "find"];
        a.extend_from_slice(args);
        json(&p.kontext_in_ok(&repo, &[], &a))
    };

    // The current section comes first; superseded docs, historical ones by
    // path, and anything in node_modules do not show up.
    let r = find(&["geocercas"]);
    let locs = locations(&r);
    assert_eq!(locs[0], "docs/adr/ADR-0001-geocercas.md:1-5", "{r}");
    assert!(
        locs.iter().all(|l| !l.contains("ADR-0002")
            && !l.contains("work/")
            && !l.contains("node_modules")),
        "{locs:?}"
    );
    assert_eq!(r[0]["status"], "current");
    assert_eq!(r[0]["statusLabel"], "Accepted");
    assert_eq!(r[0]["code"], serde_json::json!(["src/geo.rs"]));
    assert_eq!(r[0]["citedBy"], serde_json::json!(["src/geo.rs"]));

    // --all brings them back; ADR-0003 supersedes ADR-0004 even though ADR-0004 says Accepted.
    let locs = locations(&find(&["outbox", "--all"]));
    assert!(locs.iter().any(|l| l.contains("ADR-0002")), "{locs:?}");
    assert!(locs.iter().any(|l| l.contains("ADR-0004")), "{locs:?}");
    let r = find(&["outbox", "--all"]);
    let viejo = r
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["location"].as_str().unwrap().contains("ADR-0004"))
        .unwrap();
    assert_eq!(viejo["statusLabel"], "superseded by ADR-0003");
    let locs = locations(&find(&["outbox"]));
    assert!(
        locs.iter()
            .all(|l| !l.contains("ADR-0004") && !l.contains("ADR-0002")),
        "{locs:?}"
    );

    // Synonyms from <kontext-home>/synonyms.toml: "zona" finds "geofence".
    assert!(locations(&find(&["zona"])).is_empty());
    std::fs::write(
        p.home.join("synonyms.toml"),
        "groups = [[\"geocerca\", \"geofence\", \"zona\"]]\n",
    )
    .unwrap();
    assert_eq!(
        locations(&find(&["zona"]))[0],
        "docs/adr/ADR-0001-geocercas.md:1-5"
    );

    // --show prints the section that contains the line.
    let out = p.kontext_in_ok(
        &repo,
        &[],
        &["find", "--show", "docs/adr/ADR-0001-geocercas.md:6"],
    );
    assert_eq!(out, "## Detalle\nOtra cosa.\n\n");
    let out = p.kontext_in(&repo, &[], &["find", "--show", "docs/none.md:1"]);
    assert_eq!(out.status.code(), Some(1));

    // --check: paths cited by current docs that no longer exist.
    let out = p.kontext_in(&repo, &[], &["find", "--check"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("docs/adr/ADR-0003-colas.md:1  →  src/gone.rs"),
        "{text}"
    );
    assert_eq!(out.status.code(), Some(1));
    let base = p.dir.path().join("base.txt");
    p.kontext_in_ok(
        &repo,
        &[],
        &[
            "find",
            "--check",
            "--baseline",
            base.to_str().unwrap(),
            "--update-baseline",
        ],
    );
    assert_eq!(
        std::fs::read_to_string(&base).unwrap(),
        "1 docs/adr/ADR-0003-colas.md\n"
    );
    p.kontext_in_ok(
        &repo,
        &[],
        &["find", "--check", "--baseline", base.to_str().unwrap()],
    );
}

#[test]
fn index_is_incremental() {
    let p = Project::new();
    let repo = docs_repo(&p);
    p.kontext_in_ok(&repo, &[], &["find", "geocercas"]);
    let db = rusqlite::Connection::open(p.home.join(".kontext/index.sqlite")).unwrap();
    let ids = |db: &rusqlite::Connection| -> Vec<(String, i64)> {
        let mut st = db
            .prepare("SELECT path, id FROM docs_files ORDER BY path")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let before = ids(&db);
    assert_eq!(before.len(), 6, "{before:?}");
    // One doc changes: only that one is read again (it gets a new id); a new one comes in.
    std::fs::write(
        repo.join("docs/adr/ADR-0004-viejo.md"),
        "# ADR-0004\n**Status:** Accepted\n\nCentrífugo.\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("docs/nuevo.md"),
        "# Nuevo\nCentrífugo en tiempo real.\n",
    )
    .unwrap();
    std::fs::remove_file(repo.join("docs/work/plan.md")).unwrap();
    let r = json(&p.kontext_in_ok(&repo, &[], &["--json", "find", "centrifugo", "--all"]));
    assert_eq!(r.as_array().unwrap().len(), 2, "{r}");
    let after = ids(&db);
    let same = |path: &str| {
        before.iter().find(|x| x.0 == path).map(|x| x.1)
            == after.iter().find(|x| x.0 == path).map(|x| x.1)
    };
    assert!(same("docs/adr/ADR-0001-geocercas.md"));
    let size: i64 = db
        .query_row(
            "SELECT size FROM docs_files WHERE path = 'docs/adr/ADR-0004-viejo.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(size, 46);
    assert!(after.iter().any(|x| x.0 == "docs/nuevo.md"));
    assert!(after.iter().all(|x| x.0 != "docs/work/plan.md"));
    // `kontext reindex` deletes the docs; the next search reads them again.
    p.kontext_ok(&["reindex"]);
    assert!(ids(&db).is_empty());
    let r = json(&p.kontext_in_ok(&repo, &[], &["--json", "find", "centrifugo", "--all"]));
    assert_eq!(r.as_array().unwrap().len(), 2);
}

#[test]
fn needs_a_local_repo() {
    let p = Project::new();
    let out = p.kontext(&["find", "x"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no repo of the project is on this machine"),
        "{err}"
    );
}

/// Copies a folder into `dst`, file by file.
fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

/// The docs of a fictitious web shop (`tests/fixtures/find-docs`) in a git
/// repo, configured with `examples/find-repo.toml` and, if `synonyms`, with
/// `examples/find-synonyms.toml`.
fn shop(synonyms: bool) -> (Project, std::path::PathBuf) {
    let p = Project::new();
    let repo = p.dir.path().join("code/shop");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/find-docs"),
        &repo,
    );
    // "Exists", for the paths a doc cites, is what git tracks.
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "shop"]);
    p.add_repo_toml(include_str!("../examples/find-repo.toml"));
    if synonyms {
        std::fs::write(
            p.home.join("synonyms.toml"),
            include_str!("../examples/find-synonyms.toml"),
        )
        .unwrap();
    }
    p.map_repo("shop", &repo);
    (p, repo)
}

/// `(location, status, statusLabel)` of each result.
fn hits(v: &Value) -> Vec<(String, String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["location"].as_str().unwrap().to_string(),
                h["status"].as_str().unwrap().to_string(),
                h["statusLabel"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Golden queries over the shop fixture: the exact top results, in order,
/// with their status. Superseded ADRs (by a `Supersedes:` line or by their
/// own `**Status:**`) and historical paths only come back with `--all`;
/// drafts are shown as proposed; templates are excluded.
#[test]
fn golden_queries() {
    let (p, _repo) = shop(true);
    let find = |q: &str, all: bool| -> Vec<(String, String, String)> {
        let mut a = vec!["--json", "find", q, "--limit", "5"];
        if all {
            a.push("--all");
        }
        hits(&json(&p.kontext_ok(&a)))
    };
    let current = |loc: &str, label: &str| (loc.to_string(), "current".into(), label.into());
    let historical = |loc: &str, label: &str| (loc.to_string(), "historical".into(), label.into());

    // Two sections per file at most; the historical path only with --all.
    let checkout = [
        current("docs/guides/checkout.md:1-7", "undeclared"),
        current("docs/guides/checkout.md:8-13", "undeclared"),
        historical("docs/archive/2023-checkout-v1.md:1-5", "historical path"),
        current("README.md:1-8", "undeclared"),
        current("docs/adr/ADR-0004-stripe-payments.md:5-9", "Accepted"),
    ];
    assert_eq!(find("checkout", true), checkout);
    let in_force: Vec<_> = checkout
        .iter()
        .filter(|h| h.1 != "historical")
        .cloned()
        .collect();
    assert_eq!(find("checkout", false), in_force);

    // ADR-0003 says "Supersedes: ADR-0001", which still says Accepted.
    let login = [
        current("docs/adr/ADR-0003-token-login.md:11-15", "Accepted"),
        current("docs/adr/ADR-0003-token-login.md:1-5", "Accepted"),
        historical(
            "docs/adr/ADR-0001-session-cookies.md:10-14",
            "superseded by ADR-0003",
        ),
        historical(
            "docs/adr/ADR-0001-session-cookies.md:1-4",
            "superseded by ADR-0003",
        ),
    ];
    assert_eq!(find("login session", true), login);
    assert_eq!(find("login session", false), login[..2]);

    // ADR-0002 declares itself superseded in its own status line.
    let payments = [
        current("docs/adr/ADR-0004-stripe-payments.md:1-4", "Accepted"),
        current("docs/adr/ADR-0004-stripe-payments.md:5-9", "Accepted"),
        historical(
            "docs/adr/ADR-0002-paypal-payments.md:1-4",
            "Superseded by ADR-0004",
        ),
        historical(
            "docs/adr/ADR-0002-paypal-payments.md:5-9",
            "Superseded by ADR-0004",
        ),
    ];
    assert_eq!(find("payments", true), payments);
    assert_eq!(find("payments", false), payments[..2]);

    // "stock" is only in the synonyms: the doc says inventory and warehouse.
    assert_eq!(
        find("stock", false),
        [
            current("docs/guides/inventory.md:1-7", "undeclared"),
            current("docs/guides/inventory.md:8-12", "undeclared"),
        ]
    );
    let (bare, _) = shop(false);
    assert_eq!(bare.kontext_ok(&["--json", "find", "stock"]).trim(), "[]");

    // A draft is proposed, and it is shown without --all.
    assert_eq!(
        find("search index", false),
        [
            (
                "docs/adr/ADR-0005-search-index.md:1-4".to_string(),
                "proposed".to_string(),
                "draft".to_string()
            ),
            (
                "docs/adr/ADR-0005-search-index.md:5-9".to_string(),
                "proposed".to_string(),
                "draft".to_string()
            ),
        ]
    );

    // "Read when" weighs as much as a heading.
    assert_eq!(
        find("delivery fees", false)[0],
        current("docs/guides/shipping.md:1-7", "undeclared")
    );
    assert_eq!(
        find("roll back", false)[..2],
        [
            current("docs/operations/deploy.md:8-11", "undeclared"),
            current("docs/operations/deploy.md:1-7", "undeclared"),
        ]
    );

    // The template mentions checkout too, but `exclude` leaves it out.
    let all =
        locations(&json(&p.kontext_ok(&[
            "--json", "find", "checkout", "--all", "--limit", "50",
        ])));
    assert!(all.iter().all(|l| !l.contains("templates/")), "{all:?}");

    // --check: the missing path cited by a current doc, not the one cited by
    // the archived doc.
    let out = p.kontext(&["find", "--check"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("docs/guides/checkout.md:1  →  src/checkout/coupons.ts\n"),
        "{text}"
    );
    assert!(!text.contains("legacy"), "{text}");
    assert_eq!(out.status.code(), Some(1));
}
