//! Where each repo of the project is on this machine (DESIGN §11, "Several
//! repos").
//!
//! First it looks in `projects.toml` (`[projects.<p>.repos] <name> =
//! "<path>"`), then it compares the repo the command runs in with the ones in
//! `[[repos]]`: by the normalized remote and, if no remote matches, by folder
//! name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::{Machine, Project, Repo, expand_tilde};
use crate::git;
use crate::install::normalize_remote;

/// The git repo the command runs in.
#[derive(Debug, Clone)]
pub struct Here {
    pub root: PathBuf,
    /// The normalized, lowercased remote.
    pub remote: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Repos {
    pub list: Vec<Repo>,
    mapped: BTreeMap<String, PathBuf>,
    here: Option<Here>,
}

fn remote_key(url: &str) -> String {
    normalize_remote(url).to_lowercase()
}

impl Repos {
    /// The repos of `project`, using what `projects.toml` says on this machine
    /// and the repo that contains `cwd`.
    pub fn load(project: &Project, cwd: &Path) -> Result<Repos> {
        let machine = Machine::load()?;
        let mut mapped = BTreeMap::new();
        if let Some(p) = machine.projects.get(&project.name) {
            for (name, path) in &p.repos {
                mapped.insert(name.clone(), expand_tilde(path)?);
            }
        }
        Ok(Repos::new(project.repos.clone(), mapped, cwd))
    }

    pub fn new(list: Vec<Repo>, mapped: BTreeMap<String, PathBuf>, cwd: &Path) -> Repos {
        let here = git::root(cwd).map(|root| Here {
            remote: git::remote_url(&root).map(|r| remote_key(&r)),
            root,
        });
        Repos { list, mapped, here }
    }

    pub fn here(&self) -> Option<&Here> {
        self.here.as_ref()
    }

    pub fn get(&self, name: &str) -> Option<&Repo> {
        self.list.iter().find(|r| r.name == name)
    }

    /// The name of a local repo: the one in `[[repos]]` with the same remote,
    /// or with the same folder name, or the folder name itself. The second
    /// value says whether it is in `[[repos]]`.
    pub fn name_for(&self, root: &Path, remote: Option<&str>) -> (String, bool) {
        if let Some(rem) = remote
            && let Some(r) = self
                .list
                .iter()
                .find(|r| r.remote.as_deref().map(remote_key).as_deref() == Some(rem))
        {
            return (r.name.clone(), true);
        }
        for (name, path) in &self.mapped {
            if same_dir(path, root) {
                return (name.clone(), self.get(name).is_some());
            }
        }
        let dir = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".into());
        let known = self.get(&dir).is_some();
        (dir, known)
    }

    /// The name of the repo the command runs in.
    pub fn here_name(&self) -> Option<(String, bool)> {
        let h = self.here.as_ref()?;
        Some(self.name_for(&h.root, h.remote.as_deref()))
    }

    /// The root of the local checkout of `name`, if there is one.
    pub fn locate(&self, name: &str) -> Option<PathBuf> {
        if let Some(p) = self.mapped.get(name)
            && p.is_dir()
        {
            return Some(git::root(p).unwrap_or_else(|| p.clone()));
        }
        let h = self.here.as_ref()?;
        let (here, _) = self.name_for(&h.root, h.remote.as_deref());
        (here == name).then(|| h.root.clone())
    }

    /// The `[[repos]]` that are on this machine, with their root.
    pub fn available(&self) -> Vec<(Repo, PathBuf)> {
        self.list
            .iter()
            .filter_map(|r| self.locate(&r.name).map(|p| (r.clone(), p)))
            .collect()
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}
