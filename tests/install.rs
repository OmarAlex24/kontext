//! `kontext install`: idempotent, leaves what is not ours alone, and `--dry-run` writes nothing.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Project;

fn repo(p: &Project) -> PathBuf {
    let r = p.dir.path().join("billing-app");
    fs::create_dir_all(r.join(".claude")).unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&r)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:omar/billing-app.git",
    ]);
    fs::write(
        r.join(".claude/settings.json"),
        "{\n  \"permissions\": {\"allow\": [\"Bash(ls:*)\"]},\n  \"zeta\": 1,\n  \"alpha\": true,\n  \"hooks\": {\"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo hola\"}]}]}\n}\n",
    )
    .unwrap();
    r
}

fn install(p: &Project, dir: &Path, args: &[&str]) -> Output {
    let mut a = vec!["install"];
    a.extend_from_slice(args);
    p.cmd(&a).current_dir(dir).output().unwrap()
}

fn snapshot(p: &Project, r: &Path) -> Vec<(String, Option<String>)> {
    [
        r.join(".kontext.toml"),
        r.join(".claude/settings.json"),
        r.join(".claude/skills/kontext/SKILL.md"),
        r.join(".claude/skills/kontext-orchestrate/SKILL.md"),
        p.home.join("project.toml"),
    ]
    .iter()
    .map(|f| (f.display().to_string(), fs::read_to_string(f).ok()))
    .collect()
}

#[test]
fn dry_run_writes_nothing_then_install_is_idempotent() {
    let p = Project::new();
    let r = repo(&p);
    let before = snapshot(&p, &r);
    let out = install(&p, &r, &["--dry-run"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("+  \"hooks\"") || stdout.contains("\"SessionStart\""),
        "{stdout}"
    );
    assert!(stdout.contains("+project = \"demo\""), "{stdout}");
    assert!(stdout.contains("+name = \"billing-app\""), "{stdout}");
    assert!(stdout.contains("nothing was written"), "{stdout}");
    assert_eq!(snapshot(&p, &r), before);
    assert!(!r.join(".claude/skills").exists());

    let out = install(&p, &r, &["--role", "pm"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let settings = fs::read_to_string(r.join(".claude/settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&settings).unwrap();
    // What was already there stays the same, in the same order.
    assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
    assert_eq!(v["zeta"], 1);
    assert_eq!(
        v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "echo hola"
    );
    let (pz, pa, ph) = (
        settings.find("\"zeta\"").unwrap(),
        settings.find("\"alpha\"").unwrap(),
        settings.find("\"hooks\"").unwrap(),
    );
    assert!(pz < pa && pa < ph, "{settings}");
    let ss = &v["hooks"]["SessionStart"][0];
    assert_eq!(ss["matcher"], "startup|resume|clear|compact");
    assert_eq!(ss["hooks"][0]["timeout"], 15);
    let cmd = ss["hooks"][0]["command"].as_str().unwrap();
    assert!(
        cmd.ends_with("kontext hook session-start") && cmd.starts_with('/'),
        "{cmd}"
    );

    // With the distill on (what `kontext init` leaves): PreCompact and SessionEnd.
    for (event, sub) in [("PreCompact", "pre-compact"), ("SessionEnd", "session-end")] {
        let h = &v["hooks"][event][0];
        assert!(h.get("matcher").is_none(), "{h}");
        let c = h["hooks"][0]["command"].as_str().unwrap();
        assert!(
            c.ends_with(&format!("kontext hook {sub}")) && c.starts_with('/'),
            "{c}"
        );
    }

    let skill = fs::read_to_string(r.join(".claude/skills/kontext/SKILL.md")).unwrap();
    assert!(
        skill.starts_with("---\nname: kontext\ndescription: "),
        "{skill}"
    );
    assert!(skill.lines().count() <= 80);
    // The orchestrate skill, next to it and in the same way.
    let orq = fs::read_to_string(r.join(".claude/skills/kontext-orchestrate/SKILL.md")).unwrap();
    assert!(
        orq.starts_with("---\nname: kontext-orchestrate\ndescription: "),
        "{orq}"
    );
    let front = orq.split("---").nth(1).unwrap();
    for trigger in [
        "from scratch",
        "several agents",
        "this is a lot of work",
        "orchestrate",
    ] {
        assert!(front.contains(trigger), "{trigger}: {front}");
    }
    for needed in [
        "| **XXL** (greenfield)",
        "kontext plan",
        "--after",
        "delegate_task",
        "Workflow",
        "kontext-implementer",
        "kontext done <T> --run",
        "--since <mark>",
        "kontext ask --to pm",
        "Shall I start?",
        "stale context",
    ] {
        assert!(orq.contains(needed), "missing {needed:?}");
    }
    assert!(orq.lines().count() <= 150, "{} lines", orq.lines().count());
    assert_eq!(
        fs::read_to_string(r.join(".kontext.toml")).unwrap(),
        "project = \"demo\"\ndefault_role = \"pm\"\n"
    );
    let toml = fs::read_to_string(p.home.join("project.toml")).unwrap();
    assert!(
        toml.contains(
            "[[repos]]\nname = \"billing-app\"\nremote = \"github.com/omar/billing-app\"\n"
        ),
        "{toml}"
    );
    // The rest of project.toml keeps its comments.
    assert!(
        toml.contains("require_signatures = false        # phase 5"),
        "{toml}"
    );

    // Second time: nothing to change.
    let after = snapshot(&p, &r);
    let out = install(&p, &r, &[]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to change"));
    assert_eq!(snapshot(&p, &r), after);

    // From a subfolder of the repo, the same.
    let sub = r.join("src/deep");
    fs::create_dir_all(&sub).unwrap();
    let out = install(&p, &sub, &[]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to change"));
}

#[test]
fn refuses_invalid_settings_json() {
    let p = Project::new();
    let r = repo(&p);
    fs::write(r.join(".claude/settings.json"), "{ this is not json").unwrap();
    let before = snapshot(&p, &r);
    let out = install(&p, &r, &[]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("is not valid JSON"), "{err}");
    assert_eq!(snapshot(&p, &r), before);
}

#[test]
fn keeps_existing_kontext_toml() {
    let p = Project::new();
    let r = repo(&p);
    fs::write(
        r.join(".kontext.toml"),
        "project = \"demo\"\ndefault_role = \"coordinator\"\n",
    )
    .unwrap();
    assert!(install(&p, &r, &[]).status.success());
    assert_eq!(
        fs::read_to_string(r.join(".kontext.toml")).unwrap(),
        "project = \"demo\"\ndefault_role = \"coordinator\"\n"
    );
}

#[test]
fn installs_agent_files_and_keeps_edited_ones() {
    let p = Project::new();
    let r = repo(&p);
    let out = install(&p, &r, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let agents = r.join(".claude/agents");
    for name in [
        "kontext-coordinator",
        "kontext-implementer",
        "kontext-tester",
        "kontext-scout",
    ] {
        let text = fs::read_to_string(agents.join(format!("{name}.md"))).unwrap();
        assert!(
            text.starts_with(&format!("---\nname: {name}\ndescription: ")),
            "{text}"
        );
        let front = text.split("---").nth(1).unwrap();
        assert!(!front.contains("model:"), "{name}: {front}");
        assert!(text.contains("<!-- No `model` key"), "{name}");
        assert!(
            text.lines().count() <= 60,
            "{name}: {} lines",
            text.lines().count()
        );
        assert!(text.contains("kontext "), "{name}");
    }
    let scout = fs::read_to_string(agents.join("kontext-scout.md")).unwrap();
    assert!(scout.contains("tools: Read, Grep, Glob, Bash\n"), "{scout}");
    let coord = fs::read_to_string(agents.join("kontext-coordinator.md")).unwrap();
    assert!(coord.contains("next --epic <name> --claim"), "{coord}");
    assert!(
        coord.contains("kontext --actor agent:impl-1 --role implementer"),
        "{coord}"
    );
    assert!(coord.contains("kontext ask \"…\" --to pm"), "{coord}");
    let skill = fs::read_to_string(r.join(".claude/skills/kontext/SKILL.md")).unwrap();
    assert!(
        skill.contains("kontext epic add") && skill.contains("kontext-coordinator"),
        "{skill}"
    );

    // Idempotente.
    let out = install(&p, &r, &[]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to change"));

    // An edited file is kept and a warning is shown; --force replaces it.
    let edited = agents.join("kontext-implementer.md");
    fs::write(&edited, "my version\n").unwrap();
    fs::remove_file(agents.join("kontext-tester.md")).unwrap();
    let out = install(&p, &r, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Kept") && stdout.contains("kontext-implementer.md"),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&edited).unwrap(), "my version\n");
    assert!(agents.join("kontext-tester.md").exists());
    let out = install(&p, &r, &["--force"]);
    assert!(out.status.success());
    assert!(
        fs::read_to_string(&edited)
            .unwrap()
            .starts_with("---\nname: kontext-implementer")
    );
    let out = install(&p, &r, &[]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to change"));
}

/// `install --skills-only --global`: only the skills, in your user's folder,
/// without a project. `--dry-run` writes nothing, and the second time there is nothing to do.
#[test]
fn skills_only_global_needs_no_project() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("h");
    fs::create_dir_all(&home).unwrap();
    let run = |args: &[&str]| {
        Command::new(common::BIN)
            .args(args)
            .current_dir(dir.path())
            .env("HOME", &home)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env("KONTEXT_CONFIG_DIR", dir.path().join("no-config"))
            .output()
            .unwrap()
    };
    let skills = home.join(".claude/skills");
    let out = run(&["install", "--skills-only", "--global", "--dry-run"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("kontext-migrate/SKILL.md (new)"), "{text}");
    assert!(!skills.exists());

    let out = run(&["install", "--skills-only", "--global"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for s in [
        "kontext",
        "kontext-orchestrate",
        "kontext-show",
        "kontext-migrate",
    ] {
        let t = fs::read_to_string(skills.join(s).join("SKILL.md")).unwrap();
        assert!(t.starts_with(&format!("---\nname: {s}\n")), "{s}: {t}");
    }
    let out = run(&["install", "--skills-only", "--global"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("already up to date"));
    // --global without --skills-only is not accepted.
    assert!(!run(&["install", "--global"]).status.success());
}
