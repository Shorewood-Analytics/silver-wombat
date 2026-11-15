//! Repository paths. The root is the empty string; there is no leading slash.

/// Resolves `input` against `cwd`. Handles `/` (root), `.`, `..` and repeated
/// slashes. `..` at the root stays at the root, as in a shell.
#[must_use]
pub fn resolve(cwd: &str, input: &str) -> String {
    let mut parts: Vec<&str> = if input.starts_with('/') {
        Vec::new()
    } else {
        cwd.split('/').filter(|part| !part.is_empty()).collect()
    };
    for part in input.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// The directory that contains `path` (`""` for a top-level entry).
#[must_use]
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The last component of `path`.
#[must_use]
pub fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Joins a directory and a name without doubling or leading slashes.
#[must_use]
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// How a path is shown to the person: always with a leading slash.
#[must_use]
pub fn display(path: &str) -> String {
    format!("/{path}")
}

/// Whether `path` is `dir` itself or inside it.
#[must_use]
pub fn is_within(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Matches `name` against a shell glob with `*` and `?` (no brackets).
#[must_use]
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((star_p, star_n)) = star {
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_absolute() {
        assert_eq!(resolve("", "src"), "src");
        assert_eq!(resolve("src", "app/main.py"), "src/app/main.py");
        assert_eq!(resolve("src/app", ".."), "src");
        assert_eq!(resolve("src/app", "../../.."), "");
        assert_eq!(resolve("src", "/docs//a.md"), "docs/a.md");
        assert_eq!(resolve("src", "./x/./y/"), "src/x/y");
    }

    #[test]
    fn parent_and_name() {
        assert_eq!(parent("a/b/c.rs"), "a/b");
        assert_eq!(parent("c.rs"), "");
        assert_eq!(file_name("a/b/c.rs"), "c.rs");
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a", "b"), "a/b");
    }

    #[test]
    fn within() {
        assert!(is_within("a/b", "a"));
        assert!(is_within("a", "a"));
        assert!(is_within("anything", ""));
        assert!(!is_within("ab/c", "a"));
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "main.py"));
        assert!(glob_match("ma?n.*", "main.rs"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
        assert!(glob_match("naïve*", "naïve.txt"));
    }
}
