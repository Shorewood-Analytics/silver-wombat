//! The whole UI: sign-in, repository list and the workspace panes.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, Input, InputEvent, InputState, RopeExt as _,
};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, SharedString, Subscription, Window, div, px,
    relative,
};
use silver_wombat_core::Error;
use silver_wombat_core::fake::FakeGitHub;
use silver_wombat_core::github::{GitHub, Repo};
use silver_wombat_core::path;
use silver_wombat_core::shell::Shell;
use silver_wombat_core::workspace::{
    Change, ChangeKind, DirEntry, EntryKind, FileContent, SaveState, Workspace,
};

use crate::platform::{self, FetchHttp};
use crate::theme::{self, Look, MONO, SANS};

const KEY_TOKEN: &str = "silver-wombat.token";
const KEY_LOOK: &str = "silver-wombat.look";
const KEY_MONO: &str = "silver-wombat.mono";
/// Wait this long after the last edit before writing to the device.
const DEVICE_SAVE_DELAY: Duration = Duration::from_millis(400);
/// Wait this long after the last edit before checkpointing to GitHub.
const CHECKPOINT_DELAY: Duration = Duration::from_secs(5);
/// Terminal text kept on screen. Older output is dropped from the top.
const TERMINAL_KEEP_BYTES: usize = 200 * 1024;
/// Windows at least this wide show the file list beside the open pane.
const WIDE: f32 = 900.0;
/// Smallest height of anything tappable.
const TAP: f32 = 48.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    SignIn,
    Repos,
    Work,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Files,
    Editor,
    Terminal,
    Changes,
    More,
}

impl Pane {
    const ALL: [Self; 5] = [
        Self::Files,
        Self::Editor,
        Self::Terminal,
        Self::Changes,
        Self::More,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Editor => "Editor",
            Self::Terminal => "Terminal",
            Self::Changes => "Changes",
            Self::More => "More",
        }
    }
}

struct Session {
    github: GitHub,
    login: String,
    demo: bool,
}

struct Work {
    workspace: Rc<Workspace>,
    shell: Rc<Shell>,
    dir: String,
    entries: Vec<DirEntry>,
    open: Option<String>,
    terminal_text: String,
    history_at: Option<usize>,
    diff_of: Option<String>,
    /// Bumped on every local change; a delayed save only runs if it is still current.
    save_generation: u64,
    device_error: Option<String>,
}

/// The root view.
pub struct Ide {
    screen: Screen,
    pane: Pane,
    look: Look,
    mono: bool,
    busy: bool,
    notice: Option<String>,
    session: Option<Session>,
    repos: Vec<Repo>,
    work: Option<Work>,
    token_input: Entity<InputState>,
    repo_input: Entity<InputState>,
    command_input: Entity<InputState>,
    commit_input: Entity<InputState>,
    editor: Entity<EditorState>,
    terminal: Entity<EditorState>,
    diff: Entity<EditorState>,
    editor_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

fn language_for(file: &str) -> &'static str {
    match path::file_name(file).rsplit_once('.').map(|(_, ext)| ext) {
        Some("py" | "pyi") => "python",
        Some("rs") => "rust",
        Some("md" | "markdown") => "markdown",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yml" | "yaml") => "yaml",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts" | "tsx") => "typescript",
        Some("html" | "htm") => "html",
        Some("css") => "css",
        Some("sh" | "bash" | "zsh") => "bash",
        Some("swift") => "swift",
        Some("c" | "h") => "c",
        Some("sql") => "sql",
        Some("diff" | "patch") => "diff",
        _ => "text",
    }
}

fn drafts_key(workspace: &Workspace, demo: bool) -> String {
    format!(
        "silver-wombat.{}drafts.{}@{}",
        if demo { "demo." } else { "" },
        workspace.repo(),
        workspace.branch()
    )
}

fn read_only(window: &mut Window, cx: &mut Context<Ide>, language: &str) -> Entity<EditorState> {
    let language = language.to_owned();
    cx.new(|cx| {
        let mut state = EditorState::new(window, cx)
            .language(language)
            .line_number(false)
            .indent_guides(false)
            .soft_wrap(true);
        state.set_readonly(true, cx);
        state
    })
}

impl Ide {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let token_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Paste a GitHub token")
        });
        let repo_input = cx.new(|cx| InputState::new(window, cx).placeholder("owner/name"));
        let command_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Type a command, then Run"));
        let commit_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Describe the change"));
        let editor = cx.new(|cx| EditorState::new(window, cx).language("text"));
        let terminal = read_only(window, cx, "text");
        let diff = read_only(window, cx, "diff");

        let subscriptions = vec![
            cx.subscribe_in(
                &token_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.sign_in(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &repo_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.open_typed_repo(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &command_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.run_typed_command(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &commit_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.commit(window, cx);
                    }
                },
            ),
        ];

        let look = Look::from_name(&platform::load(KEY_LOOK).unwrap_or_default());
        let mono = platform::load(KEY_MONO).as_deref() == Some("1");
        theme::apply(look, cx);

        let mut this = Self {
            screen: Screen::SignIn,
            pane: Pane::Files,
            look,
            mono,
            busy: false,
            notice: None,
            session: None,
            repos: Vec::new(),
            work: None,
            token_input,
            repo_input,
            command_input,
            commit_input,
            editor,
            terminal,
            diff,
            editor_subscription: None,
            _subscriptions: subscriptions,
        };
        // A token saved on this device signs in without asking again.
        if let Some(token) = platform::load(KEY_TOKEN) {
            this.start_session(token, window, cx);
        }
        this
    }

    fn fail(&mut self, error: &Error, cx: &mut Context<Self>) {
        self.busy = false;
        self.notice = Some(error.to_string());
        if *error == Error::Unauthorized {
            platform::forget(KEY_TOKEN);
            self.session = None;
            self.work = None;
            self.screen = Screen::SignIn;
        }
        cx.notify();
    }

    // ----- Sign-in and repositories -----

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = self.token_input.read(cx).value().trim().to_owned();
        if token.is_empty() {
            self.notice = Some("Paste a GitHub token first.".to_owned());
            cx.notify();
            return;
        }
        self.start_session(token, window, cx);
    }

    fn start_session(&mut self, token: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.notice = None;
        cx.notify();
        let github = GitHub::new(Rc::new(FetchHttp::new(token.clone())));
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let user = github.user().await?;
                let repos = github.repos().await?;
                Ok::<_, Error>((user, repos))
            }
            .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok((user, repos)) => {
                    // Only a token GitHub accepted is kept.
                    if let Err(reason) = platform::save(KEY_TOKEN, &token) {
                        this.notice = Some(format!(
                            "Signed in, but the token could not be kept on this device: {reason}"
                        ));
                    }
                    this.token_input
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    this.session = Some(Session {
                        github,
                        login: user.login,
                        demo: false,
                    });
                    this.repos = repos;
                    this.screen = Screen::Repos;
                    this.busy = false;
                    cx.notify();
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn start_demo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let github = GitHub::new(Rc::new(FakeGitHub::demo()));
        self.session = Some(Session {
            github,
            login: "demo".to_owned(),
            demo: true,
        });
        self.notice = None;
        self.open_repo(
            "demo/hello-python".to_owned(),
            "main".to_owned(),
            window,
            cx,
        );
    }

    fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        platform::forget(KEY_TOKEN);
        self.session = None;
        self.work = None;
        self.repos.clear();
        self.screen = Screen::SignIn;
        self.notice = Some(
            "Signed out. Changes saved on this device are kept and come back when you reopen the repository."
                .to_owned(),
        );
        self.token_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn open_typed_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.repo_input.read(cx).value().trim().to_owned();
        if name.split('/').filter(|part| !part.is_empty()).count() != 2 {
            self.notice = Some("Type the repository as owner/name.".to_owned());
            cx.notify();
            return;
        }
        let Some(session) = &self.session else { return };
        let github = session.github.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = github.repo(&name).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(repo) => {
                    this.busy = false;
                    this.open_repo(repo.full_name, repo.default_branch, window, cx);
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn open_repo(
        &mut self,
        full_name: String,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &self.session else { return };
        if self.busy {
            return;
        }
        let github = session.github.clone();
        let demo = session.demo;
        self.busy = true;
        self.notice = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let workspace = Workspace::open(github, &full_name, &branch).await?;
                let mut restored = 0;
                if let Some(saved) = platform::load(&drafts_key(&workspace, demo)) {
                    // Saved changes that cannot be read are reported, not dropped silently.
                    restored = workspace.import_drafts(&saved)?;
                }
                let entries = workspace.list_dir("").await?;
                Ok::<_, Error>((workspace, restored, entries))
            }
            .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok((workspace, restored, entries)) => {
                    this.work = Some(Work {
                        workspace: Rc::new(workspace),
                        shell: Rc::new(Shell::new()),
                        dir: String::new(),
                        entries,
                        open: None,
                        terminal_text: String::new(),
                        history_at: None,
                        diff_of: None,
                        save_generation: 0,
                        device_error: None,
                    });
                    this.screen = Screen::Work;
                    this.pane = Pane::Files;
                    this.busy = false;
                    this.notice = (restored > 0).then(|| {
                        format!(
                            "Restored {restored} unsaved change{} from this device.",
                            if restored == 1 { "" } else { "s" }
                        )
                    });
                    this.print("Type help to see what this terminal can do.\n", window, cx);
                    cx.notify();
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    // ----- Files and the editor -----

    fn open_dir(&mut self, dir: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = workspace.list_dir(&dir).await;
            this.update_in(cx, |this, _, cx| match result {
                Ok(entries) => {
                    if let Some(work) = &mut this.work {
                        work.dir = dir;
                        work.entries = entries;
                    }
                    cx.notify();
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn refresh_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(work) = &self.work {
            let dir = work.dir.clone();
            self.open_dir(dir, window, cx);
        }
    }

    fn open_file(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = workspace.read(&file).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(FileContent::Text(text)) => {
                    let language = language_for(&file);
                    // The editor's language is fixed when it is created, so each
                    // opened file gets a fresh editor.
                    let editor = cx.new(|cx| {
                        EditorState::new(window, cx)
                            .language(language)
                            .line_number(true)
                            .soft_wrap(true)
                            .default_value(text)
                    });
                    this.editor_subscription = Some(cx.subscribe_in(
                        &editor,
                        window,
                        |this, _, event: &InputEvent, window, cx| {
                            if matches!(event, InputEvent::Change) {
                                this.editor_changed(window, cx);
                            }
                        },
                    ));
                    this.editor = editor;
                    if let Some(work) = &mut this.work {
                        work.open = Some(file);
                    }
                    this.pane = Pane::Editor;
                    this.notice = None;
                    cx.notify();
                }
                Ok(FileContent::Binary { size }) => {
                    this.notice = Some(format!(
                        "{} is not text ({size} bytes), so it cannot be opened in the editor.",
                        path::display(&file)
                    ));
                    cx.notify();
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn editor_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let Some(file) = work.open.clone() else {
            return;
        };
        let workspace = work.workspace.clone();
        let text = self.editor.read(cx).value();
        cx.spawn_in(window, async move |this, cx| {
            let result = workspace.write(&file, &text).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.changed(window, cx),
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Call after anything that may have changed local files: schedules the
    /// device save and, later, the GitHub checkpoint.
    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &mut self.work else { return };
        work.save_generation += 1;
        let generation = work.save_generation;
        let workspace = work.workspace.clone();
        let demo = self.session.as_ref().is_some_and(|session| session.demo);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DEVICE_SAVE_DELAY).await;
            let current = this
                .update(cx, |this, _| {
                    this.work
                        .as_ref()
                        .is_some_and(|work| work.save_generation == generation)
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let (revision, json) = workspace.export_drafts();
            let saved = platform::save(&drafts_key(&workspace, demo), &json);
            this.update(cx, |this, cx| {
                if let Some(work) = &mut this.work {
                    match &saved {
                        Ok(()) => {
                            // Only now is it true that the device has the changes.
                            workspace.mark_on_device(revision);
                            work.device_error = None;
                        }
                        Err(reason) => work.device_error = Some(reason.clone()),
                    }
                }
                cx.notify();
            })
            .ok();

            cx.background_executor().timer(CHECKPOINT_DELAY).await;
            let current = this
                .update(cx, |this, _| {
                    this.work
                        .as_ref()
                        .is_some_and(|work| work.save_generation == generation)
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let result = workspace.checkpoint().await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.notice = Some(format!("Checkpoint to GitHub failed: {error}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----- Terminal -----

    fn print(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &mut self.work else { return };
        work.terminal_text.push_str(text);
        if work.terminal_text.len() > TERMINAL_KEEP_BYTES {
            let mut cut = work.terminal_text.len() - TERMINAL_KEEP_BYTES;
            while !work.terminal_text.is_char_boundary(cut) {
                cut += 1;
            }
            // Drop whole lines from the top so the first line is never half a line.
            if let Some(newline) = work.terminal_text[cut..].find('\n') {
                cut += newline + 1;
            }
            work.terminal_text.drain(..cut);
        }
        let shown = work.terminal_text.clone();
        self.terminal.update(cx, |terminal, cx| {
            terminal.set_value(shown, window, cx);
            let end = terminal.text().len();
            terminal.set_cursor_position(terminal.text().offset_to_position(end), window, cx);
        });
        cx.notify();
    }

    fn run_typed_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.command_input.read(cx).value().to_string();
        self.command_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.run_command(line, window, cx);
    }

    fn run_command(&mut self, line: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || line.trim().is_empty() {
            return;
        }
        let Some(work) = &mut self.work else { return };
        work.history_at = None;
        let workspace = work.workspace.clone();
        let shell = work.shell.clone();
        let prompt = shell.prompt(&workspace);
        self.busy = true;
        self.print(&format!("{prompt} {line}\n"), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let output = shell.run(&line, &workspace).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if output.clear {
                    if let Some(work) = &mut this.work {
                        work.terminal_text.clear();
                    }
                    this.print("", window, cx);
                }
                let mut text = output.text;
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                this.print(&text, window, cx);
                this.refresh_files(window, cx);
                this.changed(window, cx);
                if let Some(file) = output.open {
                    this.open_file(file, window, cx);
                } else if let Some(file) = this.work.as_ref().and_then(|work| work.open.clone()) {
                    // The command may have rewritten the file that is open.
                    this.reload_if_changed(file, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn reload_if_changed(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        let shown = self.editor.read(cx).value();
        cx.spawn_in(window, async move |this, cx| {
            let current = workspace.read(&file).await;
            this.update_in(cx, |this, window, cx| match current {
                Ok(FileContent::Text(text)) if text.as_str() != shown.as_ref() => {
                    this.open_file(file, window, cx);
                }
                Err(Error::NotFound(_)) => {
                    if let Some(work) = &mut this.work {
                        work.open = None;
                    }
                    cx.notify();
                }
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    fn complete_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        let shell = work.shell.clone();
        let line = self.command_input.read(cx).value().to_string();
        cx.spawn_in(window, async move |this, cx| {
            let completion = shell.complete(&line, &workspace).await;
            this.update_in(cx, |this, window, cx| {
                this.command_input
                    .update(cx, |input, cx| input.set_value(completion.line, window, cx));
                if !completion.candidates.is_empty() {
                    this.print(
                        &format!("{}\n", completion.candidates.join("  ")),
                        window,
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn recall(&mut self, older: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &mut self.work else { return };
        let history = work.shell.history();
        if history.is_empty() {
            return;
        }
        let at = match (work.history_at, older) {
            (None, true) => Some(history.len() - 1),
            (Some(index), true) => Some(index.saturating_sub(1)),
            (Some(index), false) if index + 1 < history.len() => Some(index + 1),
            (_, false) => None,
        };
        work.history_at = at;
        let line = at.map(|index| history[index].clone()).unwrap_or_default();
        self.command_input
            .update(cx, |input, cx| input.set_value(line, window, cx));
    }

    // ----- Changes -----

    fn show_diff(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = workspace.diff(&file).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(text) => {
                    this.diff
                        .update(cx, |diff, cx| diff.set_value(text, window, cx));
                    if let Some(work) = &mut this.work {
                        work.diff_of = Some(file);
                    }
                    cx.notify();
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn discard(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &mut self.work else { return };
        work.workspace.discard(&file);
        if work.diff_of.as_deref() == Some(file.as_str()) {
            work.diff_of = None;
        }
        self.notice = Some(format!("Discarded the change to {}.", path::display(&file)));
        self.changed(window, cx);
        self.refresh_files(window, cx);
        if self.work.as_ref().and_then(|work| work.open.as_deref()) == Some(file.as_str()) {
            self.reload_if_changed(file, window, cx);
        }
    }

    fn checkpoint_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        if self.busy {
            return;
        }
        let workspace = work.workspace.clone();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = workspace.checkpoint().await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(Some(_)) => {
                        this.notice = Some(format!(
                            "Checkpointed to GitHub on {}.",
                            workspace.autosave_branch()
                        ));
                    }
                    Ok(None) => this.notice = Some("Nothing new to checkpoint.".to_owned()),
                    Err(error) => this.fail(&error, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        if self.busy {
            return;
        }
        let workspace = work.workspace.clone();
        let message = self.commit_input.read(cx).value().to_string();
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = workspace.commit(&message).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(outcome) => {
                        this.notice = Some(format!(
                            "Committed {} file{} to {} ({}).",
                            outcome.files,
                            if outcome.files == 1 { "" } else { "s" },
                            workspace.branch(),
                            &outcome.sha[..outcome.sha.len().min(7)]
                        ));
                        this.commit_input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        if let Some(work) = &mut this.work {
                            work.diff_of = None;
                        }
                        this.changed(window, cx);
                        this.refresh_files(window, cx);
                    }
                    Err(error) => this.fail(&error, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ----- Settings -----

    fn toggle_look(&mut self, cx: &mut Context<Self>) {
        self.look = self.look.other();
        let _ = platform::save(KEY_LOOK, self.look.name());
        theme::apply(self.look, cx);
        cx.notify();
    }

    fn toggle_mono(&mut self, cx: &mut Context<Self>) {
        self.mono = !self.mono;
        let _ = platform::save(KEY_MONO, if self.mono { "1" } else { "0" });
        cx.notify();
    }

    // ----- Drawing -----

    fn button(
        id: impl Into<gpui_kit::ElementId>,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
    ) -> Button {
        Button::new(id)
            .label(label)
            .large()
            .h(px(TAP))
            .on_click(cx.listener(on_click))
    }

    fn notice_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let text = self.notice.clone()?;
        Some(
            h_flex()
                .gap_2()
                .p_3()
                .items_start()
                .bg(cx.theme().muted)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .child(div().flex_1().child(text))
                .child(
                    Self::button("notice-dismiss", "Dismiss", cx, |this, _, _, cx| {
                        this.notice = None;
                        cx.notify();
                    })
                    .ghost(),
                )
                .into_any_element(),
        )
    }

    fn sign_in_screen(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .p_5()
            .gap_4()
            .child(div().text_2xl().child("silver-wombat"))
            .child("Edit a GitHub repository from this browser. Nothing is installed and there is no server.")
            .children(self.notice_bar(cx))
            .child(
                v_flex()
                    .gap_2()
                    .child("GitHub token")
                    .child(Input::new(&self.token_input).large().h(px(TAP)))
                    .child(
                        Self::button(
                            "sign-in",
                            if self.busy { "Signing in…" } else { "Sign in" },
                            cx,
                            |this, _, window, cx| this.sign_in(window, cx),
                        )
                        .primary()
                        .disabled(self.busy),
                    ),
            )
            .child(
                div().text_color(muted).child(
                    "Use a fine-grained personal access token with Contents: Read and write on the repositories you want to edit. It is kept in this browser's storage on this device and sent only to api.github.com.",
                ),
            )
            .child(
                Self::button("demo", "Try the demo (no account)", cx, |this, _, window, cx| {
                    this.start_demo(window, cx);
                })
                .disabled(self.busy),
            )
            .into_any_element()
    }

    fn repos_screen(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let login = self
            .session
            .as_ref()
            .map(|session| session.login.clone())
            .unwrap_or_default();
        let rows: Vec<AnyElement> = self
            .repos
            .iter()
            .enumerate()
            .map(|(index, repo)| {
                let full_name = repo.full_name.clone();
                let branch = repo.default_branch.clone();
                let label = if repo.private {
                    format!("{} (private)", repo.full_name)
                } else {
                    repo.full_name.clone()
                };
                Self::button(("repo", index), label, cx, move |this, _, window, cx| {
                    this.open_repo(full_name.clone(), branch.clone(), window, cx);
                })
                .disabled(self.busy)
                .into_any_element()
            })
            .collect();
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_xl()
                            .child(format!("Repositories of {login}")),
                    )
                    .child(Self::button(
                        "sign-out",
                        "Sign out",
                        cx,
                        |this, _, window, cx| {
                            this.sign_out(window, cx);
                        },
                    )),
            )
            .children(self.notice_bar(cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.repo_input).large().h(px(TAP))),
                    )
                    .child(
                        Self::button("open-typed", "Open", cx, |this, _, window, cx| {
                            this.open_typed_repo(window, cx);
                        })
                        .primary()
                        .disabled(self.busy),
                    ),
            )
            .child(if self.busy {
                "Working…"
            } else {
                "Most recently pushed first."
            })
            .child(
                v_flex()
                    .id("repo-list")
                    .flex_1()
                    .gap_2()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    fn files_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(work) = &self.work else {
            return div().into_any_element();
        };
        let dir = work.dir.clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        if !dir.is_empty() {
            let parent = path::parent(&dir).to_owned();
            rows.push(
                Self::button("dir-up", "Up one folder", cx, move |this, _, window, cx| {
                    this.open_dir(parent.clone(), window, cx);
                })
                .into_any_element(),
            );
        }
        for (index, entry) in work.entries.iter().enumerate() {
            let full = path::join(&dir, &entry.name);
            let mark = match entry.change {
                Some(ChangeKind::Added) => "  (new)",
                Some(ChangeKind::Modified) => "  (edited)",
                Some(ChangeKind::Deleted) => "  (deleted)",
                None => "",
            };
            let is_open = work.open.as_deref() == Some(full.as_str());
            let button = match entry.kind {
                EntryKind::Dir => Self::button(
                    ("entry", index),
                    format!("{}/{mark}", entry.name),
                    cx,
                    move |this, _, window, cx| this.open_dir(full.clone(), window, cx),
                ),
                EntryKind::File | EntryKind::Symlink => Self::button(
                    ("entry", index),
                    format!("{}{mark}", entry.name),
                    cx,
                    move |this, _, window, cx| this.open_file(full.clone(), window, cx),
                )
                .ghost()
                .toggled(is_open),
                EntryKind::Submodule => Button::new(("entry", index))
                    .label(format!("{} (submodule)", entry.name))
                    .large()
                    .h(px(TAP))
                    .disabled(true),
            };
            rows.push(button.justify_start().into_any_element());
        }
        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("Folder {}", path::display(&dir))),
            )
            .child(
                v_flex()
                    .id("file-list")
                    .flex_1()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    fn editor_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(work) = &self.work else {
            return div().into_any_element();
        };
        let Some(file) = work.open.clone() else {
            return v_flex()
                .size_full()
                .p_5()
                .gap_3()
                .child("No file is open.")
                .child(Self::button(
                    "go-files",
                    "Choose a file",
                    cx,
                    |this, _, _, cx| {
                        this.pane = Pane::Files;
                        cx.notify();
                    },
                ))
                .into_any_element();
        };
        let state = work
            .workspace
            .file_state(&file)
            .map_or("No changes", SaveState::label);
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().child(path::display(&file)))
                    .child(div().text_color(cx.theme().muted_foreground).child(state)),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Editor::new(&self.editor)
                        .aria_label(format!("Code editor, {}", path::display(&file)))
                        .font_family(if self.mono { MONO } else { SANS })
                        .line_height(relative(1.5))
                        .h_full(),
                ),
            )
            .into_any_element()
    }

    fn terminal_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let quick = |id: &'static str, label: &'static str, cx: &mut Context<Self>| {
            Self::button(id, label, cx, move |this, _, window, cx| {
                this.run_command(label.to_owned(), window, cx);
            })
            .disabled(self.busy)
        };
        v_flex()
            .size_full()
            .child(
                div().flex_1().min_h_0().child(
                    Editor::new(&self.terminal)
                        .aria_label("Terminal output")
                        .line_height(relative(1.5))
                        .h_full(),
                ),
            )
            .child(
                v_flex()
                    .p_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.command_input).large().h(px(TAP))),
                            )
                            .child(
                                Self::button(
                                    "run",
                                    if self.busy { "Running…" } else { "Run" },
                                    cx,
                                    |this, _, window, cx| this.run_typed_command(window, cx),
                                )
                                .primary()
                                .disabled(self.busy),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(Self::button(
                                "complete",
                                "Complete",
                                cx,
                                |this, _, window, cx| {
                                    this.complete_command(window, cx);
                                },
                            ))
                            .child(Self::button(
                                "previous",
                                "Previous",
                                cx,
                                |this, _, window, cx| {
                                    this.recall(true, window, cx);
                                },
                            ))
                            .child(Self::button("next", "Next", cx, |this, _, window, cx| {
                                this.recall(false, window, cx);
                            }))
                            .child(Self::button("stop", "Stop", cx, |this, _, _, _| {
                                if let Some(work) = &this.work {
                                    work.shell.stop();
                                }
                            }))
                            .child(quick("quick-ls", "ls", cx))
                            .child(quick("quick-status", "git status", cx))
                            .child(quick("quick-clear", "clear", cx)),
                    ),
            )
            .into_any_element()
    }

    fn change_row(index: usize, change: &Change, cx: &mut Context<Self>) -> AnyElement {
        let kind = match change.kind {
            ChangeKind::Added => "New",
            ChangeKind::Modified => "Edited",
            ChangeKind::Deleted => "Deleted",
        };
        let for_diff = change.path.clone();
        let for_discard = change.path.clone();
        v_flex()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(format!("{kind}: {}", path::display(&change.path)))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(change.state.label()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Self::button(
                        ("diff", index),
                        "Show diff",
                        cx,
                        move |this, _, window, cx| this.show_diff(for_diff.clone(), window, cx),
                    ))
                    .child(Self::button(
                        ("discard", index),
                        "Discard",
                        cx,
                        move |this, _, window, cx| this.discard(for_discard.clone(), window, cx),
                    )),
            )
            .into_any_element()
    }

    fn changes_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(work) = &self.work else {
            return div().into_any_element();
        };
        let changes: Vec<Change> = work.workspace.changes();
        let branch = work.workspace.branch();
        let diff_of = work.diff_of.clone();
        let device_error = work.device_error.clone();
        let muted = cx.theme().muted_foreground;
        let rows: Vec<AnyElement> = changes
            .iter()
            .enumerate()
            .map(|(index, change)| Self::change_row(index, change, cx))
            .collect();
        let nothing = changes.is_empty();
        v_flex()
            .size_full()
            .p_3()
            .gap_3()
            .children(device_error.map(|reason| {
                div().text_color(cx.theme().danger).child(format!(
                    "Changes are NOT being saved on this device: {reason}. Checkpoint to GitHub so they are not lost."
                ))
            }))
            .child(if nothing {
                "Nothing changed since the last commit.".to_owned()
            } else {
                format!("{} changed file{}", changes.len(), if changes.len() == 1 { "" } else { "s" })
            })
            .child(
                v_flex()
                    .id("change-list")
                    .max_h(relative(0.4))
                    .gap_2()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .children(diff_of.map(|file| {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .child(div().text_color(muted).child(format!("Diff of {}", path::display(&file))))
                    .child(
                        div().flex_1().min_h_0().child(
                            Editor::new(&self.diff)
                                .aria_label("Diff")
                                .line_height(relative(1.5))
                                .h_full(),
                        ),
                    )
            }))
            .child(
                v_flex()
                    .gap_2()
                    .child("Commit message")
                    .child(Input::new(&self.commit_input).large().h(px(TAP)))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Self::button(
                                    "commit",
                                    format!("Commit to {branch}"),
                                    cx,
                                    |this, _, window, cx| this.commit(window, cx),
                                )
                                .primary()
                                .disabled(self.busy || nothing),
                            )
                            .child(
                                Self::button(
                                    "checkpoint",
                                    "Checkpoint now",
                                    cx,
                                    |this, _, window, cx| this.checkpoint_now(window, cx),
                                )
                                .disabled(self.busy || nothing),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn more_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let demo = self.session.as_ref().is_some_and(|session| session.demo);
        let login = self
            .session
            .as_ref()
            .map(|session| session.login.clone())
            .unwrap_or_default();
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(if demo {
                "Demo: nothing here is saved to GitHub.".to_owned()
            } else {
                format!("Signed in as {login}.")
            })
            .child(Self::button(
                "look",
                format!(
                    "Theme: {} (tap for {})",
                    self.look.name(),
                    self.look.other().name()
                ),
                cx,
                |this, _, _, cx| this.toggle_look(cx),
            ))
            .child(Self::button(
                "mono",
                if self.mono {
                    "Code font: fixed-width (tap for sans)"
                } else {
                    "Code font: sans (tap for fixed-width)"
                },
                cx,
                |this, _, _, cx| this.toggle_mono(cx),
            ))
            .child(Self::button(
                "repos",
                "Switch repository",
                cx,
                |this, _, _, cx| {
                    this.screen = Screen::Repos;
                    this.work = None;
                    if this.session.as_ref().is_some_and(|session| session.demo) {
                        this.session = None;
                        this.screen = Screen::SignIn;
                    }
                    cx.notify();
                },
            ))
            .child(Self::button(
                "sign-out-more",
                "Sign out",
                cx,
                |this, _, window, cx| {
                    this.sign_out(window, cx);
                },
            ))
            .into_any_element()
    }

    fn work_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let wide = window.viewport_size().width >= px(WIDE);
        let Some(work) = &self.work else {
            return div().into_any_element();
        };
        let title = format!("{} · {}", work.workspace.repo(), work.workspace.branch());
        let state = if work.device_error.is_some() {
            "Not saved on this device"
        } else {
            work.workspace
                .save_state()
                .map_or("No changes", SaveState::label)
        };
        let pane = if wide && self.pane == Pane::Files {
            Pane::Editor
        } else {
            self.pane
        };
        let body = match pane {
            Pane::Files => self.files_pane(cx),
            Pane::Editor => self.editor_pane(cx),
            Pane::Terminal => self.terminal_pane(cx),
            Pane::Changes => self.changes_pane(cx),
            Pane::More => self.more_pane(cx),
        };
        let tabs: Vec<AnyElement> = Pane::ALL
            .into_iter()
            .filter(|tab| !(wide && *tab == Pane::Files))
            .map(|tab| {
                let button = Button::new(tab.label())
                    .label(tab.label())
                    .compact()
                    .w_full()
                    .px_1()
                    .text_sm()
                    .h(px(56.0))
                    .toggled(tab == pane)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pane = tab;
                        cx.notify();
                    }));
                // The open pane is outlined, not only tinted, so it does not rely on colour.
                let button = if tab == pane {
                    button.outline()
                } else {
                    button.ghost()
                };
                div().flex_1().min_w_0().child(button).into_any_element()
            })
            .collect();
        let sidebar = wide.then(|| {
            div()
                .w(px(320.0))
                .h_full()
                .border_r_1()
                .border_color(cx.theme().border)
                .child(self.files_pane(cx))
        });
        v_flex()
            .size_full()
            .child(
                div()
                    .flex()
                    .when(!wide, gpui_kit::Styled::flex_col)
                    .px_3()
                    .py_2()
                    .gap_x_2()
                    .bg(cx.theme().title_bar)
                    .border_b_1()
                    .border_color(cx.theme().title_bar_border)
                    .child(div().flex_1().truncate().child(title))
                    .child(div().text_color(cx.theme().muted_foreground).child(state)),
            )
            .children(self.notice_bar(cx).map(|bar| div().p_2().child(bar)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .children(sidebar)
                    .child(div().flex_1().min_w_0().h_full().child(body)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .p_1()
                    .pb_4()
                    .bg(cx.theme().tab_bar)
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .children(tabs),
            )
            .into_any_element()
    }
}

impl Render for Ide {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = match self.screen {
            Screen::SignIn => self.sign_in_screen(cx),
            Screen::Repos => self.repos_screen(cx),
            Screen::Work => self.work_screen(window, cx),
        };
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .line_height(relative(1.5))
            .child(screen)
    }
}

/// Used by `lib.rs` to open the window.
pub fn build(window: &mut Window, cx: &mut App) -> Entity<Ide> {
    cx.new(|cx| Ide::new(window, cx))
}
