//! Helpers for the integration tests: a project in a temporary folder
//! and the `kontext` binary with its own isolated configuration.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

pub const BIN: &str = env!("CARGO_BIN_EXE_kontext");

pub struct Project {
    pub dir: tempfile::TempDir,
    pub home: PathBuf,
    pub config: PathBuf,
}

impl Project {
    /// `kontext init demo` with its own `KONTEXT_CONFIG_DIR` and writer `omar@test`.
    pub fn new() -> Project {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let config = dir.path().join("config");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("projects.toml"), "writer = \"omar@test\"\n").unwrap();
        let p = Project { dir, home, config };
        let out = p.kontext_ok(&["init", "demo", "--home", p.home.to_str().unwrap()]);
        assert!(out.contains("Project demo created"), "{out}");
        // The owner comes from `git config user.name`; in the tests it is omar.
        let toml = p.home.join("project.toml");
        let text = fs::read_to_string(&toml).unwrap();
        let text = text
            .lines()
            .map(|l| {
                if l.starts_with("owners = ") {
                    "owners = [\"omar\"]".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&toml, text).unwrap();
        p
    }

    /// Runs `kontext` with other environment variables.
    pub fn kontext_env(&self, envs: &[(&str, &str)], args: &[&str]) -> Output {
        let mut c = self.cmd(args);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    /// Like [`Project::kontext_env`], requiring success. Returns stdout.
    pub fn kontext_env_ok(&self, envs: &[(&str, &str)], args: &[&str]) -> String {
        let out = self.kontext_env(envs, args);
        assert!(
            out.status.success(),
            "kontext {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs as an agent.
    pub fn agent_ok(&self, name: &str, args: &[&str]) -> String {
        self.kontext_env_ok(&[("KONTEXT_ACTOR", &format!("agent:{name}"))], args)
    }

    /// The `id` of what a command wrote (`--json`).
    pub fn id_of(&self, envs: &[(&str, &str)], args: &[&str]) -> String {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let out = self.kontext_env_ok(envs, &a);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    /// Another machine's configuration over the same kontext-home, with another writer.
    pub fn machine(&self, writer: &str) -> PathBuf {
        let c = self.dir.path().join(format!("config-{writer}"));
        fs::create_dir_all(&c).unwrap();
        fs::write(
            c.join("projects.toml"),
            format!(
                "writer = \"{writer}\"\n\n[projects.demo]\nhome = \"{}\"\n",
                self.home.display()
            ),
        )
        .unwrap();
        c
    }

    pub fn cmd_with(&self, config: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("KONTEXT_CONFIG_DIR", config)
            .env("KONTEXT_PROJECT", "demo")
            .env("KONTEXT_ACTOR", "human:omar")
            .current_dir(self.dir.path());
        c
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        self.cmd_with(&self.config, args)
    }

    pub fn kontext(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Runs `kontext` and requires success. Returns stdout.
    pub fn kontext_ok(&self, args: &[&str]) -> String {
        let out = self.kontext(args);
        assert!(
            out.status.success(),
            "kontext {args:?} failed: {}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// `kontext debug append` with these lines on stdin, without waiting.
    pub fn spawn_append(&self, config: &Path, lines: &str, start_at: i64) -> Child {
        let mut child = self
            .cmd_with(
                config,
                &["debug", "append", "--start-at", &start_at.to_string()],
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(lines.as_bytes())
            .unwrap();
        child
    }

    pub fn append(&self, lines: &str) -> Vec<String> {
        let out = self
            .spawn_append(&self.config, lines, 0)
            .wait_with_output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn ledger(&self, writer: &str) -> PathBuf {
        self.home.join("ledger").join(format!("{writer}.jsonl"))
    }

    /// The lines of a ledger file, already parsed.
    pub fn entries(&self, writer: &str) -> Vec<serde_json::Value> {
        fs::read_to_string(self.ledger(writer))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// A moment near in the future, so that the processes start together.
pub fn soon(ms: i64) -> i64 {
    jiff::Timestamp::now().as_millisecond() + ms
}

/// A project with a signed charter and goal, and the epic `payments` (with that goal).
/// Returns the project, the goal and the epic.
pub fn with_epic() -> (Project, String, String) {
    let p = Project::new();
    let a = p.id_of(&[], &["charter", "--objective", "Charge by card"]);
    p.kontext_ok(&["sign", &a]);
    let g = p.id_of(&[], &["goal", "add", "Recurring charge"]);
    p.kontext_ok(&["sign", &g]);
    let e = p.id_of(
        &[],
        &[
            "epic",
            "add",
            "Card payments",
            "--goal",
            &g,
            "--name",
            "payments",
        ],
    );
    (p, g, e)
}

/// Variables of an implementer agent.
pub fn implementer(name: &str) -> Vec<(&'static str, String)> {
    vec![
        ("KONTEXT_ACTOR", format!("agent:{name}")),
        ("KONTEXT_ROLE", "implementer".to_string()),
    ]
}

impl Project {
    /// Like [`Project::kontext_env`], with variables that are `String`.
    pub fn run_as(&self, envs: &[(&'static str, String)], args: &[&str]) -> Output {
        let e: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.kontext_env(&e, args)
    }

    pub fn run_as_ok(&self, envs: &[(&'static str, String)], args: &[&str]) -> String {
        let e: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.kontext_env_ok(&e, args)
    }

    /// The JSON of `kontext debug state`.
    pub fn state(&self) -> serde_json::Value {
        serde_json::from_str(&self.kontext_ok(&["debug", "state"])).unwrap()
    }

    /// `kontext --json next --epic <e> --claim` as `name`: (task, mark).
    pub fn claim(&self, name: &str, epic: &str) -> (String, String) {
        let out = self.run_as_ok(
            &implementer(name),
            &["--json", "next", "--epic", epic, "--claim"],
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        (
            v["task"]["id"].as_str().unwrap().to_string(),
            v["mark"].as_str().unwrap().to_string(),
        )
    }
}

/// `git` in `dir`, without depending on the global configuration (signing, author).
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Prueba",
            "-c",
            "user.email=prueba@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Writes a file (creating folders) and leaves it in a commit.
pub fn commit_file(repo: &Path, path: &str, content: &str) -> String {
    let p = repo.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", &format!("cambia {path}")]);
    git(repo, &["rev-parse", "HEAD"])
}

impl Project {
    /// A git repo in `<tmp>/code/<name>` with a first commit, added to
    /// `[[repos]]` of project.toml (no remote: it is recognized by its folder).
    pub fn code_repo(&self, name: &str) -> PathBuf {
        let repo = self.dir.path().join("code").join(name);
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        commit_file(&repo, "README.md", &format!("# {name}\n"));
        self.add_repo_toml(&format!("[[repos]]\nname = \"{name}\"\n"));
        repo
    }

    /// Appends text to the end of project.toml.
    pub fn add_repo_toml(&self, text: &str) {
        let toml = self.home.join("project.toml");
        let mut s = fs::read_to_string(&toml).unwrap();
        s.push('\n');
        s.push_str(text);
        fs::write(&toml, s).unwrap();
    }

    /// Runs `kontext` in another folder.
    pub fn kontext_in(&self, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> Output {
        let mut c = self.cmd(args);
        c.current_dir(dir);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    pub fn kontext_in_ok(&self, dir: &Path, envs: &[(&str, &str)], args: &[&str]) -> String {
        let out = self.kontext_in(dir, envs, args);
        assert!(
            out.status.success(),
            "kontext {args:?} in {} failed: {}\n{}",
            dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Says in projects.toml where a repo is on this machine.
    pub fn map_repo(&self, name: &str, path: &Path) {
        let p = self.config.join("projects.toml");
        let mut s = fs::read_to_string(&p).unwrap();
        s.push_str(&format!(
            "\n[projects.demo.repos]\n{name} = \"{}\"\n",
            path.display()
        ));
        fs::write(&p, s).unwrap();
    }
}
