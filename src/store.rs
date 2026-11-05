//! Per-writer files, locks and the chain (DESIGN §4.1 and §7.1).
//!
//! To write: an exclusive lock (`flock`) on `.kontext/locks/<writer>.lock`.
//! Under the lock the index is caught up, the last line of the writer's file is
//! read, the new entries are chained, they are written in one go with
//! `O_APPEND`, and `fsync` is called.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::{Person, Project, validate_writer, write_atomic};
use crate::cursor::{self, Frontier};
use crate::hlc::Hlc;
use crate::index::Index;
use crate::model::{
    Actor, Entry, FORMAT_VERSION, GENESIS, RawEntry, compute_hash, compute_id, content_of,
    prefix_for,
};
use crate::sign::{self, Signer, Verdict, Verifier};
use crate::state::{Policy, State};

/// An error that is not a failure: the condition for a write was not met.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// The `ledger/*.jsonl` files, sorted, with their writer.
pub fn ledger_files(ledger_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let rd = match fs::read_dir(ledger_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for ent in rd {
        let p = ent?.path();
        if p.extension().is_some_and(|x| x == "jsonl")
            && p.is_file()
            && let Some(w) = p.file_stem().and_then(|s| s.to_str())
        {
            out.push((w.to_string(), p.clone()));
        }
    }
    out.sort();
    Ok(out)
}

/// The exclusive lock of a writer (`.kontext/locks/<writer>.lock`). It is
/// released when the file is dropped.
pub fn lock_writer(home: &Path, writer: &str) -> Result<File> {
    validate_writer(writer)?;
    let locks = crate::config::internal_dir(home).join("locks");
    fs::create_dir_all(&locks)?;
    let lock_path = locks.join(format!("{writer}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("could not open {}", lock_path.display()))?;
    lock.lock()
        .with_context(|| format!("could not take the lock {}", lock_path.display()))?;
    Ok(lock)
}

pub struct Store {
    home: PathBuf,
    writer: String,
    index: Index,
    policy: Policy,
    /// Phase 5: what signs the entries that need it.
    signer: Option<Signer>,
    /// Phase 5: `[people]` from `project.toml`, to verify signatures.
    people: BTreeMap<String, Person>,
}

/// What [`Batch::push`] returned.
#[derive(Debug, Clone)]
pub struct Pushed {
    pub entry: Entry,
    /// `false` if an identical entry already existed and nothing was written.
    pub new: bool,
}

impl Store {
    pub fn open(home: &Path, writer: &str) -> Result<Store> {
        validate_writer(writer)?;
        let index = Index::open(home)?;
        let policy = Policy::load(home)?;
        let people = if home.join("project.toml").exists() {
            Project::load(home)?.people
        } else {
            BTreeMap::new()
        };
        Ok(Store {
            home: home.to_path_buf(),
            writer: writer.to_string(),
            index,
            policy,
            signer: None,
            people,
        })
    }

    /// Signs what needs signing with this key (phase 5).
    pub fn set_signer(&mut self, signer: Option<Signer>) {
        self.signer = signer;
    }

    pub fn signer(&self) -> Option<&Signer> {
        self.signer.as_ref()
    }

    /// The policy with the result of the signatures of `entries` (only if
    /// `require_signatures`; otherwise the signatures do not change the state).
    fn policy_for(&self, entries: &[Entry]) -> Result<Policy> {
        let mut policy = self.policy.clone();
        if !policy.require_signatures {
            return Ok(policy);
        }
        let mut v = Verifier::new(&self.home, &self.people, &self.index)?;
        for e in entries {
            if let Some(who) = sign::wants_signature(e) {
                let verdict = v.check(&self.index, e, &who)?;
                policy.signatures.insert(e.id.clone(), verdict);
            }
        }
        Ok(policy)
    }

    /// Checks the signatures in the ledger files (for `kontext verify`; the
    /// files are read, not the index): for each entry with a `sig`, or that
    /// needs one under `require_signatures`, its result.
    pub fn check_signatures(&mut self) -> Result<Vec<(Entry, Verdict)>> {
        let mut entries = Vec::new();
        for (_, path) in ledger_files(&self.ledger_dir())? {
            let text = fs::read_to_string(&path)?;
            entries.extend(
                text.lines()
                    .filter_map(|l| serde_json::from_str::<Entry>(l).ok()),
            );
        }
        let mut v = Verifier::new(&self.home, &self.people, &self.index)?;
        let mut out = Vec::new();
        for e in entries {
            let has_sig = e.sig.as_ref().is_some_and(|s| !s.is_null());
            let wanted = sign::wants_signature(&e);
            if !has_sig && !(self.policy.require_signatures && wanted.is_some()) {
                continue;
            }
            let verdict = match wanted.or_else(|| sign::identity(&e.actor, &e.fields)) {
                Some(who) => v.check(&self.index, &e, &who)?,
                None => Verdict::Invalid(format!(
                    "{} is neither a human nor the pm with approved_by: it has nothing to sign with",
                    e.actor.short()
                )),
            };
            out.push((e, verdict));
        }
        Ok(out)
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn writer(&self) -> &str {
        &self.writer
    }

    pub fn ledger_dir(&self) -> PathBuf {
        self.home.join("ledger")
    }

    pub fn writer_file(&self) -> PathBuf {
        self.ledger_dir().join(format!("{}.jsonl", self.writer))
    }

    fn internal_dir(&self) -> PathBuf {
        crate::config::internal_dir(&self.home)
    }

    fn hlc_file(&self) -> PathBuf {
        self.internal_dir().join(format!("hlc-{}", self.writer))
    }

    pub fn index(&mut self) -> &mut Index {
        &mut self.index
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// The current derived state (catches up the index).
    pub fn state(&mut self) -> Result<State> {
        self.index.catch_up()?;
        let entries = self.index.all_entries()?;
        let policy = self.policy_for(&entries)?;
        Ok(State::build(entries, &policy))
    }

    /// The current frontier: how far each ledger file goes.
    pub fn frontier(&mut self) -> Result<Frontier> {
        self.index.catch_up()?;
        Ok(Frontier(self.index.frontier()?))
    }

    /// The effective cursor of a reader.
    pub fn cursor(&self, reader: &str) -> Result<Frontier> {
        cursor::load(&self.home, reader)
    }

    /// What `reader` has not seen, in `hlc` order.
    pub fn new_since(&mut self, reader: &str) -> Result<Vec<Entry>> {
        let since = self.cursor(reader)?;
        self.index.catch_up()?;
        let all = self.index.all_entries()?;
        Ok(cursor::new_since(&all, &since)
            .into_iter()
            .cloned()
            .collect())
    }

    /// Advances the cursor of `reader` on this machine up to `frontier`.
    /// It runs under the writer's lock so that `kontext sync` does not push a
    /// cursor that is half-written, or overwrite it when it brings in others'
    /// changes.
    pub fn advance(&self, reader: &str, frontier: &Frontier) -> Result<()> {
        let _lock = lock_writer(&self.home, &self.writer)?;
        cursor::advance(&self.home, reader, &self.writer, frontier)
    }

    /// Adds an entry. If an identical one already exists, returns that one.
    pub fn append(&mut self, raw: RawEntry) -> Result<Entry> {
        let (pushed, _) = self.transact(|b| b.push(raw))?;
        Ok(pushed.entry)
    }

    /// Adds an entry only if `check` accepts the current state. The check and
    /// the write happen under the same lock, so it is atomic across processes of
    /// the same writer.
    pub fn append_if(
        &mut self,
        raw: RawEntry,
        check: impl FnOnce(&State) -> Result<()>,
    ) -> Result<Entry> {
        let (pushed, _) = self.transact(|b| {
            check(b.state()?)?;
            b.push(raw)
        })?;
        Ok(pushed.entry)
    }

    /// Runs `f` under the writer's lock. What `f` adds with [`Batch::push`] is
    /// written at the end, only if `f` succeeds. Returns the result of `f` and
    /// the entries written.
    pub fn transact<T>(
        &mut self,
        f: impl FnOnce(&mut Batch) -> Result<T>,
    ) -> Result<(T, Vec<Entry>)> {
        fs::create_dir_all(self.ledger_dir())?;
        let lock = lock_writer(&self.home, &self.writer)?;

        self.index.catch_up()?;
        // With `require_signatures`, the transaction's state already carries the
        // result of the signatures (so `append_if` sees the same as a read).
        let (policy, indexed) = if self.policy.require_signatures {
            let all = self.index.all_entries()?;
            (self.policy_for(&all)?, Some(all))
        } else {
            (self.policy.clone(), None)
        };
        let path = self.writer_file();
        let (last_n, prev) = read_tail(&path)?;
        let persisted = read_hlc(&self.hlc_file())?;
        let observed = match self.index.max_hlc()? {
            Some(h) => h.parse::<Hlc>().ok(),
            None => None,
        };
        let mut batch = Batch {
            index: &self.index,
            policy,
            signer: self.signer.as_ref(),
            home: &self.home,
            people: &self.people,
            writer: &self.writer,
            next_n: last_n + 1,
            prev,
            last_hlc: persisted.max(observed),
            written_hlc: persisted,
            pending: Vec::new(),
            pending_ids: HashMap::new(),
            indexed,
            state: None,
        };
        let out = f(&mut batch)?;
        let Batch {
            pending,
            written_hlc,
            ..
        } = batch;

        if !pending.is_empty() {
            let created = !path.exists();
            let mut buf = String::new();
            for e in &pending {
                buf.push_str(&e.to_line());
                buf.push('\n');
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("could not open {}", path.display()))?;
            file.write_all(buf.as_bytes())?;
            file.sync_all()?;
            if created {
                File::open(self.ledger_dir())?.sync_all()?;
            }
            if let Some(h) = written_hlc {
                write_atomic(&self.hlc_file(), h.to_string().as_bytes())?;
            }
        }
        drop(lock);
        Ok((out, pending))
    }
}

/// The entries about to be written inside [`Store::transact`].
pub struct Batch<'a> {
    index: &'a Index,
    policy: Policy,
    signer: Option<&'a Signer>,
    home: &'a Path,
    people: &'a BTreeMap<String, Person>,
    writer: &'a str,
    next_n: u64,
    prev: String,
    /// The last one emitted or seen: the next `hlc` goes after it.
    last_hlc: Option<Hlc>,
    /// The last one emitted by this writer (saved in `.kontext/hlc-<writer>`).
    written_hlc: Option<Hlc>,
    pending: Vec<Entry>,
    pending_ids: HashMap<String, usize>,
    indexed: Option<Vec<Entry>>,
    state: Option<State>,
}

impl Batch<'_> {
    pub fn writer(&self) -> &str {
        self.writer
    }

    /// What has been added so far in this transaction.
    pub fn pending(&self) -> &[Entry] {
        &self.pending
    }

    /// The derived state: the index (caught up under the lock) plus what was
    /// added in this transaction.
    pub fn state(&mut self) -> Result<&State> {
        if self.state.is_none() {
            if self.indexed.is_none() {
                self.indexed = Some(self.index.all_entries()?);
            }
            let all = self
                .indexed
                .iter()
                .flatten()
                .chain(self.pending.iter())
                .cloned();
            self.state = Some(State::build(all, &self.policy));
        }
        Ok(self.state.as_ref().unwrap())
    }

    fn existing(&self, id: &str) -> Result<Option<Value>> {
        if let Some(i) = self.pending_ids.get(id) {
            return Ok(Some(self.pending[*i].to_value()));
        }
        self.index.get_value(id)
    }

    /// Chains a new entry. If the `id` already exists with other content, it
    /// advances the `hlc` counter and computes it again. If it exists with the
    /// same content, it writes nothing and returns the one already there.
    pub fn push(&mut self, raw: RawEntry) -> Result<Pushed> {
        if prefix_for(&raw.kind).is_none() {
            bail!("unknown entry kind: {:?}", raw.kind);
        }
        let actor = match raw.actor {
            Some(a) => a,
            None => Actor::from_env()?,
        };
        if !matches!(actor.kind.as_str(), "human" | "agent") || actor.name.trim().is_empty() {
            bail!(
                "invalid actor: type must be human or agent, and name cannot be empty (got {:?}:{:?})",
                actor.kind,
                actor.name
            );
        }
        let mut hlc = match &raw.hlc {
            Some(h) => h.parse::<Hlc>()?,
            None => Hlc::now(self.last_hlc),
        };
        let mut entry = Entry {
            v: FORMAT_VERSION,
            id: String::new(),
            writer: self.writer.to_string(),
            n: self.next_n,
            prev: self.prev.clone(),
            hash: String::new(),
            hlc: String::new(),
            actor,
            kind: raw.kind,
            title: raw.title,
            body: raw.body,
            why: raw.why,
            tags: raw.tags,
            refs: raw.refs,
            fields: raw.fields,
            sig: None,
            extra: Default::default(),
        };
        let (mut value, id) = loop {
            entry.hlc = hlc.to_string();
            let value = entry.to_value();
            let id = compute_id(&value)?;
            match self.existing(&id)? {
                None => break (value, id),
                Some(old) if content_of(&old) == content_of(&value) => {
                    return Ok(Pushed {
                        entry: Entry::from_value(old)?,
                        new: false,
                    });
                }
                // Same id, different content: 24 bits collide now and then.
                Some(_) => hlc = hlc.bump(),
            }
        };
        value["id"] = json!(id);
        let hash = compute_hash(&value);
        value["hash"] = json!(hash);
        let mut entry = Entry::from_value(value)?;
        self.sign(&mut entry)?;

        self.next_n += 1;
        self.prev = hash;
        self.last_hlc = self.last_hlc.max(Some(hlc));
        self.written_hlc = self.written_hlc.max(Some(hlc));
        self.pending_ids.insert(id, self.pending.len());
        self.pending.push(entry.clone());
        self.state = None;
        Ok(Pushed { entry, new: true })
    }
}

impl Batch<'_> {
    /// Phase 5: signs the entry if it is one that gets signed and a key is
    /// available. With `require_signatures`, without a key or with one that is
    /// not the person's, it is refused: the entry would be stored but would not
    /// count.
    fn sign(&mut self, entry: &mut Entry) -> Result<()> {
        let Some(who) = sign::wants_signature(entry) else {
            return Ok(());
        };
        let required = self.policy.require_signatures;
        let Some(signer) = self.signer else {
            if required {
                return Err(Refused(format!(
                    "project.toml requires require_signatures and this machine has no key to sign with \
                     ({} from {who}). Add to projects.toml:\n[signing]\nkey = \"~/.ssh/id_ed25519.pub\"\n\
                     and the public key to [people] in project.toml",
                    entry.kind
                ))
                .into());
            }
            return Ok(());
        };
        entry.sig = Some(json!(signer.sign(&entry.hash)?));
        if required {
            let mut v = Verifier::new(self.home, self.people, self.index)?;
            let verdict = v.check(self.index, entry, &who)?;
            if let Verdict::Invalid(reason) = &verdict {
                return Err(Refused(format!(
                    "the signature with {} does not verify for {who}: {reason}. The entry would not count; \
                     add the public key to [people] in project.toml",
                    signer.key.display()
                ))
                .into());
            }
            self.policy.signatures.insert(entry.id.clone(), verdict);
        }
        Ok(())
    }
}

/// `n` and `hash` of the last line, or `(0, GENESIS)` if the file does not exist.
/// If the last line was left half-written, nothing is written on top of it.
pub fn read_tail(path: &Path) -> Result<(u64, String)> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, GENESIS.to_string())),
        Err(e) => return Err(e.into()),
    };
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok((0, GENESIS.to_string()));
    }
    let mut last = [0u8; 1];
    f.seek(SeekFrom::Start(len - 1))?;
    f.read_exact(&mut last)?;
    if last[0] != b'\n' {
        bail!(
            "{} ends with an incomplete line (was a write cut off?). \
             Run `kontext verify` and repair it by hand before writing again",
            path.display()
        );
    }
    // Read backwards until the previous line break is found.
    let end = len - 1;
    let mut start = end;
    let mut line: Vec<u8> = Vec::new();
    const CHUNK: u64 = 8192;
    loop {
        let from = start.saturating_sub(CHUNK);
        let mut buf = vec![0u8; (start - from) as usize];
        f.seek(SeekFrom::Start(from))?;
        f.read_exact(&mut buf)?;
        let found = buf.iter().rposition(|b| *b == b'\n');
        let tail = match found {
            Some(i) => &buf[i + 1..],
            None => &buf[..],
        };
        let mut joined = tail.to_vec();
        joined.extend_from_slice(&line);
        line = joined;
        if found.is_some() || from == 0 {
            break;
        }
        start = from;
    }
    let v: Value = serde_json::from_slice(&line)
        .with_context(|| format!("the last line of {} is not valid JSON", path.display()))?;
    let n = v.get("n").and_then(Value::as_u64);
    let hash = v.get("hash").and_then(Value::as_str);
    match (n, hash) {
        (Some(n), Some(h)) => Ok((n, h.to_string())),
        _ => bail!("the last line of {} has no n and hash", path.display()),
    }
}

fn read_hlc(path: &Path) -> Result<Option<Hlc>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(s.trim().parse::<Hlc>().ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RawEntry;
    use crate::state::Status;
    use crate::verify;

    fn store(dir: &Path, writer: &str) -> Store {
        Store::open(dir, writer).unwrap()
    }

    fn raw(kind: &str, title: &str) -> RawEntry {
        let mut r = RawEntry::new(kind, title);
        r.actor = Some(Actor::human("omar"));
        r
    }

    #[test]
    fn append_chains_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "omar@mac");
        let a = s.append(raw("decision", "Use Rust")).unwrap();
        let b = s.append(raw("fact", "There is SQLite")).unwrap();
        assert_eq!((a.n, b.n), (1, 2));
        assert_eq!(a.prev, GENESIS);
        assert_eq!(b.prev, a.hash);
        assert!(a.id.starts_with("D-") && b.id.starts_with("F-"));
        assert!(b.hlc > a.hlc);
        assert_eq!(read_tail(&s.writer_file()).unwrap(), (2, b.hash.clone()));
        // The index catches up on read.
        s.index().catch_up().unwrap();
        assert_eq!(s.index().count().unwrap(), 2);
        assert_eq!(s.index().search("sqlite").unwrap(), vec![b.id.clone()]);
        assert!(verify::run(dir.path()).unwrap().ok());
    }

    #[test]
    fn identical_entry_is_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "omar@mac");
        let mut r = raw("fact", "one");
        r.hlc = Some("2026-11-03T18:22:10.123Z.0000".into());
        let a = s.append(r.clone()).unwrap();
        let b = s.append(r).unwrap();
        assert_eq!(a, b);
        assert_eq!(read_tail(&s.writer_file()).unwrap().0, 1);
    }

    #[test]
    fn id_collision_bumps_hlc() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "omar@mac");
        // Plant another entry in the index with the id the new one would get.
        let mut r = raw("fact", "collision");
        r.hlc = Some("2026-11-03T18:22:10.123Z.0000".into());
        let (pushed, _) = s
            .transact(|b| {
                let first = b.push(r.clone())?;
                // Same clock position, different content with the same id: this
                // is simulated by changing the content stored in the pending entry.
                b.pending[0].title = "other content".into();
                b.state = None;
                b.push(r.clone()).map(|p| (first, p))
            })
            .unwrap();
        let (first, second) = pushed;
        assert!(second.new);
        assert_ne!(first.entry.id, second.entry.id);
        assert_eq!(second.entry.hlc, "2026-11-03T18:22:10.123Z.0001");
    }

    #[test]
    fn append_if_checks_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "omar@mac");
        let task = s.append(raw("task", "Collect")).unwrap();
        let claim = |s: &mut Store| {
            let mut r = raw("claim", "");
            r.refs = vec![task.id.clone()];
            let t = task.id.clone();
            s.append_if(r, move |st| {
                if st.claim_of(&t).is_some() {
                    return Err(Refused("already has an owner".into()).into());
                }
                Ok(())
            })
        };
        claim(&mut s).unwrap();
        let err = claim(&mut s).unwrap_err();
        assert!(err.downcast_ref::<Refused>().is_some());
        let st = s.state().unwrap();
        assert_eq!(st.status_of(&task.id), Some(Status::Claimed));
        assert_eq!(read_tail(&s.writer_file()).unwrap().0, 2);
    }

    #[test]
    fn failed_transaction_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "omar@mac");
        let r: Result<((), Vec<Entry>)> = s.transact(|b| {
            b.push(raw("fact", "one"))?;
            bail!("no")
        });
        assert!(r.is_err());
        assert!(!s.writer_file().exists());
    }

    #[test]
    fn hlc_receive_rule_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        // Another writer with its clock a year ahead.
        let mut ana = store(dir.path(), "ana@laptop");
        let mut r = raw("fact", "from the future");
        r.hlc = Some("2099-01-01T00:00:00.000Z.0000".into());
        ana.append(r).unwrap();
        let mut omar = store(dir.path(), "omar@mac");
        let e = omar.append(raw("fact", "later")).unwrap();
        assert_eq!(e.hlc, "2099-01-01T00:00:00.000Z.0001");
        let saved = fs::read_to_string(dir.path().join(".kontext/hlc-omar@mac")).unwrap();
        assert_eq!(saved, e.hlc);
    }

    #[test]
    fn refuses_to_append_after_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "omar@mac");
        s.append(raw("fact", "one")).unwrap();
        let mut f = OpenOptions::new()
            .append(true)
            .open(s.writer_file())
            .unwrap();
        f.write_all(b"{\"v\":1,\"id\":").unwrap();
        let err = s.append(raw("fact", "two")).unwrap_err();
        assert!(err.to_string().contains("incomplete"), "{err}");
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "omar@mac");
        assert!(s.append(raw("nothing", "x")).is_err());
    }
}
