//! visualdebug: record how code executes, then scrub through it in a self-contained HTML viewer.
//! The tool is deterministic and never calls an LLM; the Claude Code skill drives the AI parts.

mod doctor;
mod project;
mod probes;
mod run;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use project::{Project, Sources};
use std::path::PathBuf;
use visualdebug_core::summarize::{render_text, summarize};

#[derive(Parser)]
#[command(name = "visualdebug", version, about = "Record how code executes and scrub through it in a self-contained HTML viewer")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Trace a command (Ruby, Python or Node) and write .visualdebug/traces/<name>.json
    Run(run::RunArgs),
    /// Print a compact, step-numbered summary of a trace (for humans and LLMs)
    Summarize {
        /// Trace name or path (default: most recent)
        trace: Option<String>,
        #[arg(long, default_value = "text")]
        format: Format,
        #[arg(long, default_value_t = 300)]
        max_lines: usize,
    },
    /// Bundle trace(s) into one HTML file and open it
    View {
        /// Trace names or paths (default: most recent)
        traces: Vec<String>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        no_open: bool,
    },
    /// Validate annotations against a trace and merge them into it
    Annotate {
        /// Trace name or path
        trace: String,
        /// annotations.json (see schema/annotations.schema.json)
        annotations: PathBuf,
    },
    /// Detect languages, runtimes, test runners and git; report problems
    Doctor {
        /// Also measure probe overhead on a micro-benchmark for each installed runtime
        #[arg(long)]
        bench: bool,
    },
    /// Install the Claude Code skill (default: --user, i.e. ~/.claude/skills/visualdebug)
    InstallSkill {
        /// ~/.claude/skills (all projects)
        #[arg(long, conflicts_with = "project")]
        user: bool,
        /// ./.claude/skills (this project; commit it to share with your team)
        #[arg(long)]
        project: bool,
    },
    /// Delete recorded runs, traces and bundles (keeps .visualdebug/examples)
    Clean,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Text,
    Json,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Run(args) => run::run(args),
        Cmd::Summarize { trace, format, max_lines } => cmd_summarize(trace.as_deref(), format, max_lines).map(|_| 0),
        Cmd::View { traces, out, no_open } => cmd_view(&traces, out, no_open).map(|_| 0),
        Cmd::Annotate { trace, annotations } => cmd_annotate(&trace, &annotations),
        Cmd::Doctor { bench } => doctor::doctor().and_then(|c| if bench { println!(); doctor::bench().map(|b| c.max(b)) } else { Ok(c) }),
        Cmd::InstallSkill { user: _, project } => cmd_install_skill(project).map(|_| 0),
        Cmd::Clean => cmd_clean().map(|_| 0),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("visualdebug: error: {e:#}");
            std::process::exit(2);
        }
    }
}

fn cmd_summarize(trace: Option<&str>, format: Format, max_lines: usize) -> Result<()> {
    let project = Project::discover()?;
    let (_, t) = project.load_trace(trace)?;
    let sources = Sources::new(&project);
    let s = summarize(&t, &|f, l| sources.line(f, l), max_lines);
    match format {
        Format::Text => print!("{}", render_text(&s, &t)),
        Format::Json => println!("{}", serde_json::to_string_pretty(&s)?),
    }
    Ok(())
}

fn cmd_view(names: &[String], out: Option<PathBuf>, no_open: bool) -> Result<()> {
    let project = Project::discover()?;
    let mut traces = Vec::new();
    if names.is_empty() {
        traces.push(project.load_trace(None)?.1);
    }
    for n in names {
        traces.push(project.load_trace(Some(n))?.1);
    }
    let html = visualdebug_viewer::bundle(&traces, &|f| project.source(f))?;
    let out = match out {
        Some(p) => p,
        None => {
            project.ensure_dir()?;
            let stem = if traces.len() == 1 { traces[0].name.clone() } else { "bundle".into() };
            project.dir().join(format!("view-{stem}.html"))
        }
    };
    std::fs::write(&out, html).with_context(|| format!("writing {}", out.display()))?;
    let steps: usize = traces.iter().map(|t| t.steps.len()).sum();
    eprintln!("visualdebug: wrote {} ({} trace(s), {steps} steps)", out.display(), traces.len());
    if no_open || std::env::var_os("VISUALDEBUG_NO_OPEN").is_some() {
        eprintln!("visualdebug: not opening a browser (--no-open / VISUALDEBUG_NO_OPEN); give the user the path above");
    } else {
        match opener::open(&out) {
            Ok(()) => eprintln!("visualdebug: opened it in the default browser"),
            Err(e) => eprintln!("visualdebug: couldn't open a browser ({e}); give the user the path above"),
        }
    }
    Ok(())
}

fn cmd_clean() -> Result<()> {
    let project = Project::discover()?;
    let dir = project.dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("visualdebug: nothing to clean");
        return Ok(());
    };
    let mut n = 0;
    for e in entries.flatten() {
        if e.file_name() == "examples" {
            continue;
        }
        let p = e.path();
        if p.is_dir() { std::fs::remove_dir_all(&p)? } else { std::fs::remove_file(&p)? }
        n += 1;
    }
    eprintln!("visualdebug: removed {n} item(s) from {} (kept examples/)", dir.display());
    Ok(())
}

fn cmd_annotate(trace: &str, file: &std::path::Path) -> Result<i32> {
    let project = Project::discover()?;
    let (path, mut t) = project.load_trace(Some(trace))?;
    let text = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let (a, report) = visualdebug_core::annotations::validate(&t, &text);
    for w in &report.warnings {
        eprintln!("visualdebug: warning: {w}");
    }
    for e in &report.errors {
        eprintln!("visualdebug: error: {e}");
    }
    let Some(a) = a.filter(|_| report.errors.is_empty()) else {
        eprintln!("visualdebug: annotations NOT merged; fix the errors above (step ids come from `visualdebug summarize {}`)", t.name);
        return Ok(1);
    };
    let stops = a.tour.len();
    let quiz = a.quiz.len();
    t.annotations = Some(serde_json::to_value(a)?);
    t.save(&path)?;
    eprintln!("visualdebug: merged {stops} tour stop(s) and {quiz} quiz question(s) into {}; next: `visualdebug view {}`", path.display(), t.name);
    Ok(0)
}

const SKILL_FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("../skill/visualdebug/SKILL.md")),
    ("references/ruby.md", include_str!("../skill/visualdebug/references/ruby.md")),
    ("references/python.md", include_str!("../skill/visualdebug/references/python.md")),
    ("references/node.md", include_str!("../skill/visualdebug/references/node.md")),
];

fn cmd_install_skill(project_scope: bool) -> Result<()> {
    let base = if project_scope {
        Project::discover()?.root.join(".claude/skills")
    } else {
        let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
        let config = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude"));
        config.join("skills")
    };
    let dir = base.join("visualdebug");
    for (rel, body) in SKILL_FILES {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(&p, body).with_context(|| format!("writing {}", p.display()))?;
    }
    eprintln!("visualdebug: installed the skill to {}", dir.display());
    eprintln!("visualdebug: in Claude Code, ask e.g. \"help me review this PR\" — the skill triggers on its own");
    Ok(())
}
