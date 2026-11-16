//! Finding a file or an action by typing a few of its letters.
//!
//! The letters must appear in order but need not be next to each other, so
//! `grpy` finds `app/greet.py`. Matches are ranked the way people expect from
//! a file finder: the file's own name counts for more than its folders, the
//! start of a word for more than its middle, and letters in a row for more
//! than scattered ones.

/// One ranked match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// The position of the item in the list that was searched.
    pub index: usize,
    /// Higher is better.
    pub score: i32,
}

fn is_separator(character: char) -> bool {
    matches!(character, '/' | '_' | '-' | '.' | ' ')
}

/// How well `query` matches `text`, or `None` if its letters do not all
/// appear in order. Letter case is ignored.
#[must_use]
pub fn score(query: &str, text: &str) -> Option<i32> {
    let wanted: Vec<char> = query
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if wanted.is_empty() {
        return Some(0);
    }
    let letters: Vec<char> = text.chars().collect();
    let name_start = letters
        .iter()
        .rposition(|character| *character == '/')
        .map_or(0, |slash| slash + 1);

    // Prefer a match inside the name itself; fall back to the whole path.
    let within = |from: usize| -> Option<i32> {
        let mut total = 0;
        let mut next = 0;
        let mut previous: Option<usize> = None;
        for (at, character) in letters.iter().enumerate().skip(from) {
            if next == wanted.len() {
                break;
            }
            if character.to_lowercase().next() != Some(wanted[next]) {
                continue;
            }
            total += 1;
            if at >= name_start {
                total += 2;
            }
            if at == name_start {
                total += 8;
            } else if at > 0 && is_separator(letters[at - 1]) {
                total += 4;
            }
            if previous.is_some_and(|before| before + 1 == at) {
                total += 5;
            }
            previous = Some(at);
            next += 1;
        }
        (next == wanted.len()).then_some(total)
    };
    let best = [within(name_start), within(0)]
        .into_iter()
        .flatten()
        .max()?;
    // Among equal matches the shorter text is the closer one.
    let length = i32::try_from(letters.len()).unwrap_or(i32::MAX);
    Some(best * 4 - length.min(200) / 8)
}

/// The best `limit` matches for `query` among `items`, best first. With an
/// empty query, the first `limit` items in their own order.
#[must_use]
pub fn rank<'a>(query: &str, items: impl IntoIterator<Item = &'a str>, limit: usize) -> Vec<Match> {
    let mut matches: Vec<Match> = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| score(query, item).map(|score| Match { index, score }))
        .collect();
    // A stable sort keeps the original order among equals.
    matches.sort_by_key(|found| std::cmp::Reverse(found.score));
    matches.truncate(limit);
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: [&str; 9] = [
        "README.md",
        "app/__init__.py",
        "app/greet.py",
        "app/main.py",
        "docs/main-ideas.md",
        "pyproject.toml",
        "src/domain/payment_gateway.py",
        "tests/test_greet.py",
        "tests/test_main.py",
    ];

    fn found(query: &str) -> Vec<&'static str> {
        rank(query, FILES, 3)
            .into_iter()
            .map(|found| FILES[found.index])
            .collect()
    }

    #[test]
    fn letters_in_order_find_a_file() {
        assert_eq!(found("grpy")[0], "app/greet.py");
        assert_eq!(found("pg")[0], "src/domain/payment_gateway.py");
        assert_eq!(found("README")[0], "README.md");
        assert_eq!(found("read me")[0], "README.md");
        assert_eq!(found("zzz"), Vec::<&str>::new());
    }

    #[test]
    fn the_file_name_counts_for_more_than_its_folders() {
        // "main" is the start of two names and the middle of a third; the
        // shortest path with the name starting there comes first.
        assert_eq!(
            found("main"),
            ["app/main.py", "docs/main-ideas.md", "tests/test_main.py"]
        );
        // A folder can still narrow it down.
        assert_eq!(found("tests main")[0], "tests/test_main.py");
        assert_eq!(found("app/g")[0], "app/greet.py");
    }

    #[test]
    fn an_empty_query_lists_the_first_items_in_order() {
        assert_eq!(found(""), ["README.md", "app/__init__.py", "app/greet.py"]);
        assert_eq!(
            found("   "),
            ["README.md", "app/__init__.py", "app/greet.py"]
        );
    }

    #[test]
    fn case_is_ignored_and_actions_match_by_word() {
        assert!(score("PYPROJ", "pyproject.toml").is_some());
        let actions = ["Go to Terminal", "Switch theme", "Checkpoint to GitHub now"];
        let best = rank("term", actions, 1);
        assert_eq!(actions[best[0].index], "Go to Terminal");
        let best = rank("ckpt", actions, 1);
        assert_eq!(actions[best[0].index], "Checkpoint to GitHub now");
    }
}
