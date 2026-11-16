//! `python`, `pip` and `uv` in the terminal.
//!
//! Python runs in the browser through Pyodide (the standard interpreter compiled to
//! WebAssembly). This module is the part that needs no browser: it reads the
//! command line, picks the Python version, collects the files the program
//! will see, and writes back the files it changed. Running the interpreter is
//! behind [`PythonHost`], which the app implements with a web worker.
//!
//! `uv` itself is a native program and cannot run in a browser. The `uv`
//! commands here are a small, compatible subset implemented on top of
//! Pyodide and its installer, and they say so.

use std::fmt::Write as _;
use std::rc::Rc;

use futures_util::stream::{self, StreamExt as _};
use serde::{Deserialize, Serialize};

use crate::http::LocalBoxFuture;
use crate::path;
use crate::shell::{External, ExternalRun, Invocation};
use crate::workspace::{FETCH_AT_ONCE, FileContent, Workspace};

/// A Python version the terminal can run, and the Pyodide release that
/// provides it. Checked by loading each release on 2026-11-15.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Runtime {
    /// `major.minor`, as people write it.
    pub python: &'static str,
    pub minor: u32,
    /// The exact interpreter version in that Pyodide release.
    pub exact: &'static str,
    /// The Pyodide release, as it names itself and as its CDN folder is named.
    pub pyodide: &'static str,
    /// The same release as npm names it (`web/python/package.json`).
    pub npm: &'static str,
    /// False for a pre-release interpreter.
    pub stable: bool,
}

/// Oldest first.
pub const RUNTIMES: [Runtime; 4] = [
    Runtime {
        python: "3.12",
        minor: 12,
        exact: "3.12.7",
        pyodide: "0.27.7",
        npm: "0.27.7",
        stable: true,
    },
    Runtime {
        python: "3.13",
        minor: 13,
        exact: "3.13.2",
        pyodide: "0.29.5",
        npm: "0.29.5",
        stable: true,
    },
    Runtime {
        python: "3.14",
        minor: 14,
        exact: "3.14.2",
        pyodide: "314.0.8",
        npm: "314.0.8",
        stable: true,
    },
    Runtime {
        python: "3.15",
        minor: 15,
        exact: "3.15.0rc1",
        pyodide: "315.0.0a2",
        npm: "315.0.0-alpha.2",
        stable: false,
    },
];

/// The version used when the project does not ask for one: the newest stable.
pub const DEFAULT_RUNTIME: Runtime = RUNTIMES[2];

/// Most files handed to one Python run.
pub const MAX_MOUNT_FILES: usize = 400;
/// Most bytes handed to one Python run.
pub const MAX_MOUNT_BYTES: usize = 16 * 1024 * 1024;
/// From this many files on, fetching them is slow enough to be worth a note.
const SLOW_MOUNT_FILES: usize = 30;

/// What to execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Entry {
    /// `python path/to/file.py`
    File(String),
    /// `python -c "code"`
    Code(String),
    /// `python -m module`
    Module(String),
    /// Only install packages.
    Nothing,
}

/// One request to the interpreter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// The Python version, `major.minor`. The runtime is served from the
    /// site's `python/<this>/` folder.
    pub python: String,
    /// The Pyodide release that is. Libraries come from its CDN folder.
    pub pyodide: String,
    /// Repository files, as `(path, text)`. Paths are relative to the root.
    pub files: Vec<(String, String)>,
    /// The folder to run in, relative to the repository root.
    pub cwd: String,
    /// Packages to install first.
    pub install: Vec<String>,
    pub entry: Entry,
    /// `sys.argv[1:]`.
    pub args: Vec<String>,
    /// Text for the program to read from standard input.
    pub stdin: Option<String>,
    /// Hand over output as it is produced, for a terminal to show while the
    /// program runs, instead of all at once in [`Outcome::output`].
    #[serde(default)]
    pub stream: bool,
}

/// What the interpreter reported back.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Outcome {
    /// Standard output and standard error, in the order they were written.
    /// Empty if the job asked for it to be streamed instead.
    pub output: String,
    /// The process exit status.
    pub status: i32,
    /// Files the program created or changed (`Some`) or deleted (`None`).
    pub changed: Vec<(String, Option<String>)>,
    /// Files the program wrote that are not text, which cannot be kept yet.
    #[serde(default)]
    pub binary: Vec<String>,
    /// Pictures the program wrote, to be shown.
    #[serde(default)]
    pub pictures: Vec<Picture>,
}

/// A picture a program wrote, such as a plot.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Picture {
    pub path: String,
    /// Its media type, such as `image/png`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The file's bytes in base64.
    pub data: String,
}

impl Picture {
    /// The file's bytes, or `None` if the data is not valid base64.
    #[must_use]
    pub fn bytes(&self) -> Option<Vec<u8>> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(&self.data)
            .ok()
    }
}

/// Runs Python. `Err` means the interpreter could not be started at all, for
/// example because it could not be downloaded.
pub trait PythonHost {
    fn run(&self, job: Job) -> LocalBoxFuture<'_, Result<Outcome, String>>;

    /// Ends the running program, if there is one. Its `run` then finishes
    /// with status 130. The default does nothing.
    fn stop(&self) {}

    /// Shows a short note at once, before a wait. The default shows nothing.
    fn note(&self, _text: &str) {}
}

/// The runtime for a `major.minor[.patch]` string such as `3.13` or `3.13.2`.
#[must_use]
pub fn runtime_named(version: &str) -> Option<Runtime> {
    let mut parts = version.trim().trim_start_matches("python").split('.');
    let (major, minor) = (parts.next()?.trim(), parts.next()?.trim());
    let minor: u32 = minor.parse().ok()?;
    (major == "3")
        .then(|| RUNTIMES.into_iter().find(|runtime| runtime.minor == minor))
        .flatten()
}

fn minor_of(version: &str) -> Option<u32> {
    let mut parts = version.trim().split('.');
    (parts.next()? == "3").then_some(())?;
    match parts.next() {
        Some(minor) => minor.trim_end_matches('*').parse().ok(),
        // A bare "3" constrains nothing we can check.
        None => None,
    }
}

/// The newest stable runtime that satisfies a `requires-python` specifier
/// such as `>=3.12,<3.14`. Clauses that cannot be read are ignored. Returns
/// `None` when no supported version satisfies it.
#[must_use]
pub fn runtime_for_specifier(specifier: &str) -> Option<Runtime> {
    let allows = |runtime: &Runtime| {
        specifier.split(',').all(|clause| {
            let clause = clause.trim();
            let (operator, version) = ["==", "~=", ">=", "<=", "!=", ">", "<"]
                .iter()
                .find_map(|operator| {
                    clause
                        .strip_prefix(operator)
                        .map(|version| (*operator, version.trim()))
                })
                .unwrap_or(("", clause));
            let Some(minor) = minor_of(version) else {
                return true;
            };
            let has_patch = version.split('.').count() > 2 && !version.ends_with('*');
            match operator {
                ">=" | "~=" => runtime.minor >= minor,
                // `>3.12` with no patch number excludes 3.12 itself only loosely;
                // every 3.12.x is greater than "3.12", so it is allowed.
                ">" => runtime.minor > minor || (runtime.minor == minor && !has_patch),
                "<=" => runtime.minor <= minor,
                "<" => runtime.minor < minor,
                "==" | "" => runtime.minor == minor,
                "!=" => has_patch || runtime.minor != minor,
                _ => true,
            }
        })
    };
    RUNTIMES
        .into_iter()
        .rev()
        .find(|runtime| runtime.stable && allows(runtime))
        .or_else(|| RUNTIMES.into_iter().rev().find(allows))
}

/// What a script declares about itself in a PEP 723 comment block:
///
/// ```text
/// # /// script
/// # requires-python = ">=3.12"
/// # dependencies = ["rich", "httpx<1"]
/// # ///
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InlineMetadata {
    pub requires_python: Option<String>,
    pub dependencies: Vec<String>,
}

fn quoted_strings(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(['"', '\'']) {
        let quote = rest[start..].chars().next().unwrap_or('"');
        let after = &rest[start + 1..];
        let Some(end) = after.find(quote) else { break };
        found.push(after[..end].to_owned());
        rest = &after[end + 1..];
    }
    found
}

/// Reads a script's PEP 723 block. Only `requires-python` and
/// `dependencies` are understood, which is all running a script needs.
#[must_use]
pub fn inline_metadata(script: &str) -> InlineMetadata {
    let mut metadata = InlineMetadata::default();
    let mut inside = false;
    let mut collecting = false;
    for line in script.lines() {
        let line = line.trim_end();
        if !inside {
            inside = line == "# /// script";
            continue;
        }
        if line == "# ///" {
            break;
        }
        let Some(content) = line.strip_prefix('#') else {
            break;
        };
        let content = content.trim();
        if collecting {
            metadata.dependencies.extend(quoted_strings(content));
            collecting = !content.contains(']');
        } else if let Some(value) = content.strip_prefix("requires-python") {
            metadata.requires_python = quoted_strings(value).into_iter().next();
        } else if let Some(value) = content.strip_prefix("dependencies") {
            metadata.dependencies.extend(quoted_strings(value));
            collecting = !value.contains(']');
        }
    }
    metadata
}

/// The `requires-python` value in a `pyproject.toml`, if it has one.
#[must_use]
pub fn requires_python(pyproject: &str) -> Option<String> {
    pyproject
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("requires-python"))
        .and_then(|value| quoted_strings(value).into_iter().next())
}

const NOT_UV: &str = "These uv commands are a small compatible subset built into this terminal. uv itself is a native program and cannot run in a browser.";

fn usage() -> String {
    let mut text = String::from(
        "Python here is Pyodide: CPython compiled to WebAssembly, running in this browser tab.\n\n  python FILE [ARGS]       run a file\n  python -c CODE           run a line of code\n  python -m MODULE [ARGS]  run a module\n  python --version\n  pip install PACKAGES     install for this session (also: -r FILE)\n  uv run FILE [ARGS]       run a file, installing the dependencies it declares (PEP 723)\n  uv pip install PACKAGES\n  uv python list           versions available\n  uv python pin VERSION    choose the version for this repository (.python-version)\n\nVersions: ",
    );
    for runtime in RUNTIMES {
        let _ = write!(
            text,
            "{}{} ",
            runtime.python,
            if runtime.stable { "" } else { " (pre-release)" }
        );
    }
    text.push_str(
        "\nThe interpreter is downloaded the first time it is used (about 13 MB), and libraries when a program imports them (numpy, pandas and several hundred more; far fewer on 3.15).\nNot possible in a browser: sockets, subprocesses, threads, and packages with native code that Pyodide has not built.\n",
    );
    text
}

/// The terminal's `python`, `python3`, `pip`, `pip3` and `uv` commands.
pub struct PythonTool {
    host: Rc<dyn PythonHost>,
}

impl PythonTool {
    /// Wraps the thing that can actually run Python.
    #[must_use]
    pub fn new(host: Rc<dyn PythonHost>) -> Self {
        Self { host }
    }

    /// The version this repository asks for: `.python-version`, then
    /// `requires-python` in `pyproject.toml`, then the default.
    async fn project_runtime(workspace: &Workspace) -> Result<Runtime, String> {
        if let Ok(pinned) = workspace.read_text(".python-version").await {
            let pinned = pinned.trim();
            return runtime_named(pinned).ok_or_else(|| {
                format!(
                    ".python-version asks for Python {pinned}, which is not available here. Run: uv python list"
                )
            });
        }
        if let Ok(pyproject) = workspace.read_text("pyproject.toml").await
            && let Some(specifier) = requires_python(&pyproject)
        {
            return runtime_for_specifier(&specifier).ok_or_else(|| {
                format!(
                    "pyproject.toml requires Python {specifier}, and no version available here satisfies that. Run: uv python list"
                )
            });
        }
        Ok(DEFAULT_RUNTIME)
    }

    /// Every text file in the repository, up to the limits. Returns the files
    /// and a note when something was left out.
    /// The repository's text files for the program, a note to print before
    /// it runs, and how many files were held back for not being text.
    async fn mount(
        &self,
        workspace: &Workspace,
    ) -> Result<(Vec<(String, String)>, String, usize), String> {
        let listed = workspace
            .files_under("")
            .await
            .map_err(|error| error.to_string())?;
        let total = listed.len();
        // The first run in a repository has to fetch every file. Say so
        // before the wait, not after it.
        let to_fetch = total.min(MAX_MOUNT_FILES);
        if to_fetch >= SLOW_MOUNT_FILES && workspace.files_in_memory() < to_fetch / 2 {
            self.host.note(&format!(
                "Getting the repository's files for Python ({to_fetch} files). This is needed once."
            ));
        }
        // Several at a time: one after another, a few hundred files would
        // take a minute on a phone. The order of the results is the order
        // of the list.
        let mut reads = stream::iter(listed.into_iter().map(|file| async move {
            let content = workspace.read(&file.path).await;
            (file.path, content)
        }))
        .buffered(FETCH_AT_ONCE);
        let mut files = Vec::new();
        let mut bytes = 0usize;
        let mut skipped = 0usize;
        let mut seen = 0usize;
        while files.len() < MAX_MOUNT_FILES && bytes < MAX_MOUNT_BYTES {
            let Some((file, content)) = reads.next().await else {
                break;
            };
            seen += 1;
            match content {
                Ok(FileContent::Text(text)) => {
                    bytes += text.len();
                    files.push((file, text));
                }
                Ok(FileContent::Binary { .. }) => skipped += 1,
                Err(error) => return Err(format!("{}: {error}", path::display(&file))),
            }
        }
        let left_out = total - seen;
        let mut note = String::new();
        if left_out > 0 {
            let _ = writeln!(
                note,
                "note: Python was given the first {} files; {left_out} more were left out (limit {MAX_MOUNT_FILES} files or {} MB).",
                files.len(),
                MAX_MOUNT_BYTES / (1024 * 1024)
            );
        }
        Ok((files, note, skipped))
    }

    async fn execute(
        &self,
        call: Invocation<'_>,
        runtime: Runtime,
        install: Vec<String>,
        entry: Entry,
        args: &[String],
    ) -> ExternalRun {
        let workspace = call.workspace;
        let (files, note, skipped) = match self.mount(workspace).await {
            Ok(mounted) => mounted,
            Err(message) => return ExternalRun::fail(1, format!("python: {message}")),
        };
        let job = Job {
            python: runtime.python.to_owned(),
            pyodide: runtime.pyodide.to_owned(),
            files,
            cwd: call.cwd.to_owned(),
            install,
            entry,
            args: args.to_vec(),
            stdin: call.stdin.map(str::to_owned),
            stream: call.live,
        };
        let outcome = match self.host.run(job).await {
            Ok(outcome) => outcome,
            Err(message) => {
                return ExternalRun::fail(
                    1,
                    format!(
                        "python: could not start Python {}: {message}",
                        runtime.python
                    ),
                );
            }
        };
        let mut err = note;
        let mut written = 0usize;
        for (file, content) in outcome.changed {
            let result = match content {
                Some(text) => workspace.write(&file, &text).await.map(|_| ()),
                None => workspace.remove(&file, false).await.map(|_| ()),
            };
            match result {
                Ok(()) => written += 1,
                Err(error) => {
                    let _ = writeln!(
                        err,
                        "python: could not keep {}: {error}",
                        path::display(&file)
                    );
                }
            }
        }
        if written > 0 {
            let _ = writeln!(
                err,
                "note: the program changed {written} file{}; see Changes.",
                if written == 1 { "" } else { "s" }
            );
        }
        if !outcome.binary.is_empty() {
            let _ = writeln!(
                err,
                "note: not kept, because only text files can be saved so far: {}",
                outcome.binary.join(", ")
            );
        }
        // Worth saying only when something went wrong: it may be why.
        if skipped > 0 && outcome.status != 0 {
            let _ = writeln!(
                err,
                "note: {skipped} file{} in the repository {} not text and {} not given to Python.",
                if skipped == 1 { "" } else { "s" },
                if skipped == 1 { "is" } else { "are" },
                if skipped == 1 { "was" } else { "were" },
            );
        }
        if !outcome.pictures.is_empty() {
            let names: Vec<&str> = outcome
                .pictures
                .iter()
                .map(|picture| picture.path.as_str())
                .collect();
            let _ = writeln!(
                err,
                "note: pictures written: {}. The Show buttons under the terminal open them.",
                names.join(", ")
            );
        }
        ExternalRun {
            out: outcome.output,
            err,
            code: outcome.status,
        }
    }

    async fn python(&self, call: Invocation<'_>, args: &[String]) -> ExternalRun {
        let (cwd, workspace) = (call.cwd, call.workspace);
        let runtime = match Self::project_runtime(workspace).await {
            Ok(runtime) => runtime,
            Err(message) => return ExternalRun::fail(1, format!("python: {message}")),
        };
        match args {
            [] => ExternalRun::fail(
                2,
                "python: the interactive prompt is not available here. Run a file (python FILE) or a line (python -c \"print(1)\").",
            ),
            [flag] if flag == "--version" || flag == "-V" => ExternalRun::ok(format!(
                "Python {} (Pyodide {})\n",
                runtime.exact, runtime.pyodide
            )),
            [flag] if flag == "--help" || flag == "-h" => ExternalRun::ok(usage()),
            [flag, code, rest @ ..] if flag == "-c" => {
                self.execute(call, runtime, Vec::new(), Entry::Code(code.clone()), rest)
                    .await
            }
            [flag, module, rest @ ..] if flag == "-m" => {
                self.execute(
                    call,
                    runtime,
                    Vec::new(),
                    Entry::Module(module.clone()),
                    rest,
                )
                .await
            }
            [flag, ..] if flag.starts_with('-') => ExternalRun::fail(
                2,
                format!(
                    "python: option {flag} is not supported here. Supported: -c, -m, --version."
                ),
            ),
            [file, rest @ ..] => {
                let script = path::resolve(cwd, file);
                if workspace.read_text(&script).await.is_err() {
                    return ExternalRun::fail(
                        2,
                        format!("python: can't open file {file}: no such text file"),
                    );
                }
                self.execute(call, runtime, Vec::new(), Entry::File(script), rest)
                    .await
            }
        }
    }

    /// The packages named by `pip install` arguments: names, and the lines
    /// of any `-r FILE`.
    async fn requested(call: Invocation<'_>, args: &[String]) -> Result<Vec<String>, String> {
        let mut packages = Vec::new();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "-r" || arg == "--requirement" {
                let Some(file) = args.next() else {
                    return Err(format!("{arg} needs a file name"));
                };
                let text = call
                    .workspace
                    .read_text(&path::resolve(call.cwd, file))
                    .await
                    .map_err(|error| format!("{file}: {error}"))?;
                for line in text.lines() {
                    let line = line.split(" #").next().unwrap_or("").trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if line.starts_with('-') {
                        return Err(format!(
                            "{file}: the line {line:?} is an option, and options are not supported here"
                        ));
                    }
                    packages.push(line.to_owned());
                }
            } else if arg.starts_with('-') {
                return Err(format!(
                    "option {arg} is not supported here. Supported: package names and -r FILE"
                ));
            } else {
                packages.push(arg.clone());
            }
        }
        if packages.is_empty() {
            return Err(
                "usage: pip install PACKAGE [PACKAGE ...] or pip install -r FILE".to_owned(),
            );
        }
        Ok(packages)
    }

    async fn install(&self, call: Invocation<'_>, args: &[String]) -> ExternalRun {
        let packages = match Self::requested(call, args).await {
            Ok(packages) => packages,
            Err(message) => return ExternalRun::fail(2, format!("pip: {message}")),
        };
        let runtime = match Self::project_runtime(call.workspace).await {
            Ok(runtime) => runtime,
            Err(message) => return ExternalRun::fail(1, format!("pip: {message}")),
        };
        let names = packages.join(", ");
        let mut run = self
            .execute(call, runtime, packages, Entry::Nothing, &[])
            .await;
        if run.code == 0 {
            let _ = writeln!(
                run.out,
                "Installed for this session: {names}. A reload forgets it. To have it installed on every run, list it in the script's own header (# /// script) and use uv run."
            );
        }
        run
    }

    async fn uv(&self, call: Invocation<'_>) -> ExternalRun {
        let (args, cwd, workspace) = (call.args, call.cwd, call.workspace);
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        match words.as_slice() {
            [] | ["--help" | "-h" | "help"] => ExternalRun::ok(format!("{NOT_UV}\n\n{}", usage())),
            ["--version" | "-V" | "version"] => ExternalRun::ok(format!("{NOT_UV}\n")),
            ["python", "list"] => {
                let chosen = Self::project_runtime(workspace).await.ok();
                let mut text = String::new();
                for runtime in RUNTIMES {
                    let _ = writeln!(
                        text,
                        "{} cpython-{}-pyodide-{}{}",
                        if chosen == Some(runtime) { "*" } else { " " },
                        runtime.exact,
                        runtime.pyodide,
                        if runtime.stable {
                            ""
                        } else {
                            "  (pre-release)"
                        }
                    );
                }
                text.push_str("* is the version this repository uses.\n");
                ExternalRun::ok(text)
            }
            ["python", "pin", version] => match runtime_named(version) {
                Some(runtime) => match workspace
                    .write(".python-version", &format!("{}\n", runtime.python))
                    .await
                {
                    Ok(_) => ExternalRun::ok(format!(
                        "Pinned .python-version to {}{}\n",
                        runtime.python,
                        if runtime.stable {
                            ""
                        } else {
                            " (a pre-release)"
                        }
                    )),
                    Err(error) => ExternalRun::fail(1, format!("uv: {error}")),
                },
                None => ExternalRun::fail(
                    2,
                    format!("uv: Python {version} is not available here. Run: uv python list"),
                ),
            },
            ["pip", "install", ..] => self.install(call, &args[2..]).await,
            ["run", "python" | "python3", ..] => self.python(call, &args[2..]).await,
            ["run", file, ..] if !file.starts_with('-') => {
                let script = path::resolve(cwd, file);
                let Ok(text) = workspace.read_text(&script).await else {
                    return ExternalRun::fail(
                        2,
                        format!("uv: can't open file {file}: no such text file"),
                    );
                };
                let metadata = inline_metadata(&text);
                let runtime = match &metadata.requires_python {
                    Some(specifier) => match runtime_for_specifier(specifier) {
                        Some(runtime) => runtime,
                        None => {
                            return ExternalRun::fail(
                                1,
                                format!(
                                    "uv: {file} requires Python {specifier}, and no version available here satisfies that."
                                ),
                            );
                        }
                    },
                    None => match Self::project_runtime(workspace).await {
                        Ok(runtime) => runtime,
                        Err(message) => return ExternalRun::fail(1, format!("uv: {message}")),
                    },
                };
                self.execute(
                    call,
                    runtime,
                    metadata.dependencies,
                    Entry::File(script),
                    &args[2..],
                )
                .await
            }
            ["add" | "sync" | "lock" | "venv" | "init", ..] => ExternalRun::fail(
                2,
                format!(
                    "uv {}: not supported here, because there is no virtual environment to manage. To use a library, list it in the script's own header (# /// script) and use uv run, or run: uv pip install NAME",
                    words[0]
                ),
            ),
            [other, ..] => ExternalRun::fail(
                2,
                format!(
                    "uv {other}: not supported here. {NOT_UV} Supported: uv run, uv pip install, uv python list, uv python pin."
                ),
            ),
        }
    }
}

impl External for PythonTool {
    fn programs(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("python", "run Python in the browser: python --help"),
            ("python3", "the same as python"),
            ("pip", "pip install PACKAGES, for this session"),
            ("pip3", "the same as pip"),
            ("uv", "uv run, uv pip install, uv python list | pin"),
        ]
    }

    fn stop(&self) {
        self.host.stop();
    }

    fn run<'a>(&'a self, call: Invocation<'a>) -> LocalBoxFuture<'a, ExternalRun> {
        Box::pin(async move {
            match call.program {
                "pip" | "pip3" => match call.args.split_first() {
                    Some((sub, rest)) if sub == "install" => self.install(call, rest).await,
                    _ => ExternalRun::fail(2, "pip: only pip install is supported here"),
                },
                "uv" => self.uv(call).await,
                _ => self.python(call, call.args).await,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_runtimes() {
        assert_eq!(runtime_named("3.13").unwrap().pyodide, "0.29.5");
        assert_eq!(runtime_named("3.12.7\n").unwrap().python, "3.12");
        assert_eq!(runtime_named("python3.14").unwrap().python, "3.14");
        assert_eq!(runtime_named("3.11"), None);
        assert_eq!(runtime_named("2.7"), None);
        assert_eq!(runtime_named("pypy"), None);
    }

    #[test]
    fn resolves_specifiers_to_the_newest_stable_match() {
        let pick = |specifier: &str| runtime_for_specifier(specifier).map(|runtime| runtime.python);
        assert_eq!(pick(">=3.12"), Some("3.14"));
        assert_eq!(pick(">=3.12,<3.14"), Some("3.13"));
        assert_eq!(pick("==3.12.*"), Some("3.12"));
        assert_eq!(pick("~=3.13"), Some("3.14"));
        assert_eq!(pick("<3.13"), Some("3.12"));
        assert_eq!(pick(">3.13"), Some("3.14"));
        assert_eq!(pick(">=3.10, !=3.14.*"), Some("3.13"));
        // Only the pre-release satisfies this, so it is used rather than nothing.
        assert_eq!(pick(">=3.15"), Some("3.15"));
        assert_eq!(pick(">=3.16"), None);
        assert_eq!(pick("<3.12"), None);
        // Unreadable clauses do not block.
        assert_eq!(pick(">=3"), Some("3.14"));
    }

    #[test]
    fn reads_inline_script_metadata() {
        let script = "#!/usr/bin/env python\n# /// script\n# requires-python = \">=3.12,<3.14\"\n# dependencies = [\n#   \"rich\",\n#   'httpx<1',\n# ]\n# ///\nimport rich\n";
        let metadata = inline_metadata(script);
        assert_eq!(metadata.requires_python.as_deref(), Some(">=3.12,<3.14"));
        assert_eq!(metadata.dependencies, ["rich", "httpx<1"]);

        let one_line = "# /// script\n# dependencies = [\"attrs\"]\n# ///\n";
        assert_eq!(inline_metadata(one_line).dependencies, ["attrs"]);
        assert_eq!(
            inline_metadata("print('no block')\n"),
            InlineMetadata::default()
        );
        // A block that is not at comment level stops at the first code line.
        assert_eq!(
            inline_metadata("# /// script\nimport os\n# dependencies = [\"x\"]\n").dependencies,
            Vec::<String>::new()
        );
    }

    #[test]
    fn reads_requires_python_from_pyproject() {
        let pyproject = "[project]\nname = \"x\"\nrequires-python = \">=3.12\"\n";
        assert_eq!(requires_python(pyproject).as_deref(), Some(">=3.12"));
        assert_eq!(requires_python("[project]\nname = \"x\"\n"), None);
    }
}
