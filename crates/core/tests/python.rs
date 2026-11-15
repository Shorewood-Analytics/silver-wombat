//! `python`, `pip` and `uv` in the terminal, with a stand-in interpreter.

use std::cell::RefCell;
use std::rc::Rc;

use futures::executor::block_on;
use silver_wombat_core::fake::FakeGitHub;
use silver_wombat_core::github::GitHub;
use silver_wombat_core::http::LocalBoxFuture;
use silver_wombat_core::python::{
    Entry, Job, MAX_MOUNT_FILES, Outcome, PythonHost, PythonTool, RUNTIMES,
};
use silver_wombat_core::shell::Shell;
use silver_wombat_core::workspace::{ChangeKind, Workspace};

const REPO: &str = "t/r";

/// Records what it was asked to run and answers with what the test set.
#[derive(Default)]
struct FakePython {
    jobs: RefCell<Vec<Job>>,
    answer: RefCell<Option<Result<Outcome, String>>>,
}

impl FakePython {
    fn answer(&self, outcome: Outcome) {
        *self.answer.borrow_mut() = Some(Ok(outcome));
    }

    fn last(&self) -> Job {
        self.jobs.borrow().last().expect("Python was run").clone()
    }

    fn runs(&self) -> usize {
        self.jobs.borrow().len()
    }
}

impl PythonHost for FakePython {
    fn run(&self, job: Job) -> LocalBoxFuture<'_, Result<Outcome, String>> {
        self.jobs.borrow_mut().push(job);
        let answer = self
            .answer
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Ok(Outcome::default()));
        Box::pin(async move { answer })
    }
}

fn setup_with(files: &[(&str, &str)]) -> (Rc<FakePython>, Workspace, Shell) {
    let fake = Rc::new(FakeGitHub::new("tester"));
    fake.add_repo(REPO, "main", files);
    let workspace = block_on(Workspace::open(GitHub::new(fake), REPO, "main")).unwrap();
    let python = Rc::new(FakePython::default());
    let shell = Shell::new();
    shell.add_tool(Rc::new(PythonTool::new(python.clone())));
    (python, workspace, shell)
}

fn setup() -> (Rc<FakePython>, Workspace, Shell) {
    setup_with(&[
        ("README.md", "# r\n"),
        ("src/main.py", "import util\nprint(util.add(1, 2))\n"),
        ("src/util.py", "def add(a, b):\n    return a + b\n"),
    ])
}

fn run(shell: &Shell, ws: &Workspace, line: &str) -> (String, i32) {
    let output = block_on(shell.run(line, ws));
    (output.text, output.status)
}

#[test]
fn the_version_table_matches_the_interpreters_the_site_ships() {
    let manifest = include_str!("../../../web/python/package.json");
    let manifest: serde_json::Value = serde_json::from_str(manifest).unwrap();
    let shipped = manifest["dependencies"].as_object().unwrap();
    assert_eq!(shipped.len(), RUNTIMES.len());
    for runtime in RUNTIMES {
        let alias = format!("py3{}", runtime.minor);
        assert_eq!(
            shipped[&alias],
            format!("npm:pyodide@{}", runtime.npm),
            "{alias}"
        );
        assert_eq!(runtime.python, format!("3.{}", runtime.minor));
        assert!(runtime.exact.starts_with(runtime.python));
        assert_eq!(
            runtime.stable,
            runtime
                .exact
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.')
        );
    }
}

#[test]
fn runs_a_file_with_the_repository_mounted() {
    let (python, ws, sh) = setup();
    python.answer(Outcome {
        output: "3\n".to_owned(),
        ..Outcome::default()
    });
    assert_eq!(
        run(&sh, &ws, "python src/main.py one 'two words'"),
        ("3\n".to_owned(), 0)
    );
    let job = python.last();
    assert_eq!(job.python, "3.14");
    assert_eq!(job.pyodide, "314.0.8");
    assert_eq!(job.entry, Entry::File("src/main.py".to_owned()));
    assert_eq!(job.args, ["one", "two words"]);
    assert_eq!(job.cwd, "");
    assert_eq!(job.stdin, None);
    assert!(job.install.is_empty());
    let paths: Vec<&str> = job.files.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(paths, ["README.md", "src/main.py", "src/util.py"]);

    // Paths follow the shell's folder, and unsaved edits are what Python sees.
    run(&sh, &ws, "echo 'print(1)' > src/main.py");
    run(&sh, &ws, "cd src && python3 main.py");
    let job = python.last();
    assert_eq!(job.entry, Entry::File("src/main.py".to_owned()));
    assert_eq!(job.cwd, "src");
    assert_eq!(
        job.files[1],
        ("src/main.py".to_owned(), "print(1)\n".to_owned())
    );
}

#[test]
fn a_missing_file_or_unsupported_option_never_reaches_python() {
    let (python, ws, sh) = setup();
    let (text, code) = run(&sh, &ws, "python nope.py");
    assert_eq!(code, 2);
    assert!(text.contains("can't open file nope.py"), "{text}");
    let (text, code) = run(&sh, &ws, "python");
    assert_eq!(code, 2);
    assert!(
        text.contains("interactive prompt is not available"),
        "{text}"
    );
    let (text, code) = run(&sh, &ws, "python -i src/main.py");
    assert_eq!(code, 2);
    assert!(text.contains("option -i is not supported"), "{text}");
    assert_eq!(run(&sh, &ws, "pip list").1, 2);
    assert_eq!(run(&sh, &ws, "pip install").1, 2);
    assert_eq!(run(&sh, &ws, "pip install --upgrade rich").1, 2);
    let (text, code) = run(&sh, &ws, "uv add rich");
    assert_eq!(code, 2);
    assert!(text.contains("no virtual environment"), "{text}");
    let (text, code) = run(&sh, &ws, "uv tool install ruff");
    assert_eq!(code, 2);
    assert!(text.contains("cannot run in a browser"), "{text}");
    assert_eq!(python.runs(), 0);
}

#[test]
fn code_modules_and_piped_input() {
    let (python, ws, sh) = setup();
    run(&sh, &ws, "python -c 'print(6 * 7)' extra");
    assert_eq!(python.last().entry, Entry::Code("print(6 * 7)".to_owned()));
    assert_eq!(python.last().args, ["extra"]);
    run(&sh, &ws, "python -m pytest -q");
    assert_eq!(python.last().entry, Entry::Module("pytest".to_owned()));
    assert_eq!(python.last().args, ["-q"]);
    run(
        &sh,
        &ws,
        "cat README.md | python -c 'import sys; print(sys.stdin.read())'",
    );
    assert_eq!(python.last().stdin.as_deref(), Some("# r\n"));

    // What Python prints goes down a pipe like any other command's output.
    python.answer(Outcome {
        output: "a\nb\nc\n".to_owned(),
        ..Outcome::default()
    });
    assert_eq!(run(&sh, &ws, "python -c 'x' | wc -l").0.trim(), "3");
    python.answer(Outcome {
        output: "kept\n".to_owned(),
        ..Outcome::default()
    });
    run(&sh, &ws, "python -c 'x' > out.txt");
    assert_eq!(block_on(ws.read_text("out.txt")).unwrap(), "kept\n");
}

#[test]
fn the_exit_status_and_error_text_come_back() {
    let (python, ws, sh) = setup();
    python.answer(Outcome {
        output: "Traceback (most recent call last):\nZeroDivisionError: division by zero\n"
            .to_owned(),
        status: 1,
        ..Outcome::default()
    });
    let (text, code) = run(&sh, &ws, "python src/main.py && echo not reached");
    assert_eq!(code, 1);
    assert!(text.contains("ZeroDivisionError"), "{text}");
    assert!(!text.contains("not reached"), "{text}");

    *python.answer.borrow_mut() = Some(Err("the download failed".to_owned()));
    let (text, code) = run(&sh, &ws, "python src/main.py");
    assert_eq!(code, 1);
    assert!(
        text.contains("could not start Python 3.14: the download failed"),
        "{text}"
    );
}

#[test]
fn files_the_program_wrote_become_unsaved_changes() {
    let (python, ws, sh) = setup();
    python.answer(Outcome {
        changed: vec![
            ("out/report.txt".to_owned(), Some("total 3\n".to_owned())),
            (
                "src/util.py".to_owned(),
                Some("def add(a, b):\n    return b + a\n".to_owned()),
            ),
            ("README.md".to_owned(), None),
        ],
        binary: vec!["plot.png".to_owned()],
        ..Outcome::default()
    });
    let (text, code) = run(&sh, &ws, "python src/main.py");
    assert_eq!(code, 0);
    assert!(text.contains("changed 3 files; see Changes"), "{text}");
    assert!(
        text.contains("not kept") && text.contains("plot.png"),
        "{text}"
    );
    let changes: Vec<(String, ChangeKind)> = ws
        .changes()
        .into_iter()
        .map(|change| (change.path, change.kind))
        .collect();
    assert_eq!(
        changes,
        [
            ("README.md".to_owned(), ChangeKind::Deleted),
            ("out/report.txt".to_owned(), ChangeKind::Added),
            ("src/util.py".to_owned(), ChangeKind::Modified),
        ]
    );
    assert_eq!(
        block_on(ws.read_text("out/report.txt")).unwrap(),
        "total 3\n"
    );
}

#[test]
fn the_version_follows_the_project() {
    let (python, ws, sh) = setup();
    assert_eq!(
        run(&sh, &ws, "python --version").0,
        "Python 3.14.2 (Pyodide 314.0.8)\n"
    );
    let (text, _) = run(&sh, &ws, "uv python list");
    assert!(text.contains("* cpython-3.14.2-pyodide-314.0.8"), "{text}");
    assert!(
        text.contains("  cpython-3.15.0rc1-pyodide-315.0.0a2  (pre-release)"),
        "{text}"
    );

    // pyproject.toml: the newest stable version it allows.
    run(&sh, &ws, "echo '[project]' > pyproject.toml");
    run(
        &sh,
        &ws,
        "echo 'requires-python = \">=3.12,<3.14\"' >> pyproject.toml",
    );
    assert_eq!(
        run(&sh, &ws, "python -V").0,
        "Python 3.13.2 (Pyodide 0.29.5)\n"
    );

    // .python-version wins over it.
    let (text, code) = run(&sh, &ws, "uv python pin 3.12");
    assert_eq!(
        (text.as_str(), code),
        ("Pinned .python-version to 3.12\n", 0)
    );
    assert_eq!(block_on(ws.read_text(".python-version")).unwrap(), "3.12\n");
    run(&sh, &ws, "python -c 1");
    assert_eq!(python.last().python, "3.12");
    assert_eq!(python.last().pyodide, "0.27.7");
    assert!(
        run(&sh, &ws, "uv python pin 3.15")
            .0
            .contains("pre-release")
    );
    assert_eq!(
        run(&sh, &ws, "python -V").0,
        "Python 3.15.0rc1 (Pyodide 315.0.0a2)\n"
    );

    assert_eq!(run(&sh, &ws, "uv python pin 3.11").1, 2);
    run(&sh, &ws, "echo 3.9 > .python-version");
    let runs = python.runs();
    let (text, code) = run(&sh, &ws, "python src/main.py");
    assert_eq!(code, 1);
    assert!(
        text.contains("asks for Python 3.9, which is not available"),
        "{text}"
    );
    assert_eq!(python.runs(), runs);
}

#[test]
fn uv_run_installs_what_the_script_declares() {
    let (python, ws, sh) = setup_with(&[
        (
            "tool.py",
            "# /// script\n# requires-python = \">=3.12,<3.14\"\n# dependencies = [\n#   \"rich\",\n#   \"httpx<1\",\n# ]\n# ///\nimport rich\n",
        ),
        ("plain.py", "print(1)\n"),
        (
            "requirements.txt",
            "# pinned\nattrs==25.1  # why\n\npyyaml\n",
        ),
    ]);
    run(&sh, &ws, "uv run tool.py --flag");
    let job = python.last();
    assert_eq!(job.python, "3.13");
    assert_eq!(job.install, ["rich", "httpx<1"]);
    assert_eq!(job.entry, Entry::File("tool.py".to_owned()));
    assert_eq!(job.args, ["--flag"]);

    run(&sh, &ws, "uv run plain.py");
    assert_eq!(python.last().python, "3.14");
    assert!(python.last().install.is_empty());
    run(&sh, &ws, "uv run python -c 'print(1)'");
    assert_eq!(python.last().entry, Entry::Code("print(1)".to_owned()));

    let (text, code) = run(&sh, &ws, "pip install rich 'httpx<1'");
    assert_eq!(code, 0);
    assert!(
        text.contains("Installed for this session: rich, httpx<1"),
        "{text}"
    );
    assert_eq!(python.last().install, ["rich", "httpx<1"]);
    assert_eq!(python.last().entry, Entry::Nothing);
    run(&sh, &ws, "uv pip install -r requirements.txt");
    assert_eq!(python.last().install, ["attrs==25.1", "pyyaml"]);

    // A failed install does not claim success.
    python.answer(Outcome {
        output: "Could not get a library: no such package\n".to_owned(),
        status: 1,
        ..Outcome::default()
    });
    let (text, code) = run(&sh, &ws, "pip install nonesuch");
    assert_eq!(code, 1);
    assert!(!text.contains("Installed"), "{text}");
}

#[test]
fn a_large_repository_is_mounted_up_to_the_limit_and_says_so() {
    let names: Vec<String> = (0..MAX_MOUNT_FILES + 5)
        .map(|i| format!("data/f{i:04}.txt"))
        .collect();
    let files: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "x\n")).collect();
    let (python, ws, sh) = setup_with(&files);
    let (text, _) = run(&sh, &ws, "python -c 1");
    assert_eq!(python.last().files.len(), MAX_MOUNT_FILES);
    assert!(
        text.contains(&format!(
            "first {MAX_MOUNT_FILES} files; 5 more were left out"
        )),
        "{text}"
    );
}

#[test]
fn help_and_completion_include_python_once_it_is_there() {
    let (_, ws, sh) = setup();
    let help = run(&sh, &ws, "help").0;
    assert!(
        help.contains("  python   run Python in the browser"),
        "{help}"
    );
    assert!(
        help.contains("Not available: cargo, npm, node, make"),
        "{help}"
    );
    assert_eq!(block_on(sh.complete("uv", &ws)).line, "uv ");
    assert_eq!(
        block_on(sh.complete("pyt", &ws)).candidates,
        ["python", "python3"]
    );
    assert!(
        run(&sh, &ws, "python --help")
            .0
            .contains("uv python pin VERSION")
    );
    assert!(run(&sh, &ws, "uv").0.contains("cannot run in a browser"));

    let plain = Shell::new();
    assert!(
        run(&plain, &ws, "help")
            .0
            .contains("Not available: python, uv, pip, cargo")
    );
}
