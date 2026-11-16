//! The terminal's command language.
//!
//! This is a small shell that runs inside the browser against a
//! [`Workspace`]. There is no machine behind it: `cargo`, `npm` and the like
//! do not exist here, and asking for them gets a plain "not available" answer
//! rather than a pretend result. Programs that can run in a browser are added
//! through [`External`]; Python is one (see [`crate::python`]).
//!
//! Supported syntax: words, `'single'` and `"double"` quotes, backslash
//! escapes, `*` and `?` globs, pipes (`|`), `&&`, `;`, and `>` / `>>` to a
//! file. Anything else (`$(...)`, backticks, `<`, `||`, `&`) is rejected with
//! a message naming it.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::rc::Rc;

use futures_util::stream::{self, StreamExt as _};

use crate::error::Error;
use crate::http::LocalBoxFuture;
use crate::path;
use crate::workspace::{ChangeKind, EntryKind, FETCH_AT_ONCE, FileContent, Workspace};

/// The most text one command may print. More than this is cut, and the
/// output says so.
pub const MAX_OUTPUT_BYTES: usize = 256 * 1024;
/// The most files `grep -r` reads in one run.
pub const MAX_GREP_FILES: usize = 400;

/// What running a line produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// Everything to print: standard output, then error messages.
    pub text: String,
    /// 0 for success, as in a shell.
    pub status: i32,
    /// A file the command asked the editor to open (`open`, `edit`).
    pub open: Option<String>,
    /// The command asked for the screen to be cleared.
    pub clear: bool,
}

/// The result of asking to complete the word under the cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completion {
    /// The line with the word completed as far as it is unambiguous.
    pub line: String,
    /// The choices, when there is more than one.
    pub candidates: Vec<String>,
}

/// What one program produced.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExternalRun {
    /// Standard output: what a pipe or `>` receives.
    pub out: String,
    /// Messages for the person at the terminal.
    pub err: String,
    /// The exit status; 0 is success.
    pub code: i32,
}

type Run = ExternalRun;

/// What a program is told when it is run.
#[derive(Clone, Copy)]
pub struct Invocation<'a> {
    /// The name it was called by.
    pub program: &'a str,
    pub args: &'a [String],
    /// Text piped in from the command before it, if any.
    pub stdin: Option<&'a str>,
    /// The shell's current folder, relative to the repository root.
    pub cwd: &'a str,
    pub workspace: &'a Workspace,
    /// The program's output is going straight to a terminal that can show it
    /// while the program runs (see [`Shell::set_live_output`]). A program
    /// that shows its output that way itself returns none of it.
    pub live: bool,
}

/// A program the shell can run that is not built into it.
pub trait External {
    /// The names this answers to, with a short description of each.
    fn programs(&self) -> &'static [(&'static str, &'static str)];

    /// Runs it.
    fn run<'a>(&'a self, invocation: Invocation<'a>) -> LocalBoxFuture<'a, ExternalRun>;

    /// Asked to stop whatever it is running. The default does nothing.
    fn stop(&self) {}
}

impl ExternalRun {
    /// Success, with this standard output.
    #[must_use]
    pub fn ok(out: impl Into<String>) -> Self {
        Self {
            out: out.into(),
            ..Self::default()
        }
    }

    /// Failure, with this message.
    #[must_use]
    pub fn fail(code: i32, err: impl Into<String>) -> Self {
        let mut err = err.into();
        if !err.ends_with('\n') {
            err.push('\n');
        }
        Self {
            out: String::new(),
            err,
            code,
        }
    }

    fn from_error(command: &str, error: &Error) -> Self {
        Self::fail(1, format!("{command}: {error}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word { text: String, glob: bool },
    Pipe,
    And,
    Semi,
    Redirect { append: bool },
}

#[derive(Debug, Default)]
struct Command {
    words: Vec<(String, bool)>,
    redirect: Option<(String, bool)>,
}

#[derive(Debug, Default)]
struct Pipeline {
    commands: Vec<Command>,
    /// Run only if the previous pipeline succeeded (`&&`).
    needs_success: bool,
}

/// Commands this shell implements, for `help` and completion.
pub const COMMANDS: &[(&str, &str)] = &[
    ("cat", "print files"),
    ("cd", "change folder"),
    ("clear", "clear the screen"),
    ("cp", "copy a file (-r for a folder)"),
    ("echo", "print text; use > file to write it"),
    ("edit", "open a file in the editor"),
    ("find", "list files: find [folder] [-name '*.py']"),
    (
        "git",
        "status, diff, commit -m, log, branch, switch, restore, pull",
    ),
    (
        "grep",
        "search: grep [-i -n -v -r -l -c -F] pattern [paths]",
    ),
    ("head", "first lines: head [-n 10] file"),
    ("help", "this list"),
    ("history", "commands run in this session"),
    ("ls", "list a folder (-l sizes, -a hidden files)"),
    ("mkdir", "folders appear when a file is created in them"),
    ("mv", "move or rename a file or folder"),
    ("open", "open a file in the editor"),
    ("pwd", "print the current folder"),
    ("rm", "remove a file (-r for a folder)"),
    ("save", "checkpoint all changes to GitHub now"),
    ("tail", "last lines: tail [-n 10] file"),
    ("touch", "create an empty file"),
    ("wc", "count lines, words and bytes"),
    ("whoami", "the signed-in GitHub account"),
];

/// Programs people will try that cannot run in a browser tab today.
const NOT_AVAILABLE: &[&str] = &[
    "python", "python3", "pip", "pip3", "uv", "node", "npm", "npx", "cargo", "rustc", "make",
    "gcc", "clang", "go", "ruby", "java", "docker", "ssh", "curl", "wget", "bash", "sh", "zsh",
    "sudo", "vim", "vi", "nano", "tmux", "gh", "pytest", "ruff", "sed", "awk",
];

fn tokenize(line: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut glob = false;
    let mut i = 0;
    let flush =
        |tokens: &mut Vec<Token>, word: &mut String, in_word: &mut bool, glob: &mut bool| {
            if *in_word {
                tokens.push(Token::Word {
                    text: std::mem::take(word),
                    glob: *glob,
                });
            }
            *in_word = false;
            *glob = false;
        };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            ' ' | '\t' | '\n' => flush(&mut tokens, &mut word, &mut in_word, &mut glob),
            '#' if !in_word => break,
            '\'' => {
                in_word = true;
                i += 1;
                loop {
                    match chars.get(i) {
                        Some('\'') => break,
                        Some(&inner) => word.push(inner),
                        None => return Err("unterminated ' quote".to_owned()),
                    }
                    i += 1;
                }
            }
            '"' => {
                in_word = true;
                i += 1;
                loop {
                    match chars.get(i) {
                        Some('"') => break,
                        Some('\\') if matches!(chars.get(i + 1), Some('"' | '\\')) => {
                            word.push(chars[i + 1]);
                            i += 1;
                        }
                        Some(&inner) => word.push(inner),
                        None => return Err("unterminated \" quote".to_owned()),
                    }
                    i += 1;
                }
            }
            '\\' => {
                let Some(escaped) = next else {
                    return Err("a line cannot end with \\".to_owned());
                };
                in_word = true;
                word.push(escaped);
                i += 1;
            }
            '|' if next == Some('|') => return Err("|| is not supported".to_owned()),
            '|' => {
                flush(&mut tokens, &mut word, &mut in_word, &mut glob);
                tokens.push(Token::Pipe);
            }
            '&' if next == Some('&') => {
                flush(&mut tokens, &mut word, &mut in_word, &mut glob);
                tokens.push(Token::And);
                i += 1;
            }
            '&' => return Err("running in the background (&) is not supported".to_owned()),
            ';' => {
                flush(&mut tokens, &mut word, &mut in_word, &mut glob);
                tokens.push(Token::Semi);
            }
            '>' => {
                flush(&mut tokens, &mut word, &mut in_word, &mut glob);
                let append = next == Some('>');
                if append {
                    i += 1;
                }
                tokens.push(Token::Redirect { append });
            }
            '<' => return Err("reading input with < is not supported".to_owned()),
            '`' => return Err("backtick substitution is not supported".to_owned()),
            '$' if next == Some('(') => return Err("$( ) substitution is not supported".to_owned()),
            other => {
                in_word = true;
                if matches!(other, '*' | '?') {
                    glob = true;
                }
                word.push(other);
            }
        }
        i += 1;
    }
    flush(&mut tokens, &mut word, &mut in_word, &mut glob);
    Ok(tokens)
}

fn parse(tokens: Vec<Token>) -> Result<Vec<Pipeline>, String> {
    let mut pipelines = Vec::new();
    let mut pipeline = Pipeline::default();
    let mut command = Command::default();
    let mut tokens = tokens.into_iter().peekable();
    let end_command = |pipeline: &mut Pipeline, command: &mut Command| -> Result<(), String> {
        if command.words.is_empty() {
            return Err("expected a command".to_owned());
        }
        pipeline.commands.push(std::mem::take(command));
        Ok(())
    };
    while let Some(token) = tokens.next() {
        match token {
            Token::Word { text, glob } => command.words.push((text, glob)),
            Token::Redirect { append } => match tokens.next() {
                Some(Token::Word { text, .. }) => command.redirect = Some((text, append)),
                _ => return Err("expected a file name after >".to_owned()),
            },
            Token::Pipe => {
                if command.redirect.is_some() {
                    return Err("> must come at the end of a pipeline".to_owned());
                }
                end_command(&mut pipeline, &mut command)?;
            }
            Token::And | Token::Semi => {
                end_command(&mut pipeline, &mut command)?;
                pipelines.push(std::mem::take(&mut pipeline));
                pipeline.needs_success = token == Token::And;
                if token == Token::And && tokens.peek().is_none() {
                    return Err("expected a command after &&".to_owned());
                }
            }
        }
    }
    if !command.words.is_empty() || !pipeline.commands.is_empty() {
        end_command(&mut pipeline, &mut command)?;
        pipelines.push(pipeline);
    }
    Ok(pipelines)
}

fn split_flags(args: &[String], known: &str) -> Result<(Vec<char>, Vec<String>), String> {
    let mut flags = Vec::new();
    let mut rest = Vec::new();
    let mut only_operands = false;
    for arg in args {
        if only_operands || !arg.starts_with('-') || arg == "-" {
            rest.push(arg.clone());
        } else if arg == "--" {
            only_operands = true;
        } else {
            for flag in arg[1..].chars() {
                if !known.contains(flag) {
                    return Err(format!("unknown option -{flag}"));
                }
                flags.push(flag);
            }
        }
    }
    Ok((flags, rest))
}

/// Takes `-n 5`, `-n5` or `-5` out of `args`, returning the count.
fn take_count(args: &mut Vec<String>, default: usize) -> Result<usize, String> {
    let mut count = default;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].clone();
        let value = if arg == "-n" {
            if i + 1 >= args.len() {
                return Err("-n needs a number".to_owned());
            }
            args.remove(i);
            Some(args.remove(i))
        } else if let Some(rest) = arg.strip_prefix("-n") {
            args.remove(i);
            Some(rest.to_owned())
        } else if arg.len() > 1
            && arg.starts_with('-')
            && arg[1..].chars().all(|c| c.is_ascii_digit())
        {
            args.remove(i);
            Some(arg[1..].to_owned())
        } else {
            i += 1;
            None
        };
        if let Some(value) = value {
            count = value
                .parse()
                .map_err(|_| format!("{value:?} is not a number"))?;
        }
    }
    Ok(count)
}

/// The shell: a current folder and the session's history.
#[derive(Default)]
pub struct Shell {
    cwd: RefCell<String>,
    history: RefCell<Vec<String>>,
    stop: Cell<bool>,
    tools: RefCell<Vec<Rc<dyn External>>>,
    live: Cell<bool>,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell")
            .field("cwd", &self.cwd)
            .field("history", &self.history)
            .finish_non_exhaustive()
    }
}

impl Shell {
    /// A shell at the repository root.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Says whether the terminal this shell is used from can show a
    /// program's output while it runs. If so, a program whose output is not
    /// going into a pipe or a file is told (see [`Invocation::live`]).
    pub fn set_live_output(&self, live: bool) {
        self.live.set(live);
    }

    /// Adds programs to this shell.
    pub fn add_tool(&self, tool: Rc<dyn External>) {
        self.tools.borrow_mut().push(tool);
    }

    fn tool_for(&self, program: &str) -> Option<Rc<dyn External>> {
        self.tools
            .borrow()
            .iter()
            .find(|tool| tool.programs().iter().any(|(name, _)| *name == program))
            .cloned()
    }

    /// Every command name: built in, then added.
    fn command_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = COMMANDS.iter().map(|(name, _)| *name).collect();
        for tool in self.tools.borrow().iter() {
            names.extend(tool.programs().iter().map(|(name, _)| *name));
        }
        names
    }

    fn help_text(&self) -> String {
        let mut text = String::from(
            "This terminal runs in your browser on the open repository. Nothing runs on a server.\n\n",
        );
        for (name, about) in COMMANDS {
            let _ = writeln!(text, "  {name:<8} {about}");
        }
        let mut added = Vec::new();
        for tool in self.tools.borrow().iter() {
            for (name, about) in tool.programs() {
                let _ = writeln!(text, "  {name:<8} {about}");
                added.push(*name);
            }
        }
        text.push_str(
            "\nYou can chain with |, && and ; and write output to a file with > or >>.\nNot available: ",
        );
        let missing: Vec<&str> = ["python", "uv", "pip", "cargo", "npm", "node", "make"]
            .into_iter()
            .filter(|name| !added.contains(name))
            .collect();
        text.push_str(&missing.join(", "));
        text.push_str(" and other programs that need a machine.\n");
        text
    }

    /// The current folder (`""` is the repository root).
    #[must_use]
    pub fn cwd(&self) -> String {
        self.cwd.borrow().clone()
    }

    /// The prompt, for example `owner/repo:/src (main) $`.
    #[must_use]
    pub fn prompt(&self, workspace: &Workspace) -> String {
        format!(
            "{}:{} ({}) $",
            workspace.repo(),
            path::display(&self.cwd()),
            workspace.branch()
        )
    }

    /// Lines run so far, oldest first.
    #[must_use]
    pub fn history(&self) -> Vec<String> {
        self.history.borrow().clone()
    }

    /// Asks the running command to stop at its next step.
    pub fn stop(&self) {
        self.stop.set(true);
        for tool in self.tools.borrow().iter() {
            tool.stop();
        }
    }

    fn resolve(&self, input: &str) -> String {
        path::resolve(&self.cwd.borrow(), input)
    }

    /// Runs one line.
    pub async fn run(&self, line: &str, workspace: &Workspace) -> Output {
        self.stop.set(false);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Output::default();
        }
        self.history.borrow_mut().push(trimmed.to_owned());
        let pipelines = match tokenize(trimmed).and_then(parse) {
            Ok(pipelines) => pipelines,
            Err(message) => {
                return Output {
                    text: format!("{message}\n"),
                    status: 2,
                    ..Output::default()
                };
            }
        };
        let mut output = Output::default();
        for pipeline in pipelines {
            if pipeline.needs_success && output.status != 0 {
                continue;
            }
            self.run_pipeline(pipeline, workspace, &mut output).await;
            if self.stop.get() {
                output.text.push_str("Stopped.\n");
                output.status = 130;
                break;
            }
        }
        if output.text.len() > MAX_OUTPUT_BYTES {
            let mut cut = MAX_OUTPUT_BYTES;
            while !output.text.is_char_boundary(cut) {
                cut -= 1;
            }
            let dropped = output.text.len() - cut;
            output.text.truncate(cut);
            let _ = write!(
                output.text,
                "\n[output cut: {dropped} more bytes not shown. Use head, tail or grep to narrow it.]\n"
            );
        }
        output
    }

    async fn run_pipeline(&self, pipeline: Pipeline, workspace: &Workspace, output: &mut Output) {
        let mut stdin: Option<String> = None;
        let mut code = 0;
        let last = pipeline.commands.len().saturating_sub(1);
        for (index, command) in pipeline.commands.into_iter().enumerate() {
            let args = match self.expand(&command.words, workspace).await {
                Ok(args) => args,
                Err(error) => {
                    let _ = writeln!(output.text, "{error}");
                    output.status = 1;
                    return;
                }
            };
            let name = args[0].clone();
            let live = self.live.get() && index == last && command.redirect.is_none();
            let mut run = self
                .dispatch(&name, &args[1..], stdin.take(), workspace, output, live)
                .await;
            if let Some((target, append)) = command.redirect {
                let target = self.resolve(&target);
                let mut content = String::new();
                if append && let Ok(existing) = workspace.read_text(&target).await {
                    content = existing;
                }
                content.push_str(&run.out);
                run.out.clear();
                if let Err(error) = workspace.write(&target, &content).await {
                    let _ = writeln!(run.err, "{name}: {error}");
                    run.code = 1;
                }
            }
            output.text.push_str(&run.err);
            code = run.code;
            if index == last {
                output.text.push_str(&run.out);
            } else {
                stdin = Some(run.out);
            }
        }
        output.status = code;
    }

    async fn expand(
        &self,
        words: &[(String, bool)],
        workspace: &Workspace,
    ) -> Result<Vec<String>, Error> {
        let mut args = Vec::new();
        for (word, glob) in words {
            if !*glob {
                args.push(word.clone());
                continue;
            }
            let (dir_typed, pattern) = match word.rsplit_once('/') {
                Some((dir, pattern)) => (format!("{dir}/"), pattern),
                None => (String::new(), word.as_str()),
            };
            let dir = self.resolve(if dir_typed.is_empty() {
                "."
            } else {
                &dir_typed
            });
            let mut matches: Vec<String> = match workspace.list_dir(&dir).await {
                Ok(entries) => entries
                    .into_iter()
                    .filter(|entry| {
                        path::glob_match(pattern, &entry.name)
                            && (pattern.starts_with('.') || !entry.name.starts_with('.'))
                    })
                    .map(|entry| format!("{dir_typed}{}", entry.name))
                    .collect(),
                Err(Error::NotFound(_) | Error::Invalid(_)) => Vec::new(),
                Err(other) => return Err(other),
            };
            if matches.is_empty() {
                // As in a shell: a pattern that matches nothing stays as typed.
                args.push(word.clone());
            } else {
                matches.sort();
                args.append(&mut matches);
            }
        }
        Ok(args)
    }

    async fn dispatch(
        &self,
        name: &str,
        args: &[String],
        stdin: Option<String>,
        workspace: &Workspace,
        output: &mut Output,
        live: bool,
    ) -> Run {
        match name {
            "help" => Run::ok(self.help_text()),
            "pwd" => Run::ok(format!("{}\n", path::display(&self.cwd()))),
            "cd" => self.cd(args, workspace).await,
            "ls" => self.ls(args, workspace).await,
            "cat" => self.cat(args, stdin, workspace).await,
            "head" | "tail" => self.head_tail(name, args, stdin, workspace).await,
            "wc" => self.wc(args, stdin, workspace).await,
            "grep" => self.grep(args, stdin, workspace).await,
            "find" => self.find(args, workspace).await,
            "echo" => Run::ok(format!("{}\n", args.join(" "))),
            "touch" => self.touch(args, workspace).await,
            "mkdir" => Run::ok(
                "Nothing to do: git does not store empty folders. A folder appears when a file is created inside it, for example: touch docs/notes.md\n",
            ),
            "rm" => self.rm(args, workspace).await,
            "mv" | "cp" => self.mv_cp(name, args, workspace).await,
            "open" | "edit" => self.open(name, args, workspace, output).await,
            "clear" => {
                output.clear = true;
                Run::default()
            }
            "history" => {
                let mut text = String::new();
                for (index, line) in self.history.borrow().iter().enumerate() {
                    let _ = writeln!(text, "{:>4}  {line}", index + 1);
                }
                Run::ok(text)
            }
            "save" => match workspace.checkpoint().await {
                Ok(Some(sha)) => Run::ok(format!(
                    "Checkpointed to GitHub on {} ({}).\n",
                    workspace.autosave_branch(),
                    &sha[..sha.len().min(7)]
                )),
                Ok(None) => Run::ok("Nothing new to checkpoint.\n"),
                Err(error) => Run::from_error("save", &error),
            },
            "whoami" => match workspace.github().user().await {
                Ok(user) => Run::ok(format!("{}\n", user.login)),
                Err(error) => Run::from_error("whoami", &error),
            },
            "git" => self.git(args, workspace).await,
            other if let Some(tool) = self.tool_for(other) => {
                let cwd = self.cwd();
                tool.run(Invocation {
                    program: other,
                    args,
                    stdin: stdin.as_deref(),
                    cwd: &cwd,
                    workspace,
                    live,
                })
                .await
            }
            other if NOT_AVAILABLE.contains(&other) => Run::fail(
                127,
                format!(
                    "{other}: not available. This terminal runs inside the browser and has no machine behind it, so it cannot run {other}. Type help for what it can do."
                ),
            ),
            other => Run::fail(
                127,
                format!("{other}: command not found. Type help for the list."),
            ),
        }
    }

    async fn cd(&self, args: &[String], workspace: &Workspace) -> Run {
        let target = match args {
            [] => String::new(),
            [dir] => self.resolve(dir),
            _ => return Run::fail(2, "cd: too many arguments"),
        };
        match workspace.kind(&target).await {
            Ok(Some(EntryKind::Dir)) => {
                *self.cwd.borrow_mut() = target;
                Run::default()
            }
            Ok(Some(_)) => Run::fail(1, format!("cd: {} is not a folder", path::display(&target))),
            Ok(None) => Run::fail(1, format!("cd: {}: no such folder", path::display(&target))),
            Err(error) => Run::from_error("cd", &error),
        }
    }

    async fn ls(&self, args: &[String], workspace: &Workspace) -> Run {
        let (flags, mut targets) = match split_flags(args, "la1") {
            Ok(parsed) => parsed,
            Err(message) => return Run::fail(2, format!("ls: {message}")),
        };
        let long = flags.contains(&'l');
        let all = flags.contains(&'a');
        if targets.is_empty() {
            targets.push(".".to_owned());
        }
        let mut run = Run::default();
        let many = targets.len() > 1;
        for target in &targets {
            let full = self.resolve(target);
            let entries = match workspace.kind(&full).await {
                Ok(Some(EntryKind::Dir)) => match workspace.list_dir(&full).await {
                    Ok(entries) => entries,
                    Err(error) => {
                        let _ = writeln!(run.err, "ls: {error}");
                        run.code = 1;
                        continue;
                    }
                },
                Ok(Some(_)) => {
                    let _ = writeln!(run.out, "{target}");
                    continue;
                }
                Ok(None) => {
                    let _ = writeln!(run.err, "ls: {target}: no such file or folder");
                    run.code = 1;
                    continue;
                }
                Err(error) => {
                    let _ = writeln!(run.err, "ls: {error}");
                    run.code = 1;
                    continue;
                }
            };
            if many {
                let _ = writeln!(run.out, "{target}:");
            }
            for entry in entries {
                if !all && entry.name.starts_with('.') {
                    continue;
                }
                let suffix = match entry.kind {
                    EntryKind::Dir => "/",
                    EntryKind::Symlink => "@",
                    EntryKind::Submodule => " (submodule)",
                    EntryKind::File => "",
                };
                if long {
                    let mark = match entry.change {
                        Some(ChangeKind::Added) => "A",
                        Some(ChangeKind::Modified) => "M",
                        Some(ChangeKind::Deleted) => "D",
                        None => " ",
                    };
                    let size = entry.size.map_or_else(|| "-".to_owned(), |s| s.to_string());
                    let _ = writeln!(run.out, "{mark} {size:>9}  {}{suffix}", entry.name);
                } else {
                    let _ = writeln!(run.out, "{}{suffix}", entry.name);
                }
            }
        }
        run
    }

    async fn read_inputs(
        &self,
        command: &str,
        files: &[String],
        stdin: Option<String>,
        workspace: &Workspace,
        run: &mut Run,
    ) -> Vec<(String, String)> {
        if files.is_empty() {
            return vec![(String::new(), stdin.unwrap_or_default())];
        }
        let mut inputs = Vec::new();
        for file in files {
            match workspace.read(&self.resolve(file)).await {
                Ok(FileContent::Text(text)) => inputs.push((file.clone(), text)),
                Ok(FileContent::Binary { size }) => {
                    let _ = writeln!(run.err, "{command}: {file}: not text ({size} bytes)");
                    run.code = 1;
                }
                Err(error) => {
                    let _ = writeln!(run.err, "{command}: {file}: {error}");
                    run.code = 1;
                }
            }
        }
        inputs
    }

    async fn cat(&self, args: &[String], stdin: Option<String>, workspace: &Workspace) -> Run {
        let mut run = Run::default();
        for (_, text) in self
            .read_inputs("cat", args, stdin, workspace, &mut run)
            .await
        {
            run.out.push_str(&text);
        }
        run
    }

    async fn head_tail(
        &self,
        name: &str,
        args: &[String],
        stdin: Option<String>,
        workspace: &Workspace,
    ) -> Run {
        let mut args = args.to_vec();
        let count = match take_count(&mut args, 10) {
            Ok(count) => count,
            Err(message) => return Run::fail(2, format!("{name}: {message}")),
        };
        let mut run = Run::default();
        let inputs = self
            .read_inputs(name, &args, stdin, workspace, &mut run)
            .await;
        let many = inputs.len() > 1;
        for (file, text) in inputs {
            if many {
                let _ = writeln!(run.out, "==> {file} <==");
            }
            let lines: Vec<&str> = text.lines().collect();
            let slice = if name == "head" {
                &lines[..count.min(lines.len())]
            } else {
                &lines[lines.len().saturating_sub(count)..]
            };
            for line in slice {
                let _ = writeln!(run.out, "{line}");
            }
        }
        run
    }

    async fn wc(&self, args: &[String], stdin: Option<String>, workspace: &Workspace) -> Run {
        let (flags, files) = match split_flags(args, "lwc") {
            Ok(parsed) => parsed,
            Err(message) => return Run::fail(2, format!("wc: {message}")),
        };
        let mut run = Run::default();
        for (file, text) in self
            .read_inputs("wc", &files, stdin, workspace, &mut run)
            .await
        {
            let counts = [
                ('l', text.lines().count()),
                ('w', text.split_whitespace().count()),
                ('c', text.len()),
            ];
            let shown: Vec<String> = counts
                .iter()
                .filter(|(flag, _)| flags.is_empty() || flags.contains(flag))
                .map(|(_, count)| count.to_string())
                .collect();
            let _ = writeln!(run.out, "{} {file}", shown.join(" "));
        }
        run
    }

    #[allow(clippy::too_many_lines)]
    async fn grep(&self, args: &[String], stdin: Option<String>, workspace: &Workspace) -> Run {
        let (flags, mut rest) = match split_flags(args, "invrlcFE") {
            Ok(parsed) => parsed,
            Err(message) => return Run::fail(2, format!("grep: {message}")),
        };
        if rest.is_empty() {
            return Run::fail(2, "grep: give a pattern to search for");
        }
        let pattern = rest.remove(0);
        let has = |flag: char| flags.contains(&flag);
        let source = if has('F') {
            regex_lite::escape(&pattern)
        } else {
            pattern.clone()
        };
        let regex = match regex_lite::RegexBuilder::new(&source)
            .case_insensitive(has('i'))
            .build()
        {
            Ok(regex) => regex,
            Err(error) => return Run::fail(2, format!("grep: bad pattern: {error}")),
        };
        let mut run = Run::default();
        let mut files: Vec<String> = Vec::new();
        let recursive = has('r');
        if recursive && rest.is_empty() {
            rest.push(".".to_owned());
        }
        for target in &rest {
            let full = self.resolve(target);
            match workspace.kind(&full).await {
                Ok(Some(EntryKind::Dir)) if recursive => match workspace.files_under(&full).await {
                    Ok(found) => files.extend(found.into_iter().map(|file| file.path)),
                    Err(error) => {
                        let _ = writeln!(run.err, "grep: {target}: {error}");
                        run.code = 2;
                    }
                },
                Ok(Some(EntryKind::Dir)) => {
                    let _ = writeln!(run.err, "grep: {target} is a folder (use -r)");
                    run.code = 2;
                }
                Ok(Some(_)) => files.push(full),
                Ok(None) => {
                    let _ = writeln!(run.err, "grep: {target}: no such file or folder");
                    run.code = 2;
                }
                Err(error) => {
                    let _ = writeln!(run.err, "grep: {target}: {error}");
                    run.code = 2;
                }
            }
        }
        let mut inputs: Vec<(String, String)> = Vec::new();
        let mut skipped = 0usize;
        if rest.is_empty() {
            inputs.push((String::new(), stdin.unwrap_or_default()));
        } else {
            let over = files.len().saturating_sub(MAX_GREP_FILES);
            files.truncate(MAX_GREP_FILES);
            // Several files at a time, in the order they were named.
            let mut reads = stream::iter(
                files
                    .iter()
                    .map(|file| async move { (file, workspace.read(file).await) }),
            )
            .buffered(FETCH_AT_ONCE);
            while let Some((file, content)) = reads.next().await {
                if self.stop.get() {
                    break;
                }
                match content {
                    Ok(FileContent::Text(text)) => inputs.push((path::display(file), text)),
                    Ok(FileContent::Binary { .. }) => skipped += 1,
                    Err(error) => {
                        let _ = writeln!(run.err, "grep: {}: {error}", path::display(file));
                        run.code = 2;
                    }
                }
            }
            if over > 0 {
                let _ = writeln!(
                    run.err,
                    "grep: searched the first {MAX_GREP_FILES} files; {over} more were not searched. Narrow the folder."
                );
            }
        }
        let show_names = files.len() > 1 || recursive;
        let mut any = false;
        for (name, text) in &inputs {
            let mut count = 0usize;
            for (number, line) in text.lines().enumerate() {
                if regex.is_match(line) == has('v') {
                    continue;
                }
                count += 1;
                if has('l') || has('c') {
                    continue;
                }
                if show_names {
                    let _ = write!(run.out, "{name}:");
                }
                if has('n') {
                    let _ = write!(run.out, "{}:", number + 1);
                }
                let _ = writeln!(run.out, "{line}");
            }
            any |= count > 0;
            if has('l') && count > 0 {
                let _ = writeln!(run.out, "{name}");
            } else if has('c') {
                if show_names {
                    let _ = write!(run.out, "{name}:");
                }
                let _ = writeln!(run.out, "{count}");
            }
        }
        if skipped > 0 {
            let _ = writeln!(run.err, "grep: skipped {skipped} file(s) that are not text");
        }
        if run.code == 0 && !any {
            run.code = 1;
        }
        run
    }

    async fn find(&self, args: &[String], workspace: &Workspace) -> Run {
        let mut dir = ".".to_owned();
        let mut name: Option<String> = None;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "-name" => match iter.next() {
                    Some(pattern) => name = Some(pattern.clone()),
                    None => return Run::fail(2, "find: -name needs a pattern"),
                },
                "-type" => match iter.next().map(String::as_str) {
                    Some("f") => {}
                    _ => return Run::fail(2, "find: only -type f is supported"),
                },
                other if other.starts_with('-') => {
                    return Run::fail(2, format!("find: unknown option {other}"));
                }
                other => other.clone_into(&mut dir),
            }
        }
        let full = self.resolve(&dir);
        match workspace.files_under(&full).await {
            Ok(files) => {
                let mut run = Run::default();
                for file in files {
                    let matches = name.as_deref().is_none_or(|pattern| {
                        path::glob_match(pattern, path::file_name(&file.path))
                    });
                    if matches {
                        let _ = writeln!(run.out, "{}", path::display(&file.path));
                    }
                }
                run
            }
            Err(error) => Run::from_error("find", &error),
        }
    }

    async fn touch(&self, args: &[String], workspace: &Workspace) -> Run {
        if args.is_empty() {
            return Run::fail(2, "touch: give a file name");
        }
        let mut run = Run::default();
        for arg in args {
            let full = self.resolve(arg);
            match workspace.kind(&full).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    if let Err(error) = workspace.write(&full, "").await {
                        let _ = writeln!(run.err, "touch: {arg}: {error}");
                        run.code = 1;
                    }
                }
                Err(error) => {
                    let _ = writeln!(run.err, "touch: {arg}: {error}");
                    run.code = 1;
                }
            }
        }
        run
    }

    async fn rm(&self, args: &[String], workspace: &Workspace) -> Run {
        let (flags, targets) = match split_flags(args, "rRf") {
            Ok(parsed) => parsed,
            Err(message) => return Run::fail(2, format!("rm: {message}")),
        };
        if targets.is_empty() {
            return Run::fail(2, "rm: give a file name");
        }
        let recursive = flags.contains(&'r') || flags.contains(&'R');
        let force = flags.contains(&'f');
        let mut run = Run::default();
        for target in &targets {
            let full = self.resolve(target);
            if full.is_empty() {
                let _ = writeln!(run.err, "rm: refusing to remove the whole repository");
                run.code = 1;
                continue;
            }
            match workspace.remove(&full, recursive).await {
                Ok(_) => {}
                Err(Error::NotFound(_)) if force => {}
                Err(error) => {
                    let _ = writeln!(run.err, "rm: {target}: {error}");
                    run.code = 1;
                }
            }
        }
        run
    }

    async fn mv_cp(&self, name: &str, args: &[String], workspace: &Workspace) -> Run {
        let (flags, operands) = match split_flags(args, "rRf") {
            Ok(parsed) => parsed,
            Err(message) => return Run::fail(2, format!("{name}: {message}")),
        };
        let [from, to] = operands.as_slice() else {
            return Run::fail(2, format!("{name}: usage: {name} FROM TO"));
        };
        let from_full = self.resolve(from);
        let mut to_full = self.resolve(to);
        if from_full.is_empty() {
            return Run::fail(1, format!("{name}: cannot {name} the whole repository"));
        }
        // Moving into an existing folder keeps the name, as in a shell.
        if matches!(workspace.kind(&to_full).await, Ok(Some(EntryKind::Dir))) {
            to_full = path::join(&to_full, path::file_name(&from_full));
        }
        let result = if name == "mv" {
            workspace.rename(&from_full, &to_full).await
        } else {
            let recursive = flags.contains(&'r') || flags.contains(&'R');
            workspace.copy(&from_full, &to_full, recursive).await
        };
        match result {
            Ok(_) => Run::default(),
            Err(error) => Run::from_error(name, &error),
        }
    }

    async fn open(
        &self,
        name: &str,
        args: &[String],
        workspace: &Workspace,
        output: &mut Output,
    ) -> Run {
        let [file] = args else {
            return Run::fail(2, format!("{name}: give one file name"));
        };
        let full = self.resolve(file);
        match workspace.kind(&full).await {
            Ok(Some(EntryKind::File | EntryKind::Symlink)) => {
                output.open = Some(full);
                Run::default()
            }
            Ok(Some(_)) => Run::fail(1, format!("{name}: {file} is not a file")),
            // Opening a file that does not exist yet starts a new one.
            Ok(None) => match workspace.write(&full, "").await {
                Ok(_) => {
                    output.open = Some(full);
                    Run::default()
                }
                Err(error) => Run::from_error(name, &error),
            },
            Err(error) => Run::from_error(name, &error),
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn git(&self, args: &[String], workspace: &Workspace) -> Run {
        let Some((sub, rest)) = args.split_first() else {
            return Run::fail(
                2,
                "git: give a subcommand: status, diff, commit -m, log, branch, switch, restore, pull",
            );
        };
        match sub.as_str() {
            "status" => {
                let mut out = format!("On branch {}\n", workspace.branch());
                let changes = workspace.changes();
                if changes.is_empty() {
                    out.push_str("Nothing changed since the last commit.\n");
                }
                for change in changes {
                    let word = match change.kind {
                        ChangeKind::Added => "new file:",
                        ChangeKind::Modified => "modified:",
                        ChangeKind::Deleted => "deleted: ",
                    };
                    let _ = writeln!(out, "  {word} {}  [{}]", change.path, change.state.label());
                }
                Run::ok(out)
            }
            "diff" => {
                let wanted: Vec<String> = rest.iter().map(|arg| self.resolve(arg)).collect();
                let mut out = String::new();
                for change in workspace.changes() {
                    if !wanted.is_empty()
                        && !wanted.iter().any(|dir| path::is_within(&change.path, dir))
                    {
                        continue;
                    }
                    match workspace.diff(&change.path).await {
                        Ok(text) => out.push_str(&text),
                        Err(error) => return Run::from_error("git diff", &error),
                    }
                }
                Run::ok(out)
            }
            "add" => Run::ok(
                "Nothing to do: there is no staging step here. git commit -m \"message\" commits every change.\n",
            ),
            "commit" => {
                let message = match rest {
                    [flag, message] if flag == "-m" || flag == "-am" => message.clone(),
                    [flag, message] if flag == "-a" && message.starts_with("-m") => {
                        message[2..].to_owned()
                    }
                    [a, flag, message] if a == "-a" && flag == "-m" => message.clone(),
                    _ => {
                        return Run::fail(2, "git commit: usage: git commit -m \"message\"");
                    }
                };
                match workspace.commit(&message).await {
                    Ok(outcome) => Run::ok(format!(
                        "[{} {}] {}\n {} file{} changed\n",
                        workspace.branch(),
                        &outcome.sha[..outcome.sha.len().min(7)],
                        message.lines().next().unwrap_or_default(),
                        outcome.files,
                        if outcome.files == 1 { "" } else { "s" }
                    )),
                    Err(error) => Run::from_error("git commit", &error),
                }
            }
            "log" => {
                let mut rest = rest.to_vec();
                let count = match take_count(&mut rest, 10) {
                    Ok(count) => count,
                    Err(message) => return Run::fail(2, format!("git log: {message}")),
                };
                match workspace
                    .github()
                    .log(workspace.repo(), &workspace.branch(), count)
                    .await
                {
                    Ok(entries) => {
                        let mut out = String::new();
                        for entry in entries {
                            let _ = writeln!(
                                out,
                                "{} {} ({}, {})",
                                &entry.sha[..entry.sha.len().min(7)],
                                entry.message.lines().next().unwrap_or_default(),
                                entry.author,
                                entry.date.get(..10).unwrap_or(&entry.date)
                            );
                        }
                        Run::ok(out)
                    }
                    Err(error) => Run::from_error("git log", &error),
                }
            }
            "branch" if rest.is_empty() => {
                match workspace.github().branches(workspace.repo()).await {
                    Ok(branches) => {
                        let current = workspace.branch();
                        let mut out = String::new();
                        for branch in branches {
                            let mark = if branch == current { "*" } else { " " };
                            let _ = writeln!(out, "{mark} {branch}");
                        }
                        Run::ok(out)
                    }
                    Err(error) => Run::from_error("git branch", &error),
                }
            }
            "switch" | "checkout" | "branch" => {
                let (create, name) = match rest {
                    [flag, name] if flag == "-c" || flag == "-b" => (true, name),
                    [name] if sub == "branch" => (true, name),
                    [name] if !name.starts_with('-') => (false, name),
                    _ => {
                        return Run::fail(2, format!("git {sub}: usage: git {sub} [-c] BRANCH"));
                    }
                };
                match workspace.switch_branch(name, create).await {
                    Ok(()) => {
                        *self.cwd.borrow_mut() = String::new();
                        Run::ok(format!(
                            "{} branch {}\n",
                            if create {
                                "Created and switched to"
                            } else {
                                "Switched to"
                            },
                            workspace.branch()
                        ))
                    }
                    Err(error) => Run::from_error(&format!("git {sub}"), &error),
                }
            }
            "restore" => {
                if rest.is_empty() {
                    return Run::fail(2, "git restore: give a file, or . for everything");
                }
                let mut restored = 0;
                for arg in rest {
                    let dir = self.resolve(arg);
                    for change in workspace.changes() {
                        if path::is_within(&change.path, &dir) && workspace.discard(&change.path) {
                            restored += 1;
                        }
                    }
                }
                Run::ok(format!(
                    "Restored {restored} file{} to the last commit.\n",
                    if restored == 1 { "" } else { "s" }
                ))
            }
            "pull" | "fetch" => match workspace.refresh().await {
                Ok(conflicts) if conflicts.is_empty() => Run::ok(format!(
                    "Up to date with GitHub at {}.\n",
                    &workspace.head()[..7.min(workspace.head().len())]
                )),
                Ok(conflicts) => Run::fail(
                    1,
                    format!(
                        "Updated, but these files changed on GitHub and here: {}. Committing is blocked until each is restored (git restore FILE) or re-applied.",
                        conflicts.join(", ")
                    ),
                ),
                Err(error) => Run::from_error("git pull", &error),
            },
            "push" => Run::ok(
                "Nothing to do: git commit here writes straight to GitHub, so there is nothing left to push.\n",
            ),
            other => Run::fail(
                2,
                format!(
                    "git {other}: not supported here. Supported: status, diff, commit -m, log, branch, switch, restore, pull."
                ),
            ),
        }
    }

    /// Completes the last word of `line`: a command name for the first word,
    /// otherwise a path.
    pub async fn complete(&self, line: &str, workspace: &Workspace) -> Completion {
        let split = line
            .rfind([' ', '|', ';', '&'])
            .map_or(0, |index| index + 1);
        let (before, word) = line.split_at(split);
        let is_command = before
            .trim_end()
            .chars()
            .next_back()
            .is_none_or(|c| matches!(c, '|' | ';' | '&'));
        let mut candidates: Vec<String> = if is_command {
            self.command_names()
                .into_iter()
                .filter(|name| name.starts_with(word))
                .map(|name| format!("{name} "))
                .collect()
        } else {
            let (dir_typed, prefix) = match word.rsplit_once('/') {
                Some((dir, prefix)) => (format!("{dir}/"), prefix),
                None => (String::new(), word),
            };
            let dir = self.resolve(if dir_typed.is_empty() {
                "."
            } else {
                &dir_typed
            });
            workspace
                .list_dir(&dir)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| entry.name.starts_with(prefix))
                .filter(|entry| prefix.starts_with('.') || !entry.name.starts_with('.'))
                .map(|entry| {
                    let end = if entry.kind == EntryKind::Dir {
                        "/"
                    } else {
                        " "
                    };
                    format!("{dir_typed}{}{end}", entry.name)
                })
                .collect()
        };
        candidates.sort();
        let completed = match candidates.as_slice() {
            [] => word.to_owned(),
            [only] => only.clone(),
            many => {
                let first = &many[0];
                let mut end = first.len();
                for other in &many[1..] {
                    end = end.min(
                        first
                            .char_indices()
                            .zip(other.chars())
                            .find(|((_, a), b)| a != b)
                            .map_or(first.len().min(other.len()), |((index, _), _)| index),
                    );
                }
                first[..end].to_owned()
            }
        };
        if candidates.len() < 2 {
            candidates.clear();
        }
        Completion {
            line: format!("{before}{completed}"),
            candidates: candidates
                .into_iter()
                .map(|candidate| candidate.trim_end().to_owned())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        tokenize(line)
            .unwrap()
            .into_iter()
            .filter_map(|token| match token {
                Token::Word { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn quoting() {
        assert_eq!(
            words(r#"echo "a b" 'c d' e\ f"#),
            ["echo", "a b", "c d", "e f"]
        );
        assert_eq!(words(r#"echo "say \"hi\"""#), ["echo", r#"say "hi""#]);
        assert_eq!(words("echo '' x"), ["echo", "", "x"]);
        assert_eq!(words("ls # a comment"), ["ls"]);
        assert_eq!(words("echo a#b"), ["echo", "a#b"]);
    }

    #[test]
    fn globs_only_when_unquoted() {
        let tokens = tokenize("ls *.py '*.rs'").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Word {
                    text: "ls".into(),
                    glob: false
                },
                Token::Word {
                    text: "*.py".into(),
                    glob: true
                },
                Token::Word {
                    text: "*.rs".into(),
                    glob: false
                },
            ]
        );
    }

    #[test]
    fn rejects_unsupported_syntax_by_name() {
        for (line, needle) in [
            ("echo $(date)", "$( )"),
            ("echo `date`", "backtick"),
            ("cat < f", "<"),
            ("a || b", "||"),
            ("sleep 1 &", "background"),
            ("echo 'open", "unterminated"),
        ] {
            let error = tokenize(line).unwrap_err();
            assert!(error.contains(needle), "{line}: {error}");
        }
    }

    #[test]
    fn parses_pipelines_and_redirects() {
        let pipelines = parse(tokenize("cat a | grep x > out.txt && ls ; pwd").unwrap()).unwrap();
        assert_eq!(pipelines.len(), 3);
        assert_eq!(pipelines[0].commands.len(), 2);
        assert_eq!(
            pipelines[0].commands[1].redirect,
            Some(("out.txt".to_owned(), false))
        );
        assert!(pipelines[1].needs_success);
        assert!(!pipelines[2].needs_success);
        assert!(parse(tokenize("| ls").unwrap()).is_err());
        assert!(parse(tokenize("ls >").unwrap()).is_err());
        assert!(parse(tokenize("ls &&").unwrap()).is_err());
    }

    #[test]
    fn counts() {
        let mut args = vec!["-n".to_owned(), "3".to_owned(), "f".to_owned()];
        assert_eq!(take_count(&mut args, 10), Ok(3));
        assert_eq!(args, ["f"]);
        let mut args = vec!["-5".to_owned()];
        assert_eq!(take_count(&mut args, 10), Ok(5));
        let mut args = vec!["-n".to_owned()];
        assert!(take_count(&mut args, 10).is_err());
    }
}
