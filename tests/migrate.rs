//! `kontext migrate` and the hint for an install of the old `ctx` command: a
//! fake machine with its own `HOME`, without touching the real one.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::BIN;

struct Old {
    _dir: tempfile::TempDir,
    home: PathBuf,
    /// The project's kontext-home (under the old name, in ~/ctx-home).
    project: PathBuf,
    repo: PathBuf,
}

/// A kontext command with its own `HOME` and without inherited kontext or
/// ctx variables.
fn kontext(home: &Path, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(dir).env("HOME", home);
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if k.starts_with("KONTEXT_") || k.starts_with("CTX_") {
            c.env_remove(&k);
        }
    }
    c.env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"));
    c
}

fn ok(out: Output) -> String {
    assert!(
        out.status.success(),
        "failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
}

/// What the old command left behind: `~/.config/ctx/projects.toml`, the home
/// in `~/ctx-home/demo` with its `.ctx/` folder, and a repo with `.ctx.toml`,
/// the hook and the `ctx` permission in `.claude/settings.json`, the `ctx`
/// skill, and an agent file changed by hand.
fn old_install() -> Old {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("h");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join(".gitconfig"),
        "[user]\n\tname = Omar\n\temail = omar@example.com\n",
    )
    .unwrap();
    let project = home.join("ctx-home").join("demo");
    let old_config = home.join(".config").join("ctx");
    fs::create_dir_all(&old_config).unwrap();
    fs::write(old_config.join("projects.toml"), "writer = \"omar@old\"\n").unwrap();
    // The new binary, pointed at the old folder, leaves the same as the old one did.
    ok(kontext(
        &home,
        &home,
        &["init", "demo", "--home", project.to_str().unwrap()],
    )
    .env("KONTEXT_CONFIG_DIR", &old_config)
    .output()
    .unwrap());
    ok(kontext(
        &home,
        &home,
        &[
            "--actor",
            "human:omar",
            "charter",
            "--objective",
            "Charge by card",
        ],
    )
    .env("KONTEXT_CONFIG_DIR", &old_config)
    .output()
    .unwrap());
    fs::rename(project.join(".kontext"), project.join(".ctx")).unwrap();
    fs::remove_file(project.join(".ctx/.gitignore")).unwrap();
    fs::write(project.join(".gitignore"), ".ctx/\n").unwrap();
    let mut toml = fs::read_to_string(old_config.join("projects.toml")).unwrap();
    toml.push_str("\n[signing]\nkey = \"~/.config/ctx/id_ed25519.pub\"\n");
    fs::write(old_config.join("projects.toml"), toml).unwrap();

    let repo = home.join("code").join("app");
    fs::create_dir_all(repo.join(".claude/skills/ctx")).unwrap();
    fs::create_dir_all(repo.join(".claude/agents")).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join(".ctx.toml"),
        "project = \"demo\"\ndefault_role = \"pm\"\n",
    )
    .unwrap();
    fs::write(
        repo.join(".claude/settings.json"),
        r#"{
  "zeta": 1,
  "permissions": {"allow": ["Bash(ls:*)", "Bash(ctx:*)"]},
  "hooks": {
    "SessionStart": [{"matcher": "startup|resume|clear|compact", "hooks": [{"type": "command", "command": "/home/x/.cargo/bin/ctx hook session-start", "timeout": 15}]}],
    "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo hola"}]}]
  }
}
"#,
    )
    .unwrap();
    fs::write(
        repo.join(".claude/skills/ctx/SKILL.md"),
        "---\nname: ctx\n---\nold\n",
    )
    .unwrap();
    fs::write(
        repo.join(".claude/agents/ctx-scout.md"),
        "my scout, changed by hand\n",
    )
    .unwrap();
    Old {
        _dir: dir,
        home,
        project,
        repo,
    }
}

fn snapshot(o: &Old) -> Vec<(PathBuf, bool)> {
    [
        o.home.join(".config/ctx/projects.toml"),
        o.home.join(".config/kontext"),
        o.project.join(".ctx"),
        o.project.join(".kontext"),
        o.repo.join(".ctx.toml"),
        o.repo.join(".kontext.toml"),
        o.repo.join(".claude/skills/kontext"),
    ]
    .into_iter()
    .map(|p| {
        let e = p.exists();
        (p, e)
    })
    .collect()
}

#[test]
fn old_install_is_noticed_without_breaking_the_hook() {
    let o = old_install();

    // A command that cannot find its config says so in one line.
    let out = kontext(&o.home, &o.repo, &["status"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("kontext migrate --dry-run"), "{err}");
    assert!(err.contains(".config/ctx/projects.toml"), "{err}");
    assert!(err.contains(".ctx.toml"), "{err}");

    // The start hook exits with 0, with nothing on stderr, and tells the session.
    let mut child = kontext(&o.home, &o.repo, &["hook", "session-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            format!(
                r#"{{"session_id": "s1", "source": "startup", "cwd": "{}"}}"#,
                o.repo.display()
            )
            .as_bytes(),
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("kontext cannot find its configuration"),
        "{text}"
    );
    assert!(text.contains("kontext migrate"), "{text}");

    // The old variables in the environment: a one-line warning.
    let out = kontext(&o.home, &o.repo, &["status"])
        .env("CTX_ACTOR", "human:omar")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("CTX_ACTOR in the environment does not count"),
        "{err}"
    );
    assert_eq!(err.matches("CTX_ACTOR").count(), 1, "only once: {err}");
    let out = kontext(&o.home, &o.repo, &["status"])
        .env("CTX_ACTOR", "human:omar")
        .env("KONTEXT_ACTOR", "human:omar")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("CTX_ACTOR"), "{err}");
}

#[test]
fn migrate_dry_run_then_apply_then_noop() {
    let o = old_install();
    let before = snapshot(&o);
    let settings_before = fs::read_to_string(o.repo.join(".claude/settings.json")).unwrap();

    let out = ok(kontext(&o.home, &o.repo, &["migrate", "--dry-run"])
        .output()
        .unwrap());
    assert!(out.starts_with("Paths it touches:"), "{out}");
    for p in [
        o.home.join(".config/ctx"),
        o.home.join(".config/kontext"),
        o.project.join(".ctx"),
        o.repo.join(".kontext.toml"),
        o.repo.join(".claude/settings.json"),
        o.repo.join(".claude/skills/kontext/SKILL.md"),
    ] {
        assert!(out.contains(&p.display().to_string()), "{p:?}\n{out}");
    }
    // The settings.json diff changes only those strings, without reformatting.
    assert!(
        out.contains("+  \"permissions\": {\"allow\": [\"Bash(ls:*)\", \"Bash(kontext:*)\"]},"),
        "{out}"
    );
    assert!(
        !out.contains("+name: kontext"),
        "without the diff of the templates: {out}"
    );
    assert!(out.contains("Kept"), "{out}");
    assert!(out.contains("nothing was written"), "{out}");
    assert_eq!(snapshot(&o), before);
    assert_eq!(
        fs::read_to_string(o.repo.join(".claude/settings.json")).unwrap(),
        settings_before
    );

    let out = ok(kontext(&o.home, &o.repo, &["migrate"]).output().unwrap());
    assert!(out.contains("Done."), "{out}");

    // The configuration moved, with the key path up to date.
    assert!(!o.home.join(".config/ctx").exists());
    let machine = fs::read_to_string(o.home.join(".config/kontext/projects.toml")).unwrap();
    assert!(machine.contains("writer = \"omar@old\""), "{machine}");
    assert!(
        machine.contains("~/.config/kontext/id_ed25519.pub"),
        "{machine}"
    );
    assert!(
        machine.contains(&o.project.display().to_string()),
        "the home does not move: {machine}"
    );
    // The internal folder, with its own .gitignore: git does not see it even
    // though the home's .gitignore says .ctx/.
    assert!(!o.project.join(".ctx").exists());
    assert!(o.project.join(".kontext/index.sqlite").exists());
    assert_eq!(
        fs::read_to_string(o.project.join(".kontext/.gitignore")).unwrap(),
        "*\n"
    );
    // The repo.
    assert!(!o.repo.join(".ctx.toml").exists());
    assert!(o.repo.join(".kontext.toml").exists());
    let settings = fs::read_to_string(o.repo.join(".claude/settings.json")).unwrap();
    assert!(settings.contains("Bash(kontext:*)"), "{settings}");
    assert!(!settings.contains("Bash(ctx:*)"), "{settings}");
    assert!(
        settings.contains("kontext hook session-start"),
        "{settings}"
    );
    assert!(!settings.contains("/ctx hook"), "{settings}");
    assert!(
        settings.contains("echo hola") && settings.contains("\"zeta\": 1"),
        "{settings}"
    );
    assert!(
        settings.find("\"zeta\"").unwrap() < settings.find("\"permissions\"").unwrap(),
        "{settings}"
    );
    assert!(!o.repo.join(".claude/skills/ctx").exists());
    assert!(o.repo.join(".claude/skills/kontext/SKILL.md").exists());
    assert!(
        o.repo.join(".claude/agents/ctx-scout.md").exists(),
        "changed by hand: it is kept"
    );

    // kontext now finds everything.
    let out = ok(kontext(&o.home, &o.repo, &["verify"]).output().unwrap());
    assert!(out.contains("OK"), "{out}");
    let out = ok(
        kontext(&o.home, &o.repo, &["resume", "--as", "pm", "--peek"])
            .output()
            .unwrap(),
    );
    assert!(out.contains("Charge by card"), "{out}");
    let out = kontext(&o.home, &o.repo, &["status"]).output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Again: nothing to do (except the notice about the agent changed by hand).
    let out = ok(kontext(&o.home, &o.repo, &["migrate"]).output().unwrap());
    assert!(
        out.starts_with("Nothing from the old `ctx` command to migrate."),
        "{out}"
    );
}

#[test]
fn migrate_merges_into_an_existing_kontext_config() {
    let o = old_install();
    // Someone already used kontext on this machine with another project.
    let new_config = o.home.join(".config/kontext");
    fs::create_dir_all(&new_config).unwrap();
    fs::write(
        new_config.join("projects.toml"),
        "writer = \"omar@new\"\n\n[projects.other]\nhome = \"/tmp/other\"\n",
    )
    .unwrap();
    // and already opened the old home (it left a .kontext folder).
    fs::create_dir_all(o.project.join(".kontext")).unwrap();
    fs::write(o.project.join(".kontext/hooks.log"), "new\n").unwrap();
    fs::write(o.project.join(".ctx/hooks.log"), "old\n").unwrap();

    ok(kontext(&o.home, &o.repo, &["migrate"]).output().unwrap());
    let machine = fs::read_to_string(new_config.join("projects.toml")).unwrap();
    assert!(machine.contains("writer = \"omar@new\""), "{machine}");
    assert!(machine.contains("[projects.demo]"), "{machine}");
    assert!(machine.contains("[projects.other]"), "{machine}");
    assert!(!o.home.join(".config/ctx").exists());
    assert!(o.home.join(".config/ctx.migrated/projects.toml").exists());
    // What clashed is set aside; the rest moved.
    assert!(!o.project.join(".ctx").exists());
    assert_eq!(
        fs::read_to_string(o.project.join(".kontext/hooks.log")).unwrap(),
        "new\n"
    );
    assert_eq!(
        fs::read_to_string(o.project.join(".ctx.migrated/hooks.log")).unwrap(),
        "old\n"
    );
    assert!(o.project.join(".kontext/index.sqlite").exists());
    let out = ok(kontext(&o.home, &o.repo, &["migrate"]).output().unwrap());
    assert!(out.starts_with("Nothing"), "{out}");
}
