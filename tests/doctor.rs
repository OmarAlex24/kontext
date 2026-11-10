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
            .env_remove("KONTEXT_EPIC")
            .env_remove("CLAUDE_CODE_SESSION_ID");
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
        self.doctor_in(exe, path, &self.repo())
    }

    /// `kontext doctor --json` from `cwd`.
    fn doctor_in(&self, exe: &Path, path: &str, cwd: &Path) -> (Output, Value) {
        let out = self
            .cmd(exe, &["doctor", "--json"], cwd, path)
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

impl Setup {
    fn git(&self, dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .env("HOME", self.root().join("h"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    /// A linked worktree of the repo, `<root>/<name>`.
    fn worktree(&self, name: &str) -> PathBuf {
        let p = self.root().join(name);
        self.git(
            &self.repo(),
            &["worktree", "add", "-q", "-b", name, p.to_str().unwrap()],
        );
        p
    }

    fn path(&self) -> String {
        self.path_with(&[&self.root().join("bin")])
    }
}

/// Project `demo` with the capture on, installed in the main checkout with
/// `install_args` (`.kontext.toml` and `.claude/` untracked, as in a public
/// repo), one commit, and two linked worktrees: `pm-desk`, marked as the
/// PM's, and `agent-1`.
fn with_worktrees(install_args: &[&str]) -> (Setup, PathBuf, PathBuf) {
    let s = bare();
    fs::write(s.config().join("projects.toml"), "writer = \"omar@test\"\n").unwrap();
    let home = s.khome();
    s.run_ok(
        &s.bin(),
        &["init", "demo", "--home", home.to_str().unwrap()],
        &s.root(),
    );
    let toml = home.join("project.toml");
    let t = fs::read_to_string(&toml).unwrap();
    fs::write(
        &toml,
        t.replacen("\n[claims]", "\n[capture]\nenabled = true\n\n[claims]", 1),
    )
    .unwrap();
    let mut args = vec!["install"];
    args.extend_from_slice(install_args);
    s.run_ok(&s.bin(), &args, &s.repo());
    s.git(&s.repo(), &["commit", "-q", "--allow-empty", "-m", "init"]);
    let pm = s.worktree("pm-desk");
    s.run_ok(&s.bin(), &["install", "--link-only"], &pm);
    let agent = s.worktree("agent-1");
    (s, pm, agent)
}

/// `kontext doctor --json` from `cwd` with `KONTEXT_ROLE` set.
fn doctor_as(s: &Setup, cwd: &Path, role: &str) -> Value {
    let out = s
        .cmd(&s.bin(), &["doctor", "--json"], cwd, &s.path())
        .env("KONTEXT_ROLE", role)
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn a_worktree_of_a_local_install_has_no_problems() {
    let (s, pm, agent) = with_worktrees(&["--local"]);
    assert!(!s.repo().join(".claude/settings.json").exists());
    let mut dirs = all_dirs(&s);
    dirs.extend([pm.clone(), agent.clone()]);
    let before = snapshot(&dirs);

    let (out, v) = s.doctor_in(&s.bin(), &s.path(), &pm);
    assert!(out.status.success(), "{:?}", findings(&v, "problem"));
    let main = s.repo();
    let local = main
        .join(".claude/settings.local.json")
        .display()
        .to_string();
    for (level, needle) in [
        (
            "ok",
            format!("project demo (from {}/.kontext.toml)", pm.display()),
        ),
        (
            "ok",
            "sessions here start with role pm (default_role in".to_string(),
        ),
        (
            "ok",
            format!(
                "SessionStart, Stop, UserPromptSubmit → {}",
                s.bin().display()
            ),
        ),
        ("ok", format!("(in {local}), this binary")),
        ("ok", "permission Bash(kontext:*)".into()),
        ("ok", "on, with its hooks".into()),
        ("ok", "a linked git worktree marked as the PM's".into()),
        ("ok", format!(".pm/ here: {}", main.join(".pm").display())),
        // No .claude/skills or .claude/agents in the worktree: Claude Code
        // loads the main checkout's.
        (
            "ok",
            format!(
                "skills current in {}",
                main.join(".claude/skills").display()
            ),
        ),
        (
            "ok",
            format!(
                "agent files current in {}",
                main.join(".claude/agents").display()
            ),
        ),
    ] {
        assert!(has(&v, level, &needle), "{needle}: {v:#}");
    }
    assert!(!has(&v, "note", "hooks are only in"), "{v:#}");

    // An agent's worktree, with no worktree_role: every session there starts
    // as the PM, a note with both steps.
    let (out, v) = s.doctor_in(&s.bin(), &s.path(), &agent);
    assert!(out.status.success(), "{:?}", findings(&v, "problem"));
    assert!(
        has(
            &v,
            "ok",
            &format!(
                "project demo (from {}/.kontext.toml, the main checkout's: this linked git \
                 worktree has none of its own)",
                main.display()
            )
        ),
        "{v:#}"
    );
    assert!(
        !has(&v, "ok", "sessions here start with role pm")
            && has(
                &v,
                "note",
                "sessions here start with role pm (default_role in"
            )
            && has(
                &v,
                "note",
                &format!(
                    "`kontext install --link-only` in the worktree where the PM works, then \
                     `kontext install --worktree-role agent` in {}",
                    main.display()
                )
            ),
        "{v:#}"
    );
    assert!(
        has(&v, "note", "not marked as the PM's") && has(&v, "note", "kontext install --link-only"),
        "{v:#}"
    );
    assert!(
        has(
            &v,
            "ok",
            &format!(".pm/ here: {}", main.join(".pm").display())
        ),
        "{v:#}"
    );
    // A role from $KONTEXT_ROLE stays ok, even pm.
    let v = doctor_as(&s, &agent, "pm");
    assert!(
        has(&v, "ok", "sessions here start with role pm ($KONTEXT_ROLE)"),
        "{v:#}"
    );

    // The main checkout itself.
    let (out, v) = s.doctor(&s.bin(), &s.path());
    assert!(out.status.success(), "{:?}", findings(&v, "problem"));
    assert!(has(&v, "ok", "a main checkout, into its own .pm/"), "{v:#}");
    assert!(
        has(
            &v,
            "ok",
            "sessions here start with role pm (default_role in"
        ),
        "{v:#}"
    );
    assert_eq!(snapshot(&dirs), before, "doctor wrote something");

    // With worktree_role = "agent", the agent's worktree is ok.
    s.run_ok(&s.bin(), &["install", "--worktree-role", "agent"], &main);
    let (_, v) = s.doctor_in(&s.bin(), &s.path(), &agent);
    assert!(
        has(
            &v,
            "ok",
            "sessions here start with role agent (worktree_role in"
        ),
        "{v:#}"
    );

    // A worktree with a .claude/skills of its own loads only that one.
    fs::create_dir_all(agent.join(".claude/skills")).unwrap();
    let (_, v) = s.doctor_in(&s.bin(), &s.path(), &agent);
    assert!(
        has(&v, "note", "skills not current (missing: .claude/skills/"),
        "{v:#}"
    );
    assert!(
        has(
            &v,
            "note",
            "`kontext install --skills-only` (this linked worktree has a .claude/skills of its own"
        ),
        "{v:#}"
    );
    // Agent files still come from the main checkout.
    assert!(
        has(
            &v,
            "ok",
            &format!(
                "agent files current in {}",
                main.join(".claude/agents").display()
            )
        ),
        "{v:#}"
    );
}

#[test]
fn hooks_only_in_settings_json_are_a_note_for_worktrees() {
    let (s, pm, _) = with_worktrees(&[]);
    let main = s.repo();
    let fix = format!("`kontext install --local` (in {})", main.display());
    let only = "which git does not track: a session in a linked git worktree reads \
                .claude/settings.json from the worktree";
    let (out, v) = s.doctor_in(&s.bin(), &s.path(), &pm);
    assert!(!out.status.success());
    // A worktree session does not read the main checkout's settings.json: the
    // problems are real, and their fix is --local in the main checkout.
    assert!(
        has(&v, "problem", "no `SessionStart` hook") && has(&v, "problem", &fix),
        "{v:#}"
    );
    assert!(has(&v, "note", only) && has(&v, "note", &fix), "{v:#}");
    // From the main checkout, which has linked worktrees: the same note.
    let (out, v) = s.doctor(&s.bin(), &s.path());
    assert!(out.status.success(), "{:?}", findings(&v, "problem"));
    assert!(has(&v, "note", only), "{v:#}");

    // Only the permission in settings.local.json: the hooks are still only
    // in settings.json.
    fs::write(
        main.join(".claude/settings.local.json"),
        "{\"permissions\": {\"allow\": [\"Bash(kontext:*)\"]}}",
    )
    .unwrap();
    let (_, v) = s.doctor(&s.bin(), &s.path());
    assert!(has(&v, "note", only), "{v:#}");
    assert!(!has(&v, "note", "in both"), "{v:#}");
    fs::remove_file(main.join(".claude/settings.local.json")).unwrap();

    // A settings.json that git tracks is in every worktree: no note.
    s.git(&main, &["add", ".claude/settings.json"]);
    s.git(&main, &["commit", "-q", "-m", "settings"]);
    let (_, v) = s.doctor(&s.bin(), &s.path());
    assert!(!has(&v, "note", "hooks are only in"), "{v:#}");
    s.git(&main, &["rm", "-q", "--cached", ".claude/settings.json"]);
    s.git(&main, &["commit", "-q", "-m", "untrack"]);

    // In both files (a copy by hand).
    let shared = fs::read_to_string(main.join(".claude/settings.json")).unwrap();
    fs::write(main.join(".claude/settings.local.json"), shared).unwrap();
    let (_, v) = s.doctor(&s.bin(), &s.path());
    assert!(has(&v, "note", "a copy that differs"), "{v:#}");
    let (out, v) = s.doctor_in(&s.bin(), &s.path(), &pm);
    assert!(out.status.success(), "{:?}", findings(&v, "problem"));
    // After --local, no note at all about where the hooks are.
    s.run_ok(&s.bin(), &["install", "--local"], &main);
    let (out, v) = s.doctor(&s.bin(), &s.path());
    assert!(out.status.success(), "{v:#}");
    assert!(
        !has(&v, "note", "in both") && !has(&v, "note", "hooks are only in"),
        "{v:#}"
    );
}

/// A repo whose `.kontext.toml` (with `worktree_role`) and
/// `.claude/settings.json` are committed, and a linked worktree of it.
#[test]
fn a_tracked_kontext_toml_gives_worktrees_their_role() {
    let s = bare();
    fs::write(s.config().join("projects.toml"), "writer = \"omar@test\"\n").unwrap();
    let home = s.khome();
    s.run_ok(
        &s.bin(),
        &["init", "demo", "--home", home.to_str().unwrap()],
        &s.root(),
    );
    let main = s.repo();
    s.run_ok(&s.bin(), &["install", "--worktree-role", "agent"], &main);
    s.git(&main, &["add", ".kontext.toml", ".claude"]);
    s.git(&main, &["commit", "-q", "-m", "kontext"]);
    let lane = s.worktree("lane-1");
    assert!(lane.join(".kontext.toml").exists());

    let (out, v) = s.doctor_in(&s.bin(), &s.path(), &lane);
    assert!(out.status.success(), "{:?}", findings(&v, "problem"));
    for (level, needle) in [
        (
            "ok",
            format!(
                "project demo (from {}/.kontext.toml, which git tracks",
                lane.display()
            ),
        ),
        (
            "ok",
            "sessions here start with role agent (worktree_role in".into(),
        ),
        (
            "ok",
            format!(
                "skills current in {}",
                lane.join(".claude/skills").display()
            ),
        ),
    ] {
        assert!(has(&v, level, &needle), "{needle}: {v:#}");
    }
    // The settings.json is tracked: the lane has its own, and no note.
    assert!(!has(&v, "note", "hooks are only in"), "{v:#}");
    let (out, v) = s.doctor(&s.bin(), &s.path());
    assert!(out.status.success(), "{:?}", findings(&v, "problem"));
    assert!(
        has(
            &v,
            "ok",
            "sessions here start with role pm (default_role in"
        ) && !has(&v, "note", "hooks are only in"),
        "{v:#}"
    );

    // A tracked file without worktree_role: the lane starts as the PM, and
    // --link-only cannot mark it, so the fix is worktree_role.
    let lane2 = s.worktree("lane-2");
    fs::write(
        lane2.join(".kontext.toml"),
        "project = \"demo\"\ndefault_role = \"pm\"\n",
    )
    .unwrap();
    s.git(&lane2, &["commit", "-q", "-am", "no worktree_role"]);
    let (_, v) = s.doctor_in(&s.bin(), &s.path(), &lane2);
    assert!(
        has(
            &v,
            "note",
            "sessions here start with role pm (default_role in"
        ) && has(
            &v,
            "note",
            &format!(
                "`kontext install --worktree-role agent` in {}, and commit the .kontext.toml",
                main.display()
            )
        ) && !has(
            &v,
            "note",
            "`kontext install --link-only` in the worktree where"
        ),
        "{v:#}"
    );
}
