//! `kontext setup` (DESIGN §14.3): one command, run in a code repo, that
//! brings it from nothing to installed and syncing, built on the commands that
//! do each part: the project (what `init` does), the hooks, skills and agent
//! files (`install`), the server (`server deploy`, or the one this machine
//! already deployed) and the project on it (`server connect`). Run again, it
//! picks up where it stopped and changes nothing that is already right.
//!
//! A step only the human can do (the Cloudflare API token) stops it with exit
//! code 4 and says what the human does ([`crate::human::NeedsHuman`]); it never
//! waits on a prompt.
//!
//! The network never blocks it: a server that does not answer within a few
//! seconds leaves the local steps done, the server steps `needed`, `offline`
//! set, and exit code 0 with the command to run again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::cli::{InstallHow, install_plan};
use crate::config::{self, Machine};
use crate::human::NeedsHuman;
use crate::remote::{self, Conn, Link, Options};
use crate::server::{self, Servers};
use crate::store::Refused;

/// What `kontext setup` was asked.
#[derive(Debug, Clone, Default)]
pub struct Args {
    /// `--project` (global).
    pub project: Option<String>,
    /// Where a new kontext-home goes.
    pub home: Option<PathBuf>,
    /// The server to use, when this machine knows several (or none).
    pub server: Option<String>,
    /// No server: the project and the install only.
    pub no_server: bool,
    /// The `workers.dev` subdomain, when it deploys to an account with none.
    pub subdomain: Option<String>,
    /// Create the project the repo's `.kontext.toml` names although this
    /// machine does not have it (it exists nowhere yet).
    pub new_project: bool,
    /// `install`'s flags.
    pub role: String,
    pub force: bool,
    pub local: bool,
    pub worktree_role: Option<String>,
}

/// One step and how it went.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Step {
    pub step: &'static str,
    /// `done` (this run did it), `already` (it was right), `needed` (the
    /// human has to do something) or `skipped`.
    pub status: &'static str,
    pub detail: String,
}

/// What `kontext setup` did.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    /// `setup` or `join`.
    pub command: &'static str,
    pub repo: String,
    pub project: String,
    pub home: Option<String>,
    pub writer: Option<String>,
    pub server: Option<String>,
    pub steps: Vec<Step>,
    /// Everything is in place.
    pub ok: bool,
    /// The server did not answer: the local steps are done, the server steps
    /// are `needed`, and the exit code is 0 (not an error of the setup).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub offline: bool,
    pub needs_human: Option<NeedsHuman>,
    /// Does the server take invitations (`kontext invite`)? `false` for a
    /// server deployed before them, which needs `kontext server deploy`
    /// first; absent when the server was not asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invites: Option<bool>,
    /// Notes (files kept because they were changed by hand, warnings).
    pub notes: Vec<String>,
    /// What to do next.
    pub next: String,
}

impl Outcome {
    pub(crate) fn step(
        &mut self,
        step: &'static str,
        status: &'static str,
        detail: impl Into<String>,
    ) {
        self.steps.push(Step {
            step,
            status,
            detail: detail.into(),
        });
    }

    pub fn text(&self) -> String {
        let mut l = vec![format!(
            "kontext {} in {} (project {}):",
            self.command, self.repo, self.project
        )];
        for s in &self.steps {
            l.push(format!("  {:<15} {:<8} {}", s.step, s.status, s.detail));
        }
        for n in &self.notes {
            l.push(format!("Note: {n}"));
        }
        if let Some(n) = &self.needs_human {
            l.push(n.text().trim_end().to_string());
            l.push(format!(
                "(For an agent: tell the human this, word for word: {})",
                n.say
            ));
        } else {
            l.push(self.next.clone());
        }
        l.join("\n") + "\n"
    }
}

/// A project name from a folder's name: letters, digits, `-`, `_` and `.`.
pub fn name_from_dir(dir: &Path) -> Option<String> {
    let raw = dir.file_name()?.to_string_lossy().to_string();
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_start_matches(['.', '-']).trim_end_matches('-');
    let s: String = s.chars().take(64).collect();
    crate::cli::valid_name(&s).then_some(s)
}

/// `kontext setup`.
pub fn run(a: &Args) -> Result<Outcome> {
    let cwd = std::env::current_dir()?;
    let Some((repo, linked)) = crate::git::checkout(&cwd) else {
        return Err(Refused(format!(
            "{} is not in a git repo: run `kontext setup` in your code repo (or `git init` first)",
            cwd.display()
        ))
        .into());
    };
    let repo = repo.canonicalize().unwrap_or(repo);
    if linked {
        let main = crate::git::main_checkout(&repo)
            .map(|m| format!(" ({})", m.display()))
            .unwrap_or_default();
        return Err(Refused(format!(
            "{} is a linked git worktree: run `kontext setup` in the main checkout{main}; a worktree \
             finds the project from there",
            repo.display()
        ))
        .into());
    }

    // The project's name: --project, $KONTEXT_PROJECT, the repo's
    // .kontext.toml, or the repo's folder name.
    let machine = Machine::load()?;
    let given = a.project.clone().or_else(|| {
        std::env::var("KONTEXT_PROJECT")
            .ok()
            .filter(|s| !s.is_empty())
    });
    let link = config::find_repo_link(&repo)?;
    let name = match (&given, &link) {
        (Some(n), _) => n.clone(),
        (None, Some(f)) => f.link.project.clone(),
        (None, None) => name_from_dir(&repo).with_context(|| {
            format!(
                "no project name can be made from {}: pass `kontext setup --project <name>`",
                repo.display()
            )
        })?,
    };
    if !crate::cli::valid_name(&name) {
        return Err(Refused(format!(
            "invalid project name {name:?}: pass `kontext setup --project <name>` with letters, digits, - _ and ."
        ))
        .into());
    }
    // The repo already says which project it belongs to: another name would
    // be a second project that the repo's hooks never use.
    if let (Some(g), Some(f)) = (&given, &link)
        && *g != f.link.project
    {
        return Err(Refused(format!(
            "{} names project {}, not {g}: this repo belongs to {}. Run `kontext setup` without --project \
             (and without KONTEXT_PROJECT) to set {} up here; to put this repo in another project, \
             change `project` in that file first",
            f.path.display(),
            f.link.project,
            f.link.project,
            f.link.project
        ))
        .into());
    }
    let mut out = Outcome {
        command: "setup",
        repo: repo.display().to_string(),
        project: name.clone(),
        home: None,
        writer: None,
        server: None,
        steps: Vec::new(),
        ok: false,
        offline: false,
        needs_human: None,
        invites: None,
        notes: Vec::new(),
        next: String::new(),
    };

    // The server, as far as it can be known without the human: the
    // project's, --server, or the one this machine deployed.
    let registered = machine.projects.contains_key(&name);

    // A clone of a repo whose project was set up on another machine: nothing
    // is made here (it would be a second project with that name and none of
    // its history); it comes with an invitation.
    if !registered
        && !a.new_project
        && let Some(f) = link.as_ref().filter(|f| f.link.project == name)
    {
        let file = f.path.display().to_string();
        out.step(
            "project",
            "needed",
            format!(
                "{file} names project {name}, which is not on this machine: it joins with `kontext join \
                 <line>` from `kontext invite` on a machine that has it"
            ),
        );
        let n =
            NeedsHuman::join_project(&name, &file, &format!("{} --new-project", setup_again(a)));
        out.next = format!(
            "Waiting on the human: once `kontext invite` on a machine that has project {name} prints a \
             `kontext join kxj1.…` line, run it here."
        );
        out.needs_human = Some(n);
        return Ok(out);
    }
    let own_server = machine.server_for(&name);
    let server = if a.no_server {
        None
    } else {
        pick_server(&name, own_server.as_deref(), a.server.as_deref())?
    };
    // Does it answer? If not, the local steps still go ahead.
    let mut silent = server.as_deref().and_then(no_answer);

    // 1. The project.
    if registered {
        let home = machine.project_home(&name)?;
        if home.canonicalize().ok().as_deref() == Some(repo.as_path()) {
            return Err(Refused(format!(
                "{} is project {name}'s kontext-home: run `kontext setup` in the code repo",
                repo.display()
            ))
            .into());
        }
        crate::cli::finish_home(&home);
        out.step(
            "project",
            "already",
            format!("{name} in {}", home.display()),
        );
    } else {
        // A project that is already on the server is not made again here: it
        // comes with an invitation. A server that fails to say is offline:
        // the local steps go ahead.
        if let Some(url) = &server
            && silent.is_none()
            && let Some(admin) = server::admin_secret(url)?
        {
            match exists_on(url, &admin, &name) {
                Ok(true) => {
                    return Err(Refused(format!(
                        "project {name} already exists on {url}, and this machine does not have it: on a \
                         machine that has it, run `kontext invite --role owner` and then the `kontext join …` \
                         line it prints here; or name this one differently with `kontext setup --project \
                         <other>`"
                    ))
                    .into());
                }
                Ok(false) => {}
                Err(e) if remote::offline(&e) => silent = Some(format!("{e:#}")),
                Err(e) => return Err(e),
            }
        }
        let writer = machine.writer()?;
        // A home an earlier run was killed in the middle of making: made again.
        let path = crate::cli::home_path(&name, a.home.clone())?;
        if crate::cli::half_made_home(&path) {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("could not remove the half-made {}", path.display()))?;
            out.notes.push(format!(
                "{} was left half made by a `kontext setup` that stopped before it finished: made again",
                path.display()
            ));
        }
        let home = crate::cli::new_home_path(&name, a.home.clone())?;
        let home = crate::cli::make_home(&name, &home, None, &writer)?;
        out.step(
            "project",
            "done",
            format!("created {name} in {}", home.display()),
        );
    }
    crate::cli::kill_point("setup:project");
    let resolved = config::resolve_project(Some(&name))?;
    out.home = Some(resolved.home.display().to_string());
    out.writer = Some(resolved.writer.clone());

    // 2. The hooks, skills and agent files.
    install_step(&mut out, &name, a)?;
    crate::cli::kill_point("setup:install");

    // 3. The server.
    if a.no_server {
        out.step(
            "server",
            "skipped",
            "--no-server: the project syncs with no server (`kontext setup` again without it adds one)",
        );
        out.ok = true;
        out.next = format!(
            "Done: project {name} is installed in this repo, without a server. Agents read and write it with \
             `kontext …` from here."
        );
        return Ok(out);
    }
    let url = match server {
        Some(u) => {
            let deployed = Servers::load()?.by_url(&u).is_some();
            out.step(
                "server",
                "already",
                if deployed {
                    format!("{u} (deployed from this machine)")
                } else {
                    u.clone()
                },
            );
            u
        }
        None => {
            if std::env::var("CLOUDFLARE_API_TOKEN").is_ok_and(|t| !t.trim().is_empty()) {
                let d = match server::deploy(&server::DeployArgs {
                    name: "kontext".into(),
                    account_id: None,
                    subdomain: a.subdomain.clone(),
                    no_check: false,
                    replace: false,
                    replace_admin_secret: false,
                    command: Some(setup_again(a)),
                }) {
                    Ok(d) => d,
                    Err(e) => {
                        let n = e.downcast::<NeedsHuman>()?;
                        out.step("server", "needed", n.why.clone());
                        out.next = format!("Waiting on the human: {}", n.say);
                        out.needs_human = Some(n);
                        return Ok(out);
                    }
                };
                out.step(
                    "server",
                    "done",
                    format!("deployed {} to Cloudflare account {}", d.url, d.account_id),
                );
                d.url
            } else {
                let n = NeedsHuman::cloudflare_token(Some(&setup_again(a)));
                out.step(
                    "server",
                    "needed",
                    "this machine has deployed no kontext server yet; it needs a Cloudflare API token from the human",
                );
                out.next = format!(
                    "Waiting on the human: once `{}` is done, run `{}` again.",
                    n.run,
                    setup_again(a)
                );
                out.needs_human = Some(n);
                return Ok(out);
            }
        }
    };
    out.server = Some(url.clone());

    // 4. The project on the server, with its history; then a sync.
    if let Some(why) = silent {
        return Ok(offline(out, &url, &why, a));
    }
    let report = match server_steps(&mut out, &name, &url, own_server.as_deref(), &resolved) {
        Err(e) if remote::offline(&e) => {
            return Ok(offline(out, &url, &format!("{e:#}"), a));
        }
        r => r?,
    };
    let mut sent: Vec<String> = Vec::new();
    if report.pushed_total() > 0 {
        sent.push(format!(
            "{} up",
            crate::views::plural(report.pushed_total() as usize, "entry", "entries")
        ));
    }
    if report.received_total() > 0 {
        sent.push(format!(
            "{} down",
            crate::views::plural(report.received_total() as usize, "entry", "entries")
        ));
    }
    if !report.config_out.is_empty() {
        sent.push(format!("config sent: {}", report.config_out.join(", ")));
    }
    if !report.config_in.is_empty() {
        sent.push(format!("config received: {}", report.config_in.join(", ")));
    }
    let complete = report.ok() && !report.partial;
    out.step(
        "sync",
        if complete { "done" } else { "needed" },
        if !complete {
            format!(
                "not everything went through: {}",
                report.text().trim().replace('\n', " ")
            )
        } else if sent.is_empty() {
            "nothing new to send or bring".into()
        } else {
            sent.join(", ")
        },
    );
    out.notes.extend(report.warnings.iter().cloned());
    out.ok = complete;
    // A server deployed before invitations syncs, but another machine cannot
    // join it until it is updated.
    out.invites = crate::invite::server_has_invites(&url, Duration::from_secs(3));
    let old_server = out.invites == Some(false);
    if old_server {
        out.notes.push(format!(
            "the kontext server at {url} was deployed before invitations: `kontext invite` (how another \
             machine joins) works once it is updated with `kontext server deploy`, run in a terminal on the \
             machine that deployed it (it keeps the projects and the admin secret)"
        ));
    }
    out.next = if out.ok && old_server {
        format!(
            "Done: project {name} is installed in this repo and syncs with {url}. Before another machine \
             can join, update the server: `kontext server deploy` in a terminal on the machine that \
             deployed it; then `kontext invite` here prints the line to run there."
        )
    } else if out.ok {
        format!(
            "Done: project {name} is installed in this repo and syncs with {url}. Another machine joins \
             with `kontext invite` here (it prints the line to run there)."
        )
    } else {
        "Almost: run `kontext sync` (or `kontext setup` again) to send the rest.".to_string()
    };
    Ok(out)
}

/// The project on the server (made there with its history, or already
/// there) and the sync that follows.
fn server_steps(
    out: &mut Outcome,
    name: &str,
    url: &str,
    own_server: Option<&str>,
    resolved: &config::Resolved,
) -> Result<remote::Report> {
    let token = config::token_for_server(name, url, own_server)?.filter(|t| t.project == name);
    Ok(if own_server == Some(url) && token.is_some() {
        let remote = Link::of(name)?
            .remote()
            .context("the project's token went missing")?;
        out.step(
            "server project",
            "already",
            format!("{name} syncs with {url} (writer {})", remote.token.writer),
        );
        remote::sync(&resolved.home, &remote, &Options::full(true))?
    } else {
        let Some(admin) = server::admin_secret(url)? else {
            return Err(Refused(format!(
                "no admin secret for {url} on this machine, so project {name} cannot be created there: run \
                 `kontext setup` on the machine that deployed it, or set KONTEXT_ADMIN_SECRET; to join a \
                 project that is already there, run `kontext invite` on a machine that has it"
            ))
            .into());
        };
        if token.is_none() {
            let project = config::Project::load(&resolved.home)?;
            let person = config::human_name_in(&project, &resolved.home);
            let made = remote::create_project(url, &admin, name, &person, &resolved.writer)?;
            crate::cli::kill_point("setup:created");
            let t = match made {
                Some(t) => t,
                // Already there: an earlier run's create whose answer never
                // came back is taken back with the admin secret; anything
                // else is another project with the same name.
                None => match remote::take_back_lost_create(
                    url,
                    &admin,
                    name,
                    &person,
                    &resolved.writer,
                )? {
                    Some(t) => {
                        out.notes.push(format!(
                            "{name} was already on {url} with nothing in it but a token whose answer never \
                             reached this machine: that token was revoked and a new one made here"
                        ));
                        t
                    }
                    None => {
                        return Err(Refused(format!(
                            "another project named {name} is already on {url}: name this one differently \
                             with `kontext setup --project <other>`, or, if it is this project, join it \
                             with `kontext invite` on a machine that has it"
                        ))
                        .into());
                    }
                },
            };
            t.save(name, url)?;
            crate::cli::kill_point("setup:token");
        }
        let r = remote::connect(name, &resolved.home, &resolved.writer, url)?;
        out.step(
            "server project",
            "done",
            format!(
                "{name} is on {url} with its history (writer {})",
                resolved.writer
            ),
        );
        r
    })
}

/// The server did not answer: what is local is done, the rest waits.
fn offline(mut out: Outcome, url: &str, why: &str, a: &Args) -> Outcome {
    let again = setup_again(a);
    if !out.steps.iter().any(|s| s.step == "server project") {
        out.step(
            "server project",
            "needed",
            format!("{url} did not answer ({why})"),
        );
        out.step("sync", "needed", "waits for the server");
    } else {
        out.step("sync", "needed", format!("{url} did not answer ({why})"));
    }
    out.offline = true;
    out.next = format!(
        "The server did not answer; the project and the install are done here. Run `{again}` again \
         when {url} answers (check the network, or the URL)."
    );
    out
}

/// `None` if the server answers `/v1/info` within a few seconds; else why not
/// (no answer, or a 5xx: the server failing).
fn no_answer(url: &str) -> Option<String> {
    let http = crate::http::Http::new(Duration::from_secs(5));
    match http.request("GET", &format!("{url}/v1/info"), &[], None) {
        Ok(r) if r.status >= 500 => Some(format!("GET {url}/v1/info answered {}", r.status)),
        Ok(_) => None,
        Err(e) => Some(format!("{e:#}")),
    }
}

/// The command to run again, with the flags that change what it does, as
/// it can be run here (this binary's path if `kontext` on the PATH is not it).
fn setup_again(a: &Args) -> String {
    let mut c = format!("{} setup", crate::human::self_command());
    if let Some(p) = &a.project {
        c.push_str(&format!(" --project {p}"));
    }
    if let Some(h) = &a.home {
        c.push_str(&format!(
            " --home {}",
            crate::install::shell_quote(&h.display().to_string())
        ));
    }
    if let Some(u) = &a.server {
        c.push_str(&format!(" --server {}", crate::install::shell_quote(u)));
    }
    c
}

/// The server to use: the project's own; `--server`; or the one this machine
/// deployed, if there is exactly one. `None` if there is none yet.
fn pick_server(name: &str, own: Option<&str>, flag: Option<&str>) -> Result<Option<String>> {
    let flag = flag.map(server::normalize_url);
    if let Some(own) = own {
        if let Some(f) = flag.as_deref().filter(|f| *f != own) {
            return Err(Refused(format!(
                "project {name} already syncs with {own}, not {f}; to move it, run `kontext server connect \
                 {f}` with that server's admin secret"
            ))
            .into());
        }
        return Ok(Some(own.to_string()));
    }
    if flag.is_some() {
        return Ok(flag);
    }
    let servers = Servers::load()?;
    match servers.servers.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(server::normalize_url(&one.url))),
        many => Err(Refused(format!(
            "this machine knows several kontext servers ({}): pick one with `kontext setup --server <url>`",
            many.iter()
                .map(|s| s.url.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .into()),
    }
}

/// Is `project` on the server? (The admin secret can ask any project.) A
/// server that fails to say gives an error [`remote::offline`] counts.
fn exists_on(url: &str, admin: &str, project: &str) -> Result<bool> {
    let conn = Conn::new(url, project, admin, Duration::from_secs(5));
    let r = match conn.get("whoami") {
        Err(e) if e.downcast_ref::<remote::TokenRefused>().is_some() => bail!(
            "{url} did not take the admin secret: is it the one `kontext server deploy` saved for it \
             (or $KONTEXT_ADMIN_SECRET)?"
        ),
        r => r?,
    };
    match r.status {
        200 => Ok(true),
        404 => Ok(false),
        _ => {
            Err(remote::ServerAnswered::of(&r, &format!("ask {url} for project {project}")).into())
        }
    }
}

/// `install` for this repo, as a step.
pub(crate) fn install_step(out: &mut Outcome, name: &str, a: &Args) -> Result<()> {
    let how = InstallHow {
        dry_run: false,
        force: a.force,
        link_only: false,
        local: a.local,
        worktree_role: a.worktree_role.as_deref(),
    };
    let (plan, repo, _) = install_plan(Some(name), &how, &a.role)?;
    for k in &plan.kept {
        out.notes.push(format!(
            "kept {}: it was changed by hand (it matches no kontext version); `kontext setup --force` replaces it",
            k.display()
        ));
    }
    out.notes.extend(plan.notes.iter().cloned());
    if let Some(n) = crate::cli::build_dir_note() {
        out.notes
            .push(n.strip_prefix("Note: ").unwrap_or(&n).to_string());
    }
    if plan.is_noop() {
        out.step(
            "install",
            "already",
            format!(
                "hooks, skills and agent files are up to date in {}",
                repo.display()
            ),
        );
        return Ok(());
    }
    let written = plan.apply()?;
    out.step(
        "install",
        "done",
        format!(
            "wrote {} file{} in {} (.kontext.toml, hooks, skills, agent files)",
            written.len(),
            if written.len() == 1 { "" } else { "s" },
            repo.display()
        ),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_folders() {
        assert_eq!(
            name_from_dir(Path::new("/x/billing")).as_deref(),
            Some("billing")
        );
        assert_eq!(
            name_from_dir(Path::new("/x/My App")).as_deref(),
            Some("My-App")
        );
        assert_eq!(
            name_from_dir(Path::new("/x/.hidden")).as_deref(),
            Some("hidden")
        );
        assert_eq!(name_from_dir(Path::new("/x/...")), None);
    }
}
