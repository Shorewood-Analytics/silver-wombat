//! The whole UI: sign-in, repository list and the workspace panes.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, Input, InputEvent, InputState, RopeExt as _,
};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, Edges, ElementId, Entity, Focusable as _,
    KeyDownEvent, Keystroke, Modifiers, Pixels, SharedString, Subscription, Window, div, px,
    relative,
};
use silver_wombat_core::Error;
use silver_wombat_core::complete::{self, Model, Processor, Reply};
use silver_wombat_core::fake::FakeGitHub;
use silver_wombat_core::github::{GitHub, Repo};
use silver_wombat_core::python::PythonTool;
use silver_wombat_core::screen::{self, Cap, KEY_PAGES, Layout, Pane, Press};
use silver_wombat_core::shell::Shell;
use silver_wombat_core::workspace::MAX_TEXT_BYTES;
use silver_wombat_core::workspace::{
    Change, ChangeKind, DirEntry, EntryKind, FileContent, SaveState, Workspace,
};
use silver_wombat_core::{palette, path};

use crate::mirror::{self, Action, Role};
use crate::platform::{self, FetchHttp, Kept};
use crate::python::WorkerPython;
use crate::suggest::SuggestWorker;
use crate::theme::{self, Look, MONO, SANS};

const KEY_TOKEN: &str = "silver-wombat.token";
/// The key of the token's password field on the sign-in screen.
const TOKEN_FIELD: &str = "token";
const KEY_LOOK: &str = "silver-wombat.look";
const KEY_MONO: &str = "silver-wombat.mono";
const KEY_KEYS: &str = "silver-wombat.keys";
const KEY_SWIPE: &str = "silver-wombat.swipe";
/// Code suggestions: "off", or where the model may run (a `Processor` name).
const KEY_SUGGEST: &str = "silver-wombat.suggest";
/// Present while the suggestion model is loading. If it is still there when
/// the app starts, the last load never finished.
const KEY_SUGGEST_LOADING: &str = "silver-wombat.suggest-loading";
/// A model description in JSON that replaces the built-in one (see README).
const KEY_SUGGEST_MODEL: &str = "silver-wombat.suggest-model";
/// How long typing must pause before a suggestion is asked for.
const SUGGEST_DELAY: Duration = Duration::from_millis(600);
/// Wait this long after the last edit before writing to the device.
const DEVICE_SAVE_DELAY: Duration = Duration::from_millis(400);
/// Wait this long after the last edit before checkpointing to GitHub.
const CHECKPOINT_DELAY: Duration = Duration::from_secs(5);
/// Terminal text kept on screen. Older output is dropped from the top.
const TERMINAL_KEEP_BYTES: usize = 200 * 1024;
/// Space between the extra keys, and at each end of their row.
const KEY_GAP: f32 = 2.0;
const KEY_EDGE: f32 = 4.0;
/// Smallest height of anything tappable.
const TAP: f32 = 48.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    SignIn,
    Repos,
    Work,
}

/// Most results the finder lists: what fits on a phone with the keyboard up.
const FINDER_ROWS: usize = 6;

/// Something the finder can do besides opening a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Show(Pane),
    RunOpenFile,
    Checkpoint,
    Theme,
    CodeFont,
    Export,
}

impl Act {
    const ALL: [Self; 10] = [
        Self::Show(Pane::Files),
        Self::Show(Pane::Editor),
        Self::Show(Pane::Terminal),
        Self::Show(Pane::Changes),
        Self::Show(Pane::More),
        Self::RunOpenFile,
        Self::Checkpoint,
        Self::Theme,
        Self::CodeFont,
        Self::Export,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Show(Pane::Files) => "Go to Files",
            Self::Show(Pane::Editor) => "Go to Editor",
            Self::Show(Pane::Terminal) => "Go to Terminal",
            Self::Show(Pane::Changes) => "Go to Changes (diff, commit)",
            Self::Show(Pane::More) => "Go to More (settings, export, sign out)",
            Self::RunOpenFile => "Run the open file with Python",
            Self::Checkpoint => "Checkpoint to GitHub now",
            Self::Theme => "Switch theme (Dark, Midnight)",
            Self::CodeFont => "Switch code font (sans, fixed-width)",
            Self::Export => "Export changes as a patch file",
        }
    }
}

/// One line of the finder's results.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Found {
    File(String),
    Act(Act),
}

/// The finder, while it is open.
struct Finder {
    /// Every file in the repository, once listed.
    files: Vec<String>,
    results: Vec<Found>,
}

/// Where the suggestion model stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelState {
    /// Not in memory.
    Unloaded,
    /// Being downloaded or started. Bytes so far, and in all.
    Loading {
        loaded: u64,
        total: u64,
    },
    Ready {
        gpu: bool,
        bytes: u64,
    },
    /// This browser has no WebGPU and the CPU has not been allowed.
    NoGpu,
    Failed(String),
    /// The last load never finished: the page was closed, or the browser
    /// ended it, while the model was loading. It is not tried again unasked,
    /// because a model too big for the device would end the page every time.
    Interrupted,
}

/// A suggestion on offer, and the place it was made for.
struct Offer {
    text: String,
    file: String,
    cursor: usize,
}

/// Code suggestions from a small model run on this device.
struct Suggest {
    model: Model,
    /// The person's setting: suggestions are wanted.
    wanted: bool,
    processor: Processor,
    state: ModelState,
    worker: Option<SuggestWorker>,
    next_id: u32,
    /// The request awaiting an answer: its id, and the file and cursor it is for.
    asked: Option<(u32, String, usize)>,
    offer: Option<Offer>,
    /// Bumped on every edit; a delayed request only goes out if still current.
    typing: u64,
}

impl Suggest {
    fn new() -> Self {
        let processor = platform::load(KEY_SUGGEST).and_then(|name| Processor::named(&name));
        let interrupted = processor.is_some() && platform::load(KEY_SUGGEST_LOADING).is_some();
        platform::forget(KEY_SUGGEST_LOADING);
        Self {
            model: platform::load(KEY_SUGGEST_MODEL)
                .and_then(|text| Model::from_json(&text))
                .unwrap_or_else(Model::tiny_starcoder),
            wanted: processor.is_some(),
            processor: processor.unwrap_or(Processor::Gpu),
            state: if interrupted {
                ModelState::Interrupted
            } else {
                ModelState::Unloaded
            },
            worker: None,
            next_id: 1,
            asked: None,
            offer: None,
            typing: 0,
        }
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

/// Which text the extra keys act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyTarget {
    Editor,
    Command,
}

struct Session {
    github: GitHub,
    login: String,
    demo: bool,
    /// The demo's stand-in for GitHub, kept so that the demo can show what
    /// happens when someone else changes the repository.
    demo_server: Option<Rc<FakeGitHub>>,
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
    /// Changed files that have also changed on GitHub, found when a commit
    /// was refused. Each has to be settled one way or the other.
    conflicts: Vec<String>,
    /// Bumped on every local change; a delayed save only runs if it is still current.
    save_generation: u64,
    device_error: Option<String>,
}

/// The root view.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent settings and flags, not states of one thing"
)]
pub struct Ide {
    screen: Screen,
    pane: Pane,
    /// What the third column shows on a screen wide enough to have one.
    side: Pane,
    look: Look,
    mono: bool,
    /// Show the extra keys even when no on-screen keyboard is open.
    keys_always: bool,
    /// A quick sideways swipe changes pane. On unless switched off.
    swipes: bool,
    /// Which group of extra keys is showing.
    key_page: usize,
    /// Set while Control is held for the next key.
    control: Option<KeyTarget>,
    /// The same fact, shared with the page-level listener that catches the
    /// next character typed (see `platform::watch_typing`).
    control_held: Rc<Cell<bool>>,
    /// Set while drawing: the extra keys are showing and act on the terminal.
    keys_beside_terminal: bool,
    finder: Option<Finder>,
    finder_input: Entity<InputState>,
    suggest: Suggest,
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
    repo_input: Entity<InputState>,
    command_input: Entity<InputState>,
    commit_input: Entity<InputState>,
    editor: Entity<EditorState>,
    terminal: Entity<EditorState>,
    diff: Entity<EditorState>,
    editor_subscription: Option<Subscription>,
    editor_watch: Option<Subscription>,
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
        let repo_input = cx.new(|cx| InputState::new(window, cx).placeholder("owner/name"));
        let command_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Type a command, then Run"));
        let commit_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Describe the change"));
        let finder_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("A few letters of its name"));
        let editor = cx.new(|cx| EditorState::new(window, cx).language("text"));
        let terminal = read_only(window, cx, "text");
        let diff = read_only(window, cx, "diff");

        let subscriptions = Self::watch_inputs(
            [&repo_input, &command_input, &finder_input, &commit_input],
            window,
            cx,
        );

        let look = Look::from_name(&platform::load(KEY_LOOK).unwrap_or_default());
        let mono = platform::load(KEY_MONO).as_deref() == Some("1");
        let keys_always = platform::load(KEY_KEYS).as_deref() == Some("always");
        theme::apply(look, cx);

        let control_held = Rc::new(Cell::new(false));
        let mut this = Self {
            screen: Screen::SignIn,
            pane: Pane::Files,
            side: Pane::Terminal,
            look,
            mono,
            keys_always,
            swipes: platform::load(KEY_SWIPE).as_deref() != Some("off"),
            key_page: 0,
            control: None,
            control_held: control_held.clone(),
            keys_beside_terminal: false,
            finder: None,
            finder_input,
            suggest: Suggest::new(),
            busy: false,
            notice: None,
            kept: None,
            persistent: None,
            asked_persistence: false,
            session: None,
            repos: Vec::new(),
            work: None,
            repo_input,
            command_input,
            commit_input,
            editor,
            terminal,
            diff,
            editor_subscription: None,
            editor_watch: None,
            _subscriptions: subscriptions,
        };
        Self::watch_page(&control_held, window, cx);
        // A token saved on this device signs in without asking again.
        if let Some(token) = platform::load(KEY_TOKEN) {
            this.start_session(token, window, cx);
        }
        this
    }

    /// The page is being hidden or closed. Keep unsaved work at once: on a
    /// phone the page may never run again. The save state moves only if that
    /// write succeeded.
    fn page_hidden(&mut self, cx: &mut Context<Self>) {
        let Some(work) = &mut self.work else { return };
        if work.workspace.save_state() != Some(SaveState::NotSaved) {
            return;
        }
        let demo = self.session.as_ref().is_some_and(|session| session.demo);
        let (revision, json) = work.workspace.export_drafts();
        match platform::keep_now(&drafts_key(&work.workspace, demo), &json) {
            Ok(()) => {
                work.workspace.mark_on_device(revision);
                work.device_error = None;
            }
            Err(reason) => work.device_error = Some(reason),
        }
        cx.notify();
    }

    /// Adds files picked from this device to the folder being shown, as
    /// unsaved changes like any other edit.
    fn import_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        let dir = work.dir.clone();
        // The picker has to be opened here, while the tap is being handled.
        let Some(picker) = platform::open_file_picker() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let files = platform::read_picked(picker, MAX_TEXT_BYTES).await;
            if files.is_empty() {
                return;
            }
            let mut added = 0;
            let mut skipped = Vec::new();
            for file in files {
                let written = match &file.text {
                    Some(text) => workspace
                        .write(&path::join(&dir, &file.name), text)
                        .await
                        .is_ok(),
                    None => false,
                };
                if written {
                    added += 1;
                } else {
                    skipped.push(file.name);
                }
            }
            this.update_in(cx, |this, window, cx| {
                let rest = if skipped.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Not added, because only text files up to 2 MB can be: {}.",
                        skipped.join(", ")
                    )
                };
                let notice = format!(
                    "Added {added} file{} to {}.{rest}",
                    if added == 1 { "" } else { "s" },
                    path::display(&dir)
                );
                this.notice = Some(notice);
                this.refresh_files(window, cx);
                this.changed(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Things the page itself is watched for, because they do not arrive as
    /// events on any one control: swipes, a character typed while Control is
    /// held on the extra keys, and the finder's keyboard shortcut.
    fn watch_page(control_held: &Rc<Cell<bool>>, window: &mut Window, cx: &mut Context<Self>) {
        // Touches are watched on the page itself, to notice a swipe between panes.
        let ide = cx.weak_entity();
        let handle = window.window_handle();
        platform::watch_touches(move |start_x, dx, dy, millis| {
            let ide = ide.clone();
            crate::with_app(|cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = ide.update(cx, |this, cx| {
                        this.touched(start_x, dx, dy, millis, window, cx);
                    });
                });
            });
        });
        let ide = cx.weak_entity();
        platform::watch_typing(control_held.clone(), move |typed| {
            let ide = ide.clone();
            crate::with_app(|cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = ide.update(cx, |this, cx| {
                        this.typed_with_control(&typed, window, cx);
                    });
                });
            });
        });
        let ide = cx.weak_entity();
        platform::watch_hidden(move || {
            let ide = ide.clone();
            crate::with_app(|cx| {
                let _ = ide.update(cx, Self::page_hidden);
            });
        });
        // With a keyboard attached, Control or Command with K opens the finder,
        // wherever the keyboard's focus is.
        let ide = cx.weak_entity();
        platform::watch_finder_shortcut(move || {
            let ide = ide.clone();
            crate::with_app(|cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = ide.update(cx, |this, cx| {
                        if this.screen == Screen::Work {
                            this.open_finder(window, cx);
                        }
                    });
                });
            });
        });
    }

    /// What Enter and typing do in each of the single-line fields.
    fn watch_inputs(
        [repo_input, command_input, finder_input, commit_input]: [&Entity<InputState>; 4],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        let mut watching = vec![
            cx.subscribe_in(
                repo_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.open_typed_repo(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                command_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.run_typed_command(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                finder_input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.pick_first(window, cx),
                    InputEvent::Change => this.find(cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                commit_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.commit(window, cx);
                    }
                },
            ),
        ];
        // Redraw when a field's text changes, so that what a screen reader is
        // told is in the field stays true.
        for field in [repo_input, command_input, finder_input, commit_input] {
            watching.push(cx.observe(field, |_, _, cx| cx.notify()));
        }
        watching
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
        let token = mirror::secret(TOKEN_FIELD).trim().to_owned();
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
            this.update_in(cx, |this, _, cx| match result {
                Ok((user, repos)) => {
                    // Only a token GitHub accepted is kept.
                    if let Err(reason) = platform::save(KEY_TOKEN, &token) {
                        this.notice = Some(format!(
                            "Signed in, but the token could not be kept on this device: {reason}"
                        ));
                    }
                    mirror::clear_secret(TOKEN_FIELD);
                    this.session = Some(Session {
                        github,
                        login: user.login,
                        demo: false,
                        demo_server: None,
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
        let server = Rc::new(FakeGitHub::demo());
        self.session = Some(Session {
            github: GitHub::new(server.clone()),
            login: "demo".to_owned(),
            demo: true,
            demo_server: Some(server),
        });
        self.notice = None;
        self.open_repo(
            "demo/hello-python".to_owned(),
            "main".to_owned(),
            window,
            cx,
        );
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) {
        platform::forget(KEY_TOKEN);
        self.session = None;
        self.work = None;
        self.repos.clear();
        self.screen = Screen::SignIn;
        self.notice = Some(
            "Signed out. Changes saved on this device are kept and come back when you reopen the repository."
                .to_owned(),
        );
        mirror::clear_secret(TOKEN_FIELD);
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
                    found_in = place;
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
                        shell: Self::new_shell(window, cx),
                        dir: String::new(),
                        entries,
                        open: None,
                        terminal_text: String::new(),
                        history_at: None,
                        diff_of: None,
                        conflicts: Vec::new(),
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

    /// Opens a file in the editor and shows the editor.
    fn open_file(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        self.load_file(file, true, window, cx);
    }

    /// Puts a file's text in the editor. With `reveal`, also switches to the
    /// editor; without, the person stays where they are (a terminal command
    /// that rewrote the open file should not pull them out of the terminal).
    fn load_file(
        &mut self,
        file: String,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                                this.suggest_after_edit(window, cx);
                            }
                        },
                    ));
                    // Redraw when the cursor moves, so that a suggestion made
                    // for another place stops being offered.
                    this.editor_watch = Some(cx.observe(&editor, |_, _, cx| cx.notify()));
                    this.suggest.offer = None;
                    this.editor = editor;
                    if let Some(work) = &mut this.work {
                        work.open = Some(file);
                    }
                    if reveal {
                        this.pane = Pane::Editor;
                        this.notice = None;
                    }
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
                            this.kept = Some(place.clone());
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

    /// A shell with Python in it. Notes from Python while it starts or loads a
    /// library are printed to the terminal as they arrive.
    fn new_shell(window: &mut Window, cx: &mut Context<Self>) -> Rc<Shell> {
        let ide = cx.entity().downgrade();
        let handle = window.window_handle();
        let print = Rc::new(move |text: String| {
            let ide = ide.clone();
            crate::with_app(|cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = ide.update(cx, |this, cx| this.print(&text, window, cx));
                });
            });
        });
        let note = print.clone();
        let progress = Rc::new(move |text: &str| note(format!("{text}\n")));
        // What a program prints is shown as it is printed.
        let output = Rc::new(move |text: &str| print(text.to_owned()));
        let shell = Shell::new();
        shell.set_live_output(true);
        shell.add_tool(Rc::new(PythonTool::new(Rc::new(WorkerPython::new(
            progress, output,
        )))));
        Rc::new(shell)
    }

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
        // Moving the output's cursor to the end scrolls it there, but it also
        // takes the keyboard's focus. Give that back: someone typing the next
        // command while the last one prints must not lose their keys, and
        // output arriving must not take Tab away from the page.
        let focused = window.focused(cx);
        self.terminal.update(cx, |terminal, cx| {
            terminal.set_value(shown, window, cx);
            let end = terminal.text().len();
            terminal.set_cursor_position(terminal.text().offset_to_position(end), window, cx);
        });
        match focused {
            Some(handle) => window.focus(&handle, cx),
            None => window.blur(cx),
        }
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
                    this.load_file(file, false, window, cx);
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

    /// Shows a file's diff: against where the change started or, for a file
    /// in conflict, against what GitHub has now.
    fn show_diff(
        &mut self,
        file: String,
        against_github: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = if against_github {
                workspace.diff_against_github(&file).await
            } else {
                workspace.diff(&file).await
            };
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
        work.conflicts.retain(|path| *path != file);
        self.notice = Some(format!("Discarded the change to {}.", path::display(&file)));
        self.changed(window, cx);
        self.refresh_files(window, cx);
        if self.work.as_ref().and_then(|work| work.open.as_deref()) == Some(file.as_str()) {
            self.reload_if_changed(file, window, cx);
        }
    }

    /// A commit was refused because files changed on GitHub too. Fetch the
    /// branch, so that each file can be compared with their version, and
    /// list the files to settle.
    fn find_conflicts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let found = workspace.refresh().await;
            this.update_in(cx, |this, window, cx| match found {
                Ok(conflicts) => {
                    this.notice = Some(format!(
                        "Not committed, and nothing on GitHub was changed. {} of your changed files {} also changed on GitHub since you started. Choose a version for each below, then commit again.",
                        conflicts.len(),
                        if conflicts.len() == 1 { "has" } else { "have" }
                    ));
                    if let Some(work) = &mut this.work {
                        work.conflicts = conflicts;
                    }
                    this.refresh_files(window, cx);
                    cx.notify();
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Settles a conflict in favour of the local change.
    fn keep_mine(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        cx.spawn_in(window, async move |this, cx| {
            let kept = workspace.keep_mine(&file).await;
            this.update_in(cx, |this, window, cx| match kept {
                Ok(()) => {
                    if let Some(work) = &mut this.work {
                        work.conflicts.retain(|path| *path != file);
                    }
                    this.notice = Some(format!(
                        "Keeping your version of {}. It will replace GitHub's when you commit.",
                        path::display(&file)
                    ));
                    this.changed(window, cx);
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Demo only: changes a file in the demo repository as if someone else
    /// had pushed, so the person can see how a conflict is handled.
    fn demo_teammate(&mut self, cx: &mut Context<Self>) {
        let (Some(session), Some(work)) = (&self.session, &self.work) else {
            return;
        };
        let Some(server) = &session.demo_server else {
            return;
        };
        server.push(
            work.workspace.repo(),
            &work.workspace.branch(),
            "Greet more warmly",
            &[(
                "app/greet.py",
                Some("def greet(name: str) -> str:\n    return f\"Hello there, {name}!\"\n"),
            )],
        );
        self.notice = Some(
            "Demo: someone else just changed app/greet.py on GitHub. Edit that file too and commit to see what happens."
                .to_owned(),
        );
        cx.notify();
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
                            work.conflicts.clear();
                        }
                        this.changed(window, cx);
                        this.refresh_files(window, cx);
                    }
                    Err(Error::Conflict(_)) => this.find_conflicts(window, cx),
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

    fn toggle_swipes(&mut self, cx: &mut Context<Self>) {
        self.swipes = !self.swipes;
        let _ = platform::save(KEY_SWIPE, if self.swipes { "on" } else { "off" });
        cx.notify();
    }

    /// A touch ended. If it was a swipe, show the next or previous pane: the
    /// same as tapping the tab beside the current one.
    fn touched(
        &mut self,
        start_x: f32,
        dx: f32,
        dy: f32,
        millis: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.swipes || self.screen != Screen::Work || self.finder.is_some() {
            return;
        }
        let page: f32 = window.viewport_size().width.into();
        let Some(swipe) = screen::swipe(start_x, dx, dy, millis, page) else {
            return;
        };
        let (covered, _) = Self::covered(window);
        let width: f32 = (window.viewport_size().width - covered.left - covered.right).into();
        let layout = Layout::for_width(width);
        let (main, side) = layout.shown(self.pane, self.side);
        if let Some(pane) = swipe.from(side.unwrap_or(main), &layout.tabs()) {
            self.pane = pane;
            if pane.is_side() {
                self.side = pane;
            }
            cx.notify();
        }
    }

    fn toggle_keys(&mut self, cx: &mut Context<Self>) {
        self.keys_always = !self.keys_always;
        let _ = platform::save(KEY_KEYS, if self.keys_always { "always" } else { "auto" });
        cx.notify();
    }

    // ----- Code suggestions -----
    //
    // A small language model run on this device (see `complete` in the core
    // crate, and `web/complete-worker.js`). It is downloaded only when the
    // person switches suggestions on, runs in a worker, and is dropped from
    // memory when they switch them off.

    fn suggest_turn_on(
        &mut self,
        processor: Processor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.suggest.wanted = true;
        self.suggest.processor = processor;
        let _ = platform::save(KEY_SUGGEST, processor.name());
        self.suggest_load(window, cx);
    }

    fn suggest_turn_off(&mut self, cx: &mut Context<Self>) {
        self.suggest.wanted = false;
        let _ = platform::save(KEY_SUGGEST, "off");
        self.suggest_unload(cx);
    }

    /// Ends the worker, which gives the model's memory back.
    fn suggest_unload(&mut self, cx: &mut Context<Self>) {
        self.suggest.worker = None;
        platform::forget(KEY_SUGGEST_LOADING);
        self.suggest.state = ModelState::Unloaded;
        self.suggest.asked = None;
        self.suggest.offer = None;
        cx.notify();
    }

    fn suggest_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.suggest.state,
            ModelState::Loading { .. } | ModelState::Ready { .. }
        ) {
            return;
        }
        let ide = cx.weak_entity();
        let handle = window.window_handle();
        let on_reply = Rc::new(move |reply: Reply| {
            let ide = ide.clone();
            crate::with_app(|cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = ide.update(cx, |this, cx| this.suggest_reply(reply, window, cx));
                });
            });
        });
        match SuggestWorker::start(on_reply) {
            Ok(worker) => {
                let id = self.suggest.id();
                worker.send(&self.suggest.model.load_request(id, self.suggest.processor));
                self.suggest.worker = Some(worker);
                let _ = platform::save(KEY_SUGGEST_LOADING, "1");
                self.suggest.state = ModelState::Loading {
                    loaded: 0,
                    total: 0,
                };
            }
            Err(message) => self.suggest.state = ModelState::Failed(message),
        }
        cx.notify();
    }

    fn suggest_reply(&mut self, reply: Reply, window: &mut Window, cx: &mut Context<Self>) {
        // Any answer but progress means the load is over, one way or the other.
        if matches!(self.suggest.state, ModelState::Loading { .. })
            && !matches!(reply, Reply::Progress { .. })
        {
            platform::forget(KEY_SUGGEST_LOADING);
        }
        match reply {
            Reply::Progress { loaded, total } => {
                if matches!(self.suggest.state, ModelState::Loading { .. }) {
                    self.suggest.state = ModelState::Loading { loaded, total };
                }
            }
            Reply::Ready { gpu, bytes } => {
                self.suggest.state = ModelState::Ready { gpu, bytes };
                // If the person was typing while it loaded, answer that.
                self.suggest_after_edit(window, cx);
            }
            Reply::NoGpu => {
                self.suggest.worker = None;
                self.suggest.state = ModelState::NoGpu;
            }
            Reply::Failed(message) => {
                // A failed suggestion is simply not offered. A failure to
                // load, or the worker dying, ends the worker.
                if self.suggest.asked.take().is_none()
                    || !matches!(self.suggest.state, ModelState::Ready { .. })
                {
                    self.suggest.worker = None;
                    self.suggest.state = ModelState::Failed(message);
                }
            }
            Reply::Text { id, text, stopped } => {
                let Some((asked, file, cursor)) = self.suggest.asked.clone() else {
                    return;
                };
                if asked != id {
                    return;
                }
                self.suggest.asked = None;
                let open = self.work.as_ref().and_then(|work| work.open.clone());
                let editor = self.editor.read(cx);
                // Only for the place it was asked about, if nothing has moved.
                if stopped || open.as_deref() != Some(file.as_str()) || editor.cursor() != cursor {
                    return;
                }
                let value = editor.value();
                let after = value.get(cursor..).unwrap_or("");
                self.suggest.offer =
                    complete::suggestion(&text, after).map(|text| Offer { text, file, cursor });
            }
            Reply::Done => {}
        }
        cx.notify();
    }

    /// Called after every edit: withdraws the current offer and, once typing
    /// pauses, asks for a new one.
    fn suggest_after_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suggest.offer = None;
        self.suggest.typing += 1;
        let Some(file) = self.work.as_ref().and_then(|work| work.open.clone()) else {
            return;
        };
        if !self.suggest.wanted || !self.suggest.model.suits(&file) {
            return;
        }
        // The model is brought into memory the first time it is needed, not
        // when the app opens.
        if self.suggest.state == ModelState::Unloaded {
            self.suggest_load(window, cx);
        }
        let typing = self.suggest.typing;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SUGGEST_DELAY).await;
            this.update_in(cx, |this, _, cx| {
                if this.suggest.typing == typing {
                    this.suggest_ask(file, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn suggest_ask(&mut self, file: String, cx: &mut Context<Self>) {
        if !matches!(self.suggest.state, ModelState::Ready { .. }) {
            return;
        }
        let editor = self.editor.read(cx);
        let cursor = editor.cursor();
        let Some(prompt) = self.suggest.model.prompt(&editor.value(), cursor) else {
            return;
        };
        let id = self.suggest.id();
        if let Some(worker) = &self.suggest.worker {
            worker.send(&complete::complete_request(id, &prompt));
            self.suggest.asked = Some((id, file, cursor));
        }
    }

    /// The offer, if it still fits where the cursor is.
    fn current_offer(&self, cx: &App) -> Option<&Offer> {
        let offer = self.suggest.offer.as_ref()?;
        let open = self.work.as_ref()?.open.as_deref()?;
        (open == offer.file && self.editor.read(cx).cursor() == offer.cursor).then_some(offer)
    }

    fn suggest_insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.current_offer(cx).map(|offer| offer.text.clone()) else {
            return;
        };
        self.suggest.offer = None;
        self.editor.update(cx, |editor, cx| {
            editor.focus(window, cx);
            editor.insert(text, window, cx);
        });
        cx.notify();
    }

    fn suggest_forget(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suggest_unload(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = platform::forget_model().await;
            this.update_in(cx, |this, _, cx| {
                this.notice = Some(match result {
                    Ok(true) => {
                        "The downloaded suggestion model was removed from this device.".to_owned()
                    }
                    Ok(false) => "There was no downloaded suggestion model to remove.".to_owned(),
                    Err(message) => format!("The suggestion model could not be removed: {message}"),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The offer and its two buttons, shown under the editor.
    fn suggest_bar(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let text = self.current_offer(cx)?.text.clone();
        // On one line: a suggested line break is drawn as a return sign.
        let shown = format!("Suggestion: {}", text.replace('\n', "\u{21b5} "));
        Some(
            h_flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    Self::say("suggestion", Role::Status, shown)
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(MONO),
                )
                .child(Self::tap(
                    cx,
                    Tap::new("suggest-insert", "Insert")
                        .named("Insert suggestion")
                        .primary(),
                    Self::suggest_insert,
                ))
                .child(Self::tap(
                    cx,
                    Tap::new("suggest-dismiss", "Dismiss")
                        .named("Dismiss suggestion")
                        .ghost(),
                    |this, _, cx| {
                        this.suggest.offer = None;
                        cx.notify();
                    },
                )),
        )
    }

    /// The settings for the extra keys and for swiping.
    fn touch_settings(&mut self, cx: &mut Context<Self>) -> [Div; 2] {
        [
            Self::tap(
                cx,
                // The visible label is short enough for a phone; the name
                // assistive technology reads adds what activating it does.
                if self.keys_always {
                    Tap::new("keys", "Extra keys: always shown").named(
                        "Extra keys: always shown. Switch to only with the on-screen keyboard",
                    )
                } else {
                    Tap::new("keys", "Extra keys: with the keyboard")
                        .named("Extra keys: with the keyboard. Switch to always shown")
                },
                |this, _, cx| this.toggle_keys(cx),
            ),
            Self::tap(
                cx,
                if self.swipes {
                    Tap::new("swipes", "Swipe to change pane: on")
                        .named("Swipe to change pane: on. Turn off")
                } else {
                    Tap::new("swipes", "Swipe to change pane: off")
                        .named("Swipe to change pane: off. Turn on")
                },
                |this, _, cx| this.toggle_swipes(cx),
            ),
        ]
    }

    /// Shown only where WebGPU drawing failed and the app fell back to
    /// WebGL2 (see `web/boot.js`): says so, and offers the way back.
    fn renderer_settings(cx: &mut Context<Self>) -> Option<Div> {
        (platform::load(platform::KEY_RENDERER).as_deref() == Some("webgl")).then(|| {
            v_flex()
                .gap_3()
                .child(Self::say(
                    "renderer",
                    Role::Text,
                    "Drawing uses WebGL2 on this device, because WebGPU drawing stopped working here once.",
                ))
                .child(Self::tap(
                    cx,
                    Tap::new("renderer-retry", "Try WebGPU drawing again")
                        .named("Try WebGPU drawing again (reloads the page)"),
                    |_, _, _| platform::retry_webgpu(),
                ))
        })
    }

    /// The settings for suggestions: what they cost, where the model stands,
    /// and the way to switch them off and get the memory and storage back.
    fn suggest_settings(&mut self, cx: &mut Context<Self>) -> Vec<Div> {
        let model = &self.suggest.model;
        let name = model.name.clone();
        let sizes = if model.megabytes_gpu_f16 == model.megabytes_gpu {
            format!("about {} MB", model.megabytes_gpu)
        } else {
            format!(
                "{} or {} MB, depending on the device",
                model.megabytes_gpu_f16, model.megabytes_gpu
            )
        };
        let cpu_megabytes = model.megabytes_cpu;
        let only = if model.extensions.is_empty() {
            String::new()
        } else {
            format!(" Offered in .{} files.", model.extensions.join(" and ."))
        };
        let mut rows = Vec::new();
        let note = match &self.suggest.state {
            _ if !self.suggest.wanted => format!(
                "Code suggestions are off. They come from a small model, {name}, that runs on this device. Switching them on downloads it once: {sizes}.{only}"
            ),
            ModelState::Unloaded => format!(
                "Code suggestions are on. The model, {name}, is loaded when you next edit a file it is for.{only}"
            ),
            ModelState::Loading { total: 0, .. } => {
                "Getting the suggestion model ready.".to_owned()
            }
            ModelState::Loading { loaded, total } => format!(
                "Getting the suggestion model: {} of {} MB.",
                complete::megabytes(*loaded),
                complete::megabytes(*total)
            ),
            ModelState::Ready { gpu, bytes } => format!(
                "Code suggestions are on. {name} ({} MB) is running on {}.{only}",
                complete::megabytes(*bytes),
                if *gpu {
                    "the GPU (WebGPU)"
                } else {
                    "the CPU, which is slower"
                }
            ),
            ModelState::NoGpu => format!(
                "This browser has no WebGPU, so the suggestion model cannot run on the GPU. It can run on the CPU instead: slower, and a download of about {cpu_megabytes} MB."
            ),
            ModelState::Failed(message) => {
                format!("The suggestion model could not be started: {message}")
            }
            ModelState::Interrupted => "The suggestion model was not started. Last time, the page closed while it was loading. If that was the browser running out of memory, it is likely to happen again.".to_owned(),
        };
        rows.push(Self::say("suggest-note", Role::Status, note));
        self.suggest_choices(&mut rows, cx);
        rows.push(Self::tap(
            cx,
            Tap::new("suggest-forget", "Remove the downloaded model")
                .named("Remove the downloaded model for code suggestions")
                .ghost(),
            Self::suggest_forget,
        ));
        rows
    }

    /// The buttons that go with where the model stands.
    fn suggest_choices(&self, rows: &mut Vec<Div>, cx: &mut Context<Self>) {
        let stuck = matches!(
            self.suggest.state,
            ModelState::Failed(_) | ModelState::Interrupted
        );
        if !self.suggest.wanted {
            rows.push(Self::tap(
                cx,
                Tap::new("suggest-on", "Turn code suggestions on"),
                |this, window, cx| this.suggest_turn_on(Processor::Gpu, window, cx),
            ));
            return;
        }
        if self.suggest.state == ModelState::NoGpu {
            rows.push(Self::tap(
                cx,
                Tap::new("suggest-cpu", "Run suggestions on the CPU"),
                |this, window, cx| this.suggest_turn_on(Processor::GpuOrCpu, window, cx),
            ));
            return;
        }
        if stuck {
            rows.push(Self::tap(
                cx,
                Tap::new("suggest-retry", "Try again"),
                |this, window, cx| {
                    let processor = this.suggest.processor;
                    this.suggest_turn_on(processor, window, cx);
                },
            ));
            // The GPU may be what failed; the CPU is slower but separate.
            if self.suggest.processor != Processor::Cpu {
                rows.push(Self::tap(
                    cx,
                    Tap::new("suggest-cpu-instead", "Use the CPU instead")
                        .named("Use the CPU instead for code suggestions"),
                    |this, window, cx| this.suggest_turn_on(Processor::Cpu, window, cx),
                ));
            }
        }
        rows.push(Self::tap(
            cx,
            Tap::new("suggest-off", "Turn code suggestions off")
                .named("Turn code suggestions off and free their memory"),
            |this, _, cx| this.suggest_turn_off(cx),
        ));
    }

    // ----- Finder -----
    //
    // "Go to": type a few letters of a file's name, or of an action, and
    // pick it. One way to reach everything without walking folders or tabs.

    fn open_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(work) = &self.work else { return };
        let workspace = work.workspace.clone();
        self.finder = Some(Finder {
            files: Vec::new(),
            results: Vec::new(),
        });
        self.finder_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.find(cx);
        cx.spawn_in(window, async move |this, cx| {
            let listed = workspace.files_under("").await;
            this.update_in(cx, |this, _, cx| match listed {
                Ok(files) => {
                    if let Some(finder) = &mut this.finder {
                        finder.files = files.into_iter().map(|file| file.path).collect();
                        this.find(cx);
                    }
                }
                Err(error) => this.fail(&error, cx),
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn close_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finder = None;
        // The finder's field is going away. Let go of the keyboard, so that
        // Tab moves between the page's controls again.
        window.blur(cx);
        cx.notify();
    }

    /// Ranks files and actions against what has been typed.
    fn find(&mut self, cx: &mut Context<Self>) {
        let query = self.finder_input.read(cx).value();
        let Some(finder) = &mut self.finder else {
            return;
        };
        let files = finder.files.iter().map(String::as_str);
        let actions = Act::ALL.iter().map(|act| act.label());
        let best = palette::rank(&query, files.chain(actions), FINDER_ROWS);
        let results = best
            .into_iter()
            .map(|found| match finder.files.get(found.index) {
                Some(file) => Found::File(file.clone()),
                None => Found::Act(Act::ALL[found.index - finder.files.len()]),
            })
            .collect();
        finder.results = results;
        cx.notify();
    }

    fn pick_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self
            .finder
            .as_ref()
            .and_then(|finder| finder.results.first().cloned());
        if let Some(found) = first {
            self.pick(found, window, cx);
        }
    }

    fn pick(&mut self, found: Found, window: &mut Window, cx: &mut Context<Self>) {
        self.close_finder(window, cx);
        match found {
            Found::File(file) => self.open_file(file, window, cx),
            Found::Act(Act::Show(pane)) => {
                self.pane = pane;
                if pane.is_side() {
                    self.side = pane;
                }
            }
            Found::Act(Act::RunOpenFile) => {
                let open = self.work.as_ref().and_then(|work| work.open.clone());
                self.pane = Pane::Terminal;
                self.side = Pane::Terminal;
                match open {
                    // Quoted, so a name with a space in it stays one word.
                    Some(file) => {
                        let quoted = path::display(&file).replace('\'', "'\\''");
                        self.run_command(format!("python '{quoted}'"), window, cx);
                    }
                    None => self.print("No file is open to run.\n", window, cx),
                }
            }
            Found::Act(Act::Checkpoint) => self.checkpoint_now(window, cx),
            Found::Act(Act::Theme) => self.toggle_look(cx),
            Found::Act(Act::CodeFont) => self.toggle_mono(cx),
            Found::Act(Act::Export) => self.export_patch(window, cx),
        }
        cx.notify();
    }

    fn finder_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let results = match &self.finder {
            Some(finder) => finder.results.clone(),
            None => Vec::new(),
        };
        let none = results.is_empty();
        let rows: Vec<Div> = results
            .into_iter()
            .enumerate()
            .map(|(index, found)| {
                let (label, name) = match &found {
                    Found::File(file) => {
                        (path::display(file), format!("Open {}", path::display(file)))
                    }
                    Found::Act(act) => (act.label().to_owned(), act.label().to_owned()),
                };
                Self::tap(
                    cx,
                    Tap::new(format!("found-{index}"), label)
                        .named(name)
                        .start(),
                    move |this, window, cx| this.pick(found.clone(), window, cx),
                )
            })
            .collect();
        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_finder(window, cx);
                }
            }))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Self::field(
                            "finder",
                            "Find a file or action",
                            &self.finder_input,
                            Self::typed(&self.finder_input, cx),
                        )
                        .flex_1(),
                    )
                    .child(Self::tap(
                        cx,
                        Tap::new("finder-close", "Close"),
                        Self::close_finder,
                    )),
            )
            .when(none, |pane| {
                pane.child(
                    Self::say("finder-none", Role::Status, "Nothing matches.")
                        .text_color(cx.theme().muted_foreground),
                )
            })
            .children(rows)
            .into_any_element()
    }

    // ----- Extra keys -----
    //
    // The row of keys an on-screen keyboard lacks (see `screen::KEY_PAGES`).

    /// Which text the extra keys act on: the command line when the terminal
    /// is the open pane or, where both show, when it is being typed in.
    fn key_target(&self, window: &Window, cx: &App) -> KeyTarget {
        let typing_a_command = self.command_input.focus_handle(cx).is_focused(window);
        let (covered, _) = Self::covered(window);
        let width: f32 = (window.viewport_size().width - covered.left - covered.right).into();
        match Layout::for_width(width) {
            Layout::Large if typing_a_command => KeyTarget::Command,
            Layout::Large => KeyTarget::Editor,
            _ if self.pane == Pane::Terminal => KeyTarget::Command,
            _ => KeyTarget::Editor,
        }
    }

    fn press_key(&mut self, cap: Cap, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.key_target(window, cx);
        match cap.press {
            Press::NextPage => self.key_page = (self.key_page + 1) % KEY_PAGES.len(),
            Press::Control => {
                let held = self.take_control().is_none();
                if held {
                    self.control = Some(target);
                    self.control_held.set(true);
                }
                // Keep the text focused, so the next key typed goes to it.
                self.focus_target(target, window, cx);
            }
            Press::Text(text) => {
                if self.take_control().is_some() {
                    self.send_key(target, text, true, window, cx);
                } else {
                    self.focus_target(target, window, cx);
                    match target {
                        KeyTarget::Editor => self
                            .editor
                            .update(cx, |editor, cx| editor.insert(text, window, cx)),
                        KeyTarget::Command => self
                            .command_input
                            .update(cx, |input, cx| input.insert(text, window, cx)),
                    }
                }
            }
            Press::Key(key) => {
                let control = self.take_control().is_some();
                self.send_key(target, key, control, window, cx);
            }
        }
        cx.notify();
    }

    fn focus_target(&mut self, target: KeyTarget, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            KeyTarget::Editor => self
                .editor
                .update(cx, |editor, cx| editor.focus(window, cx)),
            KeyTarget::Command => self
                .command_input
                .update(cx, |input, cx| input.focus(window, cx)),
        }
    }

    /// Lets go of Control, returning what it was held for.
    fn take_control(&mut self) -> Option<KeyTarget> {
        self.control_held.set(false);
        self.control.take()
    }

    /// A character was typed while Control was held on the extra keys. The
    /// page caught it before any text field did, so it is sent as a Control
    /// key and never typed.
    fn typed_with_control(&mut self, typed: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.take_control() {
            self.send_key(target, typed, true, window, cx);
            cx.notify();
        }
    }

    /// Sends a key to the target as if a keyboard had. The terminal's own
    /// keys (history, completion, stop, clear) are handled here.
    fn send_key(
        &mut self,
        target: KeyTarget,
        key: &str,
        control: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if target == KeyTarget::Command {
            match (control, key.to_lowercase().as_str()) {
                (false, "tab") => return self.complete_command(window, cx),
                (false, "up") => return self.recall(true, window, cx),
                (false, "down") => return self.recall(false, window, cx),
                (true, "c") => {
                    if let Some(work) = &self.work {
                        work.shell.stop();
                    }
                    return;
                }
                (true, "l") => return self.run_command("clear".to_owned(), window, cx),
                (true, "u") | (false, "escape") => {
                    self.command_input
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    return;
                }
                _ => {}
            }
        }
        self.focus_target(target, window, cx);
        let keystroke = Keystroke {
            modifiers: if control {
                Modifiers::control()
            } else {
                Modifiers::none()
            },
            key: key.to_lowercase(),
            key_char: None,
        };
        // After this update, so that whatever the key does can reach this view.
        window.defer(cx, move |window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        });
    }

    /// The row of extra keys, as wide as `width` points.
    fn key_row(&mut self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let page = KEY_PAGES[self.key_page % KEY_PAGES.len()];
        let rows = screen::key_rows(page.len(), width, KEY_GAP, KEY_EDGE);
        let held = self.control.is_some();
        v_flex()
            .gap(px(KEY_GAP))
            .px(px(KEY_EDGE))
            .py(px(KEY_GAP))
            .bg(cx.theme().tab_bar)
            .border_t_1()
            .border_color(cx.theme().border)
            .children(page.chunks(page.len().div_ceil(rows)).map(|caps| {
                h_flex()
                    .gap(px(KEY_GAP))
                    .children(caps.iter().copied().map(|cap| {
                        let on = held && cap.press == Press::Control;
                        let name = if on {
                            "Control, on".to_owned()
                        } else {
                            cap.name.to_owned()
                        };
                        let mut tap = Tap::new(format!("key-{}", cap.name), cap.label)
                            .named(name)
                            .keycap();
                        if on {
                            tap = tap.primary();
                        }
                        Self::tap(cx, tap, move |this, window, cx| {
                            this.press_key(cap, window, cx);
                        })
                    }))
            }))
            .into_any_element()
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
        let mut button =
            Button::new(ElementId::Name(tap.key.clone().into())).label(tap.label.clone());
        // The size sets the label's type size: tabs share a row five ways,
        // so they use a smaller one to keep their names whole.
        button = match (tap.current.is_some(), tap.tiny) {
            (true, true) => button.xsmall(),
            (true, false) => button,
            (false, _) => button.large(),
        };
        let mut button = button
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
            button = button.compact().toggled(current);
            button = if current {
                button.outline()
            } else {
                button.ghost()
            };
        }
        if tap.keycap {
            button = button.compact().px_1();
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
                tap.name.unwrap_or(tap.label),
                tap.disabled,
                (!tap.disabled).then_some(action),
            ))
    }

    /// A field for a secret: a real HTML password field placed here (see
    /// `mirror::Role::Secret`). `on_enter` runs when Enter is pressed in it.
    fn secret(
        cx: &mut Context<Self>,
        key: &str,
        label: &str,
        on_enter: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Div {
        let entity = cx.entity().downgrade();
        let action: Action = Rc::new(move |window: &mut Window, cx: &mut App| {
            let _ = entity.update(cx, |this, cx| on_enter(this, window, cx));
        });
        div().relative().h(px(TAP)).child(mirror::mark(
            key,
            Role::Secret,
            label,
            false,
            Some(action),
        ))
    }

    /// A single-line text field. `typed` is its content for a screen reader
    /// to read back.
    fn field(key: &str, label: &str, state: &Entity<InputState>, typed: String) -> Div {
        let target = state.clone();
        let action: Action = Rc::new(move |window: &mut Window, cx: &mut App| {
            target.update(cx, |input, cx| input.focus(window, cx));
        });
        div()
            .relative()
            .child(Input::new(state).large().h(px(TAP)))
            .child(mirror::mark_field(key, label, Some(typed), action))
    }

    fn typed(state: &Entity<InputState>, cx: &App) -> String {
        state.read(cx).value().to_string()
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
                    .child(Self::secret(cx, TOKEN_FIELD, "GitHub token", Self::sign_in))
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
                        |this, _, cx| {
                            this.sign_out(cx);
                        },
                    )),
            )
            .children(self.notice_bar(cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Self::field(
                            "repo-name",
                            "Repository, as owner/name",
                            &self.repo_input,
                            Self::typed(&self.repo_input, cx),
                        )
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
            .child(Self::tap(
                cx,
                Tap::new("import", "Add files from this device"),
                Self::import_files,
            ))
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
            .children(self.suggest_bar(cx))
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
                            .child(
                                Self::field(
                                    "command",
                                    "Command",
                                    &self.command_input,
                                    Self::typed(&self.command_input, cx),
                                )
                                .flex_1(),
                            )
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
                            // The extra keys' Tab, Up and Down do these three
                            // jobs, so beside them these would only take room.
                            .when(!self.keys_beside_terminal, |row| {
                                row.child(Self::tap(
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
                            })
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

    fn change_row(
        index: usize,
        change: &Change,
        conflict: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = match change.kind {
            ChangeKind::Added => "New",
            ChangeKind::Modified => "Edited",
            ChangeKind::Deleted => "Deleted",
        };
        let shown = path::display(&change.path);
        let for_diff = change.path.clone();
        let for_discard = change.path.clone();
        let row = v_flex()
            .gap_1()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius);
        if !conflict {
            return row
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
                            move |this, window, cx| {
                                this.show_diff(for_diff.clone(), false, window, cx);
                            },
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
                .into_any_element();
        }
        // In conflict: this file changed on GitHub too. Nothing of theirs is
        // replaced, and nothing of the person's is dropped, until they choose.
        let for_mine = change.path.clone();
        row.child(Self::say(
            &format!("change-{index}"),
            Role::Text,
            format!("{kind}: {shown}. Also changed on GitHub. Choose which version to keep."),
        ))
        .child(Self::tap(
            cx,
            Tap::new(format!("diff-{index}"), "Compare with GitHub's")
                .named(format!("Compare {shown} with GitHub's version")),
            move |this, window, cx| this.show_diff(for_diff.clone(), true, window, cx),
        ))
        .child(
            h_flex()
                .gap_2()
                .child(Self::tap(
                    cx,
                    Tap::new(format!("mine-{index}"), "Keep mine")
                        .named(format!("Keep my version of {shown}")),
                    move |this, window, cx| this.keep_mine(for_mine.clone(), window, cx),
                ))
                .child(Self::tap(
                    cx,
                    Tap::new(format!("discard-{index}"), "Use GitHub's")
                        .named(format!("Use GitHub's version of {shown}")),
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
        let conflicts = work.conflicts.clone();
        let rows: Vec<AnyElement> = changes
            .iter()
            .enumerate()
            .map(|(index, change)| {
                Self::change_row(index, change, conflicts.contains(&change.path), cx)
            })
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
                    .child(Self::field(
                        "commit-message",
                        "Commit message",
                        &self.commit_input,
                        Self::typed(&self.commit_input, cx),
                    ))
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
        let place = match &self.kept {
            Some(Kept::Files) => {
                "Unsaved changes are kept in this browser's file storage on this device.".to_owned()
            }
            Some(Kept::Small(why)) => format!(
                "This browser's file storage could not be used ({why}), so unsaved changes are kept in a small store that holds about 5 MB."
            ),
            None => "Unsaved changes are kept in this browser's storage on this device.".to_owned(),
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
            .id("settings")
            .size_full()
            .overflow_y_scroll()
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
            .when(demo, |pane| {
                pane.child(Self::tap(
                    cx,
                    Tap::new("demo-teammate", "Demo: someone else changes a file")
                        .named("Demo: someone else changes a file on GitHub"),
                    |this, _, cx| this.demo_teammate(cx),
                ))
            })
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
            .children(self.touch_settings(cx))
            .children(self.suggest_settings(cx))
            .children(Self::renderer_settings(cx))
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
                |this, _, cx| {
                    this.sign_out(cx);
                },
            ))
            .into_any_element()
    }

    /// How much of each edge of the page is covered (by the notch, rounded
    /// corners, the home indicator, or the on-screen keyboard), and whether
    /// the keyboard is up.
    ///
    /// Safari keeps the page its full height when the keyboard opens and
    /// covers the bottom of it; what stays in view is the "visual viewport".
    /// Pinch-zoom shrinks that too, and then nothing should move.
    fn covered(window: &Window) -> (Edges<Pixels>, bool) {
        if platform::zoomed() {
            return (window.insets().safe_area, false);
        }
        let page = window.viewport_size();
        let visible = window.fully_visible_bounds();
        let covered = Edges {
            top: visible.origin.y,
            left: visible.origin.x,
            right: page.width - visible.origin.x - visible.size.width,
            bottom: page.height - visible.origin.y - visible.size.height,
        };
        let keyboard = screen::keyboard_is_open(
            page.height.into(),
            window.visual_viewport_bounds().size.height.into(),
        );
        (covered, keyboard)
    }

    /// The bar across the top: what is open and whether it is saved. With the
    /// keyboard up it also holds the button that puts the keyboard away, and
    /// on a phone it drops the title to leave room.
    fn header(
        title: String,
        state: &'static str,
        layout: Layout,
        keyboard: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let cramped = keyboard && layout == Layout::Compact;
        // On a phone the title sits above the save state; with more width
        // they share a line. With the keyboard up a phone drops the title.
        let stacked = layout == Layout::Compact;
        h_flex()
            .items_center()
            .px_3()
            .py_1()
            .gap_2()
            .bg(cx.theme().title_bar)
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .child(
                div()
                    .flex()
                    .when(stacked, gpui_kit::Styled::flex_col)
                    .flex_1()
                    .min_w_0()
                    .gap_x_2()
                    .when(!cramped, |text| {
                        text.child(
                            Self::say("workspace", Role::Heading, title)
                                .flex_1()
                                .truncate(),
                        )
                    })
                    .child(
                        Self::say("save-state", Role::Status, state)
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(Self::tap(
                cx,
                Tap::new("go-to", "Go to").named("Go to a file or action"),
                Self::open_finder,
            ))
            .when(keyboard, |header| {
                header.child(Self::tap(
                    cx,
                    Tap::new("hide-keyboard", "Hide keyboard").ghost(),
                    |_, window, _| window.dismiss_virtual_keyboard(),
                ))
            })
    }

    fn work_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (covered, keyboard) = Self::covered(window);
        let width: f32 = (window.viewport_size().width - covered.left - covered.right).into();
        let layout = Layout::for_width(width);
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

        let (main, side) = layout.shown(self.pane, self.side);
        let current = side.unwrap_or(main);
        // The extra keys are for typing, so they show where text is typed.
        let typing = layout == Layout::Large || matches!(main, Pane::Editor | Pane::Terminal);
        let show_keys = typing && (keyboard || self.keys_always);
        self.keys_beside_terminal = show_keys && main == Pane::Terminal;
        let draw = |this: &mut Self, pane: Pane, cx: &mut Context<Self>| match pane {
            Pane::Files => this.files_pane(cx),
            Pane::Editor => this.editor_pane(cx),
            Pane::Terminal => this.terminal_pane(cx),
            Pane::Changes => this.changes_pane(cx),
            Pane::More => this.more_pane(cx),
        };
        let tabs: Vec<Div> = layout
            .tabs()
            .into_iter()
            .map(|tab| {
                Self::tap(
                    cx,
                    Tap::new(format!("tab-{}", tab.label()), tab.label())
                        .tab(tab == current)
                        // Five tabs share a phone's width; smaller type keeps
                        // every name whole.
                        .tiny(layout == Layout::Compact),
                    move |this, _, cx| {
                        this.pane = tab;
                        if tab.is_side() {
                            this.side = tab;
                        }
                        cx.notify();
                    },
                )
            })
            .collect();
        let sidebar = layout.sidebar_width().map(|sidebar| {
            div()
                .w(px(sidebar))
                .flex_none()
                .h_full()
                .border_r_1()
                .border_color(cx.theme().border)
                .child(self.files_pane(cx))
        });
        let side_column = side.map(|pane| {
            div()
                .w(px(layout.side_width(width).unwrap_or(400.0)))
                .flex_none()
                .h_full()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(draw(self, pane, cx))
        });
        let body = if self.finder.is_some() {
            self.finder_pane(cx)
        } else {
            draw(self, main, cx)
        };
        let keys = show_keys.then(|| self.key_row(width, cx));
        // With the keyboard up a phone has little room left: the tabs give
        // way to the extra keys, and come back when the keyboard is put away.
        let cramped = keyboard && layout == Layout::Compact;

        v_flex()
            .size_full()
            .child(Self::header(title, state, layout, keyboard, cx))
            .children(self.notice_bar(cx).map(|bar| div().p_2().child(bar)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .children(sidebar)
                    .child(div().flex_1().min_w_0().h_full().child(body))
                    .children(side_column),
            )
            .children(keys)
            .when(!cramped, |screen| {
                screen.child(
                    h_flex()
                        .p_1()
                        .bg(cx.theme().tab_bar)
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .children(tabs),
                )
            })
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent options of a builder"
)]
struct Tap {
    key: String,
    label: String,
    kind: Kind,
    disabled: bool,
    /// What assistive technology calls it, when the label is a symbol.
    name: Option<String>,
    keycap: bool,
    /// For a tab on a very narrow screen: smaller type.
    tiny: bool,
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
            name: None,
            keycap: false,
            tiny: false,
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

    fn tiny(mut self, tiny: bool) -> Self {
        self.tiny = tiny;
        self
    }

    fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// One of the extra keys: shares its row equally with the others.
    fn keycap(mut self) -> Self {
        self.keycap = true;
        self.fill = true;
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
        let (covered, _) = Self::covered(window);
        div()
            .size_full()
            // Control held on the extra keys applies to the next key pressed.
            // Keys that type a character are caught by the page before they
            // get here (`platform::watch_typing`); this catches the rest,
            // such as an arrow or Enter on an attached keyboard.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let Some(target) = this.take_control() else {
                    return;
                };
                cx.notify();
                let keys = &event.keystroke;
                if keys.modifiers.control || keys.modifiers.alt || keys.modifiers.platform {
                    return;
                }
                let key = keys.key.clone();
                cx.stop_propagation();
                this.send_key(target, &key, true, window, cx);
            }))
            .pt(covered.top)
            .pl(covered.left)
            .pr(covered.right)
            .pb(covered.bottom)
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
