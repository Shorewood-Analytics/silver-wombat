//! The same client and workspace code, run against the real GitHub API.
//!
//! Ignored by default because it writes to a real repository. To run it:
//!
//! ```sh
//! SILVER_WOMBAT_LIVE_REPO=owner/name GITHUB_TOKEN=... \
//!     cargo test -p silver-wombat-core --test live -- --ignored --nocapture
//! ```
//!
//! It creates two branches in that repository and leaves them there for
//! inspection: `live-test/<time>` and `autosave/live-test/<time>`. It never
//! touches any other branch. Requests are made with the `curl` command, so
//! the test adds no HTTP dependency to the crate.

use std::process::Command;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::executor::block_on;
use silver_wombat_core::Error;
use silver_wombat_core::github::GitHub;
use silver_wombat_core::http::{Http, LocalBoxFuture, Request, Response};
use silver_wombat_core::shell::Shell;
use silver_wombat_core::workspace::Workspace;

struct Curl {
    token: Option<String>,
}

impl Http for Curl {
    fn send(&self, request: Request) -> LocalBoxFuture<'_, Result<Response, String>> {
        Box::pin(async move {
            let mut command = Command::new("curl");
            command
                .args([
                    "--silent",
                    "--show-error",
                    "--request",
                    request.method.as_str(),
                ])
                .args(["--header", "Accept: application/vnd.github+json"])
                .args(["--write-out", "\n%{http_code}"]);
            if let Some(token) = &self.token {
                command.args(["--header", &format!("Authorization: Bearer {token}")]);
            }
            if let Some(body) = &request.body {
                command.args(["--header", "Content-Type: application/json", "--data", body]);
            }
            command.arg(format!("https://api.github.com{}", request.path));
            let output = command.output().map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned());
            }
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            let (body, status) = text.rsplit_once('\n').ok_or("no status line")?;
            Ok(Response {
                status: status.trim().parse().map_err(|_| "bad status")?,
                body: body.to_owned(),
                ..Response::default()
            })
        })
    }
}

fn run(shell: &Shell, workspace: &Workspace, line: &str) -> (String, i32) {
    let output = block_on(shell.run(line, workspace));
    println!("$ {line}\n{}", output.text);
    (output.text, output.status)
}

// One scenario, told in order; splitting it would hide the story.
#[allow(clippy::too_many_lines)]
#[test]
#[ignore = "writes to a real GitHub repository; see the file header"]
fn workspace_against_real_github() {
    let repo = std::env::var("SILVER_WOMBAT_LIVE_REPO").expect("set SILVER_WOMBAT_LIVE_REPO");
    let github = GitHub::new(Rc::new(Curl {
        token: std::env::var("GITHUB_TOKEN").ok(),
    }));
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let branch = format!("live-test/{stamp}");

    let user = block_on(github.user()).unwrap();
    println!("signed in as {}", user.login);
    let info = block_on(github.repo(&repo)).unwrap();
    assert_eq!(info.full_name.to_lowercase(), repo.to_lowercase());
    assert!(
        block_on(github.branches(&repo))
            .unwrap()
            .contains(&info.default_branch)
    );
    assert_eq!(
        block_on(github.branch_head(&repo, "no-such-branch-here")).unwrap(),
        None
    );

    let workspace = block_on(Workspace::open(github.clone(), &repo, &info.default_branch)).unwrap();
    let shell = Shell::new();
    let base = workspace.head();
    assert_eq!(run(&shell, &workspace, "ls -a").1, 0);
    assert_eq!(run(&shell, &workspace, "git log -n 2").1, 0);

    // A branch of our own, so nothing else is touched.
    assert_eq!(
        run(&shell, &workspace, &format!("git switch -c {branch}")).1,
        0
    );
    assert_eq!(
        block_on(github.branch_head(&repo, &branch)).unwrap(),
        Some(base.clone())
    );
    // Creating it again must be refused, not overwritten.
    assert!(matches!(
        block_on(github.create_branch(&repo, &branch, &base)),
        Err(Error::Rejected(_))
    ));

    run(&shell, &workspace, "echo first line > live-test/a.txt");
    run(
        &shell,
        &workspace,
        "echo 'naïve ✓ 日本語' > live-test/deep/unicode.txt",
    );
    let checkpoint = block_on(workspace.checkpoint())
        .unwrap()
        .expect("a checkpoint");
    let autosave = workspace.autosave_branch();
    assert_eq!(
        block_on(github.branch_head(&repo, &autosave)).unwrap(),
        Some(checkpoint.clone())
    );
    assert_eq!(
        block_on(github.commit(&repo, &checkpoint)).unwrap().parents,
        [base.clone()]
    );
    // The branch itself has not moved.
    assert_eq!(
        block_on(github.branch_head(&repo, &branch)).unwrap(),
        Some(base.clone())
    );
    assert_eq!(block_on(workspace.checkpoint()).unwrap(), None);

    let (text, code) = run(
        &shell,
        &workspace,
        "git commit -m 'Live test: add two files'",
    );
    assert_eq!(code, 0, "{text}");
    let first = workspace.head();
    let commit = block_on(github.commit(&repo, &first)).unwrap();
    assert_eq!(commit.parents, std::slice::from_ref(&base));
    assert_eq!(commit.message, "Live test: add two files");

    // A second workspace reads back exactly what was written.
    let reader = block_on(Workspace::open(github.clone(), &repo, &branch)).unwrap();
    assert_eq!(
        block_on(reader.read_text("live-test/a.txt")).unwrap(),
        "first line\n"
    );
    assert_eq!(
        block_on(reader.read_text("live-test/deep/unicode.txt")).unwrap(),
        "naïve ✓ 日本語\n"
    );

    // "Someone else" (the second workspace) changes a file. Our stale edit to
    // the same file must be refused, and nothing may be written.
    block_on(reader.write("live-test/a.txt", "changed elsewhere\n")).unwrap();
    let theirs = block_on(reader.commit("Live test: a change made elsewhere"))
        .unwrap()
        .sha;
    run(&shell, &workspace, "echo mine >> live-test/a.txt");
    run(&shell, &workspace, "rm live-test/deep/unicode.txt");
    let (text, code) = run(
        &shell,
        &workspace,
        "git commit -m 'Live test: must be refused'",
    );
    assert_eq!(code, 1);
    assert!(
        text.contains("Changed on GitHub since you opened it: live-test/a.txt"),
        "{text}"
    );
    assert_eq!(
        block_on(github.branch_head(&repo, &branch)).unwrap(),
        Some(theirs.clone())
    );

    // Give up our edit to that file; the deletion still commits, on top of theirs.
    run(&shell, &workspace, "git restore live-test/a.txt");
    assert_eq!(run(&shell, &workspace, "git pull").1, 0);
    assert_eq!(
        run(&shell, &workspace, "cat live-test/a.txt").0,
        "changed elsewhere\n"
    );
    let (text, code) = run(
        &shell,
        &workspace,
        "git commit -m 'Live test: delete a file'",
    );
    assert_eq!(code, 0, "{text}");
    let last = block_on(github.commit(&repo, &workspace.head())).unwrap();
    assert_eq!(last.parents, [theirs]);
    let after = block_on(Workspace::open(github.clone(), &repo, &branch)).unwrap();
    assert!(matches!(
        block_on(after.read("live-test/deep/unicode.txt")),
        Err(Error::NotFound(_))
    ));
    assert_eq!(
        block_on(after.read_text("live-test/a.txt")).unwrap(),
        "changed elsewhere\n"
    );
    // Removing the only file in a folder removed the folder.
    assert_eq!(block_on(after.kind("live-test/deep")).unwrap(), None);

    // A branch update that is not a fast-forward is refused by GitHub itself.
    assert!(matches!(
        block_on(github.advance_branch(&repo, &branch, &first)),
        Err(Error::Rejected(_))
    ));
    println!("left for inspection: {branch} and {autosave}");
}
