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

/// The first line of a place where a merge could not choose between the two
/// sides. The lines down to [`MARK_MIDDLE`] are from this device, and from
/// there to [`MARK_END`] from GitHub.
pub const MARK_START: &str = "<<<<<<< mine";
pub const MARK_MIDDLE: &str = "=======";
pub const MARK_END: &str = ">>>>>>> GitHub";

/// Two sets of changes to the same text, combined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub text: String,
    /// Places where both sides changed the same lines differently. Each is
    /// left in the text between marker lines, with both versions.
    pub conflicts: usize,
}

/// One side's change to the base: lines `start..end` of the base become `lines`.
struct Hunk<'a> {
    start: usize,
    end: usize,
    lines: Vec<&'a str>,
    /// The part of the base this change could be said to be in. Among
    /// repeated lines a change has more than one equally good description:
    /// with two identical lines, "the first was removed" and "the second was
    /// removed" give the same text. `reach` covers every such description.
    reach: (usize, usize),
}

impl Hunk<'_> {
    /// Works out the reach, given the base and how far the neighbouring
    /// changes on the same side let this one extend.
    ///
    /// An unchanged line next to the change that is identical to a line the
    /// change removed or added may itself be the one that was removed or
    /// added, with the other one kept: the text is the same either way. So
    /// the reach runs on, up and down, over every such line.
    fn find_reach(&mut self, base: &[&str], floor: usize, ceiling: usize) {
        let touched =
            |line: &str| base[self.start..self.end].contains(&line) || self.lines.contains(&line);
        let (mut up, mut down) = (self.start, self.end);
        while down < ceiling && touched(base[down]) {
            down += 1;
        }
        while up > floor && touched(base[up - 1]) {
            up -= 1;
        }
        self.reach = (up, down);
    }
}

/// One side's changes to the base, in order, each with its reach. Changes
/// whose reaches touch are joined into one, because together they may reach
/// further than either does alone.
fn hunks<'a>(diff: &'a TextDiff<'a, 'a, 'a, str>) -> Vec<Hunk<'a>> {
    let base = diff.old_slices();
    let changed = diff.new_slices();
    let mut hunks: Vec<Hunk<'a>> = diff
        .ops()
        .iter()
        .filter_map(|op| {
            let (tag, base, other) = op.as_tag_tuple();
            (tag != similar::DiffTag::Equal).then(|| Hunk {
                start: base.start,
                end: base.end,
                lines: changed[other].to_vec(),
                reach: (base.start, base.end),
            })
        })
        .collect();
    loop {
        for index in 0..hunks.len() {
            let floor = index.checked_sub(1).map_or(0, |before| hunks[before].end);
            let ceiling = hunks.get(index + 1).map_or(base.len(), |after| after.start);
            hunks[index].find_reach(base, floor, ceiling);
        }
        let Some(first) =
            (1..hunks.len()).find(|&index| hunks[index - 1].reach.1 >= hunks[index].reach.0)
        else {
            return hunks;
        };
        let second = hunks.remove(first);
        let joined = &mut hunks[first - 1];
        joined
            .lines
            .extend_from_slice(&base[joined.end..second.start]);
        joined.lines.extend(second.lines);
        joined.end = second.end;
    }
}

/// What `start..end` of the base looks like on one side, given that side's
/// changes inside that range.
fn side<'a>(base: &[&'a str], changes: &[Hunk<'a>], start: usize, end: usize) -> Vec<&'a str> {
    let mut lines = Vec::new();
    let mut at = start;
    for change in changes {
        lines.extend_from_slice(&base[at..change.start]);
        lines.extend_from_slice(&change.lines);
        at = change.end;
    }
    lines.extend_from_slice(&base[at..end]);
    lines
}

/// Combines two sets of changes to `base`: `mine` (this device) and `theirs`
/// (GitHub). Changes to different lines are both kept. Where both changed the
/// same lines, or lines next to each other, and not in the same way, both
/// versions are left in the text between marker lines and counted. Among
/// repeated lines, where it is not certain that two changes are apart, they
/// are treated as touching: the person is asked before anything is guessed.
///
/// Returns `None` if the texts are too long to compare here.
#[must_use]
pub fn merge(base: &str, mine: &str, theirs: &str) -> Option<Merged> {
    let clean = |text: &str| Merged {
        text: text.to_owned(),
        conflicts: 0,
    };
    if mine == theirs || theirs == base {
        return Some(clean(mine));
    }
    if mine == base {
        return Some(clean(theirs));
    }
    let longest = [base, mine, theirs]
        .iter()
        .map(|text| text.lines().count())
        .max()
        .unwrap_or(0);
    if longest > MAX_DIFF_LINES {
        return None;
    }
    let to_mine = TextDiff::from_lines(base, mine);
    let to_theirs = TextDiff::from_lines(base, theirs);
    let base_lines = to_mine.old_slices();
    let (ours, others) = (hunks(&to_mine), hunks(&to_theirs));

    let mut text = String::new();
    let mut conflicts = 0;
    let mut at = 0;
    let (mut i, mut j) = (0, 0);
    while i < ours.len() || j < others.len() {
        // The next change from either side opens a group. Every change that
        // touches the group joins it, from whichever side.
        let start = match (ours.get(i), others.get(j)) {
            (Some(a), Some(b)) => a.reach.0.min(b.reach.0),
            (Some(a), None) => a.reach.0,
            (None, Some(b)) => b.reach.0,
            (None, None) => break,
        };
        let (first_ours, first_others) = (i, j);
        let mut end = start;
        loop {
            let before = (i, j);
            while ours.get(i).is_some_and(|change| change.reach.0 <= end) {
                end = end.max(ours[i].reach.1);
                i += 1;
            }
            while others.get(j).is_some_and(|change| change.reach.0 <= end) {
                end = end.max(others[j].reach.1);
                j += 1;
            }
            if before == (i, j) {
                break;
            }
        }
        for line in &base_lines[at..start] {
            text.push_str(line);
        }
        at = end;
        let a = side(base_lines, &ours[first_ours..i], start, end);
        let b = side(base_lines, &others[first_others..j], start, end);
        let chosen = if first_others == j || a == b {
            // Only this device changed these lines, or both made the same change.
            Some(&a)
        } else if first_ours == i {
            Some(&b)
        } else {
            None
        };
        if let Some(lines) = chosen {
            for line in lines {
                text.push_str(line);
            }
            continue;
        }
        conflicts += 1;
        let mut put = |lines: &[&str]| {
            for line in lines {
                text.push_str(line);
            }
            // A marker must start on a line of its own.
            if lines.last().is_some_and(|line| !line.ends_with('\n')) {
                text.push('\n');
            }
        };
        put(&[MARK_START, "\n"]);
        put(&a);
        put(&[MARK_MIDDLE, "\n"]);
        put(&b);
        put(&[MARK_END, "\n"]);
    }
    for line in &base_lines[at..] {
        text.push_str(line);
    }
    Some(Merged { text, conflicts })
}

/// Whether a merge left a place in `text` for the person to settle.
#[must_use]
pub fn has_marks(text: &str) -> bool {
    text.lines().any(|line| line == MARK_START)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(base: &str, mine: &str, theirs: &str) -> (String, usize) {
        let merged = merge(base, mine, theirs).unwrap();
        (merged.text, merged.conflicts)
    }

    #[test]
    fn changes_to_different_lines_are_both_kept() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let mine = "ONE\ntwo\nthree\nfour\nfive\n";
        let theirs = "one\ntwo\nthree\nfour\nFIVE\nsix\n";
        assert_eq!(
            merged(base, mine, theirs),
            ("ONE\ntwo\nthree\nfour\nFIVE\nsix\n".to_owned(), 0)
        );
        // Which side is called mine does not matter to a clean merge.
        assert_eq!(merged(base, theirs, mine).0, merged(base, mine, theirs).0);
    }

    #[test]
    fn one_side_unchanged_gives_the_other_side() {
        assert_eq!(merged("a\n", "a\n", "b\n"), ("b\n".to_owned(), 0));
        assert_eq!(merged("a\n", "b\n", "a\n"), ("b\n".to_owned(), 0));
        assert_eq!(merged("a\n", "b\n", "b\n"), ("b\n".to_owned(), 0));
    }

    #[test]
    fn the_same_change_on_both_sides_is_made_once() {
        let base = "a\nb\nc\nd\ne\n";
        let mine = "a\nB\nc\nd\nmine\n";
        let theirs = "a\nB\nc\nd\ne\n";
        assert_eq!(
            merged(base, mine, theirs),
            ("a\nB\nc\nd\nmine\n".to_owned(), 0)
        );
    }

    #[test]
    fn insertions_and_deletions_in_different_places_combine() {
        let base = "a\nb\nc\nd\ne\nf\n";
        let mine = "start\na\nb\nc\nd\ne\nf\n";
        let theirs = "a\nb\nc\nd\nf\nend\n";
        assert_eq!(
            merged(base, mine, theirs),
            ("start\na\nb\nc\nd\nf\nend\n".to_owned(), 0)
        );
    }

    #[test]
    fn the_same_line_changed_two_ways_is_marked_with_both() {
        let base = "a\nb\nc\n";
        let (text, conflicts) = merged(base, "a\nmine\nc\n", "a\ntheirs\nc\n");
        assert_eq!(conflicts, 1);
        assert_eq!(
            text,
            "a\n<<<<<<< mine\nmine\n=======\ntheirs\n>>>>>>> GitHub\nc\n"
        );
        assert!(has_marks(&text));
        assert!(!has_marks(base));
    }

    #[test]
    fn a_deletion_against_an_edit_is_marked() {
        let (text, conflicts) = merged("a\nb\nc\n", "a\nc\n", "a\nb2\nc\n");
        assert_eq!(conflicts, 1);
        assert_eq!(text, "a\n<<<<<<< mine\n=======\nb2\n>>>>>>> GitHub\nc\n");
    }

    #[test]
    fn two_separate_clashes_are_counted_and_the_rest_merges() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let mine = "1\nA\n3\n4\nmine\n6\n7\nB\n9\n";
        let theirs = "1\nX\n3\n4\n5\n6\ntheirs\nY\n9\n";
        let (text, conflicts) = merged(base, mine, theirs);
        assert_eq!(conflicts, 2);
        assert_eq!(
            text,
            "1\n<<<<<<< mine\nA\n=======\nX\n>>>>>>> GitHub\n3\n4\nmine\n6\n\
             <<<<<<< mine\n7\nB\n=======\ntheirs\nY\n>>>>>>> GitHub\n9\n"
        );
    }

    // Found by comparing with git on made-up files (tests/merge.rs). Among
    // repeated lines the same text can be described as different changes;
    // a merge that trusts one description can drop a change without saying so.
    #[test]
    fn among_repeated_lines_an_unclear_overlap_is_asked_about() {
        // Mine changes the last of two equal lines; theirs removes one of
        // them. Which one theirs removed cannot be known.
        let base = "a\nsame\nsame\n";
        let (_, conflicts) = merged(base, "a\nsame\nmine\n", "a\nsame\n");
        assert_eq!(conflicts, 1);
        // Mine removes one of two equal lines; theirs rewrites one of them.
        let base = "x\nzero\none\nzero\nzero\none\none\nend\n";
        let mine = "x\nzero\none\nzero\none\none\nend\nmine added\n";
        let theirs = "x\none\ntheirs changed\nzero\none\none\ntheirs added\nend\n";
        let (text, conflicts) = merged(base, mine, theirs);
        assert_eq!(conflicts, 1, "{text}");
        // What is clear on each side still merges around the unclear place.
        assert!(text.starts_with("x\none\n<<<<<<< mine\n"), "{text}");
        assert!(text.ends_with("theirs added\nend\nmine added\n"), "{text}");
    }

    #[test]
    fn two_changes_that_together_reach_further_are_followed_together() {
        // Theirs removes "five" and one of the "one" lines around it: before
        // it or after it, the text is the same. Mine adds a line between the
        // two after it. If theirs removed one of those two, mine's line has
        // lost its place, so this has to be asked.
        let base = "one\none\nfive\none\none\nend\n";
        let mine = "one\none\nfive\none\nmine added\none\nend\n";
        let theirs = "one\none\none\nend\n";
        assert_eq!(merged(base, mine, theirs).1, 1);
    }

    #[test]
    fn a_last_line_without_a_newline_does_not_swallow_a_marker() {
        let (text, conflicts) = merged("a", "mine", "theirs");
        assert_eq!(conflicts, 1);
        assert_eq!(
            text,
            "<<<<<<< mine\nmine\n=======\ntheirs\n>>>>>>> GitHub\n"
        );
    }

    #[test]
    fn a_file_new_on_both_sides_merges_against_nothing() {
        assert_eq!(merged("", "same\n", "same\n"), ("same\n".to_owned(), 0));
        assert_eq!(merged("", "mine\n", "theirs\n").1, 1);
    }

    #[test]
    fn too_long_to_merge_is_refused() {
        let big = "x\n".repeat(MAX_DIFF_LINES + 1);
        assert_eq!(merge("a\n", &big, "b\n"), None);
    }

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
