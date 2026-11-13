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

    // An account with no workers.dev subdomain: it says so and changes nothing.
    let o = deploy(&config, &api, TOKEN, &[]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("--subdomain"), "{}", text(&o));
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
    let o = deploy(&config, &api, TOKEN, &[]);
    assert!(!o.status.success());
    assert!(text(&o).contains("v7"), "{}", text(&o));
    // With another name it goes ahead.
    let o = deploy(&config, &api, TOKEN, &["--name", "kontext-2"]);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("https://kontext-2.omar-kx.workers.dev"));
}
