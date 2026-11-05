//! Project root, the `.visualdebug/` directory, and resolving trace names to files.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use visualdebug_core::Trace;

pub const DIR: &str = ".visualdebug";

const MARKERS: &[&str] = &[
    "Gemfile",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "Pipfile",
    ".git",
];

/// Nearest ancestor of `cwd` with a project marker; `cwd` itself if none.
pub fn find_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|d| MARKERS.iter().any(|m| d.join(m).exists()))
        .unwrap_or(cwd)
        .to_path_buf()
}

pub struct Project {
    pub root: PathBuf,
}

impl Project {
    pub fn discover() -> Result<Project> {
        let cwd = std::env::current_dir()?;
        let root = find_root(&cwd);
        // Canonical, because some runtimes report realpaths (Node; macOS's /var -> /private/var).
        Ok(Project {
            root: root.canonicalize().unwrap_or(root),
        })
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join(DIR)
    }
    pub fn traces_dir(&self) -> PathBuf {
        self.dir().join("traces")
    }
    pub fn runs_dir(&self) -> PathBuf {
        self.dir().join("runs")
    }

    /// Create `.visualdebug/`; on first creation add it to an existing `.gitignore`.
    pub fn ensure_dir(&self) -> Result<()> {
        let dir = self.dir();
        if dir.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&dir)?;
        let gi = self.root.join(".gitignore");
        if let Ok(text) = std::fs::read_to_string(&gi)
            && !text.lines().any(|l| {
                matches!(
                    l.trim(),
                    ".visualdebug" | ".visualdebug/" | "/.visualdebug" | "/.visualdebug/"
                )
            })
        {
            let sep = if text.ends_with('\n') || text.is_empty() {
                ""
            } else {
                "\n"
            };
            std::fs::write(&gi, format!("{text}{sep}.visualdebug/\n"))?;
            eprintln!("visualdebug: added .visualdebug/ to {}", gi.display());
        }
        Ok(())
    }

    /// A trace argument: a path to a trace file, or a name under `.visualdebug/traces/`.
    pub fn resolve_trace(&self, arg: &str) -> Result<PathBuf> {
        let p = PathBuf::from(arg);
        if p.is_file() {
            return Ok(p);
        }
        let named = self
            .traces_dir()
            .join(format!("{}.json", arg.trim_end_matches(".json")));
        if named.is_file() {
            return Ok(named);
        }
        let known = self.trace_names();
        if known.is_empty() {
            bail!("no trace {arg:?}: nothing recorded yet (run `visualdebug run -- <command>`)");
        }
        bail!("no trace {arg:?}; known traces: {}", known.join(", "))
    }

    pub fn trace_names(&self) -> Vec<String> {
        let mut v: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(self.traces_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.strip_suffix(".json")?.to_string();
                Some((e.metadata().ok()?.modified().ok()?, name))
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, n)| n).collect()
    }

    pub fn latest_trace(&self) -> Result<PathBuf> {
        let name = self
            .trace_names()
            .pop()
            .context("nothing recorded yet (run `visualdebug run -- <command>`)")?;
        Ok(self.traces_dir().join(format!("{name}.json")))
    }

    pub fn load_trace(&self, arg: Option<&str>) -> Result<(PathBuf, Trace)> {
        let path = match arg {
            Some(a) => self.resolve_trace(a)?,
            None => self.latest_trace()?,
        };
        let t = Trace::load(&path)?;
        Ok((path, t))
    }

    /// Source text of a project-relative file.
    pub fn source(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(rel)).ok()
    }

    /// The project root relative to the git root ("" when equal), if in a git repo.
    pub fn git_subdir(&self) -> Option<String> {
        let out = git(&self.root, &["rev-parse", "--show-prefix"]).ok()?;
        Some(out.trim().trim_end_matches('/').to_string())
    }
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .context("running git (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Line-indexed source cache for summaries.
pub struct Sources<'a> {
    project: &'a Project,
    cache: std::cell::RefCell<std::collections::HashMap<String, Option<Vec<String>>>>,
}

impl<'a> Sources<'a> {
    pub fn new(project: &'a Project) -> Self {
        Sources {
            project,
            cache: Default::default(),
        }
    }
    pub fn line(&self, file: &str, line: u32) -> Option<String> {
        let mut c = self.cache.borrow_mut();
        let lines = c.entry(file.to_string()).or_insert_with(|| {
            self.project
                .source(file)
                .map(|s| s.lines().map(str::to_string).collect())
        });
        lines.as_ref()?.get(line.checked_sub(1)? as usize).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_nearest_marker() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("app/sub")).unwrap();
        std::fs::write(d.path().join("app/Gemfile"), "").unwrap();
        assert_eq!(find_root(&d.path().join("app/sub")), d.path().join("app"));
    }
}
