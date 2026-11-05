//! SSH signatures (DESIGN §4.2 and §14, phase 5).
//!
//! The `hash` of the entry (which does not include `sig`) is signed with
//! `ssh-keygen -Y sign -n kontext -f <key>`, and the armored signature (the
//! text `-----BEGIN SSH SIGNATURE-----`) goes in `sig`. To verify, an
//! `allowed_signers` file is built from the keys in `[people]` of
//! `project.toml`, and `ssh-keygen -Y verify -n kontext -f <file> -I <person>
//! -s <signature>` runs with the `hash` on stdin. Signatures made before the
//! command was called kontext use the namespace `ctx`: if it does not verify
//! with `kontext`, it is tried with `ctx` ([`LEGACY_NAMESPACE`]).
//!
//! What gets signed: `sign`, `decision`, `accept`, `reject` and `supersede`
//! written by a human (their name is the identity) or by the pm with
//! `approved_by` (the identity is the owner who approved). Verifying is slow
//! (one process per signature), so the result is stored in the index
//! (`sig_checks`).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::config::Person;
use crate::index::Index;
use crate::model::{Entry, sha256_hex};

/// The signature namespace (`-n`): a kontext signature is not valid for
/// anything else, and the reverse.
pub const NAMESPACE: &str = "kontext";

/// The namespace the command signed with when it was called `ctx`. It is only
/// used to verify what was already signed; never to sign.
pub const LEGACY_NAMESPACE: &str = "ctx";

/// The kinds that get signed.
pub const SIGNED_KINDS: &[&str] = &["sign", "decision", "accept", "reject", "supersede"];

/// Does this kind get signed?
pub fn is_signed_kind(kind: &str) -> bool {
    SIGNED_KINDS.contains(&kind)
}

/// The person who signs an entry: the human, or the owner in `approved_by` if
/// the pm wrote it. `None` for everything else (an agent has no key).
pub fn identity(
    actor: &crate::model::Actor,
    fields: &serde_json::Map<String, Value>,
) -> Option<String> {
    if actor.is_human() {
        return Some(actor.name.clone());
    }
    if actor.role.as_deref() == Some("pm") {
        return fields
            .get("approved_by")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    None
}

/// Does this entry need a signature (or require one)? Entries imported from v0
/// do not: the v0 rule had already accepted them.
pub fn wants_signature(e: &Entry) -> Option<String> {
    if !is_signed_kind(&e.kind) || e.fields.contains_key("v0_id") {
        return None;
    }
    identity(&e.actor, &e.fields)
}

/// A person's name works as a principal in `allowed_signers` if it has no
/// spaces, commas or quotes.
fn valid_principal(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
}

/// The contents of `allowed_signers` with the keys from `[people]`.
pub fn allowed_signers(people: &BTreeMap<String, Person>) -> String {
    let mut out = String::new();
    for (name, p) in people {
        if !valid_principal(name) {
            continue;
        }
        for k in &p.keys {
            let k = k.trim();
            if k.is_empty() || k.contains('\n') {
                continue;
            }
            out.push_str(&format!(
                "{name} namespaces=\"{NAMESPACE},{LEGACY_NAMESPACE}\" {k}\n"
            ));
        }
    }
    out
}

/// Signs with a key from this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    pub key: PathBuf,
}

impl Signer {
    /// Signs `hash`. Returns the armored signature.
    pub fn sign(&self, hash: &str) -> Result<String> {
        if !self.key.exists() {
            bail!(
                "the key to sign with does not exist: {} ([signing] key in projects.toml)",
                self.key.display()
            );
        }
        let out = run_with_stdin(
            Command::new("ssh-keygen")
                .args(["-q", "-Y", "sign", "-n", NAMESPACE, "-f"])
                .arg(&self.key),
            hash.as_bytes(),
        )
        .context("could not run ssh-keygen to sign")?;
        if !out.status.success() {
            bail!(
                "ssh-keygen could not sign with {}: {}",
                self.key.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let sig = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !sig.starts_with("-----BEGIN SSH SIGNATURE-----") {
            bail!("ssh-keygen did not return a signature: {sig:?}");
        }
        Ok(sig)
    }
}

fn run_with_stdin(cmd: &mut Command, input: &[u8]) -> std::io::Result<std::process::Output> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(input)?;
    child.wait_with_output()
}

/// The result of checking a signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Valid,
    /// The reason: "signature is missing", "invalid signature for omar"…
    Invalid(String),
}

impl Verdict {
    pub fn is_valid(&self) -> bool {
        matches!(self, Verdict::Valid)
    }
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_path(dir: &Path, what: &str) -> PathBuf {
    dir.join(format!(
        "{what}-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Verifies signatures against the keys in `[people]`, with the result stored
/// in the index. Temporary files go in `<home>/.kontext/tmp`.
pub struct Verifier<'a> {
    people: &'a BTreeMap<String, Person>,
    allowed: String,
    tmp_dir: PathBuf,
    allowed_file: Option<PathBuf>,
    cache: HashMap<String, Verdict>,
}

impl<'a> Verifier<'a> {
    pub fn new(home: &Path, people: &'a BTreeMap<String, Person>, index: &Index) -> Result<Self> {
        Ok(Verifier {
            people,
            allowed: allowed_signers(people),
            tmp_dir: crate::config::internal_dir(home).join("tmp"),
            allowed_file: None,
            cache: index.sig_checks()?,
        })
    }

    /// The cache key: everything that decides the result.
    fn key(&self, e: &Entry, who: &str, sig: &str) -> String {
        sha256_hex(format!("{}\0{who}\0{}\0{sig}\0{}", e.id, e.hash, self.allowed).as_bytes())
    }

    /// Checks the signature of `e` as `who`. New results are stored in `index`.
    pub fn check(&mut self, index: &Index, e: &Entry, who: &str) -> Result<Verdict> {
        let Some(sig) = e
            .sig
            .as_ref()
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        else {
            return Ok(Verdict::Invalid("signature is missing".into()));
        };
        let key = self.key(e, who, sig);
        if let Some(v) = self.cache.get(&key) {
            return Ok(v.clone());
        }
        let v = self.run(e, who, sig)?;
        index.put_sig_check(&key, &v)?;
        self.cache.insert(key, v.clone());
        Ok(v)
    }

    fn run(&mut self, e: &Entry, who: &str, sig: &str) -> Result<Verdict> {
        if !self.people.get(who).is_some_and(|p| !p.keys.is_empty()) {
            return Ok(Verdict::Invalid(format!(
                "{who} has no keys in [people] of project.toml"
            )));
        }
        fs::create_dir_all(&self.tmp_dir)?;
        if self.allowed_file.is_none() {
            let p = tmp_path(&self.tmp_dir, "allowed_signers");
            fs::write(&p, &self.allowed)?;
            self.allowed_file = Some(p);
        }
        let sig_file = tmp_path(&self.tmp_dir, "sig");
        fs::write(&sig_file, format!("{}\n", sig.trim()))?;
        let verify = |namespace: &str| {
            run_with_stdin(
                Command::new("ssh-keygen")
                    .args(["-Y", "verify", "-n", namespace, "-f"])
                    .arg(self.allowed_file.as_ref().unwrap())
                    .args(["-I", who, "-s"])
                    .arg(&sig_file),
                e.hash.as_bytes(),
            )
        };
        // Signatures made before the rename use the `ctx` namespace.
        let out = verify(NAMESPACE).and_then(|o| {
            if o.status.success() {
                Ok(o)
            } else {
                verify(LEGACY_NAMESPACE)
            }
        });
        let _ = fs::remove_file(&sig_file);
        let out = out.context("could not run ssh-keygen to verify")?;
        Ok(if out.status.success() {
            Verdict::Valid
        } else {
            Verdict::Invalid(format!(
                "the signature is not from a key of {who} in [people] (or the content changed)"
            ))
        })
    }
}

impl Drop for Verifier<'_> {
    fn drop(&mut self) {
        if let Some(p) = &self.allowed_file {
            let _ = fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_signers_format() {
        let mut people = BTreeMap::new();
        people.insert(
            "omar".to_string(),
            Person {
                emails: vec![],
                keys: vec!["ssh-ed25519 AAAA omar@mac".into()],
            },
        );
        people.insert(
            "with space".to_string(),
            Person {
                emails: vec![],
                keys: vec!["ssh-ed25519 BBBB".into()],
            },
        );
        assert_eq!(
            allowed_signers(&people),
            "omar namespaces=\"kontext,ctx\" ssh-ed25519 AAAA omar@mac\n"
        );
    }

    #[test]
    fn identity_rules() {
        use crate::model::Actor;
        let mut fields = serde_json::Map::new();
        assert_eq!(
            identity(&Actor::human("omar"), &fields),
            Some("omar".into())
        );
        assert_eq!(identity(&Actor::agent("x"), &fields), None);
        let mut pm = Actor::agent("pm");
        pm.role = Some("pm".into());
        assert_eq!(identity(&pm, &fields), None);
        fields.insert("approved_by".into(), "omar".into());
        assert_eq!(identity(&pm, &fields), Some("omar".into()));
    }
}
