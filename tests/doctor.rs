//! `kontext doctor`: what it finds on healthy and broken setups, and that it
//! writes nothing. Every run sets PATH, HOME, KONTEXT_CONFIG_DIR and
//! CLAUDE_CONFIG_DIR, so nothing of the real machine is read; the binary is
//! run from copies in the temporary folder, not from where cargo built it.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Setup {
    dir: tempfile::TempDir,
}

impl Setup {
    fn root(&self) -> PathBuf {
        fs::canonicalize(self.dir.path()).unwrap()
    }
    fn bin(&self) -> PathBuf {
        self.root().join("bin").join("kontext")
    }
    fn config(&self) -> PathBuf {
        self.root().join("config")
    }
    fn khome(&self) -> PathBuf {
        self.root().join("khome")
    }
    fn repo(&self) -> PathBuf {
        self.root().join("billing-app")
    }

    /// The built binary at `rel` under the temporary folder (a link or a copy).
    fn copy_bin(&self, rel: &str) -> PathBuf {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        // A hard link where the filesystem allows it: a file just written and
        // run at once can fail with "Text file busy" on Linux.
        if fs::hard_link(common::BIN, &p).is_err() {
            fs::copy(common::BIN, &p).unwrap();
        }
        p
    }

    /// `PATH` with these folders first, then only the system's (for git).
    fn path_with(&self, first: &[&Path]) -> String {
        let mut v: Vec<String> = first.iter().map(|p| p.display().to_string()).collect();
        v.extend(["/usr/bin".to_string(), "/bin".to_string()]);
        v.join(":")
    }

    fn cmd(&self, exe: &Path, args: &[&str], cwd: &Path, path: &str) -> Command {
        let mut c = Command::new(exe);
        c.args(args)
            .current_dir(cwd)
            .env("PATH", path)
            .env("HOME", self.root().join("h"))
            .env("KONTEXT_CONFIG_DIR", self.config())
            .env("CLAUDE_CONFIG_DIR", self.root().join("claude"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("KONTEXT_PROJECT")
            .env_remove("KONTEXT_ACTOR")
            .env_remove("KONTEXT_ROLE")
            .env_remove("KONTEXT_EPIC");
        c
    }

    fn run_ok(&self, exe: &Path, args: &[&str], cwd: &Path) -> String {
        let out = self
            .cmd(exe, args, cwd, &self.path_with(&[&self.root().join("bin")]))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// `kontext doctor --json` with `exe`, from the repo, with this PATH.
    fn doctor(&self, exe: &Path, path: &str) -> (Output, Value) {
        let out = self
            .cmd(exe, &["doctor", "--json"], &self.repo(), path)
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out, v)
    }
}

/// An empty setup: the binary in `bin/`, a config with a writer, a code repo.
fn bare() -> Setup {
    let s = Setup {
        dir: tempfile::tempdir().unwrap(),
    };
    for d in ["h", "claude", "config", "billing-app"] {
        fs::create_dir_all(s.root().join(d)).unwrap();
    }
    s.copy_bin("bin/kontext");
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(s.repo())
        .env("HOME", s.root().join("h"))
        .status()
        .unwrap();
    assert!(git.success());
    s
}

/// A healthy setup: project `demo`, installed in the repo with `bin/kontext`.
fn healthy() -> Setup {
    let s = bare();
    fs::write(s.config().join("projects.toml"), "writer = \"omar@test\"\n").unwrap();
    let home = s.khome();
    s.run_ok(
        &s.bin(),
        &["init", "demo", "--home", home.to_str().unwrap()],
        &s.root(),
    );
    s.run_ok(&s.bin(), &["install"], &s.repo());
    s
}

fn findings(v: &Value, level: &str) -> Vec<String> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["level"] == level)
        .map(|c| format!("[{}] {} | fix: {}", c["section"], c["message"], c["fix"]))
        .collect()
}

fn has(v: &Value, level: &str, needle: &str) -> bool {
    findings(v, level).iter().any(|f| f.contains(needle))
}

/// Path, size and content hash of every file under these folders, and every
/// folder (a stray `create_dir_all` leaves no file).
fn snapshot(dirs: &[PathBuf]) -> BTreeMap<PathBuf, (u64, String)> {
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, (u64, String)>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                out.insert(p.clone(), (0, "dir".into()));
                walk(&p, out);
            } else {
                let bytes = fs::read(&p).unwrap_or_default();
                out.insert(p, (bytes.len() as u64, kontext::model::sha256_hex(&bytes)));
            }
        }
    }
    let mut out = BTreeMap::new();
    for d in dirs {
        walk(d, &mut out);
    }
    out
}

fn all_dirs(s: &Setup) -> Vec<PathBuf> {
    vec![
        s.config(),
        s.khome(),
        s.repo(),
        s.root().join("claude"),
        s.root().join("h"),
    ]
}

#[test]
fn healthy_setup_has_no_problems_and_writes_nothing() {
    let s = healthy();
    // An entry, so the ledger has this writer's file and something not committed.
    s.run_ok(&s.bin(), &["log", "invoices export started"], &s.repo());
    let before = snapshot(&all_dirs(&s));
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(out.status.success(), "{:?}", findings(&v, "problem"));
    assert_eq!(v["ok"], true);
    assert!(findings(&v, "problem").is_empty());
    assert_eq!(snapshot(&all_dirs(&s)), before, "doctor wrote something");

    let bin = s.bin().display().to_string();
    assert!(
        has(&v, "ok", &format!("{bin}, the binary the hooks run")),
        "{v:#}"
    );
    assert!(has(&v, "ok", "project demo (from "), "{v:#}");
    assert!(has(&v, "ok", ".kontext.toml"), "{v:#}");
    assert!(has(&v, "ok", "1 entry"), "{v:#}");
    assert!(has(&v, "ok", "SessionStart"), "{v:#}");
    assert!(has(&v, "ok", "permission Bash(kontext:*)"), "{v:#}");
    assert!(has(&v, "ok", "skills current"), "{v:#}");
    assert!(has(&v, "ok", "agent files current"), "{v:#}");
    // What is not wrong but worth knowing: no remote, an entry not committed.
    assert!(has(&v, "note", "no git remote"), "{v:#}");
    assert!(
        has(&v, "note", "1 own entry not committed") && has(&v, "note", "kontext sync"),
        "{v:#}"
    );
    // The text says the same, by section.
    let out = s
        .cmd(
            &s.bin(),
            &["doctor"],
            &s.repo(),
            &s.path_with(&[&s.root().join("bin")]),
        )
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for section in [
        "This binary\n",
        "kontext on PATH\n",
        "Hooks\n",
        "Hook log\n",
    ] {
        assert!(text.contains(section), "{text}");
    }
    assert!(text.contains("No problems; 2 notes."), "{text}");
}

#[test]
fn hook_pointing_at_a_missing_binary() {
    let s = healthy();
    let old = s.copy_bin("old/kontext");
    s.run_ok(&old, &["install"], &s.repo());
    fs::remove_file(&old).unwrap();
    // Without `.kontext/`, the state in which opening the project would
    // create the index: doctor must not.
    fs::remove_dir_all(s.khome().join(".kontext")).unwrap();
    let before = snapshot(&all_dirs(&s));
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(!out.status.success());
    assert!(
        has(&v, "problem", &format!("{} (in ", old.display()))
            && has(&v, "problem", "the binary is missing"),
        "{v:#}"
    );
    assert!(has(&v, "problem", "cargo install --path"), "{v:#}");
    assert_eq!(snapshot(&all_dirs(&s)), before, "doctor wrote something");
}

#[test]
fn hook_in_a_cargo_build_directory() {
    let s = healthy();
    let dev = s.copy_bin("checkout/target/debug/kontext");
    s.run_ok(&dev, &["install"], &s.repo());
    let path = s.path_with(&[dev.parent().unwrap()]);
    let (out, v) = s.doctor(&dev, &path);
    assert!(!out.status.success());
    let checkout = s.root().join("checkout");
    assert!(
        has(&v, "problem", "cargo build directory and hooks run it"),
        "{v:#}"
    );
    assert!(
        has(
            &v,
            "problem",
            &format!("cargo install --path {} --locked", checkout.display())
        ),
        "{v:#}"
    );
    // Run from another binary, the hook's build directory is still a problem.
    let (_, v) = s.doctor(&s.bin(), &path);
    assert!(
        has(
            &v,
            "problem",
            "a cargo build directory, which `cargo clean` deletes"
        ),
        "{v:#}"
    );
}

#[test]
fn no_session_start_hook() {
    let s = healthy();
    let settings = s.repo().join(".claude/settings.json");
    let mut j: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    j["hooks"].as_object_mut().unwrap().remove("SessionStart");
    fs::write(&settings, serde_json::to_string_pretty(&j).unwrap()).unwrap();
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(!out.status.success());
    assert!(
        has(&v, "problem", "no `SessionStart` hook") && has(&v, "problem", "kontext install"),
        "{v:#}"
    );
}

#[test]
fn capture_enabled_without_its_hooks() {
    let s = healthy();
    let toml = s.khome().join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    fs::write(
        &toml,
        t.replacen("\n[claims]", "\n[capture]\nenabled = true\n\n[claims]", 1),
    )
    .unwrap();
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(!out.status.success());
    assert!(
        has(
            &v,
            "problem",
            "[capture] enabled = true, but these hooks are missing: UserPromptSubmit, Stop"
        ),
        "{v:#}"
    );
    // After `kontext install`, the capture hooks are there.
    s.run_ok(&s.bin(), &["install"], &s.repo());
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(out.status.success(), "{v:#}");
    assert!(has(&v, "ok", "on, with its hooks"), "{v:#}");
}

#[test]
fn no_project_at_all() {
    let s = bare();
    let before = snapshot(&all_dirs(&s));
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(!out.status.success());
    assert!(
        has(&v, "problem", "projects.toml does not exist") && has(&v, "problem", "kontext init"),
        "{v:#}"
    );
    // The checks that need no project still run.
    for section in ["binary", "path", "hooks", "skills"] {
        assert!(
            v["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["section"] == section),
            "{section}: {v:#}"
        );
    }
    assert!(has(&v, "problem", "no `SessionStart` hook"), "{v:#}");
    assert!(
        !v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["section"] == "home"),
        "{v:#}"
    );
    assert_eq!(snapshot(&all_dirs(&s)), before, "doctor wrote something");
    assert!(!s.khome().exists());

    // A projects.toml that does not parse.
    fs::write(s.config().join("projects.toml"), "writer = [\n").unwrap();
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(!out.status.success());
    assert!(
        has(&v, "problem", "could not read") && has(&v, "problem", "by hand"),
        "{v:#}"
    );

    // A registered project, but none chosen from here (several, no .kontext.toml).
    fs::write(
        s.config().join("projects.toml"),
        "writer = \"omar@test\"\n[projects.a]\nhome = \"/nowhere/a\"\n[projects.b]\nhome = \"/nowhere/b\"\n",
    )
    .unwrap();
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(!out.status.success());
    assert!(
        has(&v, "problem", "several projects are registered"),
        "{v:#}"
    );
    // --project is honoured, and a home that is not there is a problem.
    let out = s
        .cmd(
            &s.bin(),
            &["--project", "a", "doctor", "--json"],
            &s.repo(),
            &s.path_with(&[&s.root().join("bin")]),
        )
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(has(&v, "ok", "project a (from --project)"), "{v:#}");
    assert!(has(&v, "problem", "/nowhere/a does not exist"), "{v:#}");
}

#[test]
fn path_resolving_to_a_different_file() {
    let s = healthy();
    let other = s.root().join("other");
    fs::create_dir_all(&other).unwrap();
    let fake = other.join("kontext");
    fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = s.path_with(&[&other, &s.root().join("bin")]);
    let (out, v) = s.doctor(&s.bin(), &path);
    assert!(!out.status.success());
    assert!(
        has(
            &v,
            "problem",
            &format!(
                "{}, but the hooks run {}",
                fake.display(),
                s.bin().display()
            )
        ),
        "{v:#}"
    );
    // No kontext on PATH at all is a problem too.
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[]));
    assert!(!out.status.success());
    assert!(has(&v, "problem", "no `kontext` on PATH"), "{v:#}");
}

#[test]
fn hook_log_errors_are_counted() {
    let s = healthy();
    let log = s.khome().join(".kontext").join("hooks.log");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    // The shapes `kontext hook` writes: a plain line, `<event>: error: …`,
    // and `session-end`'s `distill error: …`.
    fs::write(
        &log,
        "2026-11-01T09:00:00Z session-start session=a source=startup role=pm mode=inject context=full chars=10\n\
         2026-11-01T09:10:00Z pre-compact: error: could not read project.toml\n\
         2026-11-01T09:20:00Z session-end session=a source=-: distill error: boom; capture: nothing written\n\
         2026-11-01T09:30:00Z stop session=a source=-: capture: nothing written\n",
    )
    .unwrap();
    let (out, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(out.status.success(), "{v:#}");
    assert!(
        has(
            &v,
            "note",
            "2 lines with an error, the last at 2026-11-01T09:20:00Z"
        ),
        "{v:#}"
    );
    // A file in another format: a note that says so.
    fs::write(&log, "something else\n").unwrap();
    let (_, v) = s.doctor(&s.bin(), &s.path_with(&[&s.root().join("bin")]));
    assert!(has(&v, "note", "format not recognized"), "{v:#}");
}
