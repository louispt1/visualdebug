//! Changed files and line ranges from `git diff --unified=0` output.

use std::collections::BTreeMap;

/// Parse unified diff text. `subdir` is the project root relative to the git root ("" if equal);
/// files outside it are dropped and paths are returned project-relative.
pub fn parse_unified_diff(text: &str, subdir: &str) -> BTreeMap<String, Vec<(u32, u32)>> {
    let prefix = if subdir.is_empty() { String::new() } else { format!("{}/", subdir.trim_end_matches('/')) };
    let mut out: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            current = path
                .strip_prefix("b/")
                .and_then(|p| p.strip_prefix(prefix.as_str()))
                .map(|p| p.trim_end().to_string());
            if let Some(f) = &current {
                out.entry(f.clone()).or_default();
            }
        } else if let (Some(f), Some(hunk)) = (&current, line.strip_prefix("@@ ")) {
            // @@ -a,b +c,d @@
            let Some(new) = hunk.split_whitespace().find(|s| s.starts_with('+')) else { continue };
            let mut it = new[1..].split(',');
            let start: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let len: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            // A pure deletion (len 0) still marks the line where code disappeared.
            let (a, b) = if len == 0 { (start.max(1), start.max(1)) } else { (start, start + len - 1) };
            let ranges = out.get_mut(f).unwrap();
            match ranges.last_mut() {
                Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
                _ => ranges.push((a, b)),
            }
        }
    }
    out
}

/// Render ranges like `L18-40, L52`.
pub fn fmt_ranges(r: &[(u32, u32)]) -> String {
    r.iter()
        .map(|&(a, b)| if a == b { format!("L{a}") } else { format!("L{a}-{b}") })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunks_subdir_and_deletions() {
        let diff = "\
diff --git a/app/x.rb b/app/x.rb
--- a/app/x.rb
+++ b/app/x.rb
@@ -10,0 +11,3 @@ def x
@@ -20 +23 @@
@@ -30,2 +33,0 @@
diff --git a/gone.rb b/gone.rb
+++ /dev/null
@@ -1,3 +0,0 @@
diff --git a/other/y.rb b/other/y.rb
+++ b/other/y.rb
@@ -1 +1,2 @@
";
        let all = parse_unified_diff(diff, "");
        assert_eq!(all["app/x.rb"], vec![(11, 13), (23, 23), (33, 33)]);
        assert!(!all.contains_key("gone.rb"));
        let sub = parse_unified_diff(diff, "app");
        assert_eq!(sub.keys().collect::<Vec<_>>(), vec!["x.rb"]);
        assert_eq!(fmt_ranges(&all["app/x.rb"]), "L11-13, L23, L33");
    }
}
