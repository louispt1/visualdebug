//! Probe JSONL -> normalized [`Trace`]: merge per-process files, collapse library call/return pairs
//! into one step, namespace frame ids, fill locations, and compute per-step variable diffs.

use crate::trace::*;
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct RawEvent {
    event: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    t: u64,
    #[serde(default)]
    pid: u64,
    #[serde(default)]
    tid: serde_json::Value,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    function: Option<String>,
    #[serde(default)]
    frame_id: Option<u64>,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    library: bool,
    #[serde(default)]
    locals: Option<Locals>,
    #[serde(default)]
    return_value: Option<Value>,
    #[serde(default)]
    exception: Option<Exception>,
    #[serde(default)]
    side_effect: Option<serde_json::Value>,
}

/// What the caller knows about the run; ingestion fills in the rest.
#[derive(Debug, Clone, Default)]
pub struct RunInfo {
    pub name: String,
    pub language: Option<String>,
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub focus: Focus,
    pub start_at: Option<String>,
}

#[derive(Debug, Default)]
pub struct IngestReport {
    pub raw_events: usize,
    pub bad_lines: usize,
    pub processes: usize,
}

pub fn ingest_dir(dir: &Path, info: RunInfo) -> Result<(Trace, IngestReport)> {
    let mut texts = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("trace-") && name.ends_with(".jsonl") {
            texts.push(std::fs::read_to_string(&p)?);
        }
    }
    Ok(ingest(texts.iter().map(String::as_str), info))
}

pub fn ingest<'a>(
    files: impl IntoIterator<Item = &'a str>,
    info: RunInfo,
) -> (Trace, IngestReport) {
    let mut report = IngestReport::default();
    let mut events = Vec::new();
    let mut truncated = false;
    let mut language = info.language.clone();
    for text in files {
        report.processes += 1;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<RawEvent>(line) {
                Ok(e) if e.event == "meta" => {
                    truncated |= e.kind.as_deref() == Some("truncated");
                    if language.is_none() {
                        language = e.language;
                    }
                }
                Ok(e) => events.push(e),
                Err(_) => report.bad_lines += 1,
            }
        }
    }
    report.raw_events = events.len();
    // Within a process seq is authoritative; across processes the shared monotonic clock orders them.
    events.sort_by_key(|e| (e.t, e.pid, e.seq));

    let mut steps: Vec<Step> = Vec::with_capacity(events.len());
    let mut frames: HashMap<(u64, u64), u32> = HashMap::new();
    let mut last_loc: HashMap<u32, (Option<String>, Option<u32>)> = HashMap::new();
    let mut last_locals: HashMap<u32, Locals> = HashMap::new();
    let mut open_lib: HashMap<String, Vec<usize>> = HashMap::new(); // pid/tid -> step indices

    for e in events {
        let thread = format!("{}/{}", e.pid, e.tid);
        let kind = match e.event.as_str() {
            "call" => Kind::Call,
            "line" => Kind::Line,
            "return" => Kind::Return,
            "raise" => Kind::Raise,
            "block_call" => Kind::BlockCall,
            "block_return" => Kind::BlockReturn,
            "side_effect" => Kind::SideEffect,
            _ => continue,
        };
        let next = frames.len() as u32 + 1;
        let frame = e
            .frame_id
            .map(|f| *frames.entry((e.pid, f)).or_insert(next))
            .unwrap_or(0);

        if e.library && kind == Kind::Return {
            // Attach to the matching open library call; never its own step.
            if let Some(stack) = open_lib.get_mut(&thread)
                && let Some(pos) = stack.iter().rposition(|&i| steps[i].function == e.function)
            {
                let i = stack[pos];
                stack.truncate(pos);
                steps[i].return_value = e.return_value;
            }
            continue;
        }

        let (mut file, mut line) = (e.file, e.line);
        if file.is_none() && (e.library || kind == Kind::SideEffect || kind == Kind::Raise) {
            // Library calls and side effects happen at the caller frame's current line.
            if let Some((f, l)) = last_loc.get(&frame) {
                file = f.clone();
                line = *l;
            }
        } else if !e.library {
            last_loc.insert(frame, (file.clone(), line));
        }

        let diff = e.locals.as_ref().and_then(|now| {
            let prev = last_locals.insert(frame, now.clone())?;
            let d = diff_locals(&prev, now);
            (!d.is_empty()).then_some(d)
        });

        if e.library && kind == Kind::Call {
            open_lib.entry(thread).or_default().push(steps.len());
        }
        steps.push(Step {
            id: steps.len() as u32 + 1,
            kind,
            file,
            line,
            function: e.function,
            frame,
            depth: e.depth.unwrap_or(0),
            library: e.library,
            locals: e.locals,
            diff,
            return_value: e.return_value,
            exception: e.exception,
            side_effect: e.side_effect,
            pid: e.pid,
        });
    }

    let trace = Trace {
        version: TRACE_VERSION,
        name: info.name,
        language,
        command: info.command,
        exit_code: info.exit_code,
        truncated,
        focus: info.focus,
        start_at: info.start_at,
        raw_events: report.raw_events as u32,
        steps,
        annotations: None,
        compare: None,
    };
    (trace, report)
}

pub fn diff_locals(prev: &Locals, now: &Locals) -> Diff {
    let mut d = Diff::default();
    for (k, v) in now {
        match prev.get(k) {
            None => d.added.push(k.clone()),
            Some(p) if repr_of(p) != repr_of(v) => {
                d.changed
                    .insert(k.clone(), serde_json::Value::String(repr_of(p).to_string()));
                if p.get("id").is_some() && p.get("id") == v.get("id") {
                    d.mutated.push(k.clone());
                }
            }
            _ => {}
        }
    }
    d.removed = prev
        .keys()
        .filter(|k| !now.contains_key(*k))
        .cloned()
        .collect();
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> Trace {
        let text = lines.join("\n");
        ingest(
            [text.as_str()],
            RunInfo {
                name: "t".into(),
                ..Default::default()
            },
        )
        .0
    }

    #[test]
    fn diffs_library_merge_and_location_fill() {
        let t = run(&[
            r#"{"event":"meta","kind":"start","language":"ruby","seq":1,"t":1,"pid":1}"#,
            r#"{"event":"call","file":"a.rb","line":1,"function":"A#f","frame_id":1,"depth":1,"locals":{"x":{"type":"NilClass","repr":"nil"}},"seq":2,"t":2,"pid":1,"tid":1}"#,
            r#"{"event":"line","file":"a.rb","line":2,"function":"A#f","frame_id":1,"depth":1,"locals":{"x":{"type":"NilClass","repr":"nil"}},"seq":3,"t":3,"pid":1,"tid":1}"#,
            r#"{"event":"call","library":true,"function":"Lib#g","frame_id":1,"depth":2,"seq":4,"t":4,"pid":1,"tid":1}"#,
            r#"{"event":"return","library":true,"function":"Lib#g","frame_id":1,"depth":2,"return_value":{"type":"Integer","repr":"5"},"seq":5,"t":5,"pid":1,"tid":1}"#,
            r#"{"event":"line","file":"a.rb","line":3,"function":"A#f","frame_id":1,"depth":1,"locals":{"x":{"type":"Integer","repr":"5"},"y":{"type":"NilClass","repr":"nil"}},"seq":6,"t":6,"pid":1,"tid":1}"#,
            "not json",
        ]);
        assert_eq!(t.language.as_deref(), Some("ruby"));
        assert_eq!(t.steps.len(), 4);
        let lib = &t.steps[2];
        assert!(lib.library);
        assert_eq!((lib.file.as_deref(), lib.line), (Some("a.rb"), Some(2)));
        assert_eq!(lib.return_value.as_ref().unwrap().repr, "5");
        let d = t.steps[3].diff.as_ref().unwrap();
        assert_eq!(d.added, vec!["y"]);
        assert_eq!(d.changed["x"], "nil");
        assert!(t.steps[1].diff.is_none());
    }

    #[test]
    fn merges_processes_by_time_and_namespaces_frames() {
        let a = r#"{"event":"call","file":"a.py","line":1,"frame_id":1,"depth":1,"seq":1,"t":10,"pid":1}"#;
        let b = r#"{"event":"call","file":"b.py","line":1,"frame_id":1,"depth":1,"seq":1,"t":5,"pid":2}"#;
        let (t, r) = ingest([a, b], RunInfo::default());
        assert_eq!(r.processes, 2);
        assert_eq!(t.steps[0].file.as_deref(), Some("b.py"));
        assert_ne!(t.steps[0].frame, t.steps[1].frame);
    }
}
