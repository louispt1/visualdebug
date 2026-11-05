//! `visualdebug compare <base-ref> -- <command>`: run the same example on the base ref (in a temporary
//! git worktree) and on the working tree, align the traces and report where behaviour diverges.

use crate::project::{Project, Sources, git};
use crate::run::{Plan, classify, derive_name, execute, focus_from_diff, focus_from_diff_reversed};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Debug)]
pub struct CompareArgs {
    /// The git ref to compare against (e.g. main)
    pub base_ref: String,
    /// Start recording at this target on both sides (see `run --start-at`)
    #[arg(long, value_name = "TARGET")]
    pub start_at: Option<String>,
    /// Trace name (default: derived from the command); the base trace is `<name>@base`
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long, default_value_t = 20_000)]
    pub max_events: u32,
    /// Keep the temporary worktree (for debugging)
    #[arg(long)]
    pub keep_worktree: bool,
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

/// Untracked things a worktree lacks but the command needs: shared (symlinked) or created empty.
const SHARED: &[&str] = &["node_modules", ".venv", "venv", "vendor/bundle", ".bundle"];
const EMPTY: &[&str] = &["storage", "tmp", "log"];

/// Source of `rel` at `rev`, relative to the project root.
pub fn git_source(project: &Project, rev: &str, rel: &str) -> Option<String> {
    let sub = project.git_subdir().unwrap_or_default();
    let path = if sub.is_empty() {
        rel.to_string()
    } else {
        format!("{sub}/{rel}")
    };
    git(&project.root, &["show", &format!("{rev}:{path}")]).ok()
}

pub fn compare(args: CompareArgs) -> Result<i32> {
    let project = Project::discover()?;
    project.ensure_dir()?;
    let sub = project
        .git_subdir()
        .context("compare needs a git repository")?;
    let base_rev = git(
        &project.root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", args.base_ref),
        ],
    )
    .with_context(|| format!("{:?} is not a commit here", args.base_ref))?
    .trim()
    .to_string();
    let (lang, is_test) = classify(&args.command);
    let start_at = args.start_at.clone().or_else(|| {
        (is_test && lang.is_some_and(|l| l != crate::run::Lang::Node)).then(|| "auto:test".into())
    });
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| derive_name(&args.command));
    let base_name = format!("{name}@base");

    // Worktree of the whole repo at the base ref; the project may be a subdirectory of it.
    let top = PathBuf::from(git(&project.root, &["rev-parse", "--show-toplevel"])?.trim());
    let wt = project.dir().join("worktrees").join(sanitize(&name));
    if wt.exists() {
        let _ = git(
            &top,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        );
        let _ = std::fs::remove_dir_all(&wt);
    }
    std::fs::create_dir_all(wt.parent().unwrap())?;
    git(
        &top,
        &[
            "worktree",
            "add",
            "--detach",
            &wt.to_string_lossy(),
            &base_rev,
        ],
    )?;
    let base_root = if sub.is_empty() {
        wt.clone()
    } else {
        wt.join(&sub)
    };
    let base_root = base_root.canonicalize().unwrap_or(base_root);
    prepare_worktree(&project.root, &base_root)?;

    let cwd = std::env::current_dir()?;
    let rel_cwd = cwd
        .strip_prefix(&project.root)
        .unwrap_or(Path::new(""))
        .to_path_buf();
    let mut head_focus = focus_from_diff(&project, &args.base_ref)?;
    let mut base_focus = focus_from_diff_reversed(&project, &args.base_ref)?;
    head_focus.files.retain(|f, _| crate::run::is_source(f));
    base_focus
        .files
        .retain(|f, _| crate::run::is_source(f) && base_root.join(f).is_file());

    eprintln!(
        "visualdebug: --- base ({}) in {}",
        args.base_ref,
        wt.display()
    );
    let result = execute(
        &project,
        Plan {
            name: base_name.clone(),
            code_root: base_root.clone(),
            cwd: base_root.join(&rel_cwd),
            command: args.command.clone(),
            lang,
            focus: base_focus,
            start_at: start_at.clone(),
            max_events: args.max_events,
        },
    );
    if !args.keep_worktree {
        let _ = git(
            &top,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        );
        let _ = std::fs::remove_dir_all(&wt);
    }
    let (base, base_code) = result?;

    eprintln!("visualdebug: --- HEAD (working tree)");
    let (mut head, head_code) = execute(
        &project,
        Plan {
            name: name.clone(),
            code_root: project.root.clone(),
            cwd,
            command: args.command.clone(),
            lang,
            focus: head_focus,
            start_at,
            max_events: args.max_events,
        },
    )?;
    if base.steps.is_empty() || head.steps.is_empty() {
        bail!(
            "one side recorded nothing; check --start-at exists on both {} and HEAD",
            args.base_ref
        );
    }

    let head_src = Sources::new(&project);
    let base_cache: std::cell::RefCell<std::collections::HashMap<String, Option<Vec<String>>>> =
        Default::default();
    let base_line = |f: &str, l: u32| -> Option<String> {
        let mut c = base_cache.borrow_mut();
        let lines = c.entry(f.to_string()).or_insert_with(|| {
            git_source(&project, &base_rev, f).map(|t| t.lines().map(str::to_string).collect())
        });
        lines.as_ref()?.get(l.checked_sub(1)? as usize).cloned()
    };
    let cmp =
        visualdebug_core::compare::compare(&base, &head, &args.base_ref, &base_line, &|f, l| {
            head_src.line(f, l)
        });
    match &cmp.divergence {
        Some(d) => eprintln!(
            "\nvisualdebug: first divergence ({}) at base #{} / HEAD #{}: {}",
            d.kind,
            d.base_step.map(|s| s.to_string()).unwrap_or("-".into()),
            d.head_step.map(|s| s.to_string()).unwrap_or("-".into()),
            d.description
        ),
        None => eprintln!(
            "\nvisualdebug: no divergence: both sides ran the same code with the same values"
        ),
    }
    if base_code != head_code {
        eprintln!("visualdebug: exit status differs: base {base_code}, HEAD {head_code}");
    }
    head.compare = Some(cmp);
    head.save(&project.traces_dir().join(format!("{name}.json")))?;
    eprintln!("visualdebug: next: `visualdebug view {name}` (side by side with {base_name})");
    Ok(head_code)
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn prepare_worktree(root: &Path, base_root: &Path) -> Result<()> {
    for d in SHARED {
        let (src, dst) = (root.join(d), base_root.join(d));
        if src.exists() && !dst.exists() {
            std::fs::create_dir_all(dst.parent().unwrap())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&src, &dst)?;
        }
    }
    for d in EMPTY {
        if root.join(d).is_dir() {
            std::fs::create_dir_all(base_root.join(d))?;
        }
    }
    // Temporary examples the skill wrote live in the (ignored) .visualdebug/examples.
    let ex = root.join(".visualdebug/examples");
    if ex.is_dir() {
        let dst = base_root.join(".visualdebug/examples");
        std::fs::create_dir_all(&dst)?;
        for e in std::fs::read_dir(&ex)?.flatten() {
            if e.path().is_file() {
                std::fs::copy(e.path(), dst.join(e.file_name()))?;
            }
        }
    }
    Ok(())
}
