//! `install.sh` against a local "release" (an archive of the built binary and
//! its SHA256SUMS, through `KONTEXT_DOWNLOAD_BASE=file://…`), with a throwaway
//! HOME and a PATH made for each case: where it puts the binary, what it says
//! next, and that a join string installs and joins in one line.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::BIN;
use common::server::{ADMIN, LocalServer};
use serde_json::{Value, json};

fn target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        _ => "",
    }
}

/// `dist/` with `kontext-<target>.tar.gz` and `SHA256SUMS`.
fn release(dir: &Path) -> PathBuf {
    let dist = dir.join("dist");
    let stage = dir.join("stage");
    fs::create_dir_all(&dist).unwrap();
    fs::create_dir_all(&stage).unwrap();
    fs::copy(BIN, stage.join("kontext")).unwrap();
    let asset = format!("kontext-{}.tar.gz", target());
    let st = Command::new("tar")
        .args(["-czf", dist.join(&asset).to_str().unwrap(), "-C"])
        .arg(&stage)
        .arg("kontext")
        .status()
        .unwrap();
    assert!(st.success());
    let o = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "cd '{}' && (sha256sum {asset} 2>/dev/null || shasum -a 256 {asset})",
            dist.display()
        ))
        .output()
        .unwrap();
    fs::write(dist.join("SHA256SUMS"), &o.stdout).unwrap();
    dist
}

struct Box {
    home: PathBuf,
    dist: PathBuf,
}

impl Box {
    fn run(&self, path: &str, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
        let mut c = Command::new("/bin/sh");
        c.arg(&script)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .env(
                "KONTEXT_DOWNLOAD_BASE",
                format!("file://{}", self.dist.display()),
            )
            .env("KONTEXT_CONFIG_DIR", self.home.join(".config/kontext"))
            .env("GIT_CONFIG_GLOBAL", self.home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null());
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }
}

fn err(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn new_box(dir: &Path, name: &str, dist: &Path) -> Box {
    let home = dir.join(name);
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join(".gitconfig"),
        "[user]\n\tname = Omar\n\temail = omar@example.com\n[init]\n\tdefaultBranch = main\n",
    )
    .unwrap();
    Box {
        home: home.canonicalize().unwrap(),
        dist: dist.to_path_buf(),
    }
}

#[test]
fn install_sh_uses_a_folder_on_the_path_and_says_what_comes_next() {
    if target().is_empty() {
        eprintln!("skipped: no release target for this machine");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dist = release(dir.path());
    let sys = "/usr/bin:/bin:/usr/sbin:/sbin";

    // ~/.local/bin on the PATH but not there yet: created and used.
    let b = new_box(dir.path(), "a", &dist);
    let local = b.home.join(".local/bin");
    let o = b.run(&format!("{}:{sys}", local.display()), &b.home, &[], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(local.join("kontext").exists());
    let t = err(&o);
    assert!(
        t.contains("next: in your code repo, run `kontext setup`"),
        "{t}"
    );
    assert!(!t.contains("full path"), "{t}");

    // Another folder of the user's on the PATH, writable: that one.
    let b = new_box(dir.path(), "b", &dist);
    let mine = b.home.join("tools/bin");
    fs::create_dir_all(&mine).unwrap();
    let o = b.run(&format!("{}:{sys}", mine.display()), &b.home, &[], &[]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(mine.join("kontext").exists(), "{}", err(&o));
    assert!(!b.home.join(".local/bin/kontext").exists());

    // Nothing of the user's on the PATH: ~/.local/bin, and the next command
    // with its full path so it works as printed.
    let b = new_box(dir.path(), "c", &dist);
    let o = b.run(sys, &b.home, &[], &[]);
    assert!(o.status.success(), "{}", err(&o));
    let t = err(&o);
    assert!(t.contains("is not on your PATH"), "{t}");
    assert!(t.contains("no shell file was changed"), "{t}");
    let full = b.home.join(".local/bin/kontext");
    assert!(
        t.contains(&format!("run `{} setup`", full.display())),
        "{t}"
    );
    // No shell startup file is written.
    for f in [
        ".profile",
        ".zshrc",
        ".bashrc",
        ".zprofile",
        ".bash_profile",
    ] {
        assert!(!b.home.join(f).exists(), "{f} was written");
    }

    // An older kontext comes first on the PATH: the new one goes in the first
    // writable folder of the user's, and since `kontext` still finds the old
    // one, the next command uses the full path, and says why.
    let b = new_box(dir.path(), "f", &dist);
    let old = b.home.join("old/bin");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("kontext"), "#!/bin/sh\necho kontext 0.1.0\n").unwrap();
    Command::new("chmod")
        .args(["755", old.join("kontext").to_str().unwrap()])
        .status()
        .unwrap();
    // Not writable: the old one stays and comes first.
    Command::new("chmod")
        .args(["555", old.to_str().unwrap()])
        .status()
        .unwrap();
    let local = b.home.join(".local/bin");
    let o = b.run(
        &format!("{}:{}:{sys}", old.display(), local.display()),
        &b.home,
        &[],
        &[],
    );
    Command::new("chmod")
        .args(["755", old.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert!(local.join("kontext").exists(), "{}", err(&o));
    let t = err(&o);
    assert!(
        t.contains(&format!(
            "another kontext comes first on your PATH ({})",
            old.join("kontext").display()
        )),
        "{t}"
    );
    assert!(
        t.contains(&format!("run `{} setup`", local.join("kontext").display())),
        "{t}"
    );

    // PATH order: the first writable folder of the user's, so `kontext` is
    // the new one.
    let b = new_box(dir.path(), "g", &dist);
    let first = b.home.join("first/bin");
    fs::create_dir_all(&first).unwrap();
    let local = b.home.join(".local/bin");
    fs::create_dir_all(&local).unwrap();
    let o = b.run(
        &format!("{}:{}:{sys}", first.display(), local.display()),
        &b.home,
        &[],
        &[],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(first.join("kontext").exists(), "{}", err(&o));
    assert!(err(&o).contains("run `kontext setup`"), "{}", err(&o));

    // KONTEXT_INSTALL_DIR still wins.
    let b = new_box(dir.path(), "d", &dist);
    let there = b.home.join("elsewhere");
    let o = b.run(
        &format!("{}:{sys}", b.home.join(".local/bin").display()),
        &b.home,
        &[],
        &[("KONTEXT_INSTALL_DIR", there.to_str().unwrap())],
    );
    assert!(o.status.success(), "{}", err(&o));
    assert!(there.join("kontext").exists());
    assert!(!b.home.join(".local/bin/kontext").exists());

    // An argument that is not a join string: nothing is installed.
    let b = new_box(dir.path(), "e", &dist);
    let o = b.run(sys, &b.home, &["kxt1.not.this"], &[]);
    assert!(!o.status.success());
    assert!(err(&o).contains("join line"), "{}", err(&o));
    // What was passed is not repeated (it could be a token).
    assert!(!err(&o).contains("kxt1.not.this"), "{}", err(&o));
    assert!(!b.home.join(".local/bin/kontext").exists());
    // A join line cut short or with a stray character: refused before the
    // download.
    for bad in [
        "kxj1.eyJzIjoiaHR0cHM6",
        "kxj1.eyJzIjoiaHR0cHM6Ly9rLmV4YW1wbGUuY29tIiwicCI6ImIiLCJj'",
    ] {
        let o = b.run(sys, &b.home, &[bad], &[]);
        assert!(!o.status.success());
        let t = err(&o);
        assert!(
            t.contains("not a whole join line") || t.contains("too short"),
            "{t}"
        );
        assert!(!t.contains("downloading"), "{t}");
        assert!(!b.home.join(".local/bin/kontext").exists());
    }
}

#[test]
fn install_sh_with_a_join_string_installs_and_joins() {
    if target().is_empty() {
        return;
    }
    let Some(server) = LocalServer::start() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let dist = release(dir.path());

    // Machine A: a project on the local server, and an invitation.
    let a = new_box(dir.path(), "a", &dist);
    let config = a.home.join(".config/kontext");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("projects.toml"), "writer = \"omar@a\"\n").unwrap();
    fs::write(
        config.join("servers.json"),
        json!({ "servers": [{ "url": server.url, "admin_secret": ADMIN }] }).to_string(),
    )
    .unwrap();
    let repo = a.home.join("billing");
    fs::create_dir_all(&repo).unwrap();
    let kx = |cwd: &Path, args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("HOME", &a.home)
            .env("KONTEXT_CONFIG_DIR", &config)
            .env("GIT_CONFIG_GLOBAL", a.home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("KONTEXT_PROJECT")
            .env_remove("KONTEXT_TOKEN")
            .env_remove("KONTEXT_SERVER")
            .env_remove("KONTEXT_ADMIN_SECRET")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    let o = kx(&repo, &["setup"]);
    assert!(o.status.success(), "{}", err(&o));
    let o = kx(&repo, &["--json", "invite"]);
    assert!(o.status.success(), "{}", err(&o));
    let inv: Value = serde_json::from_slice(&o.stdout).unwrap();
    let join = inv["join"].as_str().unwrap();

    // Machine B: one line, in its code repo.
    let b = new_box(dir.path(), "b", &dist);
    let rb = b.home.join("code/billing");
    fs::create_dir_all(&rb).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&rb)
            .status()
            .unwrap()
            .success()
    );
    let path = format!("{}:/usr/bin:/bin", b.home.join(".local/bin").display());
    let o = b.run(&path, &rb, &[join], &[]);
    let t = err(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("joining"), "{t}");
    assert!(t.contains("project"), "{t}");
    assert!(b.home.join(".config/kontext/tokens/billing.token").exists());
    assert!(rb.join(".kontext.toml").exists());
    assert!(rb.join(".claude/settings.json").exists());
    // The hooks point at the installed binary.
    let settings = fs::read_to_string(rb.join(".claude/settings.json")).unwrap();
    assert!(
        settings.contains(&b.home.join(".local/bin/kontext").display().to_string()),
        "{settings}"
    );
}
