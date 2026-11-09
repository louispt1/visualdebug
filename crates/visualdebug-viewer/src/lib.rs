//! Bundles traces into one self-contained HTML file: assets, trace data and pre-highlighted
//! source are inlined; the page makes no network requests.

use anyhow::Result;
use serde_json::{Value as J, json};
use std::collections::{BTreeMap, BTreeSet};
use syntect::html::{ClassStyle, ClassedHTMLGenerator, css_for_theme_with_class_style};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use visualdebug_core::Trace;

const INDEX: &str = include_str!("../assets/index.html");
const CSS: &str = include_str!("../assets/viewer.css");
const JS: &str = include_str!("../assets/viewer.js");
const STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "s-" };

pub struct Highlighter {
    syntaxes: SyntaxSet,
}

impl Default for Highlighter {
    fn default() -> Self {
        Highlighter {
            syntaxes: two_face::syntax::extra_newlines(),
        }
    }
}

impl Highlighter {
    /// One self-contained HTML fragment per source line (spans closed and reopened at line ends).
    pub fn lines(&self, file: &str, text: &str) -> Vec<String> {
        let ext = file.rsplit('.').next().unwrap_or("");
        let ext = match ext {
            "cjs" | "mjs" => "js",
            "mts" | "cts" => "ts",
            "rake" | "ru" | "gemspec" => "rb",
            e => e,
        };
        let syntax = self
            .syntaxes
            .find_syntax_by_extension(ext)
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());
        let mut generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, &self.syntaxes, STYLE);
        for line in LinesWithEndings::from(text) {
            if generator
                .parse_html_for_line_which_includes_newline(line)
                .is_err()
            {
                return text.lines().map(escape).collect();
            }
        }
        split_lines(&generator.finalize())
    }
}

/// Split highlighted HTML at newlines, closing open spans at each line end and reopening them.
fn split_lines(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open: Vec<&str> = Vec::new();
    let mut cur = String::new();
    let mut has_text = false;
    let mut rest = html;
    while !rest.is_empty() {
        if rest.starts_with("<span") {
            let end = rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
            open.push(&rest[..end]);
            cur += &rest[..end];
            rest = &rest[end..];
        } else if rest.starts_with("</span>") {
            open.pop();
            cur += "</span>";
            rest = &rest[7..];
        } else if rest.starts_with('\n') {
            cur += &"</span>".repeat(open.len());
            out.push(std::mem::take(&mut cur));
            cur.extend(open.iter().copied());
            has_text = false;
            rest = &rest[1..];
        } else {
            let end = rest.find(['<', '\n']).unwrap_or(rest.len());
            cur += &rest[..end];
            has_text |= end > 0;
            rest = &rest[end..];
        }
    }
    if has_text {
        cur += &"</span>".repeat(open.len());
        out.push(cur);
    }
    out
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn theme_css() -> String {
    let themes = two_face::theme::extra();
    let light = css_for_theme_with_class_style(
        themes.get(two_face::theme::EmbeddedThemeName::Github),
        STYLE,
    )
    .unwrap_or_default();
    let dark = css_for_theme_with_class_style(
        themes.get(two_face::theme::EmbeddedThemeName::OneHalfDark),
        STYLE,
    )
    .unwrap_or_default();
    format!("{light}\n@media (prefers-color-scheme: dark) {{\n{dark}\n}}\n")
}

/// Build the HTML. `source(rel_path)` returns a project file's text.
pub fn bundle(traces: &[Trace], source: &dyn Fn(&Trace, &str) -> Option<String>) -> Result<String> {
    let hl = Highlighter::default();
    let mut out_traces = Vec::new();
    for t in traces {
        let files: BTreeSet<&str> = t
            .steps
            .iter()
            .filter_map(|s| s.file.as_deref())
            .chain(t.focus.files.keys().map(String::as_str))
            .collect();
        let mut file_data: BTreeMap<&str, J> = BTreeMap::new();
        for f in files {
            let Some(text) = source(t, f) else { continue };
            let changed = t.focus.files.get(f).cloned().unwrap_or_default();
            file_data.insert(
                f,
                json!({ "lines": hl.lines(f, &text), "changed": changed }),
            );
        }
        out_traces.push(json!({
            "name": t.name,
            "language": t.language,
            "command": t.command,
            "exit_code": t.exit_code,
            "truncated": t.truncated,
            "start_at": t.start_at,
            "focus": t.focus,
            "steps": t.steps,
            "files": file_data,
            "annotations": t.annotations,
            "compare": t.compare,
        }));
    }
    let data = json!({ "version": env!("CARGO_PKG_VERSION"), "traces": out_traces });
    // Inline as JSON inside a script tag: "</" must not appear literally.
    let data = serde_json::to_string(&data)?
        .replace("</", "<\\/")
        .replace("<!--", "<\\!--");
    let title = traces
        .iter()
        .map(|t| t.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let title = escape(&format!("visualdebug: {title}"));
    let css = format!("{CSS}\n{}", theme_css());
    Ok(fill(
        INDEX,
        &[
            ("{{TITLE}}", &title),
            ("{{CSS}}", &css),
            ("{{DATA}}", &data),
            ("{{JS}}", JS),
        ],
    ))
}

/// Single-pass placeholder substitution (inserted text is never re-scanned).
fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out =
        String::with_capacity(template.len() + vars.iter().map(|v| v.1.len()).sum::<usize>());
    let mut rest = template;
    while let Some((pos, (k, v))) = vars
        .iter()
        .filter_map(|kv| rest.find(kv.0).map(|p| (p, kv)))
        .min_by_key(|x| x.0)
    {
        out += &rest[..pos];
        out += v;
        rest = &rest[pos + k.len()..];
    }
    out + rest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_spans_balanced_per_line() {
        let lines = split_lines("<span class=\"a\">x\n<span class=\"b\">y</span>\nz</span>");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "<span class=\"a\">x</span>");
        assert_eq!(
            lines[1],
            "<span class=\"a\"><span class=\"b\">y</span></span>"
        );
        assert_eq!(lines[2], "<span class=\"a\">z</span>");
    }

    #[test]
    fn highlights_supported_languages() {
        let hl = Highlighter::default();
        for (f, src) in [
            ("a.rb", "def x\n  1\nend\n"),
            ("a.py", "def x():\n    return 1\n"),
            ("a.ts", "const x: number = 1;\n"),
            ("a.tsx", "const x = <div/>;\n"),
        ] {
            let lines = hl.lines(f, src);
            assert_eq!(lines.len(), src.lines().count(), "{f}");
            assert!(
                lines[0].contains("class=\"s-"),
                "{f} not highlighted: {}",
                lines[0]
            );
        }
    }
}
