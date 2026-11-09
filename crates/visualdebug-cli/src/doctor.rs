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
    let o = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn version_tuple(v: &str) -> (u32, u32) {
    let mut it = v
        .trim_start_matches('v')
        .split(['.', ' ', '-'])
        .filter_map(|x| x.parse().ok());
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
    if [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
        "Pipfile",
    ]
    .iter()
    .any(|f| has(f))
    {
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
    let mut r = Report {
        problems: vec![],
        notes: vec![],
    };
    println!("visualdebug {}", env!("CARGO_PKG_VERSION"));
    println!("project root  {}", root.display());

    // git
    match out("git", &["--version"], root) {
        None => r
            .problems
            .push("git not found: --focus-diff needs it".into()),
        Some(v) => {
            let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).ok();
            match branch {
                Some(b) => {
                    let base = git(
                        root,
                        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
                    )
                    .ok()
                    .map(|s| s.trim().to_string())
                    .or_else(|| {
                        ["main", "master"]
                            .iter()
                            .find(|b| git(root, &["rev-parse", "--verify", "-q", b]).is_ok())
                            .map(|s| s.to_string())
                    });
                    println!(
                        "git           {} · branch {} · likely base {}",
                        v.trim_start_matches("git version "),
                        b.trim(),
                        base.as_deref().unwrap_or("?")
                    );
                }
                None => r.notes.push(
                    "not a git repository: use --focus <file> instead of --focus-diff".into(),
                ),
            }
        }
    }

    let langs = languages(root);
    if langs.is_empty() {
        r.notes.push(
            "no Gemfile, pyproject/requirements or package.json here: run from your project root"
                .into(),
        );
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
        Err(e) => r
            .problems
            .push(format!("can't write probes to the cache: {e:#}")),
    }
    println!();
    for n in &r.notes {
        println!("note: {n}");
    }
    for p in &r.problems {
        println!("problem: {p}");
    }
    if r.problems.is_empty() {
        println!(
            "ok: ready to trace{}",
            if langs.is_empty() {
                ""
            } else {
                " (see `visualdebug run --help`)"
            }
        );
        Ok(0)
    } else {
        Ok(1)
    }
}

fn check_ruby(root: &Path, r: &mut Report) {
    let Some(v) = out("ruby", &["-e", "print RUBY_VERSION"], root) else {
        r.problems
            .push("Ruby project but `ruby` isn't on PATH".into());
        return;
    };
    let ok = version_tuple(&v) >= (3, 2);
    let lock = std::fs::read_to_string(root.join("Gemfile.lock")).unwrap_or_default();
    let gem = |name: &str| {
        lock.lines().find_map(|l| {
            l.strip_prefix(&format!("    {name} ("))
                .map(|r| r.trim_end_matches(')').to_string())
        })
    };
    let mut tools = vec![];
    for g in ["rails", "rspec-core", "minitest"] {
        if let Some(v) = gem(g) {
            tools.push(format!("{g} {v}"));
        }
    }
    println!(
        "ruby          {v} {}{}",
        if ok { "✓" } else { "✗ (needs 3.2+)" },
        if tools.is_empty() {
            String::new()
        } else {
            format!(" · {}", tools.join(", "))
        }
    );
    if !ok {
        r.problems.push(format!(
            "Ruby {v} is too old (3.2+ needed: TracePoint target_thread)"
        ));
    }
    if root.join("Gemfile").exists() && out("bundle", &["check"], root).is_none() {
        r.problems
            .push("`bundle check` fails: run `bundle install` first".into());
    }
    if root.join("bin/spring").exists() {
        r.notes
            .push("Spring is disabled for traced runs (DISABLE_SPRING=1)".into());
    }
}

fn check_python(root: &Path, r: &mut Report) {
    let py = python_for(root);
    let Some(v) = out(
        &py,
        &["-c", "import sys; print('%d.%d.%d' % sys.version_info[:3])"],
        root,
    ) else {
        r.problems
            .push(format!("Python project but `{py}` doesn't run"));
        return;
    };
    let t = version_tuple(&v);
    let backend = if t >= (3, 12) {
        "sys.monitoring"
    } else {
        "sys.settrace"
    };
    let pytest = out(
        &py,
        &["-c", "import pytest; print(pytest.__version__)"],
        root,
    );
    println!(
        "python        {v} {} via {py} · {backend}{}",
        if t >= (3, 11) {
            "✓"
        } else {
            "✗ (needs 3.11+)"
        },
        pytest
            .as_deref()
            .map(|p| format!(" · pytest {p}"))
            .unwrap_or_default()
    );
    if t < (3, 11) {
        r.problems
            .push(format!("Python {v} is too old (3.11+ needed)"));
    }
    let cfg = ["pytest.ini", "pyproject.toml", "setup.cfg", "tox.ini"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(root.join(f)).ok())
        .collect::<String>();
    if cfg
        .lines()
        .any(|l| l.contains("addopts") && (l.contains(" -n") || l.contains("numprocesses")))
    {
        r.notes.push(
            "pytest-xdist is on by default here: add `-n 0` to traced runs for one linear trace"
                .into(),
        );
    }
    if t < (3, 12) && cfg.contains("--cov") {
        r.notes.push(
            "coverage also uses sys.settrace on Python < 3.12: drop --cov for traced runs".into(),
        );
    }
}

fn check_node(root: &Path, r: &mut Report) {
    let Some(v) = out("node", &["-v"], root) else {
        r.problems
            .push("package.json present but `node` isn't on PATH".into());
        return;
    };
    let (major, _) = version_tuple(&v);
    let pkg: serde_json::Value = std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let dep = |n: &str| {
        ["devDependencies", "dependencies"]
            .iter()
            .find_map(|k| pkg[k][n].as_str().map(|v| format!("{n} {v}")))
    };
    let tools: Vec<String> = ["jest", "vitest", "tsx", "typescript", "mocha"]
        .iter()
        .filter_map(|n| dep(n))
        .collect();
    let status = if major >= 22 {
        "✓"
    } else if major >= 18 {
        "~ (22/24 recommended)"
    } else {
        "✗ (needs 20+)"
    };
    println!(
        "node          {v} {status}{}",
        if tools.is_empty() {
            String::new()
        } else {
            format!(" · {}", tools.join(", "))
        }
    );
    if major < 18 {
        r.problems.push(format!("Node {v} is too old"));
    }
    if !tools.is_empty() && !root.join("node_modules").is_dir() {
        r.problems.push(
            "node_modules is missing: run `npm ci` (or your package manager's install)".into(),
        );
    }
    for f in [
        "vitest.config.ts",
        "vitest.config.js",
        "vitest.config.mts",
        "vite.config.ts",
    ] {
        if std::fs::read_to_string(root.join(f))
            .is_ok_and(|t| t.contains("pool") && t.contains("threads"))
        {
            r.notes.push(format!(
                "{f} uses the threads pool; traced runs force --pool=forks"
            ));
        }
    }
}

// ------------------------------------------------------------------ --bench

const RUBY_BENCH: &str = r#"class Bench
  def run(n)
    acc = 0
    n.times do |i|
      acc += helper(i)
    end
    acc
  end

  def helper(i)
    x = i * 2
    x + 1
  end
end
class Never; def never; end; end
t = Process.clock_gettime(Process::CLOCK_MONOTONIC)
Bench.new.run(Integer(ARGV[0]))
puts ((Process.clock_gettime(Process::CLOCK_MONOTONIC) - t) * 1000).round(3)
"#;

const PY_BENCH: &str = r#"import sys, time
class Bench:
    def run(self, n):
        acc = 0
        for i in range(n):
            acc += self.helper(i)
        return acc

    def helper(self, i):
        x = i * 2
        return x + 1

def never():
    pass

t = time.perf_counter()
Bench().run(int(sys.argv[1]))
print(round((time.perf_counter() - t) * 1000, 3))
"#;

const NODE_BENCH: &str = r#"function helper(i) {
  const x = i * 2;
  return x + 1;
}
function run(n) {
  let acc = 0;
  for (let i = 0; i < n; i++) {
    acc += helper(i);
  }
  return acc;
}
function never() {
  return 0;
}
const t = process.hrtime.bigint();
run(Number(process.argv[2]));
console.log((Number(process.hrtime.bigint() - t) / 1e6).toFixed(3));
"#;

struct Bench {
    lang: &'static str,
    cmd: String,
    file: &'static str,
    src: &'static str,
    iters: u32,
    window: &'static str,
    outside: &'static str,
}

/// Times the hot loop: no probe, probe armed but outside its window, and recording everything.
pub fn bench() -> Result<i32> {
    let dir = std::env::temp_dir().join(format!("visualdebug-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let dir = dir.canonicalize()?;
    let probe_dir = crate::probes::install()?;
    let mut benches = vec![];
    if out("ruby", &["-v"], &dir).is_some() {
        benches.push(Bench {
            lang: "ruby",
            cmd: "ruby".into(),
            file: "bench.rb",
            src: RUBY_BENCH,
            iters: 2000,
            window: "Bench#run",
            outside: "Never#never",
        });
    }
    let py = python_for(&std::env::current_dir()?);
    if out(&py, &["-V"], &dir).is_some() {
        benches.push(Bench {
            lang: "python",
            cmd: py,
            file: "bench.py",
            src: PY_BENCH,
            iters: 2000,
            window: "Bench.run",
            outside: "never",
        });
    }
    if out("node", &["-v"], &dir).is_some() {
        benches.push(Bench {
            lang: "node",
            cmd: "node".into(),
            file: "bench.js",
            src: NODE_BENCH,
            iters: 300,
            window: "bench.js:7",
            outside: "bench.js:14",
        });
    }
    println!(
        "{:<8} {:>10} {:>16} {:>14} {:>9} {:>12}",
        "runtime", "baseline", "armed, outside", "recording", "steps", "per step"
    );
    for b in &benches {
        std::fs::write(dir.join(b.file), b.src)?;
        let time = |start: Option<&str>, line_files: Option<Vec<&str>>| -> Result<(f64, usize)> {
            let run_dir = dir.join(format!(
                "run-{}-{}",
                b.lang,
                start.unwrap_or("none").replace([':', '#', '.'], "_")
            ));
            let _ = std::fs::remove_dir_all(&run_dir);
            std::fs::create_dir_all(&run_dir)?;
            let cfg = run_dir.join("config.json");
            std::fs::write(
                &cfg,
                serde_json::to_vec(&serde_json::json!({
                    "version": 1, "root": dir, "out_dir": run_dir, "line_files": line_files, "start_at": start,
                    "max_events": 1_000_000, "redact": crate::run::DEFAULT_REDACT,
                }))?,
            )?;
            let mut c = Command::new(&b.cmd);
            c.arg(dir.join(b.file))
                .arg(b.iters.to_string())
                .current_dir(&dir);
            if start.is_some() {
                c.envs(crate::probes::env(&probe_dir, &cfg));
            }
            let o = c.output()?;
            let ms: f64 = String::from_utf8_lossy(&o.stdout)
                .trim()
                .lines()
                .last()
                .unwrap_or("")
                .parse()
                .unwrap_or(f64::NAN);
            let steps = std::fs::read_dir(&run_dir)?
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("trace-"))
                .map(|e| {
                    std::fs::read_to_string(e.path())
                        .unwrap_or_default()
                        .lines()
                        .filter(|l| !l.contains("\"meta\""))
                        .count()
                })
                .sum();
            Ok((ms, steps))
        };
        let lf = if b.lang == "node" {
            Some(vec![b.file])
        } else {
            None
        };
        let (base, _) = time(None, None)?;
        let (armed, _) = time(Some(b.outside), lf.clone())?;
        let (rec, steps) = time(Some(b.window), lf)?;
        let per = if steps > 0 {
            (rec - base) * 1000.0 / steps as f64
        } else {
            f64::NAN
        };
        println!(
            "{:<8} {:>8.2}ms {:>9.2}ms ({:>3.0}x) {:>12.1}ms {:>9} {:>9.1}µs",
            b.lang,
            base,
            armed,
            armed / base.max(0.001),
            rec,
            steps,
            per
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "\n'armed, outside' = probe loaded, recording window never opened (should be ≈ baseline;"
    );
    println!(
        "Python 3.11's settrace backend is the exception). 'per step' = extra time per recorded event."
    );
    Ok(0)
}
