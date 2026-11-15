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
    AnyElement, App, ClickEvent, Context, Div, ElementId, Entity, SharedString, Subscription,
    Window, div, px, relative,
};
use silver_wombat_core::Error;
use silver_wombat_core::fake::FakeGitHub;
use silver_wombat_core::github::{GitHub, Repo};
use silver_wombat_core::path;
use silver_wombat_core::shell::Shell;
use silver_wombat_core::workspace::{
    Change, ChangeKind, DirEntry, EntryKind, FileContent, SaveState, Workspace,
};

use crate::mirror::{self, Action, Role};
use crate::platform::{self, FetchHttp, Kept};
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
    /// Where unsaved work went the last time it was saved on this device.
    kept: Option<Kept>,
    /// The browser's answer to "do not evict this site's storage".
    persistent: Option<bool>,
    asked_persistence: bool,
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
            kept: None,
            persistent: None,
            asked_persistence: false,
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
                let mut found_in = None;
                if let Some((saved, place)) = platform::recall(&drafts_key(&workspace, demo)).await
                {
                    // Saved changes that cannot be read are reported, not dropped silently.
                    restored = workspace.import_drafts(&saved)?;
                    found_in = Some(place);
                }
                let entries = workspace.list_dir("").await?;
                Ok::<_, Error>((workspace, restored, found_in, entries))
            }
            .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok((workspace, restored, found_in, entries)) => {
                    if found_in.is_some() {
                        this.kept = found_in;
                    }
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
            let kept = platform::keep(&drafts_key(&workspace, demo), &json).await;
            // After the first successful save, ask the browser once not to
            // evict this site's storage when the device runs low on space.
            let ask = kept.is_ok()
                && this
                    .update(cx, |this, _| {
                        !std::mem::replace(&mut this.asked_persistence, true)
                    })
                    .unwrap_or(false);
            let persistent = if ask {
                platform::request_persistence().await
            } else {
                None
            };
            this.update(cx, |this, cx| {
                if ask {
                    this.persistent = persistent;
                }
                if let Some(work) = &mut this.work {
                    match &kept {
                        Ok(place) => {
                            // Only now is it true that the device has the changes.
                            workspace.mark_on_device(revision);
                            work.device_error = None;
                            this.kept = Some(*place);
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
        let line = match at {
            Some(index) => history[index].clone(),
            None => String::new(),
        };
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

    fn export_patch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = workspace.patch().await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(patch) => {
                        let name = format!(
                            "{}-{}.patch",
                            workspace.repo().replace('/', "-"),
                            workspace.branch().replace('/', "-")
                        );
                        this.notice = Some(match platform::download(&name, &patch) {
                            Ok(()) => format!(
                                "Exported your changes as {name}. Apply it on a computer with: git apply {name}"
                            ),
                            Err(reason) => format!("Could not export the changes: {reason}"),
                        });
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
    //
    // Everything the person can tap, type into or read goes through `tap`,
    // `field`, `code` or `say`, which draw it and also describe it to
    // assistive technology (see `mirror.rs`).

    fn tap(
        cx: &mut Context<Self>,
        tap: Tap,
        on: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Div {
        let on = Rc::new(on);
        let entity = cx.entity().downgrade();
        let action: Action = {
            let on = on.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let _ = entity.update(cx, |this, cx| on(this, window, cx));
            })
        };
        let mut button = Button::new(ElementId::Name(tap.key.clone().into()))
            .label(tap.label.clone())
            .large()
            .h(px(tap.height))
            .disabled(tap.disabled)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| on(this, window, cx)));
        button = match tap.kind {
            Kind::Primary => button.primary(),
            Kind::Ghost => button.ghost(),
            Kind::Normal => button,
        };
        if let Some(current) = tap.current {
            // The open pane is outlined, not only tinted, so it does not rely on colour.
            button = button.compact().px_1().text_sm().toggled(current);
            button = if current {
                button.outline()
            } else {
                button.ghost()
            };
        }
        if tap.fill {
            button = button.w_full();
        }
        if tap.start {
            button = button.justify_start();
        }
        let role = match tap.current {
            Some(current) => Role::Tab { current },
            None => Role::Button,
        };
        div()
            .relative()
            .when(tap.fill, |wrapper| wrapper.flex_1().min_w_0())
            .child(button)
            // A disabled button must do nothing, however it is activated.
            .child(mirror::mark(
                tap.key,
                role,
                tap.label,
                tap.disabled,
                (!tap.disabled).then_some(action),
            ))
    }

    fn field(key: &str, label: &str, state: &Entity<InputState>) -> Div {
        let target = state.clone();
        let action: Action = Rc::new(move |window: &mut Window, cx: &mut App| {
            target.update(cx, |input, cx| input.focus(window, cx));
        });
        div()
            .relative()
            .child(Input::new(state).large().h(px(TAP)))
            .child(mirror::mark(
                key,
                Role::TextField { multiline: false },
                label,
                false,
                Some(action),
            ))
    }

    /// An editor that fills the space it is given.
    fn code(key: &str, label: String, state: &Entity<EditorState>, family: &'static str) -> Div {
        let target = state.clone();
        let action: Action = Rc::new(move |window: &mut Window, cx: &mut App| {
            target.update(cx, |editor, cx| editor.focus(window, cx));
        });
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                Editor::new(state)
                    .aria_label(label.clone())
                    .font_family(family)
                    .line_height(relative(1.5))
                    .h_full(),
            )
            .child(mirror::mark(
                key,
                Role::TextField { multiline: true },
                label,
                false,
                Some(action),
            ))
    }

    fn say(key: &str, role: Role, text: impl Into<SharedString>) -> Div {
        let text: SharedString = text.into();
        div().relative().child(text.clone()).child(mirror::mark(
            key,
            role,
            text.to_string(),
            false,
            None,
        ))
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
                .child(Self::say("notice", Role::Status, text).flex_1())
                .child(Self::tap(
                    cx,
                    Tap::new("notice-dismiss", "Dismiss").ghost(),
                    |this, _, cx| {
                        this.notice = None;
                        cx.notify();
                    },
                ))
                .into_any_element(),
        )
    }

    fn sign_in_screen(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .size_full()
            .p_5()
            .gap_4()
            .child(Self::say("title", Role::Heading, "silver-wombat").text_2xl())
            .child(Self::say(
                "intro",
                Role::Text,
                "Edit a GitHub repository from this browser. Nothing is installed and there is no server.",
            ))
            .children(self.notice_bar(cx))
            .child(
                v_flex()
                    .gap_2()
                    .child("GitHub token")
                    .child(Self::field("token", "GitHub token", &self.token_input))
                    .child(Self::tap(
                        cx,
                        Tap::new("sign-in", if self.busy { "Signing in…" } else { "Sign in" })
                            .primary()
                            .disabled(self.busy),
                        Self::sign_in,
                    )),
            )
            .child(
                Self::say(
                    "token-help",
                    Role::Text,
                    "Use a fine-grained personal access token with Contents: Read and write on the repositories you want to edit. It is kept in this browser's storage on this device and sent only to api.github.com.",
                )
                .text_color(muted),
            )
            .child(Self::tap(
                cx,
                Tap::new("demo", "Try the demo (no account)").disabled(self.busy),
                Self::start_demo,
            ))
            .into_any_element()
    }

    fn repos_screen(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let login = match &self.session {
            Some(session) => session.login.clone(),
            None => String::new(),
        };
        let rows: Vec<Div> = self
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
                Self::tap(
                    cx,
                    Tap::new(format!("repo-{index}"), label)
                        .start()
                        .disabled(self.busy),
                    move |this, window, cx| {
                        this.open_repo(full_name.clone(), branch.clone(), window, cx);
                    },
                )
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
                        Self::say("title", Role::Heading, format!("Repositories of {login}"))
                            .flex_1()
                            .text_xl(),
                    )
                    .child(Self::tap(
                        cx,
                        Tap::new("sign-out", "Sign out"),
                        |this, window, cx| {
                            this.sign_out(window, cx);
                        },
                    )),
            )
            .children(self.notice_bar(cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Self::field("repo-name", "Repository, as owner/name", &self.repo_input)
                            .flex_1(),
                    )
                    .child(Self::tap(
                        cx,
                        Tap::new("open-typed", "Open").primary().disabled(self.busy),
                        Self::open_typed_repo,
                    )),
            )
            .child(Self::say(
                "repo-hint",
                Role::Status,
                if self.busy {
                    "Working…"
                } else {
                    "Most recently pushed first."
                },
            ))
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
        let mut rows: Vec<Div> = Vec::new();
        if !dir.is_empty() {
            let parent = path::parent(&dir).to_owned();
            rows.push(Self::tap(
                cx,
                Tap::new("dir-up", "Up one folder").start(),
                move |this, window, cx| this.open_dir(parent.clone(), window, cx),
            ));
        }
        for (index, entry) in work.entries.iter().enumerate() {
            let full = path::join(&dir, &entry.name);
            let mark = match entry.change {
                Some(ChangeKind::Added) => "  (new)",
                Some(ChangeKind::Modified) => "  (edited)",
                Some(ChangeKind::Deleted) => "  (deleted)",
                None => "",
            };
            let key = format!("entry-{index}");
            rows.push(match entry.kind {
                EntryKind::Dir => Self::tap(
                    cx,
                    Tap::new(key, format!("Folder {}{mark}", entry.name)).start(),
                    move |this, window, cx| this.open_dir(full.clone(), window, cx),
                ),
                EntryKind::File | EntryKind::Symlink => {
                    let is_open = work.open.as_deref() == Some(full.as_str());
                    let label = if is_open {
                        format!("{}{mark}  (open)", entry.name)
                    } else {
                        format!("{}{mark}", entry.name)
                    };
                    Self::tap(
                        cx,
                        Tap::new(key, label).ghost().start(),
                        move |this, window, cx| {
                            this.open_file(full.clone(), window, cx);
                        },
                    )
                }
                EntryKind::Submodule => Self::tap(
                    cx,
                    Tap::new(key, format!("{} (submodule, cannot be opened)", entry.name))
                        .start()
                        .disabled(true),
                    |_, _, _| {},
                ),
            });
        }
        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .child(
                Self::say(
                    "folder",
                    Role::Section,
                    format!("Folder {}", path::display(&dir)),
                )
                .text_color(cx.theme().muted_foreground),
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
                .child(Self::say("no-file", Role::Text, "No file is open."))
                .child(Self::tap(
                    cx,
                    Tap::new("go-files", "Choose a file"),
                    |this, _, cx| {
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
                    .child(Self::say("file-name", Role::Section, path::display(&file)).flex_1())
                    .child(
                        Self::say("file-state", Role::Status, state)
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(Self::code(
                "editor",
                format!("Code editor, {}", path::display(&file)),
                &self.editor,
                if self.mono { MONO } else { SANS },
            ))
            .into_any_element()
    }

    fn terminal_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy;
        let quick = |key: &'static str, label: &'static str, cx: &mut Context<Self>| {
            Self::tap(
                cx,
                Tap::new(key, format!("Run {label}")).disabled(busy),
                move |this, window, cx| this.run_command(label.to_owned(), window, cx),
            )
        };
        // What a screen reader hears: the most recent output, not the whole scrollback.
        let recent = match &self.work {
            Some(work) => tail(&work.terminal_text, 3000).to_owned(),
            None => String::new(),
        };
        v_flex()
            .size_full()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(&self.terminal)
                            .aria_label("Terminal output")
                            .line_height(relative(1.5))
                            .h_full(),
                    )
                    .child(mirror::mark(
                        "terminal-output",
                        Role::Log,
                        recent,
                        false,
                        None,
                    )),
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
                            .child(Self::field("command", "Command", &self.command_input).flex_1())
                            .child(Self::tap(
                                cx,
                                Tap::new("run", if busy { "Running…" } else { "Run" })
                                    .primary()
                                    .disabled(busy),
                                Self::run_typed_command,
                            )),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(Self::tap(
                                cx,
                                Tap::new("complete", "Complete"),
                                |this, window, cx| {
                                    this.complete_command(window, cx);
                                },
                            ))
                            .child(Self::tap(
                                cx,
                                Tap::new("previous", "Previous"),
                                |this, window, cx| {
                                    this.recall(true, window, cx);
                                },
                            ))
                            .child(Self::tap(
                                cx,
                                Tap::new("next", "Next"),
                                |this, window, cx| {
                                    this.recall(false, window, cx);
                                },
                            ))
                            .child(Self::tap(cx, Tap::new("stop", "Stop"), |this, _, _| {
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
        let shown = path::display(&change.path);
        let for_diff = change.path.clone();
        let for_discard = change.path.clone();
        v_flex()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(Self::say(
                &format!("change-{index}"),
                Role::Text,
                format!("{kind}: {shown}. {}.", change.state.label()),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(Self::tap(
                        cx,
                        Tap::new(format!("diff-{index}"), format!("Show diff of {shown}")),
                        move |this, window, cx| this.show_diff(for_diff.clone(), window, cx),
                    ))
                    .child(Self::tap(
                        cx,
                        Tap::new(
                            format!("discard-{index}"),
                            format!("Discard change to {shown}"),
                        ),
                        move |this, window, cx| this.discard(for_discard.clone(), window, cx),
                    )),
            )
            .into_any_element()
    }

    fn diff_view(&self, file: &str, cx: &mut Context<Self>) -> Div {
        // What a screen reader hears: the end of the diff, not all of it.
        let heard = tail(&self.diff.read(cx).value(), 3000).to_owned();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_1()
            .child(
                Self::say(
                    "diff-title",
                    Role::Section,
                    format!("Diff of {}", path::display(file)),
                )
                .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(&self.diff)
                            .aria_label("Diff")
                            .line_height(relative(1.5))
                            .h_full(),
                    )
                    .child(mirror::mark("diff-text", Role::Text, heard, false, None)),
            )
    }

    fn changes_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(work) = &self.work else {
            return div().into_any_element();
        };
        let changes: Vec<Change> = work.workspace.changes();
        let branch = work.workspace.branch();
        let diff_of = work.diff_of.clone();
        let device_error = work.device_error.clone();
        let busy = self.busy;
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
                Self::say(
                    "device-error",
                    Role::Status,
                    format!(
                        "Changes are NOT being saved on this device: {reason}. Checkpoint to GitHub so they are not lost."
                    ),
                )
                .text_color(cx.theme().danger)
            }))
            .child(Self::say(
                "change-count",
                Role::Section,
                if nothing {
                    "Nothing changed since the last commit.".to_owned()
                } else {
                    format!(
                        "{} changed file{}",
                        changes.len(),
                        if changes.len() == 1 { "" } else { "s" }
                    )
                },
            ))
            .child(
                v_flex()
                    .id("change-list")
                    .max_h(relative(0.4))
                    .gap_2()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .children(diff_of.map(|file| self.diff_view(&file, cx)))
            .child(
                v_flex()
                    .gap_2()
                    .child("Commit message")
                    .child(Self::field("commit-message", "Commit message", &self.commit_input))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(Self::tap(
                                cx,
                                Tap::new("commit", format!("Commit to {branch}"))
                                    .primary()
                                    .disabled(busy || nothing),
                                Self::commit,
                            ))
                            .child(Self::tap(
                                cx,
                                Tap::new("checkpoint", "Checkpoint now").disabled(busy || nothing),
                                Self::checkpoint_now,
                            )),
                    ),
            )
            .into_any_element()
    }

    /// Says plainly where unsaved work lives and whether the browser may clear it.
    fn storage_note(&self) -> String {
        let place = match self.kept {
            Some(Kept::Files) => {
                "Unsaved changes are kept in this browser's file storage on this device."
            }
            Some(Kept::Small) => {
                "This browser has no file storage, so unsaved changes are kept in a small store that holds about 5 MB."
            }
            None => "Unsaved changes are kept in this browser's storage on this device.",
        };
        let eviction = if self.persistent == Some(true) {
            "The browser has agreed not to clear it."
        } else {
            "The browser may clear it if the device runs low on space or the site goes unused, so the checkpoint on GitHub is the durable copy."
        };
        format!("{place} {eviction}")
    }

    fn more_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let nothing = self
            .work
            .as_ref()
            .is_none_or(|work| work.workspace.changes().is_empty());
        let demo = self.session.as_ref().is_some_and(|session| session.demo);
        let login = match &self.session {
            Some(session) => session.login.clone(),
            None => String::new(),
        };
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(Self::say(
                "account",
                Role::Text,
                if demo {
                    "Demo: nothing here is saved to GitHub.".to_owned()
                } else {
                    format!("Signed in as {login}.")
                },
            ))
            .child(Self::tap(
                cx,
                Tap::new(
                    "look",
                    format!(
                        "Theme: {}. Switch to {}",
                        self.look.name(),
                        self.look.other().name()
                    ),
                ),
                |this, _, cx| this.toggle_look(cx),
            ))
            .child(Self::tap(
                cx,
                Tap::new(
                    "mono",
                    if self.mono {
                        "Code font: fixed-width. Switch to sans"
                    } else {
                        "Code font: sans. Switch to fixed-width"
                    },
                ),
                |this, _, cx| this.toggle_mono(cx),
            ))
            .child(Self::say("storage", Role::Text, self.storage_note()))
            .child(Self::tap(
                cx,
                Tap::new("export", "Export changes as a patch file").disabled(nothing),
                Self::export_patch,
            ))
            .child(Self::tap(
                cx,
                Tap::new("repos", "Switch repository"),
                |this, _, cx| {
                    this.screen = Screen::Repos;
                    this.work = None;
                    if this.session.as_ref().is_some_and(|session| session.demo) {
                        this.session = None;
                        this.screen = Screen::SignIn;
                    }
                    cx.notify();
                },
            ))
            .child(Self::tap(
                cx,
                Tap::new("sign-out-more", "Sign out"),
                |this, window, cx| {
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
        let tabs: Vec<Div> = Pane::ALL
            .into_iter()
            .filter(|tab| !(wide && *tab == Pane::Files))
            .map(|tab| {
                Self::tap(
                    cx,
                    Tap::new(format!("tab-{}", tab.label()), tab.label()).tab(tab == pane),
                    move |this, _, cx| {
                        this.pane = tab;
                        cx.notify();
                    },
                )
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
                    .child(
                        Self::say("workspace", Role::Heading, title)
                            .flex_1()
                            .truncate(),
                    )
                    .child(
                        Self::say("save-state", Role::Status, state)
                            .text_color(cx.theme().muted_foreground),
                    ),
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

/// The last `max` bytes of `text`, starting on a character boundary.
fn tail(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Normal,
    Primary,
    Ghost,
}

/// A button, described once for both the canvas and assistive technology.
struct Tap {
    key: String,
    label: String,
    kind: Kind,
    disabled: bool,
    /// `Some` makes this a pane tab; the value says whether it is the open one.
    current: Option<bool>,
    fill: bool,
    start: bool,
    height: f32,
}

impl Tap {
    fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            kind: Kind::Normal,
            disabled: false,
            current: None,
            fill: false,
            start: false,
            height: TAP,
        }
    }

    fn primary(mut self) -> Self {
        self.kind = Kind::Primary;
        self
    }

    fn ghost(mut self) -> Self {
        self.kind = Kind::Ghost;
        self
    }

    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    fn tab(mut self, current: bool) -> Self {
        self.current = Some(current);
        self.fill = true;
        self.height = 56.0;
        self
    }

    /// Left-align the label, for rows in a list.
    fn start(mut self) -> Self {
        self.start = true;
        self
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
            .child(mirror::flush())
    }
}

/// Used by `lib.rs` to open the window.
pub fn build(window: &mut Window, cx: &mut App) -> Entity<Ide> {
    cx.new(|cx| Ide::new(window, cx))
}
