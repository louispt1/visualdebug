//! The normalized trace: what `run` writes to `.visualdebug/traces/<name>.json` and what
//! `summarize`, `annotate` and `view` read. Schema: `schema/trace.schema.json`.

use serde::{Deserialize, Serialize};
use serde_json::Map;
use std::collections::BTreeMap;

pub const TRACE_VERSION: u32 = 1;

/// A serialized runtime value, as produced by the probes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Value {
    #[serde(rename = "type")]
    pub ty: String,
    pub repr: String,
    #[serde(default)]
    pub truncated: bool,
    /// Object identity, when the runtime can provide a stable one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exception {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Call,
    Line,
    Return,
    Raise,
    BlockCall,
    BlockReturn,
    SideEffect,
}

impl Kind {
    pub fn is_call(self) -> bool {
        matches!(self, Kind::Call | Kind::BlockCall)
    }
    pub fn is_return(self) -> bool {
        matches!(self, Kind::Return | Kind::BlockReturn)
    }
}

/// How the locals of a step differ from the previous snapshot of the same frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Diff {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    /// name -> repr before the change
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub changed: Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    /// Changed names whose object identity stayed the same (mutated in place).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutated: Vec<String>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

pub type Locals = Map<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// 1-based, stable; what annotations target.
    pub id: u32,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Trace-wide frame index (probe frame ids are namespaced by pid).
    pub frame: u32,
    pub depth: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub library: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locals: Option<Locals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<Diff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exception: Option<Exception>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effect: Option<serde_json::Value>,
    pub pid: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Focus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// project-relative path -> changed line ranges (inclusive). Empty ranges = whole file in focus.
    #[serde(default)]
    pub files: BTreeMap<String, Vec<(u32, u32)>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub focus: Focus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_at: Option<String>,
    /// Number of probe events before normalization (library call/return pairs merge into one step).
    #[serde(default)]
    pub raw_events: u32,
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
}

impl Trace {
    pub fn step(&self, id: u32) -> Option<&Step> {
        id.checked_sub(1).and_then(|i| self.steps.get(i as usize))
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Trace> {
        use anyhow::Context;
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let t: Trace = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        anyhow::ensure!(t.version == TRACE_VERSION, "{}: unsupported trace version {}", path.display(), t.version);
        Ok(t)
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec(self)?)?;
        Ok(())
    }

    /// Is `file:line` inside a changed range?
    pub fn is_changed(&self, file: &str, line: u32) -> bool {
        self.focus
            .files
            .get(file)
            .is_some_and(|r| r.iter().any(|&(a, b)| line >= a && line <= b))
    }
}

/// `repr` of a serialized value inside a locals map (values are kept as raw JSON for speed).
pub fn repr_of(v: &serde_json::Value) -> &str {
    v.get("repr").and_then(|r| r.as_str()).unwrap_or("?")
}
