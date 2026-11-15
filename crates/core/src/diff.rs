//! Line diffs for `git diff` and the Changes view.

use similar::TextDiff;

/// Above this many lines on either side the diff is not computed, because the
/// worst case is quadratic and this runs on a phone's main thread.
pub const MAX_DIFF_LINES: usize = 20_000;

/// A unified diff of `old` against `new`, with git-style `a/` and `b/` headers.
/// Returns a one-line note instead when the inputs are too large to diff.
#[must_use]
pub fn unified(path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let longest = old.lines().count().max(new.lines().count());
    if longest > MAX_DIFF_LINES {
        return format!(
            "{path}: changed (not shown: {longest} lines is over the {MAX_DIFF_LINES}-line diff limit)\n"
        );
    }
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

/// How many lines were added and removed.
#[must_use]
pub fn stat(old: &str, new: &str) -> (usize, usize) {
    if old.lines().count().max(new.lines().count()) > MAX_DIFF_LINES {
        return (0, 0);
    }
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_changed_lines_with_headers() {
        let text = unified("a.py", "one\ntwo\nthree\n", "one\n2\nthree\n");
        assert!(text.contains("--- a/a.py"), "{text}");
        assert!(text.contains("+++ b/a.py"), "{text}");
        assert!(text.contains("-two"), "{text}");
        assert!(text.contains("+2"), "{text}");
    }

    #[test]
    fn identical_is_empty() {
        assert_eq!(unified("a", "x\n", "x\n"), "");
    }

    #[test]
    fn counts() {
        assert_eq!(stat("a\nb\n", "a\nc\nd\n"), (2, 1));
    }

    #[test]
    fn oversized_is_reported_not_computed() {
        let big = "x\n".repeat(MAX_DIFF_LINES + 1);
        let text = unified("big.txt", &big, "y\n");
        assert!(text.contains("not shown"), "{text}");
    }
}
