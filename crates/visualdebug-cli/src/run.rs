//! `visualdebug run`: inject probes, run the command, ingest the trace.

use crate::project::{Project, git};
use crate::probes;
use anyhow::{Context, Result, bail};
use std::process::Command;
use visualdebug_core::Focus;
use visualdebug_core::focus::{fmt_ranges, parse_unified_diff};
use visualdebug_core::ingest::{RunInfo, ingest_dir};

pub const DEFAULT_REDACT: &str =
    "password|passwd|secret|token|api_key|apikey|authorization|cookie|session|private_key|credential";

#[derive(clap::Args, Debug)]
pub struct RunArgs {
    /// Focus on what changed since this git ref: line-level detail in changed files, calls only elsewhere.
    #[arg(long, value_name = "BASE")]
    pub focus_diff: Option<String>,
    /// Extra file(s) to record line by line (repeatable).
    #[arg(long, value_name = "FILE")]
    pub focus: Vec<String>,
    /// Start recording when execution first enters this: `file:line`, `Class#method`, `Class.method`, `module.func`.
    #[arg(long, value_name = "TARGET")]
    pub start_at: Option<String>,
    /// Trace name (default: derived from the command).
    #[arg(long)]
    pub name: Option<String>,
    /// Stop recording after this many events.
    #[arg(long, default_value_t = 20_000)]
    pub max_events: u32,
    /// The command to trace, after `--`.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ruby,
    Python,
    Node,
}

impl Lang {
    pub fn name(self) -> &'static str {
        match self {
            Lang::Ruby => "ruby",
            Lang::Python => "python",
            Lang::Node => "node",
        }
    }
}

/// Guess the language and whether the command is a test runner, from the command line.
pub fn classify(cmd: &[String]) -> (Option<Lang>, bool) {
    let toks: Vec<String> = cmd
        .iter()
        .map(|t| t.rsplit('/').next().unwrap_or(t).to_string())
        .collect();
    let has = |s: &str| toks.iter().any(|t| t == s);
    let file_ext = |ext: &str| cmd.iter().any(|t| t.split(':').next().unwrap_or(t).ends_with(ext));
    if has("rspec") || has("rails") || has("rake") || has("ruby") || has("bundle") || has("m") || file_ext(".rb") {
        let test = has("rspec")
            || (has("rails") || has("rake")) && (has("test") || has("spec") || toks.iter().any(|t| t.starts_with("test:")))
            || cmd.iter().any(|t| t.contains("_test.rb") || t.contains("_spec.rb"));
        return (Some(Lang::Ruby), test);
    }
    if has("pytest") || has("py.test") || toks.iter().any(|t| t.starts_with("python")) || file_ext(".py") {
        let test = has("pytest") || has("py.test") || cmd.windows(2).any(|w| w[0] == "-m" && w[1] == "pytest");
        return (Some(Lang::Python), test);
    }
    let node_tool = ["node", "npx", "npm", "pnpm", "yarn", "bun", "jest", "vitest", "tsx", "ts-node", "mocha"];
    if node_tool.iter().any(|t| has(t)) || [".js", ".cjs", ".mjs", ".ts", ".mts", ".cts"].iter().any(|e| file_ext(e)) {
        let test = has("jest") || has("vitest") || has("mocha") || has("test");
        return (Some(Lang::Node), test);
    }
    (None, false)
}

/// Force test runners into a single process where we safely can; otherwise say what to pass.
pub fn adjust_for_runners(cmd: &mut Vec<String>, env: &mut Vec<(String, String)>) -> Vec<String> {
    let mut notes = Vec::new();
    let names: Vec<String> = cmd.iter().map(|t| t.rsplit('/').next().unwrap_or(t).to_string()).collect();
    let direct = |tool: &str| names.iter().position(|t| t == tool);
    let has_flag = |cmd: &[String], f: &[&str]| cmd.iter().any(|t| f.iter().any(|f| t == f || t.starts_with(&format!("{f}="))));
    let via_script = names.first().is_some_and(|t| matches!(t.as_str(), "npm" | "pnpm" | "yarn" | "bun")) && !names.iter().any(|t| t == "exec");
    if direct("jest").is_some() && !via_script {
        if !has_flag(cmd, &["--runInBand", "-i"]) {
            cmd.push("--runInBand".into());
            notes.push("added --runInBand so Jest runs tests in this process".into());
        }
    } else if direct("vitest").is_some() && !via_script {
        if !has_flag(cmd, &["--pool"]) {
            cmd.push("--pool=forks".into());
        }
        if !has_flag(cmd, &["--no-file-parallelism", "--maxWorkers", "--fileParallelism"]) {
            cmd.push("--no-file-parallelism".into());
        }
        notes.push("Vitest runs with --pool=forks --no-file-parallelism (worker threads can't be traced)".into());
    } else if via_script && names.iter().any(|t| matches!(t.as_str(), "test" | "jest" | "vitest")) {
        notes.push(
            "running tests through a package script: if they use workers, call the runner directly (`npx jest --runInBand`, `npx vitest run --pool=forks`)".into(),
        );
    }
    if (names.iter().any(|t| t == "pytest") || cmd.windows(2).any(|w| w[0] == "-m" && w[1] == "pytest"))
        && cmd.iter().any(|t| t == "-n" || t.starts_with("-n") && t.len() > 2 && t[2..].chars().all(|c| c.is_ascii_digit()) || t.starts_with("--numprocesses"))
    {
        notes.push("pytest-xdist workers are traced as separate processes; add `-n 0` for one linear trace".into());
    }
    env.push(("DISABLE_SPRING".into(), "1".into()));
    env.push(("PARALLEL_WORKERS".into(), "1".into())); // Rails parallel tests
    notes
}

pub fn derive_name(cmd: &[String]) -> String {
    let target = cmd.iter().rev().find(|t| {
        !t.starts_with('-') && (t.contains('/') || t.contains('.')) && !t.starts_with("http")
    });
    let raw = match target {
        Some(t) => {
            let (path, suffix) = match t.find("::").or_else(|| t.find(':')) {
                Some(i) => (&t[..i], t[i..].trim_start_matches(':')),
                None => (t.as_str(), ""),
            };
            let stem = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or(path);
            if suffix.is_empty() { stem.to_string() } else { format!("{stem}-{suffix}") }
        }
        None => cmd.iter().take(2).cloned().collect::<Vec<_>>().join("-"),
    };
    let clean: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    let clean = clean.trim_matches('-').to_string();
    if clean.is_empty() { "trace".into() } else { clean }
}

pub fn focus_from_diff(project: &Project, base: &str) -> Result<Focus> {
    let merge_base = git(&project.root, &["merge-base", base, "HEAD"])
        .with_context(|| format!("--focus-diff {base}: is {base:?} a valid git ref here?"))?;
    // Diff merge-base against the working tree: committed + staged + unstaged changes.
    let text = git(&project.root, &["diff", "--unified=0", "--no-color", "--no-ext-diff", merge_base.trim()])?;
    let subdir = project.git_subdir().unwrap_or_default();
    let mut files = parse_unified_diff(&text, &subdir);
    files.retain(|f, _| project.root.join(f).is_file());
    // Untracked files count as wholly changed.
    if let Ok(untracked) = git(&project.root, &["ls-files", "--others", "--exclude-standard"]) {
        for f in untracked.lines().filter(|f| !f.starts_with(".visualdebug/")) {
            let n = project.source(f).map(|s| s.lines().count() as u32).unwrap_or(0);
            if n > 0 {
                files.entry(f.to_string()).or_insert_with(|| vec![(1, n)]);
            }
        }
    }
    Ok(Focus { base: Some(base.to_string()), files })
}

fn is_file_line(s: &str) -> bool {
    s.rsplit_once(':').is_some_and(|(f, l)| !f.is_empty() && !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()))
}

const JS_EXTS: &[&str] = &["js", "cjs", "mjs", "ts", "mts", "cts", "jsx", "tsx"];

/// `name`, `Class.method` or `Class#method` -> `file:line` of its definition, by scanning sources.
pub fn resolve_js_name(root: &std::path::Path, target: &str) -> Option<String> {
    let (class, name) = match target.rsplit_once(['.', '#']) {
        Some((c, n)) => (Some(c.rsplit(['.', '#']).next().unwrap_or(c)), n),
        None => (None, target),
    };
    let defines = |line: &str| {
        let t = line.trim_start();
        let t = t.strip_prefix("export ").unwrap_or(t);
        let t = t.strip_prefix("default ").unwrap_or(t);
        let t = t.strip_prefix("async ").unwrap_or(t);
        let after = |p: &str| t.strip_prefix(p).and_then(|r| r.strip_prefix(name));
        let next_is = |r: Option<&str>, f: &dyn Fn(&str) -> bool| r.is_some_and(|r| f(r.trim_start()));
        next_is(after("function "), &|r| r.starts_with('(') || r.starts_with('<'))
            || next_is(after("function* "), &|r| r.starts_with('('))
            || ["const ", "let ", "var "].iter().any(|k| next_is(after(k), &|r| r.starts_with('=') || r.starts_with(':')))
            || ["", "static ", "public ", "private ", "protected ", "get ", "set "].iter().any(|k| {
                next_is(after(k), &|r| (r.starts_with('(') || r.starts_with('<')) && line.trim_end().ends_with('{'))
            })
    };
    let mut stack = vec![root.to_path_buf()];
    let mut hits: Vec<(bool, String)> = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let fname = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                if !fname.starts_with('.') && !matches!(fname.as_str(), "node_modules" | "dist" | "build" | "coverage" | "out") {
                    stack.push(p);
                }
                continue;
            }
            if !fname.rsplit('.').next().is_some_and(|e| JS_EXTS.contains(&e)) || fname.ends_with(".d.ts") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let in_class = class.is_none_or(|c| text.contains(&format!("class {c}")));
            for (i, line) in text.lines().enumerate() {
                if defines(line) {
                    let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().into_owned();
                    let is_test = rel.contains(".test.") || rel.contains(".spec.") || rel.contains("__tests__");
                    // break on the first body line; one-line arrows have their body on the same line
                    let body = if line.trim_end().ends_with('{') { i + 2 } else { i + 1 };
                    hits.push((!in_class || is_test, format!("{rel}:{body}")));
                }
            }
        }
    }
    hits.sort();
    hits.into_iter().next().map(|(_, h)| h)
}

const SOURCE_EXTS: &[&str] = &["rb", "py", "js", "cjs", "mjs", "ts", "mts", "cts", "jsx", "tsx", "rake", "ru"];

pub fn run(args: RunArgs) -> Result<i32> {
    let project = Project::discover()?;
    project.ensure_dir()?;
    let (lang, is_test) = classify(&args.command);

    let mut focus = match &args.focus_diff {
        Some(base) => focus_from_diff(&project, base)?,
        None => Focus::default(),
    };
    focus.files.retain(|f, _| f.rsplit('.').next().is_some_and(|e| SOURCE_EXTS.contains(&e)));
    for f in &args.focus {
        let rel = std::path::Path::new(f);
        let abs = if rel.is_absolute() { rel.to_path_buf() } else { std::env::current_dir()?.join(rel) };
        let abs = abs.canonicalize().with_context(|| format!("--focus {f}: no such file"))?;
        let rel = abs
            .strip_prefix(project.root.canonicalize()?)
            .with_context(|| format!("--focus {f} is outside the project root {}", project.root.display()))?;
        focus.files.entry(rel.to_string_lossy().into_owned()).or_default();
    }
    if args.focus_diff.is_some() && focus.files.is_empty() {
        eprintln!("visualdebug: warning: no changed source files under {}; recording all project files", project.root.display());
    }

    let start_at = match (&args.start_at, lang, is_test) {
        // The Node probe breaks on lines; resolve function names here.
        (Some(s), Some(Lang::Node), _) if !is_file_line(s) => {
            let found = resolve_js_name(&project.root, s)
                .with_context(|| format!("--start-at {s}: no function, method or arrow named that in the project's JS/TS files; pass file:line instead"))?;
            eprintln!("visualdebug: --start-at {s} -> {found}");
            Some(found)
        }
        (Some(s), _, _) => Some(s.clone()),
        (None, Some(Lang::Ruby | Lang::Python), true) => Some("auto:test".to_string()),
        _ => None,
    };
    if lang == Some(Lang::Node) && focus.files.is_empty() && args.start_at.is_none() {
        bail!(
            "Node tracing needs something to focus on: pass --focus-diff <base>, --focus <file>, or --start-at <file:line>"
        );
    }

    let name = args.name.clone().unwrap_or_else(|| derive_name(&args.command));
    let run_dir = project.runs_dir().join(&name);
    if run_dir.exists() {
        std::fs::remove_dir_all(&run_dir)?;
    }
    std::fs::create_dir_all(&run_dir)?;
    let config_path = run_dir.join("config.json");
    let line_files: Option<Vec<&String>> = (!focus.files.is_empty()).then(|| focus.files.keys().collect());
    let config = serde_json::json!({
        "version": 1,
        "root": project.root,
        "out_dir": run_dir,
        "line_files": line_files,
        "start_at": start_at,
        "max_events": args.max_events,
        "redact": DEFAULT_REDACT,
        "limits": { "repr": 200, "items": 10, "depth": 3 },
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config)?)?;

    let probe_dir = probes::install()?;
    let mut env = probes::env(&probe_dir, &config_path);
    let mut command = args.command.clone();
    for note in adjust_for_runners(&mut command, &mut env) {
        eprintln!("visualdebug: {note}");
    }
    if !focus.files.is_empty() {
        let list: Vec<String> = focus
            .files
            .iter()
            .map(|(f, r)| if r.is_empty() { f.clone() } else { format!("{f} [{}]", fmt_ranges(r)) })
            .collect();
        eprintln!("visualdebug: line-level focus: {}", list.join(", "));
    }
    if let Some(s) = &start_at {
        eprintln!("visualdebug: recording starts at {s}");
    }

    let status = Command::new(&command[0])
        .args(&command[1..])
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()
        .with_context(|| format!("could not start {:?}", command[0]))?;
    let code = exit_code(&status);

    let info = RunInfo {
        name: name.clone(),
        language: lang.map(|l| l.name().to_string()),
        command: args.command.clone(),
        exit_code: Some(code),
        focus,
        start_at,
    };
    let (trace, report) = ingest_dir(&run_dir, info)?;
    let out = project.traces_dir().join(format!("{name}.json"));
    trace.save(&out)?;

    let rel_out = out.strip_prefix(&project.root).unwrap_or(&out);
    eprintln!(
        "\nvisualdebug: trace {name:?}: {} steps from {} process(es){} -> {}",
        trace.steps.len(),
        report.processes,
        if code == 0 { String::new() } else { format!(", command exited {code}") },
        rel_out.display()
    );
    if trace.truncated {
        eprintln!(
            "visualdebug: warning: hit --max-events {}; the trace is TRUNCATED. Narrow it with --start-at <entry point> or --focus-diff.",
            args.max_events
        );
    }
    if trace.steps.is_empty() {
        eprintln!("visualdebug: warning: nothing was recorded. {}", empty_hint(&trace, lang));
    }
    let errlog = run_dir.join("probe-errors.log");
    if let Ok(text) = std::fs::read_to_string(&errlog) {
        let n = text.lines().count();
        if n > 0 {
            eprintln!("visualdebug: {n} probe error(s) logged to {} (the traced program was not affected)", errlog.display());
        }
    }
    if report.bad_lines > 0 {
        eprintln!("visualdebug: skipped {} unreadable trace line(s)", report.bad_lines);
    }
    if !trace.steps.is_empty() {
        eprintln!("visualdebug: next: `visualdebug summarize {name}` or `visualdebug view {name}`");
    }
    Ok(code)
}

fn empty_hint(trace: &visualdebug_core::Trace, lang: Option<Lang>) -> String {
    match (&trace.start_at, lang) {
        (Some(s), _) if s != "auto:test" => format!("Execution never entered --start-at {s}: check the name (e.g. `Pricer#call`, `pricing.apply_discount`, `src/x.ts:12`)."),
        (_, None) => "Couldn't tell which language this command runs; is it Ruby, Python or Node?".into(),
        (_, Some(Lang::Node)) => "No focus line executed. Check --focus-diff/--focus cover code this command runs.".into(),
        _ => "No project code ran in the recording window. Try --start-at <entry point>, or check that the code lives under the project root.".into(),
    }
}

#[cfg(unix)]
fn exit_code(s: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    s.code().or_else(|| s.signal().map(|n| 128 + n)).unwrap_or(1)
}

#[cfg(not(unix))]
fn exit_code(s: &std::process::ExitStatus) -> i32 {
    s.code().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn classifies_commands() {
        assert_eq!(classify(&v("bundle exec rspec spec/x_spec.rb:4")), (Some(Lang::Ruby), true));
        assert_eq!(classify(&v("bin/rails test test/models/a_test.rb")), (Some(Lang::Ruby), true));
        assert_eq!(classify(&v("ruby script.rb")), (Some(Lang::Ruby), false));
        assert_eq!(classify(&v("pytest tests/test_a.py::test_b")), (Some(Lang::Python), true));
        assert_eq!(classify(&v("python3 -m pytest")), (Some(Lang::Python), true));
        assert_eq!(classify(&v("python main.py")), (Some(Lang::Python), false));
        assert_eq!(classify(&v("npx vitest run src/a.test.ts")), (Some(Lang::Node), true));
        assert_eq!(classify(&v("node src/main.js")), (Some(Lang::Node), false));
    }

    #[test]
    fn names_from_commands() {
        assert_eq!(derive_name(&v("bundle exec rspec spec/services/pricer_spec.rb:42")), "pricer_spec-42");
        assert_eq!(derive_name(&v("pytest tests/test_pricing.py::test_bulk_discount")), "test_pricing-test_bulk_discount");
        assert_eq!(derive_name(&v("npx vitest run src/pricing.test.ts")), "pricing.test");
        assert_eq!(derive_name(&v("make check")), "make-check");
    }

    #[test]
    fn resolves_js_names() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(
            d.path().join("src/a.ts"),
            "export async function priceOrder(o: Order): Promise<number> {\n  return 1;\n}\nconst sub = (x) => x;\nclass Cart {\n  total() {\n    return 2;\n  }\n}\n",
        )
        .unwrap();
        let r = d.path();
        assert_eq!(resolve_js_name(r, "priceOrder").as_deref(), Some("src/a.ts:2"));
        assert_eq!(resolve_js_name(r, "sub").as_deref(), Some("src/a.ts:4"));
        assert_eq!(resolve_js_name(r, "Cart.total").as_deref(), Some("src/a.ts:7"));
        assert_eq!(resolve_js_name(r, "nope"), None);
        assert!(is_file_line("src/a.ts:12") && !is_file_line("Cart.total"));
    }

    #[test]
    fn runner_adjustments() {
        let mut env = vec![];
        let mut c = v("npx jest src");
        adjust_for_runners(&mut c, &mut env);
        assert_eq!(c.last().unwrap(), "--runInBand");
        let mut c = v("npx vitest run --pool=threads");
        adjust_for_runners(&mut c, &mut env);
        assert!(c.contains(&"--pool=threads".to_string()) && c.contains(&"--no-file-parallelism".to_string()));
        let mut c = v("npm test");
        let notes = adjust_for_runners(&mut c, &mut env);
        assert_eq!(c, v("npm test"));
        assert!(!notes.is_empty());
    }
}
