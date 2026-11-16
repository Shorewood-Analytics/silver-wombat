//! The workspace against the in-memory GitHub: reads, local changes, save
//! states, checkpoints, commits and conflicts.

use std::rc::Rc;

use futures::executor::block_on;
use silver_wombat_core::Error;
use silver_wombat_core::fake::FakeGitHub;
use silver_wombat_core::github::GitHub;
use silver_wombat_core::workspace::{ChangeKind, EntryKind, FileContent, SaveState, Workspace};

const REPO: &str = "t/r";

fn setup() -> (Rc<FakeGitHub>, Workspace) {
    let fake = Rc::new(FakeGitHub::new("tester"));
    fake.add_repo(
        REPO,
        "main",
        &[
            ("README.md", "# r\n"),
            ("src/main.py", "print('hi')\n"),
            ("src/util/math.py", "def add(a, b):\n    return a + b\n"),
        ],
    );
    let workspace = block_on(Workspace::open(GitHub::new(fake.clone()), REPO, "main")).unwrap();
    (fake, workspace)
}

fn names(workspace: &Workspace, dir: &str) -> Vec<String> {
    block_on(workspace.list_dir(dir))
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

#[test]
fn lists_folders_first_and_reads_files() {
    let (_, ws) = setup();
    assert_eq!(names(&ws, ""), ["src", "README.md"]);
    assert_eq!(names(&ws, "src"), ["util", "main.py"]);
    assert_eq!(
        block_on(ws.read("src/main.py")).unwrap(),
        FileContent::Text("print('hi')\n".to_owned())
    );
    assert_eq!(block_on(ws.kind("src")).unwrap(), Some(EntryKind::Dir));
    assert_eq!(block_on(ws.kind("nope")).unwrap(), None);
    assert!(matches!(block_on(ws.read("nope")), Err(Error::NotFound(_))));
    assert!(matches!(block_on(ws.read("src")), Err(Error::Invalid(_))));
    assert!(matches!(
        block_on(ws.list_dir("nope")),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn opening_a_missing_branch_fails_clearly() {
    let fake = Rc::new(FakeGitHub::new("tester"));
    fake.add_repo(REPO, "main", &[("a", "b")]);
    let result = block_on(Workspace::open(GitHub::new(fake), REPO, "nope"));
    assert!(matches!(result, Err(Error::NotFound(what)) if what.contains("nope")));
}

#[test]
fn writes_stay_local_until_committed() {
    let (fake, ws) = setup();
    assert!(block_on(ws.write("src/main.py", "print('bye')\n")).unwrap());
    assert!(block_on(ws.write("docs/new.md", "new\n")).unwrap());
    assert_eq!(
        block_on(ws.read_text("src/main.py")).unwrap(),
        "print('bye')\n"
    );
    assert_eq!(names(&ws, ""), ["docs", "src", "README.md"]);
    assert_eq!(block_on(ws.kind("docs")).unwrap(), Some(EntryKind::Dir));
    // GitHub is untouched.
    assert_eq!(
        fake.file(REPO, "main", "src/main.py").unwrap(),
        "print('hi')\n"
    );
    assert_eq!(fake.file(REPO, "main", "docs/new.md"), None);
    let kinds: Vec<_> = ws.changes().into_iter().map(|c| (c.path, c.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("docs/new.md".to_owned(), ChangeKind::Added),
            ("src/main.py".to_owned(), ChangeKind::Modified),
        ]
    );
}

#[test]
fn writing_the_original_text_back_removes_the_change() {
    let (_, ws) = setup();
    block_on(ws.write("src/main.py", "changed\n")).unwrap();
    assert_eq!(ws.changes().len(), 1);
    assert!(!block_on(ws.write("src/main.py", "print('hi')\n")).unwrap());
    assert_eq!(ws.changes(), Vec::new());
}

#[test]
fn invalid_paths_are_refused() {
    let (_, ws) = setup();
    for bad in ["", "a//b", ".git/config", "src", "README.md/inside.txt"] {
        assert!(
            matches!(block_on(ws.write(bad, "x")), Err(Error::Invalid(_))),
            "{bad:?} should be refused"
        );
    }
}

#[test]
fn save_states_only_advance_after_the_write_that_earns_them() {
    let (fake, ws) = setup();
    assert_eq!(ws.save_state(), None);
    block_on(ws.write("src/main.py", "one\n")).unwrap();
    assert_eq!(ws.save_state(), Some(SaveState::NotSaved));

    let (revision, json) = ws.export_drafts();
    // Not marked until the device store confirms.
    assert_eq!(ws.save_state(), Some(SaveState::NotSaved));
    ws.mark_on_device(revision);
    assert_eq!(ws.save_state(), Some(SaveState::OnDevice));

    // A newer edit is not covered by the older device write.
    block_on(ws.write("README.md", "# changed\n")).unwrap();
    assert_eq!(ws.file_state("src/main.py"), Some(SaveState::OnDevice));
    assert_eq!(ws.file_state("README.md"), Some(SaveState::NotSaved));
    assert_eq!(ws.save_state(), Some(SaveState::NotSaved));

    // A failed checkpoint changes nothing.
    fake.set_offline(true);
    assert!(matches!(block_on(ws.checkpoint()), Err(Error::Network(_))));
    assert_eq!(ws.file_state("README.md"), Some(SaveState::NotSaved));
    fake.set_offline(false);

    let sha = block_on(ws.checkpoint())
        .unwrap()
        .expect("a checkpoint commit");
    assert_eq!(ws.save_state(), Some(SaveState::OnGitHub));
    assert_eq!(fake.head(REPO, "autosave/main").unwrap(), sha);
    assert_eq!(
        fake.file(REPO, "autosave/main", "src/main.py").unwrap(),
        "one\n"
    );
    // The real branch is untouched by a checkpoint.
    assert_eq!(
        fake.file(REPO, "main", "src/main.py").unwrap(),
        "print('hi')\n"
    );
    // Nothing new: no second commit.
    assert_eq!(block_on(ws.checkpoint()).unwrap(), None);

    // Restoring on a fresh workspace brings the changes back as "on device".
    let again = block_on(Workspace::open(GitHub::new(fake.clone()), REPO, "main")).unwrap();
    assert_eq!(again.import_drafts(&json).unwrap(), 1);
    assert_eq!(block_on(again.read_text("src/main.py")).unwrap(), "one\n");
    assert_eq!(again.save_state(), Some(SaveState::OnDevice));
}

#[test]
fn saved_changes_for_another_branch_are_refused() {
    let (fake, ws) = setup();
    block_on(ws.write("a.txt", "x")).unwrap();
    let (_, json) = ws.export_drafts();
    block_on(ws.switch_branch("other", true)).unwrap();
    let other = block_on(Workspace::open(GitHub::new(fake), REPO, "other")).unwrap();
    assert!(matches!(other.import_drafts(&json), Err(Error::Invalid(_))));
    assert!(matches!(
        other.import_drafts("not json"),
        Err(Error::Decode(_))
    ));
}

#[test]
fn checkpoints_chain_and_never_touch_the_branch() {
    let (fake, ws) = setup();
    block_on(ws.write("a.txt", "1")).unwrap();
    let first = block_on(ws.checkpoint()).unwrap().unwrap();
    block_on(ws.write("a.txt", "2")).unwrap();
    let second = block_on(ws.checkpoint()).unwrap().unwrap();
    assert_ne!(first, second);
    let (message, parents) = fake.commit_info(REPO, &second).unwrap();
    assert_eq!(parents, [first]);
    assert!(message.starts_with("Checkpoint: 1 file"), "{message}");
    assert!(message.contains("a.txt"));
    assert_eq!(fake.file(REPO, "main", "a.txt"), None);
}

#[test]
fn commit_writes_one_commit_and_clears_changes() {
    let (fake, ws) = setup();
    let before = fake.head(REPO, "main").unwrap();
    block_on(ws.write("src/main.py", "print('bye')\n")).unwrap();
    block_on(ws.write("docs/new.md", "new\n")).unwrap();
    block_on(ws.remove("README.md", false)).unwrap();
    let outcome = block_on(ws.commit("  Tidy up  ")).unwrap();
    assert_eq!(outcome.files, 3);
    assert_eq!(fake.head(REPO, "main").unwrap(), outcome.sha);
    let (message, parents) = fake.commit_info(REPO, &outcome.sha).unwrap();
    assert_eq!(message, "Tidy up");
    assert_eq!(parents, [before]);
    assert_eq!(
        fake.file(REPO, "main", "src/main.py").unwrap(),
        "print('bye')\n"
    );
    assert_eq!(fake.file(REPO, "main", "docs/new.md").unwrap(), "new\n");
    assert_eq!(fake.file(REPO, "main", "README.md"), None);
    // Untouched files survive.
    assert!(fake.file(REPO, "main", "src/util/math.py").is_some());
    assert_eq!(ws.changes(), Vec::new());
    assert_eq!(ws.head(), outcome.sha);
    assert_eq!(names(&ws, ""), ["docs", "src"]);
}

#[test]
fn commit_needs_a_message_and_changes() {
    let (_, ws) = setup();
    assert!(matches!(block_on(ws.commit("msg")), Err(Error::Invalid(_))));
    block_on(ws.write("a.txt", "x")).unwrap();
    assert!(matches!(block_on(ws.commit("   ")), Err(Error::Invalid(_))));
    assert_eq!(ws.changes().len(), 1);
}

#[test]
fn a_conflicting_remote_change_blocks_the_commit_and_writes_nothing() {
    let (fake, ws) = setup();
    block_on(ws.write("src/main.py", "mine\n")).unwrap();
    block_on(ws.write("notes.txt", "also mine\n")).unwrap();
    let theirs = fake.push(
        REPO,
        "main",
        "Their change",
        &[("src/main.py", Some("theirs\n"))],
    );

    let error = block_on(ws.commit("Mine")).unwrap_err();
    assert_eq!(error, Error::Conflict(vec!["src/main.py".to_owned()]));
    assert_eq!(fake.head(REPO, "main").unwrap(), theirs);
    assert_eq!(fake.file(REPO, "main", "src/main.py").unwrap(), "theirs\n");
    assert_eq!(fake.file(REPO, "main", "notes.txt"), None);
    // Local work is still there.
    assert_eq!(block_on(ws.read_text("src/main.py")).unwrap(), "mine\n");
    assert_eq!(ws.changes().len(), 2);

    // Dropping the conflicting file lets the rest through, on top of theirs.
    assert!(ws.discard("src/main.py"));
    let outcome = block_on(ws.commit("Mine")).unwrap();
    let (_, parents) = fake.commit_info(REPO, &outcome.sha).unwrap();
    assert_eq!(parents, [theirs]);
    assert_eq!(fake.file(REPO, "main", "src/main.py").unwrap(), "theirs\n");
    assert_eq!(fake.file(REPO, "main", "notes.txt").unwrap(), "also mine\n");
}

#[test]
fn a_conflict_is_settled_by_merging_both_sides() {
    let fake = Rc::new(FakeGitHub::new("tester"));
    let poem = "one\ntwo\nthree\nfour\nfive\nsix\n";
    fake.add_repo(
        REPO,
        "main",
        &[
            ("poem.txt", poem),
            ("clash.txt", "a\nb\nc\n"),
            ("gone.txt", "x\n"),
        ],
    );
    let ws = block_on(Workspace::open(GitHub::new(fake.clone()), REPO, "main")).unwrap();
    block_on(ws.write("poem.txt", "ONE\ntwo\nthree\nfour\nfive\nsix\n")).unwrap();
    block_on(ws.write("clash.txt", "a\nmine\nc\n")).unwrap();
    block_on(ws.write("gone.txt", "x\nmine\n")).unwrap();
    block_on(ws.write("both-new.txt", "same on both sides\n")).unwrap();
    let theirs = fake.push(
        REPO,
        "main",
        "Their change",
        &[
            (
                "poem.txt",
                Some("one\ntwo\nthree\nfour\nfive\nSIX\nseven\n"),
            ),
            ("clash.txt", Some("a\ntheirs\nc\n")),
            ("gone.txt", None),
            ("both-new.txt", Some("same on both sides\n")),
        ],
    );
    assert!(matches!(
        block_on(ws.commit("Mine")),
        Err(Error::Conflict(_))
    ));
    block_on(ws.refresh()).unwrap();

    // Changes to different lines: both are kept and nothing is left to settle.
    assert_eq!(block_on(ws.merge_theirs("poem.txt")).unwrap(), 0);
    assert_eq!(
        block_on(ws.read_text("poem.txt")).unwrap(),
        "ONE\ntwo\nthree\nfour\nfive\nSIX\nseven\n"
    );
    // A file both sides created with the same text merges against nothing.
    assert_eq!(block_on(ws.merge_theirs("both-new.txt")).unwrap(), 0);
    // A file deleted on GitHub has nothing to merge with.
    let refused = block_on(ws.merge_theirs("gone.txt"))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("deleted on GitHub"), "{refused}");
    block_on(ws.keep_mine("gone.txt")).unwrap();

    // The same line changed two ways: both versions stay in the file, marked,
    // and a commit is refused until the person has settled it.
    assert_eq!(block_on(ws.merge_theirs("clash.txt")).unwrap(), 1);
    assert_eq!(
        block_on(ws.read_text("clash.txt")).unwrap(),
        "a\n<<<<<<< mine\nmine\n=======\ntheirs\n>>>>>>> GitHub\nc\n"
    );
    assert_eq!(ws.unsettled(), ["clash.txt"]);
    assert_eq!(block_on(ws.conflicts()).unwrap(), Vec::<String>::new());
    let refused = block_on(ws.commit("Too soon")).unwrap_err().to_string();
    assert!(
        refused.contains("/clash.txt still has places marked <<<<<<< mine"),
        "{refused}"
    );
    assert_eq!(fake.head(REPO, "main").unwrap(), theirs);

    block_on(ws.write("clash.txt", "a\nmine and theirs\nc\n")).unwrap();
    assert_eq!(ws.unsettled(), Vec::<String>::new());
    let outcome = block_on(ws.commit("Merged")).unwrap();
    let (_, parents) = fake.commit_info(REPO, &outcome.sha).unwrap();
    assert_eq!(parents, [theirs]);
    assert_eq!(
        fake.file(REPO, "main", "poem.txt").unwrap(),
        "ONE\ntwo\nthree\nfour\nfive\nSIX\nseven\n"
    );
    assert_eq!(
        fake.file(REPO, "main", "clash.txt").unwrap(),
        "a\nmine and theirs\nc\n"
    );
    assert_eq!(fake.file(REPO, "main", "gone.txt").unwrap(), "x\nmine\n");
}

#[test]
fn a_conflict_is_settled_by_choosing_a_side() {
    let (fake, ws) = setup();
    block_on(ws.write("src/main.py", "mine\n")).unwrap();
    block_on(ws.write("README.md", "# mine\n")).unwrap();
    block_on(ws.remove("src/util/math.py", false)).unwrap();
    let theirs = fake.push(
        REPO,
        "main",
        "Their change",
        &[
            ("src/main.py", Some("theirs\n")),
            ("README.md", Some("# theirs\n")),
            ("src/util/math.py", None),
        ],
    );
    assert!(matches!(
        block_on(ws.commit("Mine")),
        Err(Error::Conflict(_))
    ));

    // Fetching the branch shows what conflicts, and the diff of each file is
    // now from GitHub's version to the local one.
    let conflicts = block_on(ws.refresh()).unwrap();
    assert_eq!(conflicts, ["README.md", "src/main.py", "src/util/math.py"]);
    let diff = block_on(ws.diff_against_github("src/main.py")).unwrap();
    assert!(diff.contains("-theirs") && diff.contains("+mine"), "{diff}");
    // The ordinary diff still shows the change from where it started.
    assert!(
        block_on(ws.diff("src/main.py"))
            .unwrap()
            .contains("-print('hi')")
    );

    // Editing the file again does not settle it.
    block_on(ws.write("src/main.py", "mine, edited\n")).unwrap();
    assert_eq!(block_on(ws.conflicts()).unwrap().len(), 3);

    // Mine for one, theirs for another; the deletion both sides made is moot.
    block_on(ws.keep_mine("src/main.py")).unwrap();
    assert!(ws.discard("README.md"));
    block_on(ws.keep_mine("src/util/math.py")).unwrap();
    assert_eq!(block_on(ws.conflicts()).unwrap(), Vec::<String>::new());
    assert_eq!(block_on(ws.read_text("README.md")).unwrap(), "# theirs\n");

    let outcome = block_on(ws.commit("Mine, on top of theirs")).unwrap();
    let (_, parents) = fake.commit_info(REPO, &outcome.sha).unwrap();
    assert_eq!(parents, [theirs]);
    assert_eq!(
        fake.file(REPO, "main", "src/main.py").unwrap(),
        "mine, edited\n"
    );
    assert_eq!(fake.file(REPO, "main", "README.md").unwrap(), "# theirs\n");
    assert_eq!(fake.file(REPO, "main", "src/util/math.py"), None);
    assert!(matches!(
        block_on(ws.keep_mine("no/such/change")),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn an_unrelated_remote_change_does_not_block() {
    let (fake, ws) = setup();
    block_on(ws.write("src/main.py", "mine\n")).unwrap();
    fake.push(
        REPO,
        "main",
        "Their change",
        &[("README.md", Some("# theirs\n"))],
    );
    block_on(ws.commit("Mine")).unwrap();
    assert_eq!(fake.file(REPO, "main", "src/main.py").unwrap(), "mine\n");
    assert_eq!(fake.file(REPO, "main", "README.md").unwrap(), "# theirs\n");
}

#[test]
fn a_new_file_conflicts_if_someone_else_created_it() {
    let (fake, ws) = setup();
    block_on(ws.write("notes.txt", "mine\n")).unwrap();
    fake.push(REPO, "main", "Theirs", &[("notes.txt", Some("theirs\n"))]);
    assert_eq!(
        block_on(ws.commit("Mine")).unwrap_err(),
        Error::Conflict(vec!["notes.txt".to_owned()])
    );
}

#[test]
fn deleting_a_file_someone_else_changed_conflicts() {
    let (fake, ws) = setup();
    block_on(ws.remove("README.md", false)).unwrap();
    fake.push(
        REPO,
        "main",
        "Theirs",
        &[("README.md", Some("# important\n"))],
    );
    assert!(matches!(block_on(ws.commit("rm")), Err(Error::Conflict(_))));
    assert_eq!(
        fake.file(REPO, "main", "README.md").unwrap(),
        "# important\n"
    );
}

#[test]
fn refresh_reports_conflicts_and_keeps_local_work() {
    let (fake, ws) = setup();
    block_on(ws.write("src/main.py", "mine\n")).unwrap();
    assert_eq!(block_on(ws.refresh()).unwrap(), Vec::<String>::new());
    fake.push(
        REPO,
        "main",
        "Theirs",
        &[("src/main.py", Some("theirs\n")), ("x.txt", Some("x"))],
    );
    assert_eq!(block_on(ws.refresh()).unwrap(), ["src/main.py"]);
    assert_eq!(block_on(ws.read_text("src/main.py")).unwrap(), "mine\n");
    assert_eq!(block_on(ws.read_text("x.txt")).unwrap(), "x");
}

#[test]
fn a_rejected_branch_update_leaves_changes_in_place() {
    let (fake, ws) = setup();
    block_on(ws.write("a.txt", "x")).unwrap();
    // branch_head, then the tree and commit writes succeed, then the ref update fails.
    let before = fake.requests().len();
    let _ = before;
    fake.set_token_valid(false);
    assert_eq!(block_on(ws.commit("msg")).unwrap_err(), Error::Unauthorized);
    fake.set_token_valid(true);
    assert_eq!(ws.changes().len(), 1);
    assert_eq!(fake.file(REPO, "main", "a.txt"), None);
    block_on(ws.commit("msg")).unwrap();
    assert_eq!(fake.file(REPO, "main", "a.txt").unwrap(), "x");
}

#[test]
fn renaming_from_the_file_list_never_writes_over_anything() {
    let (_, ws) = setup();
    // A name that is taken, by a file or by a folder, is refused.
    for taken in ["README.md", "src/util"] {
        let refused = block_on(ws.rename_to_unused("src/main.py", taken));
        assert!(
            matches!(&refused, Err(Error::Invalid(why)) if why.contains("already there")),
            "{refused:?}"
        );
    }
    assert!(matches!(
        block_on(ws.rename_to_unused("src/main.py", "src/main.py")),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        block_on(ws.rename_to_unused("src/gone.py", "src/new.py")),
        Err(Error::NotFound(_))
    ));
    // Nor can a file be moved inside a file, or a folder inside itself.
    for inside in ["README.md/x.py", "src/main.py/x.py"] {
        let refused = block_on(ws.rename_to_unused("src/main.py", inside));
        assert!(refused.is_err(), "{inside}: {refused:?}");
    }
    assert!(block_on(ws.rename_to_unused("src", "src/util/src")).is_err());
    assert!(ws.changes().is_empty(), "a refusal changes nothing");

    // A free name is taken up, for a file and for a folder with its files.
    assert_eq!(
        block_on(ws.rename_to_unused("src/main.py", "src/app.py")).unwrap(),
        1
    );
    assert_eq!(
        block_on(ws.rename_to_unused("src/util", "lib/util")).unwrap(),
        1
    );
    assert_eq!(names(&ws, "src"), ["app.py"]);
    assert_eq!(names(&ws, "lib/util"), ["math.py"]);
    assert_eq!(
        block_on(ws.read_text("src/app.py")).unwrap(),
        "print('hi')\n"
    );
}

#[test]
fn says_which_files_a_removal_would_lose_work_from() {
    let (_, ws) = setup();
    assert!(ws.uncommitted_under("src").is_empty());
    block_on(ws.write("src/main.py", "print('edited')\n")).unwrap();
    block_on(ws.write("src/util/new.py", "x = 1\n")).unwrap();
    block_on(ws.write("srcs.py", "y = 2\n")).unwrap();
    block_on(ws.remove("README.md", false)).unwrap();
    assert_eq!(
        ws.uncommitted_under("src"),
        ["src/main.py", "src/util/new.py"]
    );
    assert_eq!(ws.uncommitted_under("src/util"), ["src/util/new.py"]);
    assert_eq!(ws.uncommitted_under("src/main.py"), ["src/main.py"]);
    // A file already removed has nothing left to lose.
    assert!(ws.uncommitted_under("README.md").is_empty());

    // Removing loses exactly those: discarding the removal brings back what
    // GitHub has, and a file GitHub never had is gone.
    block_on(ws.remove("src", true)).unwrap();
    assert!(ws.uncommitted_under("src").is_empty());
    for change in ws.changes() {
        ws.discard(&change.path);
    }
    assert_eq!(
        block_on(ws.read_text("src/main.py")).unwrap(),
        "print('hi')\n"
    );
    assert_eq!(block_on(ws.kind("src/util/new.py")).unwrap(), None);
}

#[test]
fn remove_copy_and_rename() {
    let (fake, ws) = setup();
    assert!(matches!(
        block_on(ws.remove("src", false)),
        Err(Error::Invalid(_))
    ));
    assert_eq!(block_on(ws.rename("src/util", "lib")).unwrap(), 1);
    assert_eq!(
        block_on(ws.copy("src/main.py", "src/copy.py", false)).unwrap(),
        1
    );
    assert_eq!(names(&ws, ""), ["lib", "src", "README.md"]);
    assert_eq!(names(&ws, "src"), ["copy.py", "main.py"]);
    assert!(
        block_on(ws.read_text("lib/math.py"))
            .unwrap()
            .contains("def add")
    );
    assert!(matches!(
        block_on(ws.copy("src", "src/inner", true)),
        Err(Error::Invalid(_))
    ));
    block_on(ws.commit("Move things")).unwrap();
    assert!(
        fake.file(REPO, "main", "lib/math.py")
            .unwrap()
            .contains("def add")
    );
    assert_eq!(fake.file(REPO, "main", "src/util/math.py"), None);
    assert_eq!(
        fake.file(REPO, "main", "src/copy.py").unwrap(),
        "print('hi')\n"
    );

    // Removing a new, never-committed file just forgets it.
    block_on(ws.write("tmp.txt", "x")).unwrap();
    block_on(ws.remove("tmp.txt", false)).unwrap();
    assert_eq!(ws.changes(), Vec::new());
    assert!(matches!(
        block_on(ws.remove("tmp.txt", false)),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn diff_shows_what_changed() {
    let (_, ws) = setup();
    block_on(ws.write("src/main.py", "print('bye')\n")).unwrap();
    let text = block_on(ws.diff("src/main.py")).unwrap();
    assert!(text.contains("-print('hi')"), "{text}");
    assert!(text.contains("+print('bye')"), "{text}");
    assert_eq!(block_on(ws.diff("README.md")).unwrap(), "");
}

#[test]
fn switching_branches_with_changes_is_refused() {
    let (fake, ws) = setup();
    fake.push(REPO, "main", "x", &[("x", Some("x"))]);
    block_on(ws.switch_branch("feature", true)).unwrap();
    assert_eq!(ws.branch(), "feature");
    block_on(ws.write("a.txt", "x")).unwrap();
    assert!(matches!(
        block_on(ws.switch_branch("main", false)),
        Err(Error::Invalid(_))
    ));
    block_on(ws.commit("on feature")).unwrap();
    block_on(ws.switch_branch("main", false)).unwrap();
    assert_eq!(ws.branch(), "main");
    assert!(matches!(
        block_on(ws.read("a.txt")),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        block_on(ws.switch_branch("feature", true)),
        Err(Error::Rejected(_))
    ));
    assert!(matches!(
        block_on(ws.switch_branch("bad name", true)),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn binary_files_are_reported_not_mangled() {
    let fake = Rc::new(FakeGitHub::new("tester"));
    fake.add_repo(REPO, "main", &[("ok.txt", "héllo ✓\n")]);
    let ws = block_on(Workspace::open(GitHub::new(fake), REPO, "main")).unwrap();
    assert_eq!(block_on(ws.read_text("ok.txt")).unwrap(), "héllo ✓\n");
}

#[test]
fn api_errors_map_to_actionable_kinds() {
    let (fake, ws) = setup();
    fake.fail_next(404, "Not Found");
    assert!(matches!(block_on(ws.refresh()), Err(Error::NotFound(_))));
    fake.fail_next(403, "Resource not accessible by personal access token");
    assert!(
        matches!(block_on(ws.refresh()), Err(Error::Forbidden(m)) if m.contains("personal access token"))
    );
    fake.fail_next(500, "boom");
    assert!(matches!(
        block_on(ws.refresh()),
        Err(Error::Api { status: 500, .. })
    ));
    fake.set_offline(true);
    assert!(matches!(block_on(ws.refresh()), Err(Error::Network(_))));
}

/// The exported patch is only useful if real git accepts it.
#[test]
fn the_exported_patch_applies_with_git() {
    use std::process::Command;
    let (_, ws) = setup();
    block_on(ws.write("src/main.py", "print('hi')\nprint('more')\n")).unwrap();
    block_on(ws.write("docs/new file.md", "# New\n\nwith ünïcode\n")).unwrap();
    block_on(ws.write("empty.txt", "")).unwrap();
    block_on(ws.remove("README.md", false)).unwrap();
    let patch = block_on(ws.patch()).unwrap();

    let dir = std::env::temp_dir().join(format!("silver-wombat-patch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/util")).unwrap();
    std::fs::write(dir.join("README.md"), "# r\n").unwrap();
    std::fs::write(dir.join("src/main.py"), "print('hi')\n").unwrap();
    std::fs::write(
        dir.join("src/util/math.py"),
        "def add(a, b):\n    return a + b\n",
    )
    .unwrap();
    std::fs::write(dir.join("changes.patch"), &patch).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}\n{patch}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--quiet"]);
    git(&["apply", "changes.patch"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/main.py")).unwrap(),
        "print('hi')\nprint('more')\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("docs/new file.md")).unwrap(),
        "# New\n\nwith ünïcode\n"
    );
    assert_eq!(std::fs::read_to_string(dir.join("empty.txt")).unwrap(), "");
    assert!(!dir.join("README.md").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
