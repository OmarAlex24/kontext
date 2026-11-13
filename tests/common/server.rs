//! A local kontext server for the tests: `node server/dev.mjs`, which runs
//! `server/worker.js` in workerd through Miniflare (a dev dependency under
//! `server/`, installed with `npm ci --prefix server`).
//!
//! Without Node or without `server/node_modules`, the tests that need it are
//! skipped with a message; with `KONTEXT_REQUIRE_SERVER_TESTS=1` (CI) they fail
//! instead, so CI always runs them.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const ADMIN: &str = "test-admin-secret";

pub struct LocalServer {
    child: Child,
    pub url: String,
    pub admin: String,
    persist: Option<PathBuf>,
}

fn server_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("server")
}

fn required() -> bool {
    std::env::var("KONTEXT_REQUIRE_SERVER_TESTS").is_ok_and(|v| v == "1")
}

/// Why the local server cannot run here, if it cannot.
fn missing() -> Option<String> {
    if !server_dir().join("node_modules/miniflare").is_dir() {
        return Some("server/node_modules is missing (run `npm ci --prefix server`)".into());
    }
    match Command::new("node").arg("--version").output() {
        Ok(o) if o.status.success() => None,
        _ => Some("node is not installed".into()),
    }
}

impl LocalServer {
    /// Starts one; `None` means skip this test (and it says why).
    pub fn start() -> Option<LocalServer> {
        LocalServer::start_with(None)
    }

    /// With its Durable Objects kept in `persist`, so a restart keeps the data.
    pub fn start_with(persist: Option<&Path>) -> Option<LocalServer> {
        if let Some(why) = missing() {
            if required() {
                panic!("KONTEXT_REQUIRE_SERVER_TESTS=1 but the local server cannot run: {why}");
            }
            eprintln!("skipped: needs the local kontext server, and {why}");
            return None;
        }
        Some(LocalServer::spawn(persist, 0))
    }

    fn spawn(persist: Option<&Path>, port: u16) -> LocalServer {
        let mut c = Command::new("node");
        c.arg(server_dir().join("dev.mjs"))
            .args(["--port", &port.to_string(), "--admin-secret", ADMIN])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // Its own process group: workerd is node's child, and stopping the
            // server has to stop it too (otherwise it keeps the port).
            .process_group(0);
        if let Some(p) = persist {
            c.arg("--persist").arg(p);
        }
        let mut child = c.spawn().expect("could not start node server/dev.mjs");
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line)
            .expect("no URL from server/dev.mjs");
        let url = line.trim().to_string();
        assert!(
            url.starts_with("http://127.0.0.1:"),
            "server/dev.mjs printed {url:?}"
        );
        // Keep reading its stdout so it never blocks on a full pipe.
        std::thread::spawn(move || {
            let mut sink = String::new();
            while out.read_line(&mut sink).unwrap_or(0) > 0 {
                sink.clear();
            }
        });
        LocalServer {
            child,
            url,
            admin: ADMIN.into(),
            persist: persist.map(Path::to_path_buf),
        }
    }

    /// The port it listens on.
    pub fn port(&self) -> u16 {
        self.url.rsplit(':').next().unwrap().parse().unwrap()
    }

    /// Stops it, workerd included (the network goes away for the clients).
    pub fn stop(&mut self) {
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let group = -(self.child.id() as i32);
        // SIGTERM lets dev.mjs dispose of Miniflare (and workerd); the whole
        // group gets SIGKILL if that takes too long.
        unsafe { libc::kill(group, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe { libc::kill(group, libc::SIGKILL) };
        let _ = self.child.wait();
    }

    /// Starts it again on the same port, with the same data (needs `persist`).
    pub fn restart(&mut self) {
        let port = self.port();
        self.stop();
        let persist = self.persist.clone();
        let mut last = None;
        for _ in 0..50 {
            // The port may take a moment to be free again.
            match std::panic::catch_unwind(|| LocalServer::spawn(persist.as_deref(), port)) {
                Ok(s) => {
                    *self = s;
                    return;
                }
                Err(e) => last = Some(e),
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        std::panic::resume_unwind(last.unwrap());
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.stop();
    }
}
