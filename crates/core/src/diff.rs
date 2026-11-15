//! Line diffs for `git diff` and the Changes view.

use similar::TextDiff;

/// Above this many lines on either side the diff is not computed, because the
/// worst case is quadratic and this runs on a phone's main thread.
pub const MAX_DIFF_LINES: usize = 20_000;

/// A unified diff of `old` against `new`, with git-style `a/` and `b/` headers.
/// Returns a one-line note instead when the inputs are too large to diff.
#[must_use]
pub fn unified(path: &str, old: &str, new: &str) -> String {
    between(path, Some(old), Some(new))
}

/// Like [`unified`], but `None` means the file does not exist on that side.
/// The result carries git's own headers (`diff --git`, `new file mode`,
/// `/dev/null`), so `git apply` accepts it, including for empty files.
#[must_use]
pub fn between(path: &str, old: Option<&str>, new: Option<&str>) -> String {
    let mut out = format!("diff --git a/{path} b/{path}\n");
    let old_name = if old.is_some() {
        format!("a/{path}")
    } else {
        out.push_str("new file mode 100644\n");
        "/dev/null".to_owned()
    };
    let new_name = if new.is_some() {
        format!("b/{path}")
    } else {
        out.push_str("deleted file mode 100644\n");
        "/dev/null".to_owned()
    };
    let exists_on_both_sides = old.is_some() && new.is_some();
    let (old, new) = (old.unwrap_or(""), new.unwrap_or(""));
    if old == new {
        // Unchanged text: nothing to show, unless the file itself came or went
        // (an empty file created or deleted), which the headers alone express.
        return if exists_on_both_sides {
            String::new()
        } else {
            out
        };
    }
    let longest = old.lines().count().max(new.lines().count());
    if longest > MAX_DIFF_LINES {
        return format!(
            "{path}: changed (not shown: {longest} lines is over the {MAX_DIFF_LINES}-line diff limit)\n"
        );
    }
    out.push_str(
        &TextDiff::from_lines(old, new)
            .unified_diff()
            .context_radius(3)
            .header(&old_name, &new_name)
            .to_string(),
    );
    out
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
    fn new_and_deleted_files_use_dev_null() {
        let added = between("n.txt", None, Some("x\n"));
        assert!(
            added.starts_with(
                "diff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n"
            ),
            "{added}"
        );
        let removed = between("n.txt", Some("x\n"), None);
        assert!(
            removed.contains("deleted file mode 100644\n--- a/n.txt\n+++ /dev/null\n"),
            "{removed}"
        );
        assert_eq!(
            between("e.txt", None, Some("")),
            "diff --git a/e.txt b/e.txt\nnew file mode 100644\n"
        );
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
