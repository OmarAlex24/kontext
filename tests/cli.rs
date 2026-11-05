//! `init`, project resolution, `verify` and `reindex` from the binary.

mod common;

use std::fs;
use std::process::Command;

use common::{BIN, Project};

#[test]
fn init_creates_kontext_home() {
    let p = Project::new();
    for f in [
        "project.toml",
        ".gitignore",
        "ledger",
        "cursors",
        ".git",
        ".kontext/index.sqlite",
    ] {
        assert!(p.home.join(f).exists(), "missing {f}");
    }
    assert_eq!(
        fs::read_to_string(p.home.join(".gitignore")).unwrap(),
        ".kontext/\n"
    );
    let toml = fs::read_to_string(p.home.join("project.toml")).unwrap();
    assert!(toml.starts_with("name = \"demo\"\n"), "{toml}");
    let reg = fs::read_to_string(p.config.join("projects.toml")).unwrap();
    assert!(reg.contains("writer = \"omar@test\""), "{reg}");
    assert!(reg.contains("[projects.demo]"), "{reg}");
    // An empty kontext-home verifies.
    assert_eq!(
        p.kontext_ok(&["verify"]).trim(),
        "OK: 0 entries in 0 writers"
    );
    // It cannot be registered twice.
    let out = p.kontext(&[
        "init",
        "demo",
        "--home",
        p.dir.path().join("otro").to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already registered"));
}

#[test]
fn project_resolution() {
    let p = Project::new();
    let run = |args: &[&str], envs: &[(&str, &str)], cwd: &std::path::Path| {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("KONTEXT_CONFIG_DIR", &p.config)
            .env_remove("KONTEXT_PROJECT")
            .current_dir(cwd);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    };
    // With only one registered, that one is used.
    assert!(run(&["verify"], &[], p.dir.path()).status.success());
    // With two, one has to be chosen.
    let other = p.dir.path().join("otro");
    let out = run(
        &["init", "otro", "--home", other.to_str().unwrap()],
        &[],
        p.dir.path(),
    );
    assert!(out.status.success());
    let out = run(&["verify"], &[], p.dir.path());
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--project"));
    assert!(
        run(&["--project", "otro", "verify"], &[], p.dir.path())
            .status
            .success()
    );
    assert!(
        run(&["verify"], &[("KONTEXT_PROJECT", "demo")], p.dir.path())
            .status
            .success()
    );
    // .kontext.toml in a code repo, searching upward.
    let repo = p.dir.path().join("repo/src/deep");
    fs::create_dir_all(&repo).unwrap();
    fs::write(
        p.dir.path().join("repo/.kontext.toml"),
        "project = \"otro\"\ndefault_role = \"pm\"\n",
    )
    .unwrap();
    assert!(run(&["verify"], &[], &repo).status.success());
    let out = run(&["--project", "nothing", "verify"], &[], p.dir.path());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not registered"));
}

#[test]
fn verify_reports_file_and_line() {
    let p = Project::new();
    p.append("{\"kind\":\"fact\",\"title\":\"uno\"}\n{\"kind\":\"fact\",\"title\":\"dos\"}\n");
    let path = p.ledger("omar@test");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("\"dos\"", "\"tres\"")).unwrap();
    let out = p.kontext(&["verify"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("ledger/omar@test.jsonl:2: the hash does not match"),
        "{stdout}"
    );
}

#[test]
fn reindex_rebuilds_from_files() {
    let p = Project::new();
    p.append("{\"kind\":\"fact\",\"title\":\"uno\"}\n{\"kind\":\"decision\",\"title\":\"dos\",\"refs\":[]}\n");
    fs::remove_file(p.home.join(".kontext/index.sqlite")).unwrap();
    assert_eq!(
        p.kontext_ok(&["reindex"]).trim(),
        "Index rebuilt: 2 entries"
    );
    assert_eq!(
        p.kontext_ok(&["reindex"]).trim(),
        "Index rebuilt: 2 entries"
    );
}

#[test]
fn index_catches_up_with_lines_written_by_others() {
    // A file that arrives through git (another writer) enters the index on open.
    let p = Project::new();
    let ana = p.machine("ana@laptop");
    let out = p
        .spawn_append(&ana, "{\"kind\":\"fact\",\"title\":\"from ana\"}\n", 0)
        .wait_with_output()
        .unwrap();
    assert!(out.status.success());
    let idx = kontext::index::Index::open(&p.home).unwrap();
    assert_eq!(idx.count().unwrap(), 1);
    assert_eq!(idx.search("ana").unwrap().len(), 1);
}
