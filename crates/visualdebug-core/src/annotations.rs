//! `annotations.json`: validated against a trace, then merged into it. Schema:
//! `schema/annotations.schema.json`.

use crate::trace::Trace;
use serde::{Deserialize, Serialize};

pub const MAX_TOUR_STOPS: usize = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Annotations {
    pub version: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub tour: Vec<Stop>,
    #[serde(default)]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub quiz: Vec<Question>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    pub step: u32,
    #[serde(default)]
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlight_vars: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub step: u32,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub question: String,
    pub options: Vec<String>,
    pub answer: usize,
    #[serde(default)]
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Parse and check annotations against `trace`. Errors make the annotations unusable.
pub fn validate(trace: &Trace, json: &str) -> (Option<Annotations>, Report) {
    let mut r = Report::default();
    let a: Annotations = match serde_json::from_str(json) {
        Ok(a) => a,
        Err(e) => {
            r.errors.push(format!("not valid annotations JSON: {e}"));
            return (None, r);
        }
    };
    if a.version != 1 {
        r.errors.push(format!("version must be 1 (got {})", a.version));
    }
    let n = trace.steps.len() as u32;
    let step_ok = |s: u32| s >= 1 && s <= n;
    let range = if n == 0 { "the trace has no steps".to_string() } else { format!("valid steps are 1..={n}") };
    for (i, s) in a.tour.iter().enumerate() {
        if !step_ok(s.step) {
            r.errors.push(format!("tour[{i}] targets step {} which doesn't exist ({range})", s.step));
            continue;
        }
        if s.body.trim().is_empty() {
            r.errors.push(format!("tour[{i}] (step {}) has an empty body", s.step));
        }
        let locals = trace.step(s.step).and_then(|st| st.locals.as_ref());
        for v in &s.highlight_vars {
            if !locals.is_some_and(|l| l.contains_key(v)) {
                r.warnings.push(format!("tour[{i}] highlights {v:?}, which isn't a variable at step {}", s.step));
            }
        }
    }
    for (i, s) in a.notes.iter().enumerate() {
        if !step_ok(s.step) {
            r.errors.push(format!("notes[{i}] targets step {} which doesn't exist ({range})", s.step));
        }
    }
    for (i, q) in a.quiz.iter().enumerate() {
        if q.options.len() < 2 {
            r.errors.push(format!("quiz[{i}] needs at least 2 options"));
        }
        if q.answer >= q.options.len() {
            r.errors.push(format!("quiz[{i}].answer is {} but there are {} options (0-based)", q.answer, q.options.len()));
        }
        if let Some(s) = q.step
            && !step_ok(s)
        {
            r.errors.push(format!("quiz[{i}] links step {s} which doesn't exist ({range})"));
        }
    }
    if a.tour.len() > MAX_TOUR_STOPS {
        r.warnings.push(format!("the tour has {} stops; more than ~{MAX_TOUR_STOPS} loses the reader — aim for 5–12", a.tour.len()));
    }
    if a.tour.is_empty() {
        r.warnings.push("no tour stops: the viewer will open in free-scrubbing mode".into());
    }
    if a.overview.trim().is_empty() && !a.tour.is_empty() {
        r.warnings.push("no overview: give background and intuition before the first stop".into());
    }
    (Some(a), r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{RunInfo, ingest};

    fn trace() -> Trace {
        let lines = r#"{"event":"call","file":"a.rb","line":1,"function":"f","frame_id":1,"depth":1,"locals":{"x":{"type":"Integer","repr":"1"}},"seq":1,"t":1,"pid":1}
{"event":"line","file":"a.rb","line":2,"function":"f","frame_id":1,"depth":1,"locals":{"x":{"type":"Integer","repr":"1"}},"seq":2,"t":2,"pid":1}"#;
        ingest([lines], RunInfo::default()).0
    }

    #[test]
    fn accepts_valid_and_warns() {
        let json = r#"{"version":1,"title":"t","overview":"o","tour":[{"step":2,"title":"s","body":"b","highlight_vars":["x","nope"]}],
            "notes":[{"step":1,"body":"n"}],"quiz":[{"question":"q","options":["a","b"],"answer":1,"explanation":"e","step":2}]}"#;
        let (a, r) = validate(&trace(), json);
        assert!(a.is_some() && r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("nope"));
    }

    #[test]
    fn rejects_bad_steps_answers_and_typos() {
        let (_, r) = validate(&trace(), r#"{"version":1,"tour":[{"step":9,"body":"b"}],"quiz":[{"question":"q","options":["a"],"answer":3}]}"#);
        assert_eq!(r.errors.len(), 3, "{:?}", r.errors);
        let (_, r) = validate(&trace(), r#"{"version":1,"tuor":[]}"#);
        assert!(r.errors[0].contains("unknown field"));
    }
}
