//! Align a base-ref trace with a HEAD trace of the same example and find where behaviour diverges.

use crate::trace::{Kind, Step, Trace, repr_of};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How far ahead to look for a resync point after a mismatch. Keeps alignment linear-ish.
const LOOKAHEAD: usize = 400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Divergence {
    /// Index into `alignment`.
    pub at: usize,
    pub base_step: Option<u32>,
    pub head_step: Option<u32>,
    /// "control" (different code ran) or "data" (same code, different values)
    pub kind: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comparison {
    pub base_ref: String,
    pub base_trace: String,
    /// Aligned (base step id, head step id) pairs; one side is null where the other has extra steps.
    pub alignment: Vec<(Option<u32>, Option<u32>)>,
    pub divergence: Option<Divergence>,
}

/// What a step "is", independent of line numbers (which shift between versions): kind, function,
/// and the source text of its line.
pub fn signature(s: &Step, source: &dyn Fn(&str, u32) -> Option<String>) -> String {
    let text = match (&s.file, s.line) {
        (Some(f), Some(l)) => source(f, l)
            .map(|t| t.trim().to_string())
            .unwrap_or_else(|| format!("{f}:{l}")),
        _ => String::new(),
    };
    let lib = if s.library { "lib:" } else { "" };
    format!(
        "{lib}{:?}|{}|{}",
        s.kind,
        s.function.as_deref().unwrap_or(""),
        text
    )
}

/// Greedy alignment: advance together while signatures match; on a mismatch, take the nearest
/// resync point (within LOOKAHEAD) and emit the skipped steps as one-sided pairs.
pub fn align(base: &[String], head: &[String]) -> Vec<(Option<usize>, Option<usize>)> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(base.len().max(head.len()));
    while i < base.len() && j < head.len() {
        if base[i] == head[j] {
            out.push((Some(i), Some(j)));
            i += 1;
            j += 1;
            continue;
        }
        // nearest (di, dj) with base[i+di] == head[j+dj], minimizing di+dj
        let mut index: HashMap<&str, usize> = HashMap::new();
        for (dj, h) in head[j..(j + LOOKAHEAD).min(head.len())].iter().enumerate() {
            index.entry(h.as_str()).or_insert(dj);
        }
        let mut best: Option<(usize, usize)> = None;
        for (di, b) in base[i..(i + LOOKAHEAD).min(base.len())].iter().enumerate() {
            if let Some(&dj) = index.get(b.as_str())
                && best.is_none_or(|(bi, bj)| di + dj < bi + bj)
            {
                best = Some((di, dj));
            }
            if best.is_some_and(|(bi, bj)| di >= bi + bj) {
                break;
            }
        }
        match best {
            Some((di, dj)) => {
                out.extend((i..i + di).map(|x| (Some(x), None)));
                out.extend((j..j + dj).map(|y| (None, Some(y))));
                i += di;
                j += dj;
            }
            None => {
                out.push((Some(i), None));
                out.push((None, Some(j)));
                i += 1;
                j += 1;
            }
        }
    }
    out.extend((i..base.len()).map(|x| (Some(x), None)));
    out.extend((j..head.len()).map(|y| (None, Some(y))));
    out
}

/// Values that differ between any two runs (timestamps, memory addresses) must not count as divergence.
pub fn scrub(repr: &str) -> String {
    let b = repr.as_bytes();
    let mut out = String::with_capacity(repr.len());
    let mut i = 0;
    let digits = |from: usize, n: usize| {
        from + n <= b.len() && b[from..from + n].iter().all(u8::is_ascii_digit)
    };
    while i < b.len() {
        // 0x1234abcd...
        if b[i] == b'0' && i + 1 < b.len() && b[i + 1] == b'x' {
            let n = b[i + 2..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            if n >= 6 {
                out += "0x…";
                i += 2 + n;
                continue;
            }
        }
        // YYYY-MM-DD[ T]HH:MM:SS[.frac]
        if digits(i, 4)
            && i + 19 <= b.len()
            && b[i + 4] == b'-'
            && digits(i + 5, 2)
            && b[i + 7] == b'-'
            && digits(i + 8, 2)
            && (b[i + 10] == b' ' || b[i + 10] == b'T')
            && digits(i + 11, 2)
            && b[i + 13] == b':'
        {
            let mut j = i + 19;
            if j < b.len() && b[j] == b'.' {
                j += 1 + b[j + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            }
            out += "<time>";
            i = j;
            continue;
        }
        let ch = repr[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn values(s: &Step) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = s
        .locals
        .iter()
        .flatten()
        .map(|(k, x)| (k.clone(), scrub(repr_of(x))))
        .collect();
    if let Some(r) = &s.return_value {
        v.push(("return".into(), scrub(&r.repr)));
    }
    v
}

fn describe(s: &Step) -> String {
    let at = match (&s.file, s.line) {
        (Some(f), Some(l)) => format!("{f}:{l}"),
        _ => "?".into(),
    };
    match s.kind {
        Kind::Call | Kind::BlockCall => {
            format!("call {} at {at}", s.function.as_deref().unwrap_or("?"))
        }
        Kind::Return | Kind::BlockReturn => format!(
            "return from {} at {at}",
            s.function.as_deref().unwrap_or("?")
        ),
        Kind::SideEffect => format!("side effect at {at}"),
        Kind::Raise => format!("exception at {at}"),
        Kind::Line => format!("line {at}"),
    }
}

pub fn compare(
    base: &Trace,
    head: &Trace,
    base_ref: &str,
    base_src: &dyn Fn(&str, u32) -> Option<String>,
    head_src: &dyn Fn(&str, u32) -> Option<String>,
) -> Comparison {
    let bs: Vec<String> = base.steps.iter().map(|s| signature(s, base_src)).collect();
    let hs: Vec<String> = head.steps.iter().map(|s| signature(s, head_src)).collect();
    let pairs = align(&bs, &hs);
    let mut divergence = None;
    for (at, &(b, h)) in pairs.iter().enumerate() {
        match (b, h) {
            (Some(b), Some(h)) => {
                let (sb, sh) = (&base.steps[b], &head.steps[h]);
                let (vb, vh) = (values(sb), values(sh));
                // Only names both sides have: a variable that exists on one side is a code change,
                // which shows up as a control divergence on the next differing step.
                let changed = vh.iter().find_map(|(k, after)| {
                    let before = vb.iter().find(|(kb, _)| kb == k).map(|(_, v)| v)?;
                    (before != after).then_some((k, before, after))
                });
                if let Some((name, before, after)) = changed {
                    divergence = Some(Divergence {
                        at,
                        base_step: Some(sb.id),
                        head_step: Some(sh.id),
                        kind: "data".into(),
                        description: format!(
                            "same code, different value at {}: {name} is {before} on base, {after} on HEAD",
                            describe(sh)
                        ),
                    });
                    break;
                }
            }
            _ => {
                let s = b
                    .map(|i| &base.steps[i])
                    .or(h.map(|i| &head.steps[i]))
                    .unwrap();
                let side = if b.is_some() {
                    "only on base"
                } else {
                    "only on HEAD"
                };
                divergence = Some(Divergence {
                    at,
                    base_step: b.map(|i| base.steps[i].id),
                    head_step: h.map(|i| head.steps[i].id),
                    kind: "control".into(),
                    description: format!("different code ran: {} ({side})", describe(s)),
                });
                break;
            }
        }
    }
    Comparison {
        base_ref: base_ref.to_string(),
        base_trace: base.name.clone(),
        alignment: pairs
            .into_iter()
            .map(|(b, h)| (b.map(|i| i as u32 + 1), h.map(|i| i as u32 + 1)))
            .collect(),
        divergence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn scrubs_volatile_values() {
        assert_eq!(
            scrub("#<Order created_at: 2026-11-05 14:21:19.047875000 UTC>"),
            "#<Order created_at: <time> UTC>"
        );
        assert_eq!(
            scrub("<pricing.Pricer object at 0x10a125fd0> 0x12"),
            "<pricing.Pricer object at 0x…> 0x12"
        );
    }

    #[test]
    fn aligns_insertions_and_deletions() {
        let p = align(&v(&["a", "b", "c", "d"]), &v(&["a", "x", "b", "d", "e"]));
        assert_eq!(
            p,
            vec![
                (Some(0), Some(0)),
                (None, Some(1)),
                (Some(1), Some(2)),
                (Some(2), None),
                (Some(3), Some(3)),
                (None, Some(4))
            ]
        );
    }

    #[test]
    fn finds_control_and_data_divergence() {
        use crate::ingest::{RunInfo, ingest};
        let mk = |second: &str, val: &str| {
            let l = format!(
                "{{\"event\":\"call\",\"file\":\"a.rb\",\"line\":1,\"function\":\"f\",\"frame_id\":1,\"depth\":1,\"locals\":{{\"x\":{{\"type\":\"I\",\"repr\":\"{val}\"}}}},\"seq\":1,\"t\":1,\"pid\":1}}\n{{\"event\":\"line\",\"file\":\"a.rb\",\"line\":{second},\"function\":\"f\",\"frame_id\":1,\"depth\":1,\"seq\":2,\"t\":2,\"pid\":1}}"
            );
            ingest([l.as_str()], RunInfo::default()).0
        };
        let src = |_: &str, l: u32| Some(format!("line {l}"));
        let c = compare(&mk("2", "1"), &mk("3", "1"), "main", &src, &src);
        assert_eq!(c.divergence.unwrap().kind, "control");
        let c = compare(&mk("2", "1"), &mk("2", "5"), "main", &src, &src);
        let d = c.divergence.unwrap();
        assert_eq!((d.kind.as_str(), d.head_step), ("data", Some(1)));
        assert!(
            d.description.contains("x is 1 on base, 5 on HEAD"),
            "{}",
            d.description
        );
    }
}
