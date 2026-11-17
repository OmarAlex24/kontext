//! What keeps a `dispatch` that dies midway from leaving things behind.
//!
//! - [`TmpFile`]: the local copies of what the sandbox gets as files (the
//!   token and the agent's credential among them), in the dispatch folder's
//!   `tmp/`, mode 0600, named with this process's id. They go when dropped,
//!   and on SIGINT or SIGTERM too: the handler unlinks every one before the
//!   signal ends the process. [`sweep`] removes what a process that died
//!   another way left (run by `dispatch`, the watcher and `stop`).
//! - [`DispatchLock`]: held by the `dispatch` process while it starts an
//!   epic's run, so the watcher knows whether anyone is still at work on a
//!   `starting` run or it has to finish or end it itself.
//! - [`checkpoint`]: lets a test stop `dispatch` at a named step, to kill it
//!   there (`KONTEXT_DISPATCH_TEST_PAUSE`).

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

use anyhow::{Context, Result};

use super::state;
use crate::config;

/// The files to unlink on a signal: a list the handler can read without
/// locking or allocating. Every change publishes a new list and leaks the
/// old one (a few bytes per dispatch), so the handler never reads freed
/// memory.
static SNAPSHOT: AtomicPtr<Vec<CString>> = AtomicPtr::new(std::ptr::null_mut());
static FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn publish(files: &[PathBuf]) {
    use std::os::unix::ffi::OsStrExt;
    let list: Vec<CString> = files
        .iter()
        .filter_map(|p| CString::new(p.as_os_str().as_bytes()).ok())
        .collect();
    SNAPSHOT.store(Box::into_raw(Box::new(list)), Ordering::SeqCst);
}

extern "C" fn on_signal(sig: libc::c_int) {
    let p = SNAPSHOT.load(Ordering::SeqCst);
    if !p.is_null() {
        // SAFETY: the list is never freed or changed once published.
        for c in unsafe { &*p } {
            unsafe { libc::unlink(c.as_ptr()) };
        }
    }
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Unlinks the registered files on SIGINT and SIGTERM, then lets the signal
/// end the process as it would have.
pub fn on_signals() {
    let h = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
    }
}

/// A local file in the dispatch folder's `tmp/`, readable only by this
/// user, removed when dropped or on a signal.
pub struct TmpFile(pub PathBuf);

impl TmpFile {
    pub fn write(dir: &Path, prefix: &str, body: &str) -> Result<TmpFile> {
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = dir.join("tmp");
        fs::create_dir_all(&tmp)?;
        let path = tmp.join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            config::random_hex(8)?
        ));
        {
            let mut files = FILES.lock().unwrap_or_else(|e| e.into_inner());
            files.push(path.clone());
            publish(&files);
        }
        let t = TmpFile(path);
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&t.0)
            .with_context(|| format!("could not write the {prefix} file"))?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        Ok(t)
    }
}

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let mut files = FILES.lock().unwrap_or_else(|e| e.into_inner());
        files.retain(|p| p != &self.0);
        publish(&files);
    }
}

fn alive(pid: i32) -> bool {
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// Removes what a dispatch that died left in `tmp/`: every file whose
/// process is gone (or that names none). Returns how many.
pub fn sweep(project: &str) -> usize {
    let Ok(dir) = state::dir(project) else {
        return 0;
    };
    let Ok(rd) = fs::read_dir(dir.join("tmp")) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let pid = name
            .split('-')
            .nth(1)
            .and_then(|p| p.parse::<i32>().ok())
            .unwrap_or(0);
        if !alive(pid) && fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

fn lock_path(project: &str, epic: &str) -> Result<PathBuf> {
    Ok(state::dir(project)?.join(format!("dispatching-{epic}.lock")))
}

/// Held by `dispatch` while it starts an epic's run; let go when dropped,
/// or when the process ends however it ends.
pub struct DispatchLock(#[allow(dead_code)] File);

impl DispatchLock {
    /// Takes it, or fails if another process holds it.
    pub fn take(project: &str, epic: &str) -> Result<DispatchLock> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path(project, epic)?)?;
        f.try_lock()
            .map_err(|_| anyhow::anyhow!("another dispatch of {epic} is at work"))?;
        Ok(DispatchLock(f))
    }
}

/// Is a `dispatch` process at work on the epic's run?
pub fn dispatching(project: &str, epic: &str) -> bool {
    let Ok(p) = lock_path(project, epic) else {
        return false;
    };
    match OpenOptions::new().write(true).open(p) {
        Ok(f) => f.try_lock().is_err(),
        Err(_) => false,
    }
}

/// For the tests: with `KONTEXT_DISPATCH_TEST_PAUSE=<step>`, `dispatch`
/// writes `paused-<step>` in its folder and waits there to be killed.
pub fn checkpoint(project: &str, step: &str) {
    if std::env::var("KONTEXT_DISPATCH_TEST_PAUSE").ok().as_deref() != Some(step) {
        return;
    }
    if let Ok(d) = state::dir(project) {
        let _ = fs::write(d.join(format!("paused-{step}")), "");
    }
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
