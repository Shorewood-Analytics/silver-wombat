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

/// The path of a new file called `name`, made from the folder `dir`. The name
/// may include folders on the way (`notes/today.md`) and may start with `/`
/// to mean the top of the repository. `Err` says what is wrong with the name,
/// in words for the person who typed it.
pub fn for_new_file(dir: &str, name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Type a name for the new file first.");
    }
    if name.ends_with('/') {
        return Err("That names a folder. Add the file's name after the last slash.");
    }
    if name.contains('\\') || name.chars().any(char::is_control) {
        return Err("A file's name cannot have a backslash or a line break in it.");
    }
    let mut parts = name.trim_start_matches('/').split('/');
    if parts.any(|part| part.is_empty() || part == "." || part == "..") {
        return Err("Write the name without \"..\", \".\" or doubled slashes.");
    }
    let full = resolve(dir, name);
    if full.split('/').any(|part| part == ".git") {
        return Err("GitHub does not accept files inside a .git folder.");
    }
    Ok(full)
}

/// The media type of a picture a browser can show, going by the file's
/// extension, or `None` if `path` is not one.
#[must_use]
pub fn picture_type(path: &str) -> Option<&'static str> {
    let (_, extension) = file_name(path).rsplit_once('.')?;
    match extension.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        _ => None,
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
    fn places_a_new_file_and_explains_a_bad_name() {
        assert_eq!(for_new_file("", "notes.md"), Ok("notes.md".to_owned()));
        assert_eq!(
            for_new_file("src", " util.py "),
            Ok("src/util.py".to_owned())
        );
        assert_eq!(
            for_new_file("src", "deep/er/a.py"),
            Ok("src/deep/er/a.py".to_owned())
        );
        assert_eq!(for_new_file("src", "/top.md"), Ok("top.md".to_owned()));
        for bad in [
            "",
            "   ",
            "folder/",
            "../up.md",
            "a/./b",
            "a//b",
            "a\\b",
            ".git/config",
            "x/.git/y",
        ] {
            assert!(for_new_file("src", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn knows_a_picture_by_its_extension() {
        assert_eq!(picture_type("docs/plot.png"), Some("image/png"));
        assert_eq!(picture_type("PHOTO.JPG"), Some("image/jpeg"));
        assert_eq!(picture_type("a/b.c/diagram.svg"), Some("image/svg+xml"));
        assert_eq!(picture_type("notes.txt"), None);
        assert_eq!(picture_type("png"), None);
        assert_eq!(picture_type("folder.png/readme"), None);
    }

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
