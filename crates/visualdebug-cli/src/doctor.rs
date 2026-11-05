//! `visualdebug doctor`: what's here, what's supported, what would block a trace.

use crate::project::{Project, git};
use crate::run::Lang;
use anyhow::Result;
use std::path::Path;
use std::process::Command;

struct Report {
    problems: Vec<String>,
    notes: Vec<String>,
}

fn out(cmd: &str, args: &[&str], dir: &Path) -> Option<String> {
    let o = Command::new(cmd).args(args).current_dir(dir).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn version_tuple(v: &str) -> (u32, u32) {
    let mut it = v.trim_start_matches('v').split(['.', ' ', '-']).filter_map(|x| x.parse().ok());
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// The interpreter a project most likely uses.
pub fn python_for(root: &Path) -> String {
    for venv in [".venv", "venv", "env"] {
        let p = root.join(venv).join("bin/python");
        if p.is_file() {
            return p.display().to_string();
        }
    }
    "python3".into()
}

pub fn languages(root: &Path) -> Vec<Lang> {
    let has = |f: &str| root.join(f).exists();
    let mut v = Vec::new();
    if has("Gemfile") || has(".ruby-version") {
        v.push(Lang::Ruby);
    }
    if ["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "Pipfile"].iter().any(|f| has(f)) {
        v.push(Lang::Python);
    }
    if has("package.json") {
        v.push(Lang::Node);
    }
    v
}

pub fn doctor() -> Result<i32> {
    let project = Project::discover()?;
    let root = &project.root;
    let mut r = Report { problems: vec![], notes: vec![] };
    println!("visualdebug {}", env!("CARGO_PKG_VERSION"));
    println!("project root  {}", root.display());

    // git
    match out("git", &["--version"], root) {
        None => r.problems.push("git not found: --focus-diff needs it".into()),
        Some(v) => {
            let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).ok();
            match branch {
                Some(b) => {
                    let base = git(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
                        .ok()
                        .map(|s| s.trim().to_string())
                        .or_else(|| ["main", "master"].iter().find(|b| git(root, &["rev-parse", "--verify", "-q", b]).is_ok()).map(|s| s.to_string()));
                    println!("git           {} · branch {} · likely base {}", v.trim_start_matches("git version "), b.trim(), base.as_deref().unwrap_or("?"));
                }
                None => r.notes.push("not a git repository: use --focus <file> instead of --focus-diff".into()),
            }
        }
    }

    let langs = languages(root);
    if langs.is_empty() {
        r.notes.push("no Gemfile, pyproject/requirements or package.json here: run from your project root".into());
    }
    for lang in &langs {
        match lang {
            Lang::Ruby => check_ruby(root, &mut r),
            Lang::Python => check_python(root, &mut r),
            Lang::Node => check_node(root, &mut r),
        }
    }

    match crate::probes::install() {
        Ok(d) => println!("probes        {}", d.display()),
        Err(e) => r.problems.push(format!("can't write probes to the cache: {e:#}")),
    }
    let gi = std::fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
    if root.join(".gitignore").exists() && !gi.contains(".visualdebug") {
        r.notes.push(".visualdebug/ will be added to .gitignore on the first run".into());
    }

    println!();
    for n in &r.notes {
        println!("note: {n}");
    }
    for p in &r.problems {
        println!("problem: {p}");
    }
    if r.problems.is_empty() {
        println!("ok: ready to trace{}", if langs.is_empty() { "" } else { " (see `visualdebug run --help`)" });
        Ok(0)
    } else {
        Ok(1)
    }
}

fn check_ruby(root: &Path, r: &mut Report) {
    let Some(v) = out("ruby", &["-e", "print RUBY_VERSION"], root) else {
        r.problems.push("Ruby project but `ruby` isn't on PATH".into());
        return;
    };
    let ok = version_tuple(&v) >= (3, 2);
    let lock = std::fs::read_to_string(root.join("Gemfile.lock")).unwrap_or_default();
    let gem = |name: &str| {
        lock.lines()
            .find_map(|l| l.strip_prefix(&format!("    {name} (")).map(|r| r.trim_end_matches(')').to_string()))
    };
    let mut tools = vec![];
    for g in ["rails", "rspec-core", "minitest"] {
        if let Some(v) = gem(g) {
            tools.push(format!("{g} {v}"));
        }
    }
    println!("ruby          {v} {}{}", if ok { "✓" } else { "✗ (needs 3.2+)" }, if tools.is_empty() { String::new() } else { format!(" · {}", tools.join(", ")) });
    if !ok {
        r.problems.push(format!("Ruby {v} is too old (3.2+ needed: TracePoint target_thread)"));
    }
    if root.join("Gemfile").exists() && out("bundle", &["check"], root).is_none() {
        r.problems.push("`bundle check` fails: run `bundle install` first".into());
    }
    if root.join("bin/spring").exists() {
        r.notes.push("Spring is disabled for traced runs (DISABLE_SPRING=1)".into());
    }
}

fn check_python(root: &Path, r: &mut Report) {
    let py = python_for(root);
    let Some(v) = out(&py, &["-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])"], root) else {
        r.problems.push(format!("Python project but `{py}` doesn't run"));
        return;
    };
    let t = version_tuple(&v);
    let backend = if t >= (3, 12) { "sys.monitoring" } else { "sys.settrace" };
    let pytest = out(&py, &["-c", "import pytest; print(pytest.__version__)"], root);
    println!(
        "python        {v} {} via {py} · {backend}{}",
        if t >= (3, 11) { "✓" } else { "✗ (needs 3.11+)" },
        pytest.as_deref().map(|p| format!(" · pytest {p}")).unwrap_or_default()
    );
    if t < (3, 11) {
        r.problems.push(format!("Python {v} is too old (3.11+ needed)"));
    }
    let cfg = ["pytest.ini", "pyproject.toml", "setup.cfg", "tox.ini"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(root.join(f)).ok())
        .collect::<String>();
    if cfg.lines().any(|l| l.contains("addopts") && (l.contains(" -n") || l.contains("numprocesses"))) {
        r.notes.push("pytest-xdist is on by default here: add `-n 0` to traced runs for one linear trace".into());
    }
    if t < (3, 12) && cfg.contains("--cov") {
        r.notes.push("coverage also uses sys.settrace on Python < 3.12: drop --cov for traced runs".into());
    }
}

fn check_node(root: &Path, r: &mut Report) {
    let Some(v) = out("node", &["-v"], root) else {
        r.problems.push("package.json present but `node` isn't on PATH".into());
        return;
    };
    let (major, _) = version_tuple(&v);
    let pkg: serde_json::Value = std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let dep = |n: &str| ["devDependencies", "dependencies"].iter().find_map(|k| pkg[k][n].as_str().map(|v| format!("{n} {v}")));
    let tools: Vec<String> = ["jest", "vitest", "tsx", "typescript", "mocha"].iter().filter_map(|n| dep(n)).collect();
    let status = if major >= 22 { "✓" } else if major >= 18 { "~ (22/24 recommended)" } else { "✗ (needs 20+)" };
    println!("node          {v} {status}{}", if tools.is_empty() { String::new() } else { format!(" · {}", tools.join(", ")) });
    if major < 18 {
        r.problems.push(format!("Node {v} is too old"));
    }
    if !tools.is_empty() && !root.join("node_modules").is_dir() {
        r.problems.push("node_modules is missing: run `npm ci` (or your package manager's install)".into());
    }
    for f in ["vitest.config.ts", "vitest.config.js", "vitest.config.mts", "vite.config.ts"] {
        if std::fs::read_to_string(root.join(f)).is_ok_and(|t| t.contains("pool") && t.contains("threads")) {
            r.notes.push(format!("{f} uses the threads pool; traced runs force --pool=forks"));
        }
    }
}
