//! `kontext server deploy` against a local fake of the Cloudflare API endpoints
//! it calls (no Cloudflare account in the tests). The fake is strict where the
//! real API is known to be: it needs the bearer token, refuses a Durable
//! Object binding with no migration declaring its class, refuses a migration
//! tag that is already applied, and drops secrets on an upload without
//! `keep_bindings: ["secret_text"]`.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use common::BIN;
use serde_json::{Value, json};

const TOKEN: &str = "cf-test-token";
const ACCOUNT: &str = "acc0123456789";

#[derive(Default)]
struct Script {
    module: Vec<u8>,
    module_type: String,
    metadata: Value,
    migration_tag: Option<String>,
    secrets: BTreeMap<String, String>,
    on_workers_dev: bool,
}

#[derive(Default)]
struct Cf {
    subdomain: Option<String>,
    scripts: BTreeMap<String, Script>,
    log: Vec<String>,
}

type Shared = Arc<Mutex<Cf>>;

fn ok(result: Value) -> (u16, Value) {
    (
        200,
        json!({ "success": true, "errors": [], "messages": [], "result": result }),
    )
}

fn err(status: u16, code: u32, message: &str) -> (u16, Value) {
    (
        status,
        json!({ "success": false, "errors": [{ "code": code, "message": message }], "messages": [], "result": null }),
    )
}

/// The parts of a multipart body: name → (content type, bytes).
fn parts(ct: &str, body: &[u8]) -> BTreeMap<String, (String, Vec<u8>)> {
    let boundary = ct.split("boundary=").nth(1).unwrap().trim();
    let sep = format!("--{boundary}");
    let text = body;
    let mut out = BTreeMap::new();
    let mut pos = 0;
    let find = |hay: &[u8], needle: &[u8], from: usize| {
        hay[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    };
    while let Some(start) = find(text, sep.as_bytes(), pos) {
        let after = start + sep.len();
        if text[after..].starts_with(b"--") {
            break;
        }
        let head_start = after + 2;
        let head_end = find(text, b"\r\n\r\n", head_start).unwrap();
        let head = String::from_utf8_lossy(&text[head_start..head_end]).to_string();
        let data_start = head_end + 4;
        let next = find(text, format!("\r\n{sep}").as_bytes(), data_start).unwrap();
        let name = head
            .split("name=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string();
        let ctype = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Type: "))
            .unwrap_or("")
            .to_string();
        out.insert(name, (ctype, text[data_start..next].to_vec()));
        pos = next + 2;
    }
    out
}

fn handle(
    cf: &Shared,
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> (u16, Value) {
    let mut cf = cf.lock().unwrap();
    cf.log.push(format!("{method} {path}"));
    if headers.get("authorization").map(String::as_str) != Some(&format!("Bearer {TOKEN}")) {
        return err(403, 10000, "Authentication error");
    }
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method, seg.as_slice()) {
        ("GET", ["accounts"]) => ok(json!([{ "id": ACCOUNT, "name": "Omar's account" }])),
        ("GET", ["accounts", a, "workers", "subdomain"]) if *a == ACCOUNT => match &cf.subdomain {
            Some(s) => ok(json!({ "subdomain": s })),
            None => err(404, 10007, "workers.api.error.subdomain_not_found"),
        },
        ("PUT", ["accounts", a, "workers", "subdomain"]) if *a == ACCOUNT => {
            let v: Value = serde_json::from_slice(body).unwrap();
            let s = v["subdomain"].as_str().unwrap().to_string();
            cf.subdomain = Some(s.clone());
            ok(json!({ "subdomain": s }))
        }
        ("GET", ["accounts", a, "workers", "scripts"]) if *a == ACCOUNT => {
            let list: Vec<Value> = cf
                .scripts
                .iter()
                .map(|(id, s)| {
                    let mut v = json!({ "id": id });
                    if let Some(t) = &s.migration_tag {
                        v["migration_tag"] = json!(t);
                    }
                    // The tags the script was uploaded with, as the list returns them.
                    if let Some(t) = s.metadata.get("tags") {
                        v["tags"] = t.clone();
                    }
                    v
                })
                .collect();
            ok(json!(list))
        }
        ("PUT", ["accounts", a, "workers", "scripts", name]) if *a == ACCOUNT => {
            let ct = headers.get("content-type").cloned().unwrap_or_default();
            if !ct.starts_with("multipart/form-data") {
                return err(400, 10021, "expected multipart/form-data");
            }
            let p = parts(&ct, body);
            let Some((_, meta)) = p.get("metadata") else {
                return err(400, 10021, "missing metadata part");
            };
            let meta: Value = serde_json::from_slice(meta).unwrap();
            let main = meta["main_module"].as_str().unwrap_or("");
            let Some((mtype, module)) = p.get(main) else {
                return err(400, 10021, "main_module names no part");
            };
            let prev_tag = cf.scripts.get(*name).and_then(|s| s.migration_tag.clone());
            let migration = meta.get("migrations");
            let new_tag = match migration {
                Some(m) => {
                    if prev_tag.is_some() && m["old_tag"].as_str() != prev_tag.as_deref() {
                        return err(400, 10079, "migration tag precondition failed");
                    }
                    if m["new_tag"].as_str() == prev_tag.as_deref() {
                        return err(400, 10079, "migration tag already applied");
                    }
                    Some(m["new_tag"].as_str().unwrap().to_string())
                }
                None => prev_tag.clone(),
            };
            let declared = migration
                .map(|m| {
                    m["new_sqlite_classes"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            for b in meta["bindings"].as_array().cloned().unwrap_or_default() {
                if b["type"] == "durable_object_namespace" {
                    let class = b["class_name"].as_str().unwrap_or("");
                    if new_tag.is_none()
                        || (prev_tag.is_none() && !declared.iter().any(|c| c == class))
                    {
                        return err(400, 10074, "durable object class has no migration");
                    }
                }
            }
            let keep_secrets = meta["keep_bindings"]
                .as_array()
                .is_some_and(|k| k.iter().any(|x| x == "secret_text"));
            let old = cf.scripts.remove(*name).unwrap_or_default();
            let script = Script {
                module: module.clone(),
                module_type: mtype.clone(),
                metadata: meta.clone(),
                migration_tag: new_tag,
                secrets: if keep_secrets {
                    old.secrets
                } else {
                    BTreeMap::new()
                },
                on_workers_dev: old.on_workers_dev,
            };
            cf.scripts.insert(name.to_string(), script);
            ok(json!({ "id": name }))
        }
        ("PUT", ["accounts", a, "workers", "scripts", name, "secrets"]) if *a == ACCOUNT => {
            let Some(s) = cf.scripts.get_mut(*name) else {
                return err(404, 10007, "script not found");
            };
            let v: Value = serde_json::from_slice(body).unwrap();
            assert_eq!(v["type"], "secret_text");
            s.secrets.insert(
                v["name"].as_str().unwrap().into(),
                v["text"].as_str().unwrap().into(),
            );
            ok(json!({ "name": v["name"], "type": "secret_text" }))
        }
        ("POST", ["accounts", a, "workers", "scripts", name, "subdomain"]) if *a == ACCOUNT => {
            if cf.subdomain.is_none() {
                return err(400, 10007, "no subdomain");
            }
            let Some(s) = cf.scripts.get_mut(*name) else {
                return err(404, 10007, "script not found");
            };
            let v: Value = serde_json::from_slice(body).unwrap();
            s.on_workers_dev = v["enabled"].as_bool().unwrap();
            ok(json!({ "enabled": s.on_workers_dev, "previews_enabled": false }))
        }
        _ => err(404, 7003, "Could not route"),
    }
}

fn serve_one(cf: &Shared, stream: TcpStream) {
    let mut r = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut it = line.split_whitespace();
    let method = it.next().unwrap_or("").to_string();
    let path = it.next().unwrap_or("").to_string();
    let mut headers = BTreeMap::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).unwrap();
    let path = path.trim_start_matches("/client/v4").to_string();
    let (status, v) = handle(cf, &method, &path, &headers, &body);
    let text = v.to_string();
    let mut s = stream;
    let _ = write!(
        s,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
}

fn fake_cf() -> (String, Shared) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/client/v4", l.local_addr().unwrap());
    let cf: Shared = Arc::new(Mutex::new(Cf::default()));
    let c = cf.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            serve_one(&c, s);
        }
    });
    (url, cf)
}

fn deploy(config: &Path, api: &str, token: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(["server", "deploy", "--no-check"])
        .args(args)
        .env("KONTEXT_CONFIG_DIR", config)
        .env("KONTEXT_CF_API", api)
        .env("CLOUDFLARE_API_TOKEN", token)
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .env_remove("KONTEXT_ADMIN_SECRET")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn deploy_creates_then_updates_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let (api, cf) = fake_cf();

    // An account with no workers.dev subdomain: the human picks it (exit 4),
    // and nothing changes.
    let o = deploy(&config, &api, TOKEN, &[]);
    assert_eq!(o.status.code(), Some(4), "{}", text(&o));
    assert!(text(&o).contains("--subdomain <name>"), "{}", text(&o));
    assert!(cf.lock().unwrap().scripts.is_empty());
    assert!(!config.join("servers.json").exists());

    // With --subdomain: registered, uploaded with the migration, the secret set.
    let o = deploy(&config, &api, TOKEN, &["--subdomain", "omar-kx", "--json"]);
    assert!(o.status.success(), "{}", text(&o));
    let d: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(d["url"], "https://kontext.omar-kx.workers.dev");
    assert_eq!(d["created"], true);
    assert_eq!(d["migration_applied"], true);
    assert_eq!(d["subdomain_created"], true);
    let file = config.join("servers.json");
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let saved: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    let secret = saved["servers"][0]["admin_secret"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(secret.len(), 64);
    {
        let cf = cf.lock().unwrap();
        let s = &cf.scripts["kontext"];
        assert_eq!(
            s.module,
            fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("server/worker.js")).unwrap()
        );
        assert_eq!(s.module_type, "application/javascript+module");
        assert_eq!(s.migration_tag.as_deref(), Some("v1"));
        assert_eq!(
            s.metadata["migrations"]["new_sqlite_classes"],
            json!(["Project"])
        );
        assert_eq!(s.secrets["ADMIN_SECRET"], secret);
        assert!(s.on_workers_dev);
        // The secret is in no request path or log line.
        assert!(cf.log.iter().all(|l| !l.contains(&secret)));
    }

    // Again: updated in place, no migration (it is applied), same secret, kept.
    let o = deploy(&config, &api, TOKEN, &["--json"]);
    assert!(o.status.success(), "{}", text(&o));
    let d: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(d["created"], false);
    assert_eq!(d["migration_applied"], false);
    assert_eq!(d["admin_secret_new"], false);
    assert_eq!(d["url"], "https://kontext.omar-kx.workers.dev");
    {
        let cf = cf.lock().unwrap();
        let s = &cf.scripts["kontext"];
        assert!(s.metadata.get("migrations").is_none());
        assert_eq!(s.secrets["ADMIN_SECRET"], secret);
        let calls: Vec<&str> = cf.log.iter().map(String::as_str).collect();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("PUT /accounts/acc0123456789/workers/scripts/kontext"))
                .count(),
            4
        );
    }
    let again: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(again["servers"].as_array().unwrap().len(), 1);
}

#[test]
fn deploy_refuses_another_workers_migrations_and_a_bad_token() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let (api, cf) = fake_cf();
    {
        let mut c = cf.lock().unwrap();
        c.subdomain = Some("omar-kx".into());
        c.scripts.insert(
            "kontext".into(),
            Script {
                migration_tag: Some("v7".into()),
                ..Script::default()
            },
        );
    }
    let o = deploy(&config, &api, "wrong", &[]);
    assert!(!o.status.success());
    assert!(text(&o).contains("Workers Scripts: Edit"), "{}", text(&o));
    // Not kontext's (no tag): refused before anything else.
    let o = deploy(&config, &api, TOKEN, &[]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    // Even with --replace, a Durable Object migration that is not kontext's is
    // not built on.
    let o = deploy(&config, &api, TOKEN, &["--replace"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("v7"), "{}", text(&o));
    // With another name it goes ahead.
    let o = deploy(&config, &api, TOKEN, &["--name", "kontext-2"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("https://kontext-2.omar-kx.workers.dev"));
}

/// A Worker named `kontext` that kontext did not upload (no `kontext-server`
/// tag, and no Durable Object migration either) is not overwritten: exit 2,
/// and nothing changes until `--replace`.
#[test]
fn deploy_refuses_an_unrelated_worker_with_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let (api, cf) = fake_cf();
    {
        let mut c = cf.lock().unwrap();
        c.subdomain = Some("omar-kx".into());
        c.scripts.insert(
            "kontext".into(),
            Script {
                module: b"export default { fetch() { return new Response('mine') } }".to_vec(),
                metadata: json!({ "main_module": "index.js", "tags": ["blog"] }),
                ..Script::default()
            },
        );
    }
    let o = deploy(&config, &api, TOKEN, &[]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("--replace"), "{}", text(&o));
    {
        let c = cf.lock().unwrap();
        let s = &c.scripts["kontext"];
        assert!(s.module.starts_with(b"export default { fetch()"));
        assert!(
            !c.log
                .iter()
                .any(|l| l.starts_with("PUT /accounts/acc0123456789/workers/scripts/kontext")),
            "{:?}",
            c.log
        );
    }
    assert!(!config.join("servers.json").exists());

    // With --replace it is overwritten, and marked as kontext's from then on.
    let o = deploy(&config, &api, TOKEN, &["--replace"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        cf.lock().unwrap().scripts["kontext"].metadata["tags"],
        json!(["kontext-server"])
    );
    let o = deploy(&config, &api, TOKEN, &[]);
    assert!(o.status.success(), "{}", text(&o));
}

/// `server deploy` with no token, as an agent runs it.
fn deploy_no_token(config: &Path, api: &str, args: &[&str]) -> Command {
    let mut c = Command::new(BIN);
    c.args(["server", "deploy", "--no-check"])
        .args(args)
        .env("KONTEXT_CONFIG_DIR", config)
        .env("KONTEXT_CF_API", api)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .env_remove("KONTEXT_ADMIN_SECRET");
    c
}

/// Without a terminal and without a token, deploy never waits on a prompt: it
/// exits with 4 and gives the pre-filled token link and the command the human
/// runs; nothing is sent to Cloudflare.
#[test]
fn deploy_without_a_token_says_what_the_human_does() {
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let (api, cf) = fake_cf();

    // stdin is /dev/null.
    let o = deploy_no_token(&config, &api, &[])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(4), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("Needs you:"), "{t}");
    assert!(
        t.contains(
            "https://dash.cloudflare.com/profile/api-tokens?permissionGroupKeys=%5B%7B%22key%22%3A%22workers_scripts%22%2C%22type%22%3A%22edit%22%7D%5D&accountId=%2A&zoneId=all&name=kontext"
        ),
        "{t}"
    );
    assert!(t.contains("server deploy`"), "{t}");

    // --json: the same as data, on stdout.
    let o = deploy_no_token(&config, &api, &["--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(4), "{}", text(&o));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let n = &v["needs_human"];
    assert_eq!(n["reason"], "cloudflare_token");
    assert_eq!(n["exit_code"], 4);
    assert!(n["open"].as_str().unwrap().contains("workers_scripts"));
    assert!(n["run"].as_str().unwrap().ends_with(" server deploy"));
    assert!(n["say"].as_str().unwrap().contains("not here"));

    // An open pipe that never writes: it gives up after a few seconds.
    let started = std::time::Instant::now();
    let mut child = deploy_no_token(&config, &api, &["--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _keep_open = child.stdin.take();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(4), "{}", text(&o));
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    assert!(
        cf.lock().unwrap().log.is_empty(),
        "{:?}",
        cf.lock().unwrap().log
    );
    assert!(!config.join("servers.json").exists());

    // A token piped on stdin still works, also when it comes a moment later.
    cf.lock().unwrap().subdomain = Some("omar-kx".into());
    let mut child = deploy_no_token(&config, &api, &[])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    writeln!(stdin, "{TOKEN}").unwrap();
    let o = child.wait_with_output().unwrap();
    drop(stdin);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("https://kontext.omar-kx.workers.dev"));
    assert!(
        !fs::read_to_string(config.join("servers.json"))
            .unwrap()
            .contains(TOKEN)
    );
}

/// A child whose stdin is a fresh pseudo-terminal, as some agent harnesses
/// run commands: "stdin is a terminal" with nobody typing. Returns the child
/// and the terminal's other end (what is written there is typed).
fn on_a_pty(mut c: Command) -> (std::process::Child, fs::File) {
    use std::os::fd::FromRawFd;
    use std::process::Stdio;
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty fills the two descriptors; the others may be null.
    let r = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(r, 0, "openpty");
    // SAFETY: both were just opened and are owned here only.
    let (master, slave) = unsafe { (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave)) };
    let child = c
        .stdin(Stdio::from(slave))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    (child, master)
}

fn no_agent_env(c: &mut Command) -> &mut Command {
    c.env_remove("CI")
        .env_remove("CLAUDECODE")
        .env_remove("KONTEXT_NO_PROMPT")
}

/// On a terminal nobody types at, deploy gives up after the prompt's limit
/// with exit 4; when the environment says an agent or CI runs it, it does not
/// prompt at all; a human typing at the terminal still deploys.
#[test]
fn deploy_on_a_terminal_with_nobody_there_stops_with_exit_4() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let (api, cf) = fake_cf();

    // Nobody types: the prompt waits KONTEXT_PROMPT_WAIT_MS (five minutes
    // when it is not set), then exit 4 with the needs-human result.
    let mut c = deploy_no_token(&config, &api, &["--json"]);
    no_agent_env(&mut c).env("KONTEXT_PROMPT_WAIT_MS", "1500");
    let started = Instant::now();
    let (child, _master) = on_a_pty(c);
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(4), "{}", text(&o));
    assert!(started.elapsed() < Duration::from_secs(15));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("Cloudflare API token"), "{err}");
    assert!(err.contains("Nothing was typed"), "{err}");
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["needs_human"]["reason"], "cloudflare_token");

    // An agent or CI: no prompt, at once.
    for (k, v) in [
        ("CLAUDECODE", "1"),
        ("CI", "true"),
        ("KONTEXT_NO_PROMPT", "1"),
    ] {
        let mut c = deploy_no_token(&config, &api, &["--json"]);
        no_agent_env(&mut c).env(k, v);
        let started = Instant::now();
        let (child, _master) = on_a_pty(c);
        let o = child.wait_with_output().unwrap();
        assert_eq!(o.status.code(), Some(4), "{k}: {}", text(&o));
        assert!(started.elapsed() < Duration::from_secs(10), "{k}");
        assert!(
            !String::from_utf8_lossy(&o.stderr).contains("Cloudflare API token ("),
            "{k} prompted: {}",
            text(&o)
        );
        let j: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(j["needs_human"]["exit_code"], 4, "{k}");
    }
    assert!(
        cf.lock().unwrap().log.is_empty(),
        "{:?}",
        cf.lock().unwrap().log
    );

    // The token is given, the account has no subdomain, nobody picks one:
    // exit 4 that names --subdomain, and nothing is uploaded.
    let mut c = deploy_no_token(&config, &api, &["--json"]);
    no_agent_env(&mut c)
        .env("CLOUDFLARE_API_TOKEN", TOKEN)
        .env("KONTEXT_PROMPT_WAIT_MS", "1500");
    let (child, _master) = on_a_pty(c);
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(4), "{}", text(&o));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["needs_human"]["reason"], "workers_subdomain");
    assert!(
        v["needs_human"]["run"]
            .as_str()
            .unwrap()
            .ends_with("server deploy --subdomain <name>")
    );
    assert!(cf.lock().unwrap().scripts.is_empty());

    // A human types the token and the subdomain: it deploys.
    let mut c = deploy_no_token(&config, &api, &[]);
    no_agent_env(&mut c).env("KONTEXT_PROMPT_WAIT_MS", "20000");
    let (child, mut master) = on_a_pty(c);
    std::thread::sleep(Duration::from_millis(300));
    writeln!(master, "{TOKEN}").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    writeln!(master, "omar-pty").unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        text(&o).contains("https://kontext.omar-pty.workers.dev"),
        "{}",
        text(&o)
    );
    assert!(
        !fs::read_to_string(config.join("servers.json"))
            .unwrap()
            .contains(TOKEN)
    );
}

/// A second machine (no servers.json) deploying to an account that already
/// has the kontext Worker is refused: a new admin secret would break every
/// other machine's. `--replace-admin-secret` is the explicit way through.
#[test]
fn deploy_from_another_machine_keeps_the_admin_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    let (api, cf) = fake_cf();
    cf.lock().unwrap().subdomain = Some("omar-kx".into());
    let o = deploy(&a, &api, TOKEN, &[]);
    assert!(o.status.success(), "{}", text(&o));
    let secret = cf.lock().unwrap().scripts["kontext"].secrets["ADMIN_SECRET"].clone();

    let o = deploy(&b, &api, TOKEN, &[]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    let t = text(&o);
    assert!(t.contains("deployed from another machine"), "{t}");
    assert!(t.contains("--replace-admin-secret"), "{t}");
    assert!(t.contains("every other machine"), "{t}");
    assert!(t.contains("kontext invite"), "{t}");
    assert_eq!(
        cf.lock().unwrap().scripts["kontext"].secrets["ADMIN_SECRET"],
        secret
    );
    assert!(!b.join("servers.json").exists());
    // The machine that deployed it still updates it, keeping the secret.
    let o = deploy(&a, &api, TOKEN, &[]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        cf.lock().unwrap().scripts["kontext"].secrets["ADMIN_SECRET"],
        secret
    );

    let o = deploy(&b, &api, TOKEN, &["--replace-admin-secret"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_ne!(
        cf.lock().unwrap().scripts["kontext"].secrets["ADMIN_SECRET"],
        secret
    );
}

/// `kontext setup` deploys with CLOUDFLARE_API_TOKEN set; an account with no
/// workers.dev subdomain is exit 4 naming `kontext setup --subdomain <name>`,
/// and that flag registers it.
#[test]
fn setup_deploys_with_its_subdomain_flag() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let home = dir.path().join("home");
    let repo = home.join("code/notes");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("projects.toml"), "writer = \"omar@a\"\n").unwrap();
    let gitconfig = dir.path().join("gitconfig");
    fs::write(
        &gitconfig,
        "[user]\n\tname = Omar\n\temail = o@example.com\n",
    )
    .unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    let (api, cf) = fake_cf();
    let setup = |args: &[&str]| {
        Command::new(BIN)
            .arg("setup")
            .arg("--json")
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("KONTEXT_CONFIG_DIR", &config)
            .env("GIT_CONFIG_GLOBAL", &gitconfig)
            .env("KONTEXT_CF_API", &api)
            .env("CLOUDFLARE_API_TOKEN", TOKEN)
            .env_remove("CLOUDFLARE_ACCOUNT_ID")
            .env_remove("KONTEXT_ADMIN_SECRET")
            .env_remove("KONTEXT_PROJECT")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };
    let o = setup(&[]);
    assert_eq!(o.status.code(), Some(4), "{}", text(&o));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let n = &v["needs_human"];
    assert_eq!(n["reason"], "workers_subdomain", "{}", text(&o));
    assert!(
        n["run"]
            .as_str()
            .unwrap()
            .ends_with(" setup --subdomain <name>"),
        "{}",
        text(&o)
    );
    assert!(cf.lock().unwrap().scripts.is_empty());

    let o = setup(&["--subdomain", "omar-setup"]);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    let server = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["step"] == "server")
        .unwrap()
        .clone();
    assert_eq!(server["status"], "done", "{}", text(&o));
    assert!(
        server["detail"]
            .as_str()
            .unwrap()
            .contains("https://kontext.omar-setup.workers.dev"),
        "{}",
        text(&o)
    );
    assert_eq!(cf.lock().unwrap().subdomain.as_deref(), Some("omar-setup"));
}
