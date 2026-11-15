//! The terminal's commands against the in-memory GitHub.

use std::rc::Rc;

use futures::executor::block_on;
use silver_wombat_core::fake::FakeGitHub;
use silver_wombat_core::github::GitHub;
use silver_wombat_core::shell::Shell;
use silver_wombat_core::workspace::Workspace;

const REPO: &str = "t/r";

fn setup() -> (Rc<FakeGitHub>, Workspace, Shell) {
    let fake = Rc::new(FakeGitHub::new("tester"));
    fake.add_repo(
        REPO,
        "main",
        &[
            ("README.md", "# r\nhello world\n"),
            (".gitignore", "target/\n"),
            ("src/main.py", "import util\nprint('hi')\n# TODO tidy\n"),
            (
                "src/util.py",
                "def add(a, b):\n    return a + b\n# todo: tests\n",
            ),
        ],
    );
    let workspace = block_on(Workspace::open(GitHub::new(fake.clone()), REPO, "main")).unwrap();
    (fake, workspace, Shell::new())
}

fn run(shell: &Shell, ws: &Workspace, line: &str) -> (String, i32) {
    let output = block_on(shell.run(line, ws));
    (output.text, output.status)
}

#[test]
fn navigation() {
    let (_, ws, sh) = setup();
    assert_eq!(run(&sh, &ws, "pwd"), ("/\n".to_owned(), 0));
    assert_eq!(run(&sh, &ws, "ls"), ("src/\nREADME.md\n".to_owned(), 0));
    assert_eq!(run(&sh, &ws, "ls -a").0, "src/\n.gitignore\nREADME.md\n");
    assert_eq!(run(&sh, &ws, "cd src && pwd"), ("/src\n".to_owned(), 0));
    assert_eq!(run(&sh, &ws, "ls").0, "main.py\nutil.py\n");
    assert_eq!(run(&sh, &ws, "cd .. ; pwd").0, "/\n");
    let (text, status) = run(&sh, &ws, "cd nope");
    assert_eq!(status, 1);
    assert!(text.contains("no such folder"), "{text}");
    assert_eq!(run(&sh, &ws, "cd README.md").1, 1);
    assert_eq!(sh.prompt(&ws), "t/r:/ (main) $");
}

#[test]
fn reading_and_pipes() {
    let (_, ws, sh) = setup();
    assert_eq!(run(&sh, &ws, "cat README.md").0, "# r\nhello world\n");
    assert_eq!(run(&sh, &ws, "head -n 1 README.md").0, "# r\n");
    assert_eq!(run(&sh, &ws, "tail -1 README.md").0, "hello world\n");
    assert_eq!(
        run(&sh, &ws, "cat src/main.py | grep -n print").0,
        "2:print('hi')\n"
    );
    assert_eq!(run(&sh, &ws, "cat src/main.py | wc -l").0, "3 \n");
    assert_eq!(run(&sh, &ws, "wc README.md").0, "2 4 16 README.md\n");
    let (text, status) = run(&sh, &ws, "cat nope.txt");
    assert_eq!(status, 1);
    assert!(text.contains("nope.txt"), "{text}");
}

#[test]
fn grep_variants() {
    let (_, ws, sh) = setup();
    let (text, status) = run(&sh, &ws, "grep -rin todo src");
    assert_eq!(status, 0);
    assert_eq!(
        text,
        "/src/main.py:3:# TODO tidy\n/src/util.py:3:# todo: tests\n"
    );
    assert_eq!(run(&sh, &ws, "grep -rl TODO .").0, "/src/main.py\n");
    assert_eq!(run(&sh, &ws, "grep -c hello README.md").0, "1\n");
    assert_eq!(run(&sh, &ws, "grep zzz README.md"), (String::new(), 1));
    assert_eq!(
        run(&sh, &ws, "grep -F 'a + b' src/util.py").0,
        "    return a + b\n"
    );
    assert_eq!(
        run(&sh, &ws, "grep 'a . b' src/util.py").0,
        "    return a + b\n"
    );
    assert_eq!(run(&sh, &ws, "grep '(' README.md").1, 2);
    let (text, status) = run(&sh, &ws, "grep x src");
    assert_eq!(status, 2);
    assert!(text.contains("use -r"), "{text}");
}

#[test]
fn globs_expand_against_the_workspace() {
    let (_, ws, sh) = setup();
    assert_eq!(
        run(&sh, &ws, "echo src/*.py").0,
        "src/main.py src/util.py\n"
    );
    assert_eq!(run(&sh, &ws, "echo *").0, "README.md src\n");
    assert_eq!(run(&sh, &ws, "echo '*.py' *.nope").0, "*.py *.nope\n");
    assert_eq!(
        run(&sh, &ws, "find . -name '*.py'").0,
        "/src/main.py\n/src/util.py\n"
    );
}

#[test]
fn editing_files_from_the_shell() {
    let (fake, ws, sh) = setup();
    assert_eq!(run(&sh, &ws, "echo first > notes.txt"), (String::new(), 0));
    run(&sh, &ws, "echo second >> notes.txt");
    assert_eq!(run(&sh, &ws, "cat notes.txt").0, "first\nsecond\n");
    assert_eq!(
        run(&sh, &ws, "touch docs/empty.md && ls docs").0,
        "empty.md\n"
    );
    assert_eq!(run(&sh, &ws, "mv src/util.py src/helpers.py").1, 0);
    assert_eq!(run(&sh, &ws, "cp README.md docs").1, 0);
    assert_eq!(run(&sh, &ws, "ls docs").0, "README.md\nempty.md\n");
    assert_eq!(run(&sh, &ws, "rm src").1, 1);
    assert_eq!(run(&sh, &ws, "rm -f nope").1, 0);
    assert_eq!(run(&sh, &ws, "rm /").1, 1);

    let (status, _) = run(&sh, &ws, "git status");
    for expected in [
        "On branch main",
        "new file: notes.txt",
        "new file: src/helpers.py",
        "deleted:  src/util.py",
        "Not yet saved",
    ] {
        assert!(
            status.contains(expected),
            "missing {expected:?} in\n{status}"
        );
    }
    let (diff, _) = run(&sh, &ws, "git diff notes.txt");
    assert!(diff.contains("+first"), "{diff}");

    // Nothing reached GitHub yet.
    assert_eq!(fake.file(REPO, "main", "notes.txt"), None);
    let (text, code) = run(&sh, &ws, "git commit -m \"Add notes\"");
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("Add notes") && text.contains("5 files changed"),
        "{text}"
    );
    assert_eq!(
        fake.file(REPO, "main", "notes.txt").unwrap(),
        "first\nsecond\n"
    );
    assert!(fake.file(REPO, "main", "src/helpers.py").is_some());
    assert_eq!(fake.file(REPO, "main", "src/util.py"), None);
    assert!(run(&sh, &ws, "git status").0.contains("Nothing changed"));
    assert!(run(&sh, &ws, "git log -n 1").0.contains("Add notes"));
}

#[test]
fn git_branching_restore_and_conflicts() {
    let (fake, ws, sh) = setup();
    assert_eq!(run(&sh, &ws, "git branch").0, "* main\n");
    assert!(
        run(&sh, &ws, "git switch -c feature/x")
            .0
            .contains("feature/x")
    );
    assert_eq!(run(&sh, &ws, "git branch").0, "* feature/x\n  main\n");
    run(&sh, &ws, "echo x > a.txt");
    assert_eq!(run(&sh, &ws, "git switch main").1, 1);
    assert!(run(&sh, &ws, "git restore .").0.contains("Restored 1 file"));
    assert_eq!(run(&sh, &ws, "git switch main").1, 0);

    run(&sh, &ws, "echo mine > README.md");
    fake.push(REPO, "main", "Theirs", &[("README.md", Some("theirs\n"))]);
    let (text, code) = run(&sh, &ws, "git commit -m mine");
    assert_eq!(code, 1);
    assert!(
        text.contains("Changed on GitHub") && text.contains("README.md"),
        "{text}"
    );
    assert_eq!(fake.file(REPO, "main", "README.md").unwrap(), "theirs\n");
    let (text, code) = run(&sh, &ws, "git pull");
    assert_eq!(code, 1);
    assert!(text.contains("README.md"), "{text}");
}

#[test]
fn save_checkpoints_without_touching_the_branch() {
    let (fake, ws, sh) = setup();
    assert!(run(&sh, &ws, "save").0.contains("Nothing new"));
    run(&sh, &ws, "echo draft > wip.txt");
    let (text, code) = run(&sh, &ws, "save");
    assert_eq!(code, 0);
    assert!(text.contains("autosave/main"), "{text}");
    assert_eq!(
        fake.file(REPO, "autosave/main", "wip.txt").unwrap(),
        "draft\n"
    );
    assert_eq!(fake.file(REPO, "main", "wip.txt"), None);
    assert!(
        run(&sh, &ws, "git status")
            .0
            .contains("Checkpointed to GitHub")
    );
}

#[test]
fn unsupported_things_say_so() {
    let (_, ws, sh) = setup();
    let (text, code) = run(&sh, &ws, "python main.py");
    assert_eq!(code, 127);
    assert!(
        text.contains("not available") && text.contains("browser"),
        "{text}"
    );
    let (text, code) = run(&sh, &ws, "frobnicate");
    assert_eq!(code, 127);
    assert!(text.contains("command not found"), "{text}");
    assert_eq!(run(&sh, &ws, "echo $(date)").1, 2);
    assert_eq!(run(&sh, &ws, "git rebase main").1, 2);
    assert!(run(&sh, &ws, "git push").0.contains("Nothing to do"));
    assert!(
        run(&sh, &ws, "mkdir x")
            .0
            .contains("git does not store empty folders")
    );
    assert!(run(&sh, &ws, "help").0.contains("grep"));
    assert_eq!(run(&sh, &ws, "whoami").0, "tester\n");
    // && stops after a failure; ; does not.
    assert!(!run(&sh, &ws, "cat nope && echo after").0.contains("after"));
    assert!(run(&sh, &ws, "cat nope ; echo after").0.contains("after"));
}

#[test]
fn open_clear_and_history() {
    let (_, ws, sh) = setup();
    let output = block_on(sh.run("open src/main.py", &ws));
    assert_eq!(output.open.as_deref(), Some("src/main.py"));
    let output = block_on(sh.run("edit brand/new.py", &ws));
    assert_eq!(output.open.as_deref(), Some("brand/new.py"));
    assert_eq!(ws.changes().len(), 1);
    assert!(block_on(sh.run("clear", &ws)).clear);
    assert_eq!(block_on(sh.run("open src", &ws)).status, 1);
    assert!(run(&sh, &ws, "history").0.contains("open src/main.py"));
}

#[test]
fn long_output_is_cut_and_says_so() {
    let (_, ws, sh) = setup();
    let big = "x".repeat(1000);
    block_on(ws.write("big.txt", &format!("{big}\n").repeat(400))).unwrap();
    let (text, _) = run(&sh, &ws, "cat big.txt");
    assert!(text.len() < 300 * 1024);
    assert!(
        text.contains("output cut"),
        "tail: {}",
        &text[text.len() - 120..]
    );
}

#[test]
fn completion() {
    let (_, ws, sh) = setup();
    let done = block_on(sh.complete("gr", &ws));
    assert_eq!(done.line, "grep ");
    let done = block_on(sh.complete("cat RE", &ws));
    assert_eq!(done.line, "cat README.md ");
    let done = block_on(sh.complete("cat src/", &ws));
    assert_eq!(done.line, "cat src/");
    assert_eq!(done.candidates, ["src/main.py", "src/util.py"]);
    let done = block_on(sh.complete("ls s", &ws));
    assert_eq!(done.line, "ls src/");
    let done = block_on(sh.complete("cat zzz", &ws));
    assert_eq!(done.line, "cat zzz");
    let done = block_on(sh.complete("cat a | hea", &ws));
    assert_eq!(done.line, "cat a | head ");
    let done = block_on(sh.complete("cat a | he", &ws));
    assert_eq!(done.candidates, ["head", "help"]);
    let done = block_on(sh.complete("c", &ws));
    assert_eq!(done.line, "c");
    assert_eq!(done.candidates, ["cat", "cd", "clear", "cp"]);
}
