//! A small line diff for previews (`--dry-run`, stale context files).
//!
//! Longest-common-subsequence over lines, rendered as a unified diff with
//! three lines of context. Generated files are small, so the quadratic
//! table is fine; inputs over [`MAX_LINES`] fall back to "replace all".

/// Above this many lines on either side, the diff is a full replacement.
const MAX_LINES: usize = 4_000;

/// Lines of context around each change.
const CONTEXT: usize = 3;

type Lines<'a, 'b> = &'a [&'b str];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Same,
    Del,
    Add,
}

/// A unified diff from `old` to `new`, labelled with `path`; empty when
/// the texts are equal.
pub(crate) fn unified(path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = script(&a, &b);
    let mut out = format!("--- {path}\n+++ {path}\n");
    for hunk in hunks(&ops) {
        render(&mut out, &ops[hunk.0..hunk.1], &a, &b, &ops[..hunk.0]);
    }
    out
}

/// The edit script, one op per line of either side.
fn script(a: &[&str], b: &[&str]) -> Vec<Op> {
    if a.len() > MAX_LINES || b.len() > MAX_LINES {
        let mut ops = vec![Op::Del; a.len()];
        ops.extend(std::iter::repeat_n(Op::Add, b.len()));
        return ops;
    }
    // lcs[i][j]: common lines of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut ops = Vec::with_capacity(a.len() + b.len());
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            ops.push(Op::Same);
            (i, j) = (i + 1, j + 1);
        } else if j < b.len() && (i == a.len() || add_first(&lcs, i, j)) {
            ops.push(Op::Add);
            j += 1;
        } else {
            ops.push(Op::Del);
            i += 1;
        }
    }
    ops
}

/// Whether adding `b[j]` keeps more common lines than deleting `a[i]`.
/// Ties delete first, so removals read before additions.
fn add_first(lcs: &[Vec<u32>], i: usize, j: usize) -> bool {
    lcs[i][j + 1] > lcs[i + 1][j]
}

/// `[start, end)` ranges of ops forming hunks with context.
fn hunks(ops: &[Op]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (k, op) in ops.iter().enumerate() {
        if *op == Op::Same {
            continue;
        }
        let start = k.saturating_sub(CONTEXT);
        let end = (k + 1 + CONTEXT).min(ops.len());
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = end,
            _ => out.push((start, end)),
        }
    }
    out
}

fn render(out: &mut String, ops: &[Op], a: Lines, b: Lines, before: &[Op]) {
    let mut i = before.iter().filter(|o| **o != Op::Add).count();
    let mut j = before.iter().filter(|o| **o != Op::Del).count();
    let old_len = ops.iter().filter(|o| **o != Op::Add).count();
    let new_len = ops.iter().filter(|o| **o != Op::Del).count();
    // An empty side starts at the line before it (`-0,0`).
    let old_start = if old_len == 0 { i } else { i + 1 };
    let new_start = if new_len == 0 { j } else { j + 1 };
    let old = format!("-{old_start},{old_len}");
    let new = format!("+{new_start},{new_len}");
    out.push_str(&format!("@@ {old} {new} @@\n"));
    for op in ops {
        match op {
            Op::Same => {
                out.push_str(&format!(" {}\n", a[i]));
                (i, j) = (i + 1, j + 1);
            }
            Op::Del => {
                out.push_str(&format!("-{}\n", a[i]));
                i += 1;
            }
            Op::Add => {
                out.push_str(&format!("+{}\n", b[j]));
                j += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::unified;

    #[test]
    fn equal_texts_have_no_diff() {
        assert_eq!(unified("f", "a\nb\n", "a\nb\n"), "");
    }

    #[test]
    fn changes_are_hunks_with_context() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let new = "1\n2\n3\n4\nfive\n6\n7\n8\n9\nten\n";
        let diff = unified("x.md", old, new);
        assert_eq!(
            diff,
            "--- x.md\n+++ x.md\n\
             @@ -2,8 +2,9 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8\n 9\n+ten\n"
        );
    }

    #[test]
    fn creation_from_empty() {
        let diff = unified("new.rs", "", "a\nb\n");
        assert_eq!(diff, "--- new.rs\n+++ new.rs\n@@ -0,0 +1,2 @@\n+a\n+b\n");
    }
}
