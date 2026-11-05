//! Compact, LLM-friendly rendering of a trace. Every row names the step id(s) annotations can
//! target. Loops are compressed here (ids stay stable: a loop row covers a step range).

use crate::focus::fmt_ranges;
use crate::trace::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub step: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_step: Option<u32>,
    pub depth: u32,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub changed: bool,
    pub text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub vars: String,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub name: String,
    pub steps: usize,
    pub shown: usize,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub language: Option<String>,
    pub command: Vec<String>,
    pub focus: Vec<String>,
    pub rows: Vec<Row>,
    pub elided: bool,
}

/// `source(file, line)` returns that source line, if available.
pub fn summarize(
    t: &Trace,
    source: &dyn Fn(&str, u32) -> Option<String>,
    max_lines: usize,
) -> Summary {
    let base = t
        .steps
        .iter()
        .map(|s| s.depth)
        .min()
        .unwrap_or(1)
        .saturating_sub(1);
    let max_seen = t.steps.iter().map(|s| s.depth).max().unwrap_or(0);
    let mut cx = Cx {
        t,
        source,
        base,
        max_depth: u32::MAX,
    };
    let mut rows = cx.render(base);
    let mut elided = false;
    // Too long: collapse deeper frames first, then cut the middle.
    let mut d = max_seen;
    while rows.len() > max_lines && d > base + 1 {
        d -= 1;
        cx.max_depth = d;
        rows = cx.render(base);
        elided = true;
    }
    if rows.len() > max_lines && max_lines >= 3 {
        let keep_head = max_lines * 2 / 3;
        let keep_tail = max_lines - keep_head - 1;
        let (a, b) = (
            rows[keep_head].step,
            rows[rows.len() - keep_tail - 1]
                .end_step
                .unwrap_or(rows[rows.len() - keep_tail - 1].step),
        );
        let marker = Row {
            step: a,
            end_step: Some(b),
            depth: base + 1,
            kind: "elided",
            file: None,
            line: None,
            line_end: None,
            changed: false,
            text: format!(
                "… {} rows omitted (raise --max-lines, or re-run with a narrower --start-at)",
                rows.len() - keep_head - keep_tail
            ),
            vars: String::new(),
        };
        let tail = rows.split_off(rows.len() - keep_tail);
        rows.truncate(keep_head);
        rows.push(marker);
        rows.extend(tail);
        elided = true;
    }
    Summary {
        name: t.name.clone(),
        steps: t.steps.len(),
        shown: rows.len(),
        exit_code: t.exit_code,
        truncated: t.truncated,
        language: t.language.clone(),
        command: t.command.clone(),
        focus: t
            .focus
            .files
            .iter()
            .map(|(f, r)| {
                if r.is_empty() {
                    f.clone()
                } else {
                    format!("{f} [changed {}]", fmt_ranges(r))
                }
            })
            .collect(),
        rows,
        elided,
    }
}

pub fn render_text(s: &Summary, t: &Trace) -> String {
    let mut out = String::new();
    let status = match s.exit_code {
        Some(0) => "exit 0".to_string(),
        Some(c) => format!("exit {c} (FAILED)"),
        None => "exit ?".to_string(),
    };
    out += &format!(
        "trace: {}  ({} steps, {} rows shown; {}{}{})\n",
        s.name,
        s.steps,
        s.shown,
        status,
        s.language
            .as_deref()
            .map(|l| format!("; {l}"))
            .unwrap_or_default(),
        if s.truncated {
            "; TRUNCATED at max events"
        } else {
            ""
        }
    );
    if !s.command.is_empty() {
        out += &format!("command: {}\n", s.command.join(" "));
    }
    if !s.focus.is_empty() {
        out += &format!("focus: {}   (* = changed line)\n", s.focus.join("; "));
    }
    let names = short_names(t);
    let base = s.rows.iter().map(|r| r.depth).min().unwrap_or(1);
    for r in &s.rows {
        let id = match r.end_step {
            Some(e) if e != r.step => format!("#{}-#{}", r.step, e),
            _ => format!("#{}", r.step),
        };
        let loc = match (&r.file, r.line) {
            (Some(f), Some(l)) => {
                let f = names.get(f.as_str()).map(String::as_str).unwrap_or(f);
                match r.line_end {
                    Some(e) if e != l => format!("{f}:{l}-{e}"),
                    _ => format!("{f}:{l}"),
                }
            }
            _ => String::new(),
        };
        let indent = "  ".repeat(r.depth.saturating_sub(base) as usize);
        let mark = if r.changed { "*" } else { " " };
        let mut line = format!("{id:<9} {mark}{loc:<24} {indent}{}", r.text);
        if !r.vars.is_empty() {
            line += "   ⟶ ";
            line += &r.vars;
        }
        out += line.trim_end();
        out.push('\n');
    }
    out
}

/// Basenames, unless two trace files share one.
fn short_names(t: &Trace) -> HashMap<&str, String> {
    let files: HashSet<&str> = t.steps.iter().filter_map(|s| s.file.as_deref()).collect();
    let mut count: HashMap<&str, usize> = HashMap::new();
    for f in &files {
        *count.entry(basename(f)).or_default() += 1;
    }
    files
        .into_iter()
        .map(|f| {
            (
                f,
                if count[basename(f)] > 1 {
                    f.to_string()
                } else {
                    basename(f).to_string()
                },
            )
        })
        .collect()
}

fn basename(f: &str) -> &str {
    f.rsplit('/').next().unwrap_or(f)
}

enum Item {
    Own(usize),
    Child(usize, usize), // [start, end)
}

struct Cx<'a> {
    t: &'a Trace,
    source: &'a dyn Fn(&str, u32) -> Option<String>,
    base: u32,
    max_depth: u32,
}

const MAX_VARS: usize = 4;

impl Cx<'_> {
    fn render(&self, base: u32) -> Vec<Row> {
        let mut rows = Vec::new();
        self.level(0, self.t.steps.len(), base, &mut rows);
        rows
    }

    fn steps(&self) -> &[Step] {
        &self.t.steps
    }

    /// Own steps at depth `d` (one frame) plus child items starting at depth d+1 calls.
    fn items(&self, a: usize, b: usize, d: u32) -> Vec<Item> {
        let mut items = Vec::new();
        for i in a..b {
            let s = &self.steps()[i];
            if s.depth <= d {
                items.push(Item::Own(i));
            } else if s.depth == d + 1 && (s.kind.is_call() || s.library)
                || !matches!(items.last(), Some(Item::Child(..)))
            {
                items.push(Item::Child(i, i + 1));
            } else if let Some(Item::Child(_, e)) = items.last_mut() {
                *e = i + 1;
            }
        }
        items
    }

    fn level(&self, a: usize, b: usize, d: u32, rows: &mut Vec<Row>) {
        let items = self.items(a, b, d);
        let mut i = 0;
        while i < items.len() {
            if let Some(next) = self.backedge_loop(&items, i, d, rows) {
                i = next;
                continue;
            }
            if d > self.base
                && let Some(next) = self.repeated_children(&items, i, d, rows)
            {
                i = next;
                continue;
            }
            match items[i] {
                Item::Own(s) => rows.push(self.row(s)),
                Item::Child(s, e) => {
                    if d + 1 > self.max_depth && e - s > 1 {
                        rows.push(self.collapsed(s, e));
                    } else {
                        self.level(s, e, d + 1, rows);
                    }
                }
            }
            i += 1;
        }
    }

    fn item_range(&self, items: &[Item], i: usize, j: usize) -> (usize, usize) {
        let start = match items[i] {
            Item::Own(s) | Item::Child(s, _) => s,
        };
        let end = match items[j - 1] {
            Item::Own(s) => s + 1,
            Item::Child(_, e) => e,
        };
        (start, end)
    }

    /// A line H that re-executes with only higher lines (and calls) in between, in one frame.
    fn backedge_loop(
        &self,
        items: &[Item],
        i: usize,
        d: u32,
        rows: &mut Vec<Row>,
    ) -> Option<usize> {
        let Item::Own(h) = items[i] else { return None };
        let head = &self.steps()[h];
        if head.kind != Kind::Line {
            return None;
        }
        let (frame, hl) = (head.frame, head.line?);
        let mut occ = vec![i];
        let mut maxline = hl;
        let mut end = i + 1;
        let mut iters = 0;
        let mut body_since_head = false;
        for (k, it) in items.iter().enumerate().skip(i + 1) {
            match *it {
                Item::Own(s) => {
                    let st = &self.steps()[s];
                    if st.frame != frame || st.kind.is_return() || st.kind.is_call() {
                        break;
                    }
                    if st.kind == Kind::Line {
                        let l = st.line.unwrap_or(0);
                        if l == hl {
                            iters += body_since_head as usize;
                            body_since_head = false;
                            occ.push(k);
                        } else if l > hl && (occ.len() == 1 || l <= maxline) {
                            maxline = maxline.max(l);
                            body_since_head = true;
                        } else {
                            break;
                        }
                    }
                    end = k + 1;
                }
                Item::Child(..) => {
                    body_since_head = true;
                    end = k + 1;
                }
            }
        }
        iters += body_since_head as usize;
        if occ.len() < 2 || iters < 2 {
            return None;
        }
        let (s, e) = self.item_range(items, i, end);
        let vars = self.frame_change(frame, s, e);
        rows.push(Row {
            step: self.steps()[s].id,
            end_step: Some(self.steps()[e - 1].id),
            depth: d,
            kind: "loop",
            file: head.file.clone(),
            line: Some(hl),
            line_end: Some(maxline),
            changed: self.any_changed(s, e),
            text: format!(
                "loop ×{iters}{}",
                self.src(head)
                    .map(|t| format!("  │ {t}"))
                    .unwrap_or_default()
            ),
            vars,
        });
        Some(end)
    }

    /// Consecutive sibling calls of the same function (e.g. Ruby `each` blocks).
    fn repeated_children(
        &self,
        items: &[Item],
        i: usize,
        d: u32,
        rows: &mut Vec<Row>,
    ) -> Option<usize> {
        let key = |it: &Item| match *it {
            Item::Child(s, _) => {
                let st = &self.steps()[s];
                (st.kind.is_call() && !st.library).then(|| (st.function.clone(), st.file.clone()))
            }
            _ => None,
        };
        let k = key(&items[i])?;
        let n = items[i..]
            .iter()
            .take_while(|it| key(it).as_ref() == Some(&k))
            .count();
        if n < 2 {
            return None;
        }
        let (s, e) = self.item_range(items, i, i + n);
        let first = &self.steps()[s];
        let parent = self.steps()[..s]
            .iter()
            .rev()
            .find(|x| x.depth == d)
            .map(|x| x.frame);
        let lines: Vec<u32> = self.steps()[s..e]
            .iter()
            .filter(|x| x.frame == first.frame)
            .filter_map(|x| x.line)
            .collect();
        rows.push(Row {
            step: first.id,
            end_step: Some(self.steps()[e - 1].id),
            depth: d + 1,
            kind: "loop",
            file: first.file.clone(),
            line: lines.iter().min().copied(),
            line_end: lines.iter().max().copied(),
            changed: self.any_changed(s, e),
            text: format!("loop ×{n} {}", first.function.as_deref().unwrap_or("?")),
            vars: parent
                .map(|f| self.frame_change(f, s, e))
                .unwrap_or_default(),
        });
        Some(i + n)
    }

    fn any_changed(&self, s: usize, e: usize) -> bool {
        self.steps()[s..e]
            .iter()
            .any(|x| matches!((&x.file, x.line), (Some(f), Some(l)) if self.t.is_changed(f, l)))
    }

    /// How frame `f`'s variables differ from just before [s, e) to just after it.
    fn frame_change(&self, f: u32, s: usize, e: usize) -> String {
        let steps = self.steps();
        let before = steps[..s]
            .iter()
            .rev()
            .find(|x| x.frame == f && x.locals.is_some());
        let after = steps[e..]
            .iter()
            .find(|x| x.frame == f && x.locals.is_some())
            .or_else(|| {
                steps[s..e]
                    .iter()
                    .rev()
                    .find(|x| x.frame == f && x.locals.is_some())
            });
        let (Some(b), Some(a)) = (
            before.and_then(|x| x.locals.as_ref()),
            after.and_then(|x| x.locals.as_ref()),
        ) else {
            return String::new();
        };
        let parts: Vec<String> = a
            .iter()
            .filter_map(|(k, v)| {
                let old = b.get(k)?;
                (repr_of(old) != repr_of(v)).then(|| {
                    format!(
                        "{k}: {} → {}",
                        short(repr_of(old), 30),
                        short(repr_of(v), 30)
                    )
                })
            })
            .collect();
        cap(parts)
    }

    fn src(&self, s: &Step) -> Option<String> {
        let line = (self.source)(s.file.as_deref()?, s.line?)?;
        Some(short(line.trim(), 60))
    }

    fn collapsed(&self, s: usize, e: usize) -> Row {
        let first = &self.steps()[s];
        let last = &self.steps()[e - 1];
        let ret = self.steps()[s..e]
            .iter()
            .rev()
            .find(|x| x.frame == first.frame && x.kind.is_return())
            .and_then(|x| x.return_value.as_ref())
            .map(|v| format!(" returned {}", short(&v.repr, 40)))
            .unwrap_or_default();
        Row {
            step: first.id,
            end_step: Some(last.id),
            depth: first.depth,
            kind: "collapsed",
            file: first.file.clone(),
            line: first.line,
            line_end: None,
            changed: self.any_changed(s, e),
            text: format!(
                "→ {} … ({} steps){ret}",
                first.function.as_deref().unwrap_or("?"),
                e - s
            ),
            vars: String::new(),
        }
    }

    fn row(&self, i: usize) -> Row {
        let s = &self.steps()[i];
        let func = s.function.as_deref().unwrap_or("?");
        let (kind, text, vars) = match s.kind {
            Kind::Call if s.library => {
                let ret = s
                    .return_value
                    .as_ref()
                    .map(|v| format!(" returned {}", short(&v.repr, 50)))
                    .unwrap_or_default();
                (
                    "library_call",
                    format!("→ {func} (library){ret}"),
                    String::new(),
                )
            }
            Kind::Call => ("call", format!("→ {func}"), args(s.locals.as_ref(), 3)),
            Kind::BlockCall => (
                "block_call",
                format!("→ {func}"),
                args(s.locals.as_ref(), 2),
            ),
            Kind::Line => (
                "line",
                self.src(s)
                    .map(|t| format!("│ {t}"))
                    .unwrap_or_else(|| "│".into()),
                diff_text(s),
            ),
            Kind::Return | Kind::BlockReturn => {
                let v = s
                    .return_value
                    .as_ref()
                    .map(|v| short(&v.repr, 60))
                    .unwrap_or_else(|| "?".into());
                ("return", format!("← {func} returned {v}"), String::new())
            }
            Kind::Raise => {
                let e = s.exception.as_ref();
                (
                    "raise",
                    format!(
                        "!! raise {}: {}",
                        e.map(|e| e.ty.as_str()).unwrap_or("?"),
                        short(e.map(|e| e.message.as_str()).unwrap_or(""), 80)
                    ),
                    String::new(),
                )
            }
            Kind::SideEffect => (
                "side_effect",
                side_effect_text(s.side_effect.as_ref()),
                String::new(),
            ),
        };
        Row {
            step: s.id,
            end_step: None,
            depth: s.depth,
            kind,
            file: s.file.clone(),
            line: s.line,
            line_end: None,
            changed: matches!((&s.file, s.line), (Some(f), Some(l)) if self.t.is_changed(f, l))
                && !s.library,
            text,
            vars,
        }
    }
}

fn side_effect_text(v: Option<&serde_json::Value>) -> String {
    let Some(v) = v else {
        return "side effect".into();
    };
    match v.get("kind").and_then(|k| k.as_str()) {
        Some("sql") => {
            let sql = v.get("sql").and_then(|s| s.as_str()).unwrap_or("");
            let ms = v
                .get("duration_ms")
                .and_then(|d| d.as_f64())
                .map(|d| format!("  ({d:.1}ms)"))
                .unwrap_or_default();
            format!(
                "SQL  {}{ms}",
                short(&sql.split_whitespace().collect::<Vec<_>>().join(" "), 100)
            )
        }
        Some(k) => format!("{k}  {}", short(&v.to_string(), 100)),
        None => short(&v.to_string(), 100),
    }
}

fn args(locals: Option<&Locals>, n: usize) -> String {
    let Some(l) = locals else {
        return String::new();
    };
    cap(l
        .iter()
        .filter(|(k, v)| !k.starts_with('@') && !matches!(repr_of(v), "nil" | "None" | "undefined"))
        .take(n)
        .map(|(k, v)| format!("{k}={}", short(repr_of(v), 40)))
        .collect())
}

fn diff_text(s: &Step) -> String {
    let (Some(d), Some(l)) = (&s.diff, &s.locals) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for k in &d.added {
        parts.push(format!(
            "{k}: (new) {}",
            short(l.get(k).map(repr_of).unwrap_or("?"), 40)
        ));
    }
    for (k, before) in &d.changed {
        let m = if d.mutated.contains(k) {
            " (mutated)"
        } else {
            ""
        };
        parts.push(format!(
            "{k}: {} → {}{m}",
            short(before.as_str().unwrap_or("?"), 30),
            short(l.get(k).map(repr_of).unwrap_or("?"), 30)
        ));
    }
    cap(parts)
}

fn cap(mut parts: Vec<String>) -> String {
    if parts.len() > MAX_VARS {
        let more = parts.len() - MAX_VARS;
        parts.truncate(MAX_VARS);
        parts.push(format!("…+{more}"));
    }
    parts.join(", ")
}

pub fn short(s: &str, n: usize) -> String {
    let one: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if one.chars().count() > n {
        one.chars().take(n).collect::<String>() + "…"
    } else {
        one
    }
}
