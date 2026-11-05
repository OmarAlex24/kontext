//! Cursors (DESIGN §7.5): what is new is exact even when entries arrive
//! late, `--peek` does not advance it and neither does the hook.

mod common;

use std::fs;

use common::Project;
use serde_json::Value;

fn standup_json(p: &Project, peek: bool) -> Value {
    let mut args = vec!["--json", "standup", "--as", "omar"];
    if peek {
        args.push("--peek");
    }
    serde_json::from_str(&p.ctx_ok(&args)).unwrap()
}

#[test]
fn late_arrival_with_older_hlc_still_shows_as_new() {
    let p = Project::new();
    p.ctx_ok(&["log", "one from omar"]);
    let ana = p.machine("ana@laptop");
    let out = p
        .spawn_append(
            &ana,
            "{\"kind\":\"journal\",\"title\":\"one from ana\"}\n",
            0,
        )
        .wait_with_output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(standup_json(&p, false)["new_entries"], 2);

    // The cursor is a vector per file, written at once.
    let cursor = p.home.join("cursors/omar/omar@test.json");
    let v: Value = serde_json::from_str(&fs::read_to_string(&cursor).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"ana@laptop": 1, "omar@test": 1}));
    let names: Vec<String> = fs::read_dir(cursor.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["omar@test.json"]);
    assert_eq!(standup_json(&p, false)["new_entries"], 0);

    // An entry from ana written earlier (older hlc) arrives through git, which
    // that omar already saw.
    let old = "{\"kind\":\"decision\",\"title\":\"old but new\",\"hlc\":\"2020-01-01T00:00:00.000Z.0000\",\"actor\":{\"type\":\"human\",\"name\":\"omar\"}}\n";
    let out = p.spawn_append(&ana, old, 0).wait_with_output().unwrap();
    assert!(out.status.success());
    let s = standup_json(&p, false);
    assert_eq!(s["new_entries"], 1, "{s}");
    assert_eq!(
        s["decisions"][0]["text"]
            .as_str()
            .unwrap()
            .split(' ')
            .next(),
        Some("old")
    );
}

#[test]
fn peek_does_not_advance() {
    let p = Project::new();
    p.ctx_ok(&["log", "something"]);
    assert_eq!(standup_json(&p, true)["new_entries"], 1);
    assert_eq!(standup_json(&p, true)["new_entries"], 1);
    assert!(!p.home.join("cursors/omar").exists());
    p.ctx_ok(&["resume", "--as", "pm", "--peek"]);
    assert!(!p.home.join("cursors/pm").exists());
    // resume without --peek advances the role's cursor.
    p.ctx_ok(&["resume", "--as", "pm"]);
    let v: Value = serde_json::from_str(
        &fs::read_to_string(p.home.join("cursors/pm/omar@test.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["omar@test"], 1);
    let out = p.ctx_ok(&["resume", "--as", "pm"]);
    assert!(
        out.contains("## What's new since your last time (0)"),
        "{out}"
    );
}

#[test]
fn readers_are_independent_and_machines_merge() {
    let p = Project::new();
    p.ctx_ok(&["log", "uno"]);
    p.ctx_ok(&["standup"]);
    // ana reads from her machine with another reader: her cursor is different.
    let ana = p.machine("ana@laptop");
    let out = p
        .cmd_with(&ana, &["--json", "standup", "--as", "ana", "--peek"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["new_entries"], 1);
    // The same reader on another machine uses the maximum of its files.
    let out = p
        .cmd_with(&ana, &["--json", "standup", "--as", "omar", "--peek"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["new_entries"], 0);
}
