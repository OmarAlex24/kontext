//! `kontext import --from-v0` contra registros hechos con el `ctx.py` real.

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Project;
use serde_json::Value;

/// The same steps that generated `tests/fixtures/v0`. The titles and bodies are
/// the Spanish v0 content on purpose: it is a real ledger, and it must migrate as
/// written.
const V0_SCRIPT: &[&[&str]] = &[
    &[
        "decide",
        "Usar Rust para ctx",
        "--why",
        "un solo binario",
        "--body",
        "Línea 1\nLínea 2 con \"comillas\" y emoji 🚀",
        "--tags",
        "core,lang",
    ],
    &[
        "decide",
        "Índice en SQLite",
        "--why",
        "FTS5 incluido",
        "--tags",
        "core",
    ],
    &[
        "propose",
        "Cambiar a jiff",
        "--why",
        "mejor API de fechas",
        "--supersedes",
        "D-0001",
        "--actor",
        "agent:coordinator",
    ],
    &[
        "propose",
        "Usar libgit2",
        "--why",
        "sin subprocesos",
        "--actor",
        "agent:scout",
    ],
    &[
        "question",
        "¿Windows entra?",
        "--body",
        "por flock",
        "--tags",
        "plataforma",
    ],
    &["answer", "Q-0001", "--body", "No en estas fases"],
    &["accept", "P-0001", "--why", "de acuerdo"],
    &["reject", "P-0002", "--why", "binario más grande"],
    &[
        "propose",
        "Guardar cursores por máquina",
        "--actor",
        "agent:coordinator",
    ],
    &[
        "accept",
        "P-0003",
        "--as-decision",
        "--actor",
        "agent:coordinator",
        "--approved-by",
        "omar",
    ],
    &[
        "fact",
        "El servidor t3-machine no tiene Rust",
        "--tags",
        "infra",
    ],
    &[
        "log",
        "Terminé el diseño",
        "--task",
        "T-diseño",
        "--next",
        "fase 1",
        "--refs",
        "D-0002",
    ],
    &[
        "decide",
        "Usar std::fs::File::lock",
        "--supersedes",
        "D-0002",
        "--why",
        "menos dependencias",
    ],
];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0")
}

fn v0_entries(home: &Path) -> Vec<Value> {
    fs::read_to_string(home.join("ledger/entries.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Imports and checks everything that must hold.
fn import_and_check(v0_home: &Path) {
    let p = Project::new();
    let v0 = v0_entries(v0_home);
    let out = p.kontext_ok(&["import", "--from-v0", v0_home.to_str().unwrap()]);
    assert!(
        out.contains(&format!("Imported {n} of {n} entries", n = v0.len())),
        "{out}"
    );

    let v1 = p.entries("omar@test");
    assert_eq!(v1.len(), v0.len());

    // Mismos tipos y conteos, mismo orden.
    let count = |es: &[Value]| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for e in es {
            *m.entry(e["kind"].as_str().unwrap().to_string())
                .or_default() += 1;
        }
        m
    };
    assert_eq!(count(&v0), count(&v1));
    for (k, n) in count(&v0) {
        assert!(
            out.contains(&format!("{k}: {n}")),
            "missing {k}: {n} in {out}"
        );
    }

    let map: HashMap<String, String> = v1
        .iter()
        .map(|e| {
            (
                e["fields"]["v0_id"].as_str().unwrap().to_string(),
                e["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let v1_ids: HashSet<&str> = v1.iter().map(|e| e["id"].as_str().unwrap()).collect();
    let mut last_hlc = String::new();
    for (old, new) in v0.iter().zip(&v1) {
        assert_eq!(old["kind"], new["kind"]);
        for k in ["title", "body", "why", "tags"] {
            assert_eq!(old[k], new[k], "{k}");
        }
        assert_eq!(new["fields"]["v0_id"], old["id"]);
        assert_eq!(new["fields"]["v0_hash"], old["hash"]);
        assert_eq!(new["fields"]["seq"], old["seq"]);
        assert_eq!(new["fields"]["status"], old["status"]);
        assert_eq!(new["fields"]["source"], old["source"]);
        // human:omar -> {type: human, name: omar}
        let (t, n) = old["actor"].as_str().unwrap().split_once(':').unwrap();
        assert_eq!(new["actor"]["type"], t);
        assert_eq!(new["actor"]["name"], n);
        // refs rewritten to v1 ids that exist.
        let old_refs: Vec<&str> = old["refs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        let new_refs: Vec<&str> = new["refs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        assert_eq!(new_refs.len(), old_refs.len());
        for (o, n) in old_refs.iter().zip(&new_refs) {
            assert_eq!(map[*o], *n);
            assert!(v1_ids.contains(n));
        }
        // Fields with ids are rewritten too.
        for k in ["supersedes", "decision_id", "from_proposal"] {
            if let Some(v) = old.get(k) {
                let rewritten = match v {
                    Value::String(s) => Value::String(map[s].clone()),
                    Value::Array(a) => a
                        .iter()
                        .map(|x| Value::String(map[x.as_str().unwrap()].clone()))
                        .collect(),
                    _ => unreachable!(),
                };
                assert_eq!(new["fields"][k], rewritten, "{k}");
            }
        }
        // The hlc comes from the v0 ts and keeps the order.
        let hlc = new["hlc"].as_str().unwrap();
        assert!(hlc.starts_with(&old["ts"].as_str().unwrap()[..19]), "{hlc}");
        assert!(hlc > last_hlc.as_str());
        last_hlc = hlc.to_string();
    }
    // What has no place of its own goes to fields (the journal's task and next).
    let journal = v1.iter().find(|e| e["kind"] == "journal").unwrap();
    assert_eq!(journal["fields"]["task"], "T-diseño");
    assert_eq!(journal["fields"]["next"], "fase 1");

    let out = p.kontext_ok(&["verify"]);
    assert_eq!(out.trim(), format!("OK: {} entries in 1 writer", v0.len()));

    // The state comes out the same as in v0.
    let state: Value = serde_json::from_str(&p.kontext_ok(&["debug", "state"])).unwrap();
    let status = |v0_id: &str| state["status"][&map[v0_id]].as_str().unwrap().to_string();
    assert_eq!(status("D-0001"), "superseded");
    assert_eq!(status("D-0002"), "superseded");
    assert_eq!(status("D-0003"), "accepted");
    assert_eq!(status("D-0004"), "accepted");
    assert_eq!(status("P-0001"), "accepted");
    assert_eq!(status("P-0002"), "rejected");
    assert_eq!(status("Q-0001"), "answered");
    // What v0 had already accepted still counts (v0 rule).
    assert_eq!(state["alerts"], serde_json::json!([]), "{state}");

    // Importing again adds nothing.
    let out = p.kontext_ok(&["import", "--from-v0", v0_home.to_str().unwrap()]);
    assert!(
        out.contains(&format!("Imported 0 of {} entries", v0.len())),
        "{out}"
    );
    assert!(
        out.contains(&format!("Already imported: {}", v0.len())),
        "{out}"
    );
    assert_eq!(p.entries("omar@test").len(), v0.len());
    p.kontext_ok(&["verify"]);
}

#[test]
fn imports_fixture() {
    import_and_check(&fixture());
}

#[test]
fn imports_ledger_from_real_ctx_py() {
    let Ok(script) = std::env::var("KONTEXT_V0_SCRIPT") else {
        eprintln!("skipped: set KONTEXT_V0_SCRIPT to the path of a v0 ctx.py");
        return;
    };
    assert!(
        Path::new(&script).exists(),
        "KONTEXT_V0_SCRIPT={script} does not exist"
    );
    let dir = tempfile::tempdir().unwrap();
    for args in V0_SCRIPT {
        let out = Command::new("python3")
            .arg(&script)
            .args(*args)
            .env("CTX_HOME", dir.path())
            // ctx.py (v0) reads the variables by their old names.
            .env("CTX_ACTOR", "human:omar")
            .env_remove("CTX_PROJECT")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ctx.py {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    import_and_check(dir.path());
}

#[test]
fn grows_incrementally() {
    // If the v0 ledger kept growing, the second import brings in only what is
    // new and maps its refs to what was already imported.
    let dir = tempfile::tempdir().unwrap();
    let full = v0_entries(&fixture());
    let raw = fs::read_to_string(fixture().join("ledger/entries.jsonl")).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    fs::create_dir_all(dir.path().join("ledger")).unwrap();
    let path = dir.path().join("ledger/entries.jsonl");
    fs::write(&path, format!("{}\n", lines[..5].join("\n"))).unwrap();
    let p = Project::new();
    let out = p.kontext_ok(&["import", "--from-v0", dir.path().to_str().unwrap()]);
    assert!(out.contains("Imported 5 of 5"), "{out}");
    fs::write(&path, raw).unwrap();
    let out = p.kontext_ok(&["import", "--from-v0", dir.path().to_str().unwrap()]);
    assert!(
        out.contains(&format!("Imported {} of {}", full.len() - 5, full.len())),
        "{out}"
    );
    let v1 = p.entries("omar@test");
    assert_eq!(v1.len(), full.len());
    let answer = v1.iter().find(|e| e["kind"] == "answer").unwrap();
    let question = v1.iter().find(|e| e["kind"] == "question").unwrap();
    assert_eq!(answer["refs"][0], question["id"]);
    p.kontext_ok(&["verify"]);
}

/// Writes a v0 ledger with its chain (`seq`, `prev_hash`, `hash`) as `ctx.py`
/// computes it.
fn write_v0(dir: &Path, entries: Vec<Value>) {
    let mut prev = "0".repeat(64);
    let mut out = String::new();
    for (i, mut e) in entries.into_iter().enumerate() {
        e["seq"] = serde_json::json!(i + 1);
        e["prev_hash"] = Value::String(prev.clone());
        prev = kontext::import_v0::v0_hash(&e);
        e["hash"] = Value::String(prev.clone());
        out.push_str(&serde_json::to_string(&e).unwrap());
        out.push('\n');
    }
    fs::create_dir_all(dir.join("ledger")).unwrap();
    fs::write(dir.join("ledger/entries.jsonl"), out).unwrap();
}

/// v0 copied the `supersedes` of a proposal onto the decision made from it.
/// After the import, the old proposal is superseded once, with no conflict.
#[test]
fn a_proposal_and_its_decision_do_not_conflict_after_import() {
    let entry = |id: &str, kind: &str, title: &str, actor: &str| {
        serde_json::json!({
            "id": id, "ts": "2026-11-04T01:24:14Z", "kind": kind, "title": title,
            "body": "", "why": "", "tags": ["billing"], "actor": actor,
            "source": "manual", "refs": [], "status": "accepted",
        })
    };
    let p1 = entry(
        "P-0001",
        "proposal",
        "Send invoices monthly",
        "agent:coordinator",
    );
    let mut p2 = entry(
        "P-0002",
        "proposal",
        "Send invoices weekly",
        "agent:coordinator",
    );
    p2["supersedes"] = serde_json::json!(["P-0001"]);
    let mut d = entry("D-0001", "decision", "Send invoices weekly", "human:omar");
    d["refs"] = serde_json::json!(["P-0002"]);
    d["from_proposal"] = serde_json::json!("P-0002");
    d["supersedes"] = serde_json::json!(["P-0001"]);
    let mut acc = entry("ACC-0001", "accept", "Accept P-0002", "human:omar");
    acc["refs"] = serde_json::json!(["P-0002"]);
    acc["decision_id"] = serde_json::json!("D-0001");
    let dir = tempfile::tempdir().unwrap();
    write_v0(dir.path(), vec![p1, p2, d, acc]);

    let p = Project::new();
    let out = p.kontext_ok(&["import", "--from-v0", dir.path().to_str().unwrap()]);
    assert!(out.contains("Imported 4 of 4 entries"), "{out}");
    let ids: HashMap<String, String> = p
        .entries("omar@test")
        .iter()
        .map(|e| {
            (
                e["fields"]["v0_id"].as_str().unwrap().to_string(),
                e["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let state: Value = serde_json::from_str(&p.kontext_ok(&["debug", "state"])).unwrap();
    let status = |v0_id: &str| state["status"][&ids[v0_id]].as_str().unwrap().to_string();
    assert_eq!(status("P-0001"), "superseded");
    assert_eq!(status("P-0002"), "accepted");
    assert_eq!(status("D-0001"), "accepted");
    assert_eq!(state["alerts"], serde_json::json!([]), "{state}");
}

#[test]
fn refuses_broken_v0_chain_unless_forced() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("ledger")).unwrap();
    let raw = fs::read_to_string(fixture().join("ledger/entries.jsonl")).unwrap();
    // Edit an entry without recomputing its hash.
    let edited = raw.replacen("FTS5 incluido", "FTS5 incluida", 1);
    assert_ne!(raw, edited);
    fs::write(dir.path().join("ledger/entries.jsonl"), edited).unwrap();

    let p = Project::new();
    let out = p.kontext(&["import", "--from-v0", dir.path().to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("the v0 ledger chain is broken") && err.contains("line 2"),
        "{err}"
    );
    assert!(!p.ledger("omar@test").exists());

    let out = p.kontext(&[
        "import",
        "--from-v0",
        dir.path().to_str().unwrap(),
        "--force",
    ]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning"));
    p.kontext_ok(&["verify"]);
}
