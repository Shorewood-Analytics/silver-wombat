//! The three-way merge, checked against git's own.
//!
//! `git merge-file` is run on many small made-up files. Wherever this merge
//! says the two sides combine cleanly, git must agree and produce the same
//! text. The reverse is allowed: this merge may ask the person about a place
//! git would have settled by itself. It must never settle one git would not.
//!
//! Half the files are made of very few distinct lines, so that repeated
//! lines, where a merge can go wrong without noticing, are everywhere. The
//! other half have mostly distinct lines, like ordinary text.

use std::process::Command;

use silver_wombat_core::diff::merge;

/// A small repeatable source of numbers, so a failure can be reproduced.
struct Numbers(u64);

impl Numbers {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % below
    }
}

fn base(numbers: &mut Numbers, distinct: u64) -> Vec<String> {
    let length = 4 + numbers.next(14);
    (0..length)
        .map(|_| format!("line {}", numbers.next(distinct)))
        .collect()
}

/// A copy of `lines` with a few lines changed, removed or added.
fn edited(lines: &[String], numbers: &mut Numbers, who: &str) -> Vec<String> {
    let mut lines = lines.to_vec();
    for edit in 0..=numbers.next(3) {
        let at = usize::try_from(numbers.next(lines.len() as u64 + 1)).unwrap();
        match numbers.next(3) {
            0 if at < lines.len() => lines[at] = format!("{who} changed {edit}"),
            1 if at < lines.len() => {
                lines.remove(at);
            }
            _ => lines.insert(at, format!("{who} added {}", numbers.next(3))),
        }
    }
    lines
}

fn text(lines: &[String]) -> String {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

#[test]
fn a_clean_merge_is_the_merge_git_makes() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("git is not installed: skipped");
        return;
    }
    let dir = std::env::temp_dir().join(format!("silver-wombat-merge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // MERGE_ROUNDS and MERGE_SEED widen the search by hand.
    let setting = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    let mut numbers = Numbers(setting("MERGE_SEED", 2026));
    let rounds = setting("MERGE_ROUNDS", 1500);
    for (kind, distinct) in [("repeated lines", 6), ("mostly distinct lines", 60)] {
        let (mut clean, mut asked, mut stricter) = (0, 0, 0);
        for round in 0..rounds / 2 {
            let original = base(&mut numbers, distinct);
            let (mine, theirs) = (
                edited(&original, &mut numbers, "mine"),
                edited(&original, &mut numbers, "theirs"),
            );
            let (original, mine, theirs) = (text(&original), text(&mine), text(&theirs));
            let merged = merge(&original, &mine, &theirs).unwrap();

            for (name, content) in [("base", &original), ("mine", &mine), ("theirs", &theirs)] {
                std::fs::write(dir.join(name), content).unwrap();
            }
            let git = Command::new("git")
                .current_dir(&dir)
                .args(["merge-file", "-p", "mine", "base", "theirs"])
                .output()
                .unwrap();
            // Its exit status is the number of conflicts; 0 is a clean merge.
            let git_clean = git.status.code() == Some(0);
            let git_text = String::from_utf8(git.stdout).unwrap();

            if merged.conflicts == 0 {
                assert!(
                    git_clean,
                    "{kind}, round {round}: merged cleanly where git found a conflict\nbase:\n{original}\nmine:\n{mine}\ntheirs:\n{theirs}\nours:\n{}\ngit:\n{git_text}",
                    merged.text
                );
                assert_eq!(
                    merged.text, git_text,
                    "{kind}, round {round}: a different clean merge from git's\nbase:\n{original}\nmine:\n{mine}\ntheirs:\n{theirs}"
                );
                clean += 1;
            } else {
                asked += 1;
                if git_clean {
                    stricter += 1;
                }
            }
        }
        // The made-up edits must exercise both outcomes, or the test shows nothing.
        assert!(
            clean > 50 && asked > 50,
            "{kind}: clean {clean}, asked {asked}"
        );
        eprintln!(
            "{kind}: {clean} clean merges matched git; {asked} asked the person, {stricter} of them where git did not"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}
