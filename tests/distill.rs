//! `kontext distill` with a fake model (a shell script): a real model is never
//! called.

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::Project;
use serde_json::{Value, json};

/// The fake model: saves each prompt in `calls/` and replies with `reply.txt`
/// (or sleeps if `sleep` exists).
fn fake_model(p: &Project) -> PathBuf {
    let dir = p.dir.path().join("fake");
    fs::create_dir_all(dir.join("calls")).unwrap();
    let script = dir.join("model.sh");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
D="{d}"
cat > "$D/calls/$(date +%s%N).txt"
if [ -f "$D/sleep" ]; then
  sleep 30 &
  echo $! > "$D/sleep.pid"
  wait
fi
cat "$D/reply.txt"
"#,
            d = dir.display()
        ),
    )
    .unwrap();
    dir
}

fn set_distill(p: &Project, command: &str, extra: &str) {
    let toml = p.home.join("project.toml");
    let text = fs::read_to_string(&toml).unwrap();
    let text = text
        .lines()
        .map(|l| {
            if l.starts_with("command = ") {
                format!("command = {}\n{extra}", toml::Value::from(command))
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&toml, text).unwrap();
}

fn reply(dir: &Path, v: &Value) {
    fs::write(dir.join("reply.txt"), v.to_string()).unwrap();
}

fn calls(dir: &Path) -> Vec<String> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir.join("calls"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    v.sort();
    v.iter().map(|p| fs::read_to_string(p).unwrap()).collect()
}

fn line(kind: &str, text: &str, ts: &str) -> String {
    json!({"type": kind, "timestamp": ts, "message": {"role": kind, "content": text}}).to_string()
        + "\n"
}

fn transcript(p: &Project) -> PathBuf {
    let t = p.dir.path().join("sesion.jsonl");
    let mut s = String::new();
    s.push_str(&line(
        "user",
        "Usemos Stripe para cobrar. La llave es sk-live-abcdefghijklmnop1234 y password=hunter2",
        "2026-11-03T10:00:00.000Z",
    ));
    s.push_str(
        &(json!({"type": "user", "isMeta": true, "message": {"content": "meta"}}).to_string()
            + "\n"),
    );
    s.push_str(&line(
        "assistant",
        "Listo: Stripe con 3DS. ¿En qué moneda cobramos?",
        "2026-11-03T10:01:00.000Z",
    ));
    fs::write(&t, s).unwrap();
    t
}

fn model_reply() -> Value {
    json!({
        "decisions": [{"title": "Cobrar con Stripe", "why": "3DS", "body": "", "tags": ["payments"]}],
        "questions": [{"title": "¿En qué moneda se cobra?", "body": "", "tags": []}],
        "facts": [{"text": "Stripe necesita 3DS para tarjetas europeas", "tags": ["payments"]}],
        "journal": [{"summary": "Se eligió Stripe", "next": "webhooks"}]
    })
}

fn entries_by(p: &Project, name: &str) -> Vec<Value> {
    if !p.ledger("omar@test").exists() {
        return Vec::new();
    }
    p.entries("omar@test")
        .into_iter()
        .filter(|e| e["actor"]["name"] == name)
        .collect()
}

#[test]
fn distill_writes_proposals_incrementally_and_dedupes() {
    let p = Project::new();
    let fake = fake_model(&p);
    let script = fake.join("model.sh");
    set_distill(
        &p,
        &format!("sh {}", script.display()),
        "model = \"fake-1\"",
    );
    reply(&fake, &model_reply());
    let t = transcript(&p);
    let ts = t.to_str().unwrap();

    // Prueba en seco: no llama al modelo ni mueve el offset.
    let out = p.ctx_ok(&["distill", "--transcript", ts, "--dry-run"]);
    assert!(out.contains("dry run"), "{out}");
    assert!(
        out.contains("secrets removed: kv_secret=1, sk_key=1"),
        "{out}"
    );
    assert!(calls(&fake).is_empty());

    let out = p.ctx_ok(&["distill", "--transcript", ts]);
    assert!(out.contains("chunk 1/1: +4 entries"), "{out}");
    let prompts = calls(&fake);
    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];
    assert!(prompt.contains("software project called demo"), "{prompt}");
    assert!(prompt.contains("a human (omar)"), "{prompt}");
    assert!(
        !prompt.contains("hunter2") && !prompt.contains("sk-live"),
        "{prompt}"
    );
    assert!(prompt.contains("[REDACTED:kv_secret]"), "{prompt}");
    assert!(
        !prompt.contains("meta"),
        "isMeta records are not sent: {prompt}"
    );
    assert!(
        !prompt.contains("Quoro") && !prompt.contains("Omar"),
        "{prompt}"
    );

    let written = entries_by(&p, "distill");
    let kinds: Vec<&str> = written
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["proposal", "question", "fact", "journal"]);
    for e in &written {
        assert_eq!(e["actor"]["type"], "agent");
        assert_eq!(e["actor"]["model"], "fake-1");
        assert_eq!(e["fields"]["source"], "distill");
        assert_eq!(
            e["fields"]["transcript"],
            fs::canonicalize(&t).unwrap().to_str().unwrap()
        );
    }
    assert_eq!(written[1]["fields"]["to"], "owner");
    assert_eq!(written[2]["fields"]["fingerprints"], json!([]));
    assert_eq!(written[3]["fields"]["next"], "webhooks");
    // Never decisions; the proposals come out as unreviewed.
    let standup = p.ctx_ok(&["standup", "--peek"]);
    assert!(standup.contains("[not reviewed] proposal"), "{standup}");

    // Second run with nothing new: does not call the model or add anything.
    let out = p.ctx_ok(&["distill", "--transcript", ts]);
    assert!(out.contains("chunks: 0"), "{out}");
    assert_eq!(calls(&fake).len(), 1);
    assert_eq!(entries_by(&p, "distill").len(), 4);

    // More conversation arrives: only the new part is sent, and titles that are
    // already there (even with different capitals or accents) are not repeated.
    let mut f = fs::OpenOptions::new().append(true).open(&t).unwrap();
    f.write_all(
        line(
            "user",
            "Y los webhooks van con reintentos",
            "2026-11-03T11:00:00.000Z",
        )
        .as_bytes(),
    )
    .unwrap();
    // A half-written line is left for later.
    f.write_all(b"{\"type\":\"user\",\"mess").unwrap();
    drop(f);
    reply(
        &fake,
        &json!({
            "decisions": [{"title": "cobrar con STRIPE"}, {"title": "Webhooks con reintentos", "why": "red"}],
            "questions": [], "facts": [], "journal": []
        }),
    );
    let out = p.ctx_ok(&["distill", "--transcript", ts]);
    assert!(out.contains("chunk 1/1: +1 entries"), "{out}");
    let prompts = calls(&fake);
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("Y los webhooks van con reintentos"));
    assert!(!prompts[1].contains("Usemos Stripe"), "{}", prompts[1]);
    // What is in force goes in the prompt with its id, so the model can say what it updates.
    let pr = &written[0]["id"];
    assert!(
        prompts[1].contains(&format!(
            "- {} [proposal] Cobrar con Stripe",
            pr.as_str().unwrap()
        )),
        "{}",
        prompts[1]
    );
    let offsets: Value = serde_json::from_str(
        &fs::read_to_string(p.home.join(".kontext/distill-offsets.json")).unwrap(),
    )
    .unwrap();
    let full = fs::metadata(&t).unwrap().len();
    let saved = offsets
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .as_u64()
        .unwrap();
    assert!(saved < full, "the half-written line does not count");
}

#[test]
fn claude_envelope_and_disabled() {
    let p = Project::new();
    let fake = fake_model(&p);
    set_distill(&p, &format!("sh {}", fake.join("model.sh").display()), "");
    let envelope = json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": format!("Aquí va:\n```json\n{}\n```", json!({"facts": ["El cobro es en MXN"]})),
        "session_id": "abc"
    });
    reply(&fake, &envelope);
    let t = transcript(&p);
    let out = p.ctx_ok(&["--json", "distill", "--transcript", t.to_str().unwrap()]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["added"].as_array().unwrap().len(), 1, "{v}");
    let facts = entries_by(&p, "distill");
    assert_eq!(facts[0]["title"], "El cobro es en MXN");
    // Without `[distill] model`, the model comes from --model in the command (there is none here).
    assert!(facts[0]["actor"].get("model").is_none());

    // Apagado: no hace nada.
    let toml = p.home.join("project.toml");
    let text = fs::read_to_string(&toml)
        .unwrap()
        .replace("enabled = true", "enabled = false");
    fs::write(&toml, text).unwrap();
    fs::write(
        &t,
        line("user", "Otra cosa distinta", "2026-11-03T12:00:00.000Z"),
    )
    .unwrap();
    let out = p.ctx_ok(&["distill", "--transcript", t.to_str().unwrap()]);
    assert!(out.contains("The distill is off"), "{out}");
    assert_eq!(calls(&fake).len(), 1);
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn timeout_kills_the_process_group() {
    let p = Project::new();
    let fake = fake_model(&p);
    set_distill(
        &p,
        &format!("sh {}", fake.join("model.sh").display()),
        "timeout = \"1s\"",
    );
    reply(&fake, &model_reply());
    fs::write(fake.join("sleep"), "").unwrap();
    let t = transcript(&p);
    let start = Instant::now();
    let out = p.ctx(&["distill", "--transcript", t.to_str().unwrap()]);
    let took = start.elapsed();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("did not answer in 1 s"), "{err}");
    // Two attempts of 1 s, not the 30 s of the sleep.
    assert!(took < Duration::from_secs(10), "{took:?}");
    assert_eq!(calls(&fake).len(), 2);
    // The script's child (sleep 30) died too.
    let pid = fs::read_to_string(fake.join("sleep.pid")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(&pid), "sleep {pid} is still alive");
    // Nothing was written and the offset did not advance.
    assert!(entries_by(&p, "distill").is_empty());
    assert!(!p.home.join(".kontext/distill-offsets.json").exists());
    let log = fs::read_to_string(p.home.join(".kontext/distill.log")).unwrap();
    assert!(log.contains("the distill failed"), "{log}");
}

fn hook(p: &Project, event: &str, envs: &[(&str, &str)], stdin: &str) -> Duration {
    let mut c = p.cmd(&["hook", event]);
    for (k, v) in envs {
        c.env(k, v);
    }
    let start = Instant::now();
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let took = start.elapsed();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    took
}

#[test]
fn hooks_spawn_distill_in_the_background() {
    let p = Project::new();
    let fake = fake_model(&p);
    // The model takes 2 s: the hook does not wait for it.
    let slow = fake.join("slow.sh");
    fs::write(
        &slow,
        format!("sleep 2\nsh {}\n", fake.join("model.sh").display()),
    )
    .unwrap();
    set_distill(&p, &format!("sh {}", slow.display()), "");
    reply(&fake, &model_reply());
    let t = transcript(&p);
    let input = json!({"session_id": "s", "transcript_path": t, "hook_event_name": "SessionEnd"})
        .to_string();

    // With KONTEXT_DISTILL_DISABLE nothing is started.
    hook(
        &p,
        "pre-compact",
        &[("KONTEXT_DISTILL_DISABLE", "1")],
        &input,
    );
    let log = fs::read_to_string(p.home.join(".kontext/hooks.log")).unwrap();
    assert!(
        log.contains("distill off because of KONTEXT_DISTILL_DISABLE"),
        "{log}"
    );

    let took = hook(&p, "session-end", &[], &input);
    assert!(took < Duration::from_millis(1500), "el hook tardó {took:?}");
    let log = fs::read_to_string(p.home.join(".kontext/hooks.log")).unwrap();
    assert!(log.contains("in the background (pid"), "{log}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while entries_by(&p, "distill").len() < 4 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(entries_by(&p, "distill").len(), 4);
    let dlog = fs::read_to_string(p.home.join(".kontext/distill.log")).unwrap();
    assert!(dlog.contains("starting the distill"), "{dlog}");
    assert!(dlog.contains("chunk 1/1: +4 entries"), "{dlog}");

    // Without transcript_path the hook does not fail.
    hook(&p, "pre-compact", &[], "{}");
    let log = fs::read_to_string(p.home.join(".kontext/hooks.log")).unwrap();
    assert!(log.contains("no transcript_path"), "{log}");
}

#[test]
fn wait_blocks_until_the_background_distill_ends() {
    let p = Project::new();
    let fake = fake_model(&p);
    // The model takes 2 s: `--wait` waits for it without anyone polling the ledger.
    let slow = fake.join("slow.sh");
    fs::write(
        &slow,
        format!("sleep 2\nsh {}\n", fake.join("model.sh").display()),
    )
    .unwrap();
    set_distill(&p, &format!("sh {}", slow.display()), "");
    reply(&fake, &model_reply());
    let t = transcript(&p);
    let input = json!({"session_id": "s", "transcript_path": t, "hook_event_name": "PreCompact"})
        .to_string();

    // Without pending distills, it returns right away.
    let out = p.ctx_ok(&["--json", "distill", "--wait"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["stale"], 0);

    hook(&p, "pre-compact", &[], &input);
    let pending = p.home.join(".kontext/distill-pending");
    let marks: Vec<_> = fs::read_dir(&pending).unwrap().collect();
    assert_eq!(marks.len(), 1, "one marker per distill started");
    let start = Instant::now();
    let out = p.ctx_ok(&["distill", "--wait", "--timeout", "30s"]);
    assert!(out.contains("No distill is pending anymore"), "{out}");
    assert!(start.elapsed() >= Duration::from_millis(1000), "no esperó");
    // When it returns, the distill has already written everything and deleted its marker.
    assert_eq!(entries_by(&p, "distill").len(), 4);
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 0);

    // A distill that fails also deletes its marker.
    fs::write(&slow, "exit 3\n").unwrap();
    let mut s = fs::read_to_string(&t).unwrap();
    s.push_str(&line("user", "otra cosa", "2026-11-03T10:02:00.000Z"));
    fs::write(&t, s).unwrap();
    hook(&p, "session-end", &[], &input);
    p.ctx_ok(&["distill", "--wait", "--timeout", "30s"]);
    assert_eq!(fs::read_dir(&pending).unwrap().count(), 0);
    let dlog = fs::read_to_string(p.home.join(".kontext/distill.log")).unwrap();
    assert!(dlog.contains("the distill failed"), "{dlog}");

    // It runs out of time with a live marker; without --wait, --timeout does not apply.
    fs::write(
        pending.join("1-vivo"),
        format!("pid={}\n", std::process::id()),
    )
    .unwrap();
    let out = p.ctx(&["distill", "--wait", "--timeout", "1s"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("1 distills are still running"), "{err}");
    let out = p.ctx(&[
        "distill",
        "--timeout",
        "1s",
        "--transcript",
        t.to_str().unwrap(),
    ]);
    assert_ne!(out.status.code(), Some(0));
}

#[test]
fn updates_replace_what_the_user_said_and_views_show_only_the_current() {
    let p = Project::new();
    let fake = fake_model(&p);
    set_distill(
        &p,
        &format!("sh {}", fake.join("model.sh").display()),
        "max_entries = 2",
    );
    let t = p.dir.path().join("s.jsonl");
    let pasted = format!(
        "Te paso el reporte.\n{}\n¿Algo raro?",
        "renglón del reporte ".repeat(800)
    );
    fs::write(
        &t,
        line("user", "El corte es el día 12.", "2026-11-03T10:00:00.000Z")
            + &line("user", &pasted, "2026-11-03T10:05:00.000Z"),
    )
    .unwrap();
    // The model returns more than fits: the user's stay.
    reply(
        &fake,
        &json!({
            "decisions": [{"title": "Cerrar el mes el día 12", "source": "user", "when": "2026-11-03 10:00"}],
            "facts": [
                {"text": "El corte es el día 12", "source": "user", "when": "2026-11-03 10:00"},
                {"text": "El reporte trae 800 renglones", "source": "document"}
            ],
            "questions": [{"title": "¿Algo raro en el reporte?", "source": "agent"}],
            "journal": [{"summary": "Se leyó el reporte", "next": ""}]
        }),
    );
    let out = p.ctx_ok(&["distill", "--transcript", t.to_str().unwrap()]);
    assert!(out.contains("chunk 1/1: +2 entries"), "{out}");
    // The pasted document is not sent whole.
    let prompt = calls(&fake).pop().unwrap();
    assert!(prompt.contains("Te paso el reporte.") && prompt.contains("¿Algo raro?"));
    assert!(
        prompt.contains("characters of pasted text omitted"),
        "{prompt}"
    );
    assert!(prompt.matches("renglón del reporte").count() < 120);
    let first = entries_by(&p, "distill");
    let kinds: Vec<&str> = first.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["proposal", "fact"]);
    let fact = first[1]["id"].as_str().unwrap().to_string();
    assert_eq!(first[1]["fields"]["origin"], "user");
    assert_eq!(first[1]["fields"]["said_at"], "2026-11-03 10:00");

    // Later the user changes it: the distill says what it updates.
    let mut f = fs::OpenOptions::new().append(true).open(&t).unwrap();
    f.write_all(
        line(
            "user",
            "Cambio: el corte ya es el día 14.",
            "2026-11-04T09:00:00.000Z",
        )
        .as_bytes(),
    )
    .unwrap();
    drop(f);
    reply(
        &fake,
        &json!({"facts": [{"text": "El corte es el día 14", "source": "user", "when": "2026-11-04 09:00", "supersedes": fact}]}),
    );
    p.ctx_ok(&["distill", "--transcript", t.to_str().unwrap()]);
    let prompt = calls(&fake).pop().unwrap();
    assert!(
        prompt.contains(&format!("- {fact} [fact] El corte es el día 12")),
        "{prompt}"
    );
    let all = entries_by(&p, "distill");
    let new = all.last().unwrap();
    assert_eq!(new["fields"]["supersedes"], json!([fact.clone()]));

    // Las vistas (también la del pm) muestran solo lo vigente, como del usuario.
    let out = p.ctx_ok(&["resume", "--as", "pm", "--peek"]);
    let said = &out[out.find("## What the user said (2)").expect(&out)..];
    let said = &said[..said[3..].find("\n## ").map(|i| i + 3).unwrap_or(said.len())];
    assert!(
        said.contains("(2026-11-04 09:00) El corte es el día 14"),
        "{out}"
    );
    assert!(
        said.contains(&format!(
            "(supersedes {fact} «El corte es el día 12», which no longer applies)"
        )),
        "{out}"
    );
    assert!(said.contains("[proposal] Cerrar el mes el día 12"), "{out}");
    // El valor viejo solo sale como reemplazado, después del nuevo.
    assert_eq!(out.matches("El corte es el día 12").count(), 1, "{out}");
    assert!(out.contains("## Pending proposals (0)"), "{out}");
    // The history stays: recall and show say what replaced it.
    let none = p.dir.path().join("sin-transcripciones/*.jsonl");
    let rec = p.ctx_env_ok(
        &[("KONTEXT_TRANSCRIPTS_GLOB", none.to_str().unwrap())],
        &["recall", "corte"],
    );
    assert!(
        rec.contains(&format!("{fact} fact, superseded by")) && rec.contains("(no longer applies)"),
        "{rec}"
    );
    let show = p.ctx_ok(&["show", &fact]);
    assert!(show.contains("superseded"), "{show}");
    // And the new one says that it replaces, not that it "would replace".
    let show = p.ctx_ok(&["show", new["id"].as_str().unwrap()]);
    assert!(show.contains("- **Supersedes:** "), "{show}");
}

/// The long-memory case: two unsigned user rules, the second replaces the first.
/// For the agent the new one is in force and the old one no longer applies; the
/// human's inbox still asks them to sign the one in force.
#[test]
fn an_unsigned_user_rule_is_in_force_and_the_replaced_one_is_not() {
    let p = Project::new();
    let fake = fake_model(&p);
    set_distill(&p, &format!("sh {}", fake.join("model.sh").display()), "");
    let t = p.dir.path().join("s.jsonl");
    fs::write(
        &t,
        line(
            "user",
            "La tasa de recargo es 1.5 % mensual.",
            "2026-11-03T10:00:00.000Z",
        ),
    )
    .unwrap();
    reply(
        &fake,
        &json!({"decisions": [{"title": "La tasa de recargo es 1.5 % mensual", "type": "rule", "source": "user", "when": "2026-11-03 10:00"}]}),
    );
    p.ctx_ok(&["distill", "--transcript", t.to_str().unwrap()]);
    let old = entries_by(&p, "distill")[0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut f = fs::OpenOptions::new().append(true).open(&t).unwrap();
    f.write_all(
        line(
            "user",
            "Salió la circular: ya rige la tasa nueva, 1.8 % mensual.",
            "2026-11-04T09:00:00.000Z",
        )
        .as_bytes(),
    )
    .unwrap();
    drop(f);
    reply(
        &fake,
        &json!({"decisions": [{"title": "La tasa de recargo es 1.8 % mensual", "type": "rule", "source": "user", "when": "2026-11-04 09:00", "supersedes": old}]}),
    );
    p.ctx_ok(&["distill", "--transcript", t.to_str().unwrap()]);
    let new = entries_by(&p, "distill").last().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let out = p.ctx_ok(&["resume", "--as", "pm", "--peek"]);
    assert!(
        out.contains(&format!(
            "- {new} (2026-11-04 09:00) [user rule, in force] La tasa de recargo es 1.8 % mensual \
             (supersedes {old} «La tasa de recargo es 1.5 % mensual», which no longer applies)"
        )),
        "{out}"
    );
    assert!(out.contains("It applies now, even when unsigned"), "{out}");
    assert!(!out.contains("unsigned)]"), "{out}");

    let none = p.dir.path().join("sin-transcripciones/*.jsonl");
    let rec = p.ctx_env_ok(
        &[("KONTEXT_TRANSCRIPTS_GLOB", none.to_str().unwrap())],
        &["recall", "tasa"],
    );
    assert!(
        rec.contains(&format!("{new} proposal, user rule, in force (unsigned)")),
        "{rec}"
    );
    assert!(
        rec.contains(&format!(
            "{old} proposal, superseded by {new} (no longer applies)"
        )),
        "{rec}"
    );
    let show = p.ctx_ok(&["show", &new]);
    assert!(
        show.contains("it is a user rule and already applies"),
        "{show}"
    );

    // The human's inbox does not change: it still asks them to sign it.
    let inbox = p.ctx_ok(&["inbox"]);
    assert!(
        inbox.contains("Your unsigned rule: La tasa de recargo es 1.8 % mensual"),
        "{inbox}"
    );
}
