//! The kontext server (DESIGN §14): `server/worker.js`, embedded here, and
//! `kontext server deploy`, which uploads it to the user's Cloudflare account
//! through the Cloudflare REST API (no wrangler, no Node).
//!
//! What `deploy` calls, in order (all under
//! `https://api.cloudflare.com/client/v4`, with `Authorization: Bearer
//! $CLOUDFLARE_API_TOKEN`):
//! 1. `GET /accounts`, only without `--account-id`/`$CLOUDFLARE_ACCOUNT_ID`;
//! 2. `GET /accounts/{a}/workers/subdomain` (the account's `workers.dev` name);
//!    `PUT` of it only with `--subdomain`, when the account has none;
//! 3. `GET /accounts/{a}/workers/scripts` (is the Worker there, is it
//!    kontext's, by the tag `kontext-server` it uploads with, and which
//!    Durable Object migration it has; one without the tag is refused unless
//!    `--replace`);
//! 4. `PUT /accounts/{a}/workers/scripts/{name}`: the module, the Durable
//!    Object binding, `keep_bindings: ["secret_text"]`, and the migration
//!    `v1` that declares the SQLite class `Project` only if it is not applied;
//! 5. `PUT /accounts/{a}/workers/scripts/{name}/secrets`: `ADMIN_SECRET`;
//! 6. `POST /accounts/{a}/workers/scripts/{name}/subdomain`: on `workers.dev`.
//!
//! The admin secret is generated once and kept in `servers.json` in the
//! kontext config folder (mode 0600), never in a repo; each deploy sets it
//! again, so it is the same whatever Cloudflare keeps.

use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config;
use crate::http::Http;

/// The Worker, exactly as it is uploaded.
pub const WORKER_JS: &str = include_str!("../server/worker.js");
/// The compatibility date it runs with (the same as `server/dev.mjs`).
pub const COMPATIBILITY_DATE: &str = "2026-04-07";
/// The Durable Object migration that declares the SQLite class.
pub const MIGRATION_TAG: &str = "v1";
pub const DO_CLASS: &str = "Project";
pub const DO_BINDING: &str = "PROJECT";
pub const ADMIN_SECRET_NAME: &str = "ADMIN_SECRET";
/// The tag every Worker kontext uploads carries (`tags` in the upload's
/// metadata, which the script list returns: "tags: Tags associated with the
/// Worker", <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/methods/list/>;
/// "tags: List of strings to use as tags for this Worker",
/// <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/methods/update/>).
/// A Worker with the same name and without it is somebody else's.
pub const SCRIPT_TAG: &str = "kontext-server";
/// The longest ledger line, in UTF-8 bytes, the server takes (`MAX_LINE_BYTES`
/// in `server/worker.js`, pinned to this one by a test). A project with a
/// server refuses to write a longer one, so every line it writes can be pushed.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const CF_API: &str = "https://api.cloudflare.com/client/v4";

/// One server this machine deployed (or was given the admin secret of).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerEntry {
    pub url: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
    pub admin_secret: String,
}

/// `servers.json` in the config folder (0600).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Servers {
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
}

impl Servers {
    pub fn path() -> Result<PathBuf> {
        Ok(config::config_dir()?.join("servers.json"))
    }

    pub fn load() -> Result<Servers> {
        let p = Servers::path()?;
        match fs::read_to_string(&p) {
            Ok(s) => {
                serde_json::from_str(&s).with_context(|| format!("could not read {}", p.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Servers::default()),
            Err(e) => Err(e).with_context(|| format!("could not read {}", p.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        config::write_private(
            &Servers::path()?,
            serde_json::to_string_pretty(self)?.as_bytes(),
        )
    }

    pub fn by_url(&self, url: &str) -> Option<&ServerEntry> {
        let u = normalize_url(url);
        self.servers.iter().find(|s| normalize_url(&s.url) == u)
    }

    fn by_script(&self, account: &str, script: &str) -> Option<&ServerEntry> {
        self.servers.iter().find(|s| {
            s.account_id.as_deref() == Some(account) && s.script.as_deref() == Some(script)
        })
    }

    fn upsert(&mut self, e: ServerEntry) {
        self.servers.retain(|s| {
            !(s.account_id.is_some() && s.account_id == e.account_id && s.script == e.script)
                && normalize_url(&s.url) != normalize_url(&e.url)
        });
        self.servers.push(e);
    }
}

/// `https://x/` → `https://x`.
pub fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// The admin secret for a server: `$KONTEXT_ADMIN_SECRET`, or the one saved
/// when it was deployed from this machine.
pub fn admin_secret(url: &str) -> Result<Option<String>> {
    if let Ok(s) = std::env::var("KONTEXT_ADMIN_SECRET")
        && !s.trim().is_empty()
    {
        return Ok(Some(s.trim().to_string()));
    }
    Ok(Servers::load()?.by_url(url).map(|s| s.admin_secret.clone()))
}

/// What `kontext server deploy` asks for.
#[derive(Debug, Clone)]
pub struct DeployArgs {
    pub name: String,
    pub account_id: Option<String>,
    pub subdomain: Option<String>,
    /// Do not wait for the URL to answer afterwards.
    pub no_check: bool,
    /// Replace a Worker with this name that kontext did not deploy.
    pub replace: bool,
}

/// What it did.
#[derive(Debug, Clone, Serialize)]
pub struct Deployed {
    pub url: String,
    pub account_id: String,
    pub script: String,
    pub created: bool,
    pub migration_applied: bool,
    pub subdomain_created: bool,
    pub admin_secret_file: String,
    pub admin_secret_new: bool,
    pub reachable: Option<bool>,
}

impl Deployed {
    pub fn text(&self) -> String {
        let mut l = vec![format!(
            "{} the Worker {} in account {}{}.",
            if self.created { "Created" } else { "Updated" },
            self.script,
            self.account_id,
            if self.migration_applied {
                " (with the Durable Object migration v1: SQLite class Project)"
            } else {
                ""
            }
        )];
        if self.subdomain_created {
            l.push("Registered the account's workers.dev subdomain.".into());
        }
        l.push(format!(
            "The admin secret is in {} (mode 0600){}; keep it out of every repo. It creates projects \
             (`kontext init --server`, `kontext server connect`).",
            self.admin_secret_file,
            if self.admin_secret_new {
                ", new"
            } else {
                ", the same as before"
            }
        ));
        match self.reachable {
            Some(true) => l.push(format!("{} answers.", self.url)),
            Some(false) => l.push(format!(
                "{} does not answer yet (a new workers.dev name can take a minute); try `curl {}/v1/info`.",
                self.url, self.url
            )),
            None => {}
        }
        l.push(format!("Server URL: {}", self.url));
        l.join("\n") + "\n"
    }
}

/// The Cloudflare API, with a token.
struct Cf {
    http: Http,
    base: String,
    token: String,
}

impl Cf {
    fn call(&self, method: &str, path: &str, body: Option<(&str, &[u8])>) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let auth = format!("Bearer {}", self.token);
        let mut headers = vec![("Authorization", auth.as_str())];
        if let Some((ct, _)) = body {
            headers.push(("Content-Type", ct));
        }
        let r = self
            .http
            .request(method, &url, &headers, body.map(|(_, b)| b))
            .with_context(|| format!("Cloudflare API {method} {path}"))?;
        let v: Value = r
            .json()
            .with_context(|| format!("Cloudflare API {method} {path}"))?;
        if r.ok() && v.get("success").and_then(Value::as_bool) != Some(false) {
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
        let errors: Vec<String> = v
            .get("errors")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|e| {
                        format!(
                            "{} ({})",
                            e.get("message").and_then(Value::as_str).unwrap_or("?"),
                            e.get("code").map(|c| c.to_string()).unwrap_or_default()
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let hint = if r.status == 401 || r.status == 403 {
            ". The token needs the account permission \"Workers Scripts: Edit\" (see `kontext server deploy --help`)"
        } else {
            ""
        };
        Err(CfError {
            status: r.status,
            message: format!(
                "Cloudflare API {method} {path} answered {}: {}{hint}",
                r.status,
                if errors.is_empty() {
                    r.text()
                } else {
                    errors.join("; ")
                }
            ),
        }
        .into())
    }
}

#[derive(Debug)]
struct CfError {
    status: u16,
    message: String,
}

impl std::fmt::Display for CfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CfError {}

/// `$CLOUDFLARE_API_TOKEN`, or one line from stdin (without echo on a terminal).
fn cloudflare_token() -> Result<String> {
    if let Ok(t) = std::env::var("CLOUDFLARE_API_TOKEN")
        && !t.trim().is_empty()
    {
        return Ok(t.trim().to_string());
    }
    let tty = std::io::stdin().is_terminal();
    if tty {
        eprint!("Cloudflare API token (Workers Scripts: Edit): ");
        let _ = std::io::stderr().flush();
        stty(&["-echo"]);
    }
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    if tty {
        stty(&["echo"]);
        eprintln!();
    }
    read.context("could not read the Cloudflare API token from stdin")?;
    let t = line.trim().to_string();
    if t.is_empty() {
        bail!("no Cloudflare API token: set CLOUDFLARE_API_TOKEN or type it on stdin");
    }
    Ok(t)
}

fn stty(args: &[&str]) {
    let _ = Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .status();
}

/// A new secret: 32 random bytes in hex.
pub fn new_secret() -> Result<String> {
    config::random_hex(32)
}

/// The multipart body of the script upload: `metadata` and the module.
fn multipart(metadata: &Value, boundary: &str) -> Vec<u8> {
    let mut b = Vec::new();
    let part = |b: &mut Vec<u8>, name: &str, filename: Option<&str>, ct: &str, data: &[u8]| {
        b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match filename {
            Some(f) => b.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n")
                    .as_bytes(),
            ),
            None => b.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
            ),
        }
        b.extend_from_slice(format!("Content-Type: {ct}\r\n\r\n").as_bytes());
        b.extend_from_slice(data);
        b.extend_from_slice(b"\r\n");
    };
    part(
        &mut b,
        "metadata",
        None,
        "application/json",
        metadata.to_string().as_bytes(),
    );
    part(
        &mut b,
        "worker.js",
        Some("worker.js"),
        "application/javascript+module",
        WORKER_JS.as_bytes(),
    );
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    b
}

/// The upload's metadata. The migration goes only when the Worker does not
/// have it yet (Cloudflare refuses a tag that is already applied).
pub fn metadata(with_migration: bool, old_tag: Option<&str>) -> Value {
    let mut m = json!({
        "main_module": "worker.js",
        "compatibility_date": COMPATIBILITY_DATE,
        "bindings": [
            { "type": "durable_object_namespace", "name": DO_BINDING, "class_name": DO_CLASS }
        ],
        "keep_bindings": ["secret_text"],
        "tags": [SCRIPT_TAG],
    });
    if with_migration {
        let mut mig = json!({ "new_tag": MIGRATION_TAG, "new_sqlite_classes": [DO_CLASS] });
        if let Some(t) = old_tag {
            mig["old_tag"] = json!(t);
        }
        m["migrations"] = mig;
    }
    m
}

fn valid_script_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// `kontext server deploy`.
pub fn deploy(args: &DeployArgs) -> Result<Deployed> {
    if !valid_script_name(&args.name) {
        bail!(
            "invalid Worker name {:?}: lowercase letters, digits and -, at most 63",
            args.name
        );
    }
    let base = std::env::var("KONTEXT_CF_API")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| CF_API.to_string());
    let cf = Cf {
        http: Http::new(Duration::from_secs(60)),
        base: normalize_url(&base),
        token: cloudflare_token()?,
    };

    // 1. The account.
    let account = match args
        .account_id
        .clone()
        .or_else(|| std::env::var("CLOUDFLARE_ACCOUNT_ID").ok())
        .filter(|s| !s.trim().is_empty())
    {
        Some(a) => a.trim().to_string(),
        None => {
            let list = cf.call("GET", "/accounts", None)?;
            let accounts: Vec<(String, String)> = list
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            Some((
                                x.get("id")?.as_str()?.to_string(),
                                x.get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            match accounts.as_slice() {
                [(id, _)] => id.clone(),
                [] => bail!(
                    "the token sees no Cloudflare account: pass --account-id (or set CLOUDFLARE_ACCOUNT_ID)"
                ),
                many => bail!(
                    "the token sees several accounts ({}): pass --account-id <id>",
                    many.iter()
                        .map(|(id, n)| format!("{n} = {id}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
    };

    // 2. The account's workers.dev subdomain, before touching anything.
    let sub_path = format!("/accounts/{account}/workers/subdomain");
    let current = match cf.call("GET", &sub_path, None) {
        Ok(v) => v
            .get("subdomain")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
        Err(e) => match e.downcast_ref::<CfError>() {
            // An account that never used workers.dev has none yet.
            Some(c) if c.status == 404 || c.status == 400 => None,
            _ => return Err(e),
        },
    };
    let mut subdomain_created = false;
    let subdomain = match (current, &args.subdomain) {
        (Some(s), _) => s,
        (None, Some(want)) => {
            let v = cf.call(
                "PUT",
                &sub_path,
                Some((
                    "application/json",
                    json!({ "subdomain": want }).to_string().as_bytes(),
                )),
            )?;
            subdomain_created = true;
            v.get("subdomain")
                .and_then(Value::as_str)
                .unwrap_or(want)
                .to_string()
        }
        (None, None) => bail!(
            "account {account} has no workers.dev subdomain yet. Pick one (it is public: \
             <name>.workers.dev) and run again with --subdomain <name>. Nothing was changed"
        ),
    };

    // 3. Is the Worker there, and with which migration?
    let scripts = cf.call("GET", &format!("/accounts/{account}/workers/scripts"), None)?;
    let existing = scripts
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|s| s.get("id").and_then(Value::as_str) == Some(args.name.as_str()))
        })
        .cloned();
    // A Worker with this name that kontext did not upload is never
    // overwritten without --replace.
    if let Some(s) = &existing {
        let ours = s
            .get("tags")
            .and_then(Value::as_array)
            .is_some_and(|t| t.iter().any(|x| x.as_str() == Some(SCRIPT_TAG)));
        if !ours && !args.replace {
            return Err(crate::store::Refused(format!(
                "account {account} already has a Worker named {name} that kontext did not deploy (it lacks \
                 the tag {SCRIPT_TAG}); nothing was changed. Deploy under another name (--name <other>), \
                 or, if that Worker is yours to overwrite, run again with --replace",
                name = args.name
            ))
            .into());
        }
    }
    let tag = existing
        .as_ref()
        .and_then(|s| s.get("migration_tag"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let with_migration = match tag.as_deref() {
        None => true,
        Some(MIGRATION_TAG) => false,
        Some(other) => bail!(
            "the Worker {} in account {account} has the Durable Object migration {other:?}, which is \
             not kontext's ({MIGRATION_TAG}): is it another Worker with the same name? Use --name",
            args.name
        ),
    };

    // The admin secret: the one saved for this Worker, or a new one, saved
    // before uploading so a failure halfway never loses it.
    let mut servers = Servers::load()?;
    let url = format!("https://{}.{subdomain}.workers.dev", args.name);
    let (secret, secret_new) = match servers
        .by_script(&account, &args.name)
        .or_else(|| servers.by_url(&url))
    {
        Some(s) => (s.admin_secret.clone(), false),
        None => (new_secret()?, true),
    };
    servers.upsert(ServerEntry {
        url: url.clone(),
        account_id: Some(account.clone()),
        script: Some(args.name.clone()),
        admin_secret: secret.clone(),
    });
    servers.save()?;

    // 4. The script.
    let boundary = format!("kontext-{}", config::random_hex(12)?);
    let body = multipart(&metadata(with_migration, None), &boundary);
    let ct = format!("multipart/form-data; boundary={boundary}");
    let script_path = format!("/accounts/{account}/workers/scripts/{}", args.name);
    cf.call("PUT", &script_path, Some((&ct, &body)))?;

    // 5. The admin secret.
    cf.call(
        "PUT",
        &format!("{script_path}/secrets"),
        Some((
            "application/json",
            json!({ "name": ADMIN_SECRET_NAME, "text": secret, "type": "secret_text" })
                .to_string()
                .as_bytes(),
        )),
    )?;

    // 6. On workers.dev.
    cf.call(
        "POST",
        &format!("{script_path}/subdomain"),
        Some((
            "application/json",
            json!({ "enabled": true, "previews_enabled": false })
                .to_string()
                .as_bytes(),
        )),
    )?;

    let reachable = if args.no_check {
        None
    } else {
        Some(wait_reachable(&url))
    };
    Ok(Deployed {
        url,
        account_id: account,
        script: args.name.clone(),
        created: existing.is_none(),
        migration_applied: with_migration,
        subdomain_created,
        admin_secret_file: Servers::path()?.display().to_string(),
        admin_secret_new: secret_new,
        reachable,
    })
}

/// Does `<url>/v1/info` answer as a kontext server (a few tries)?
fn wait_reachable(url: &str) -> bool {
    let http = Http::new(Duration::from_secs(5));
    for i in 0..6 {
        if let Ok(r) = http.request("GET", &format!("{url}/v1/info"), &[], None)
            && r.ok()
            && r.json()
                .ok()
                .and_then(|v| v.get("kontext_server").cloned())
                .is_some()
        {
            return true;
        }
        if i < 5 {
            std::thread::sleep(Duration::from_secs(5));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_has_the_migration_only_when_asked() {
        let m = metadata(true, None);
        assert_eq!(m["migrations"]["new_sqlite_classes"][0], "Project");
        assert_eq!(m["migrations"]["new_tag"], "v1");
        assert!(metadata(false, None).get("migrations").is_none());
        assert_eq!(m["keep_bindings"][0], "secret_text");
        assert_eq!(m["tags"], json!(["kontext-server"]));
        assert_eq!(m["bindings"][0]["type"], "durable_object_namespace");
    }

    #[test]
    fn script_names() {
        assert!(valid_script_name("kontext"));
        assert!(valid_script_name("kontext-2"));
        assert!(!valid_script_name("Kontext"));
        assert!(!valid_script_name("-x"));
        assert!(!valid_script_name(""));
    }

    /// The worker and the binary agree on the longest line, and it is 4 MiB.
    #[test]
    fn the_line_cap_is_the_same_on_both_sides() {
        let decl = WORKER_JS
            .lines()
            .find_map(|l| l.strip_prefix("const MAX_LINE_BYTES = "))
            .expect("MAX_LINE_BYTES in worker.js");
        let js: usize = decl
            .trim_end_matches(';')
            .split('*')
            .map(|f| f.trim().parse::<usize>().unwrap())
            .product();
        assert_eq!(js, MAX_LINE_BYTES);
        assert_eq!(MAX_LINE_BYTES, 4_194_304);
    }

    #[test]
    fn the_worker_is_embedded() {
        assert!(WORKER_JS.contains("export class Project extends DurableObject"));
        // The local server runs it with the same compatibility date.
        let dev = include_str!("../server/dev.mjs");
        assert!(dev.contains(&format!("COMPATIBILITY_DATE = \"{COMPATIBILITY_DATE}\"")));
    }
}
