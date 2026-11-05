//! Format of an entry (DESIGN §4.2 and §4.3), canonical JSON, `id` and `hash`.
//!
//! Rules:
//! - Canonical JSON = `serde_json::Value` with sorted keys (serde_json's `Map`
//!   is a `BTreeMap` unless `preserve_order` is enabled), compact and UTF-8.
//! - `hash` = sha256 in lowercase hex of the canonical JSON of the entry without
//!   `hash` or `sig`.
//! - `id` = `<PREFIX>-<6 hex>`, with the first 6 characters of the sha256 of the
//!   canonical JSON without `id`, `n`, `prev`, `hash` or `sig`.
//!
//! `hash` and `id` are always computed over the `Value` as it was read or
//! written, not over the struct: that way fields this version does not know
//! survive, and verification does not depend on how it is deserialized.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Version of the entry format.
pub const FORMAT_VERSION: u64 = 1;

/// `prev` of the first line of each file.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Fields that are not part of the `hash`.
const NOT_IN_HASH: &[&str] = &["hash", "sig"];
/// Fields that are not part of the `id` (the rest is the "content").
const NOT_IN_CONTENT: &[&str] = &["id", "n", "prev", "hash", "sig"];

/// Kinds with their own identity, and their prefix (DESIGN §4.3).
pub const ENTITY_KINDS: &[(&str, &str)] = &[
    ("charter", "A"),
    ("goal", "G"),
    ("epic", "E"),
    ("decision", "D"),
    ("proposal", "P"),
    ("question", "Q"),
    ("fact", "F"),
    ("task", "T"),
    ("journal", "J"),
];

/// Records that act on another entry (`refs[0]`). All of them use the prefix `R`.
pub const RECORD_KINDS: &[&str] = &[
    "sign",
    "accept",
    "reject",
    "answer",
    "supersede",
    "assign",
    "claim",
    "release",
    "done",
    "reopen",
    "confirm",
    "dismiss",
];

pub const RECORD_PREFIX: &str = "R";

/// The `id` prefix for a kind, or `None` if the kind does not exist.
pub fn prefix_for(kind: &str) -> Option<&'static str> {
    if let Some((_, p)) = ENTITY_KINDS.iter().find(|(k, _)| *k == kind) {
        return Some(p);
    }
    RECORD_KINDS.contains(&kind).then_some(RECORD_PREFIX)
}

pub fn is_record_kind(kind: &str) -> bool {
    RECORD_KINDS.contains(&kind)
}

/// Who wrote the entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Actor {
    pub fn human(name: &str) -> Actor {
        Actor::new("human", name)
    }

    pub fn agent(name: &str) -> Actor {
        Actor::new("agent", name)
    }

    fn new(kind: &str, name: &str) -> Actor {
        Actor {
            kind: kind.to_string(),
            name: name.to_string(),
            role: None,
            model: None,
            session: None,
            extra: Map::new(),
        }
    }

    /// `human:omar` or `agent:epic:payments` (everything after the first `:` is the name).
    pub fn parse_short(s: &str) -> Result<Actor> {
        match s.split_once(':') {
            Some(("human", name)) if !name.is_empty() => Ok(Actor::human(name)),
            Some(("agent", name)) if !name.is_empty() => Ok(Actor::agent(name)),
            _ => bail!("invalid actor {s:?}: use human:<name> or agent:<name>"),
        }
    }

    /// The default actor: `$KONTEXT_ACTOR`, or `agent:unknown`. As in v0, an
    /// agent that forgets to say who it is never ends up as a human.
    pub fn from_env() -> Result<Actor> {
        Actor::resolve(None)
    }

    /// Who writes: `--actor`, then `$KONTEXT_ACTOR`, then `agent:unknown`.
    /// For an agent, `$KONTEXT_ROLE`, `$KONTEXT_MODEL` and `$KONTEXT_SESSION`
    /// are added (role, model and session).
    pub fn resolve(flag: Option<&str>) -> Result<Actor> {
        let env = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let mut actor = match flag.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => Actor::parse_short(s)?,
            None => match env("KONTEXT_ACTOR") {
                Some(s) => Actor::parse_short(&s)?,
                None => Actor::agent("unknown"),
            },
        };
        if actor.is_agent() {
            actor.role = env("KONTEXT_ROLE");
            actor.model = env("KONTEXT_MODEL");
            actor.session = env("KONTEXT_SESSION");
        }
        Ok(actor)
    }

    /// Like [`Actor::resolve`], with `--role` and `--model` taking precedence over
    /// `$KONTEXT_ROLE` and `$KONTEXT_MODEL`. That way an agent says who it is with
    /// arguments, not with variables in front of the command, which a
    /// `Bash(kontext:*)` permission in Claude Code does not cover.
    pub fn resolve_with(
        flag: Option<&str>,
        role: Option<&str>,
        model: Option<&str>,
    ) -> Result<Actor> {
        let mut actor = Actor::resolve(flag)?;
        let given = |v: Option<&str>| {
            v.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        if actor.is_agent() {
            if let Some(r) = given(role) {
                actor.role = Some(r);
            }
            if let Some(m) = given(model) {
                actor.model = Some(m);
            }
        }
        Ok(actor)
    }

    pub fn is_human(&self) -> bool {
        self.kind == "human"
    }

    pub fn is_agent(&self) -> bool {
        self.kind == "agent"
    }

    /// `human:omar` or `agent:epic:payments (pm)`.
    pub fn short(&self) -> String {
        match &self.role {
            Some(r) if self.is_agent() => format!("{}:{} ({r})", self.kind, self.name),
            _ => format!("{}:{}", self.kind, self.name),
        }
    }
}

/// One line of the ledger.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub v: u64,
    pub id: String,
    pub writer: String,
    pub n: u64,
    pub prev: String,
    pub hash: String,
    pub hlc: String,
    pub actor: Actor,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub fields: Map<String, Value>,
    #[serde(default)]
    pub sig: Option<Value>,
    /// Fields this version does not know. They are kept as they are.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Entry {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("an entry can always be serialized")
    }

    pub fn from_value(v: Value) -> Result<Entry> {
        Ok(serde_json::from_value(v)?)
    }

    /// The line as it goes into the file (without the `\n`).
    pub fn to_line(&self) -> String {
        canonical(&self.to_value())
    }
}

/// What comes in to be written. The store sets `v`, `writer`, `n`, `prev`,
/// `hlc` (if it is missing), `id`, `hash` and `sig`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RawEntry {
    pub kind: String,
    #[serde(default)]
    pub actor: Option<Actor>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(default)]
    pub fields: Map<String, Value>,
    /// Only for imports or syncs: a fixed `hlc` instead of the clock.
    #[serde(default)]
    pub hlc: Option<String>,
}

impl RawEntry {
    pub fn new(kind: &str, title: &str) -> RawEntry {
        RawEntry {
            kind: kind.to_string(),
            actor: None,
            title: title.to_string(),
            body: String::new(),
            why: String::new(),
            tags: Vec::new(),
            refs: Vec::new(),
            fields: Map::new(),
            hlc: None,
        }
    }
}

/// Canonical JSON: sorted keys, compact, UTF-8.
pub fn canonical(v: &Value) -> String {
    serde_json::to_string(v).expect("a Value can always be serialized")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn without(v: &Value, drop: &[&str]) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| !drop.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The content of an entry: everything except `id`, `n`, `prev`, `hash` and `sig`.
/// Two entries with the same content are the same entry.
pub fn content_of(v: &Value) -> Value {
    without(v, NOT_IN_CONTENT)
}

/// `hash` of an entry: sha256 of the canonical form without `hash` or `sig`.
pub fn compute_hash(v: &Value) -> String {
    sha256_hex(canonical(&without(v, NOT_IN_HASH)).as_bytes())
}

/// Full sha256 of the content; the first 6 characters form the `id`.
pub fn content_digest(v: &Value) -> String {
    sha256_hex(canonical(&content_of(v)).as_bytes())
}

/// `id` of an entry: the prefix of its kind plus 6 hex digits of the content.
pub fn compute_id(v: &Value) -> Result<String> {
    let kind = v.get("kind").and_then(Value::as_str).unwrap_or_default();
    let Some(prefix) = prefix_for(kind) else {
        bail!("unknown entry kind: {kind:?}");
    };
    Ok(format!("{prefix}-{}", &content_digest(v)[..6]))
}

/// Checks the shape of an `id`: `<PREFIX>-<6 lowercase hex>`.
pub fn is_valid_id(id: &str) -> bool {
    let Some((p, hex)) = id.split_once('-') else {
        return false;
    };
    !p.is_empty()
        && p.chars().all(|c| c.is_ascii_uppercase())
        && hex.len() == 6
        && hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixed_entry() -> Entry {
        Entry {
            v: 1,
            id: String::new(),
            writer: "omar@mac".into(),
            n: 1,
            prev: GENESIS.into(),
            hash: String::new(),
            hlc: "2026-11-03T18:22:10.123Z.0004".into(),
            actor: Actor {
                kind: "agent".into(),
                name: "epic:payments".into(),
                role: Some("coordinator".into()),
                model: Some("claude-opus-5-5".into()),
                session: Some("51a2".into()),
                extra: Map::new(),
            },
            kind: "decision".into(),
            title: "Charge with Stripe, not with Conekta".into(),
            body: String::new(),
            why: "Conekta does not support recurring charges with 3DS".into(),
            tags: vec!["pagos".into()],
            refs: vec![],
            fields: json!({"epic": "payments", "guard": {"cmd": "! grep -ri conekta src/", "expect": "exit0"}})
                .as_object()
                .unwrap()
                .clone(),
            sig: None,
            extra: Map::new(),
        }
    }

    fn sealed() -> Value {
        let mut v = fixed_entry().to_value();
        let id = compute_id(&v).unwrap();
        v["id"] = json!(id);
        let hash = compute_hash(&v);
        v["hash"] = json!(hash);
        v
    }

    #[test]
    fn canonical_sorts_keys_and_is_compact() {
        let v = json!({"b": 1, "a": {"d": [1, 2], "c": "ñ"}});
        assert_eq!(canonical(&v), r#"{"a":{"c":"ñ","d":[1,2]},"b":1}"#);
    }

    #[test]
    fn golden_id_and_hash() {
        let v = sealed();
        // Values computed separately with Python (json.dumps with sort_keys and
        // compact separators, without ensure_ascii). If this changes, the format
        // definition changed: every entry written so far would stop verifying.
        assert_eq!(v["id"], "D-0d84a1");
        assert_eq!(
            v["hash"],
            "7ce27631ea6b2d1bdd1f2eaf2a179dda5c15019684565b4c1dc7263032f0f382"
        );
    }

    #[test]
    fn id_ignores_position_and_chain() {
        let a = sealed();
        let mut b = a.clone();
        b["n"] = json!(99);
        b["prev"] = json!("ab".repeat(32));
        b["hash"] = json!("cd".repeat(32));
        b["sig"] = json!("signature");
        assert_eq!(compute_id(&a).unwrap(), compute_id(&b).unwrap());
        // ...but the hash does change with n and prev.
        assert_ne!(compute_hash(&a), compute_hash(&b));
    }

    #[test]
    fn id_depends_on_content_writer_and_hlc() {
        let base = sealed();
        for (k, val) in [
            ("title", json!("other")),
            ("writer", json!("ana@laptop")),
            ("hlc", json!("2026-11-03T18:22:10.123Z.0005")),
        ] {
            let mut v = base.clone();
            v[k] = val;
            assert_ne!(compute_id(&v).unwrap(), base["id"], "field {k}");
        }
    }

    #[test]
    fn hash_ignores_sig() {
        let a = sealed();
        let mut b = a.clone();
        b["sig"] = json!("ssh-sig");
        assert_eq!(compute_hash(&a), compute_hash(&b));
    }

    #[test]
    fn unknown_fields_round_trip() {
        let mut v = sealed();
        v["future"] = json!({"x": [1, 2, 3]});
        v["actor"]["team"] = json!("core");
        let hash = compute_hash(&v);
        v["hash"] = json!(hash);
        let e = Entry::from_value(v.clone()).unwrap();
        assert_eq!(e.to_value(), v);
        assert_eq!(compute_hash(&e.to_value()), hash);
    }

    #[test]
    fn prefixes() {
        assert_eq!(prefix_for("decision"), Some("D"));
        assert_eq!(prefix_for("charter"), Some("A"));
        assert_eq!(prefix_for("claim"), Some("R"));
        assert_eq!(prefix_for("nothing"), None);
        assert!(is_valid_id("D-a3b8ac"));
        assert!(!is_valid_id("D-0001"));
        assert!(!is_valid_id("d-a3b8ac"));
    }

    #[test]
    fn actor_short_form() {
        assert_eq!(
            Actor::parse_short("human:omar").unwrap(),
            Actor::human("omar")
        );
        assert_eq!(
            Actor::parse_short("agent:epic:payments").unwrap(),
            Actor::agent("epic:payments")
        );
        assert!(Actor::parse_short("omar").is_err());
    }
}
