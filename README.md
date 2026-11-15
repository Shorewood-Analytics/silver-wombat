# silver-wombat

A code editor for GitHub repositories that runs entirely in the browser, built for iPhone and iPad Safari.
It is written in Rust, compiled to WebAssembly, and drawn with [GPUI](https://www.gpui.rs) through [gpui-kit](https://gpui-kit.com).
There is no server: the page talks straight to the GitHub API with your token.

## What works today

- Sign in with a GitHub personal access token, or try the built-in demo with no account.
- Open any repository the token can reach, browse folders and open text files.
- Edit with syntax highlighting. Dark and Midnight themes, sans font, 1.5 line spacing, optional fixed-width code font.
- Three save states, always visible: **Not yet saved**, **Saved on this device**, **Checkpointed to GitHub**.
  A state is only shown after the write that earns it has succeeded.
- Continuous save: a pause in typing saves to the device, and a few seconds later checkpoints to the `autosave/<branch>` branch on GitHub. Your real branch is not touched until you commit.
- Commit all changes as one commit. Before writing, the app fetches the branch and refuses if any changed file differs on GitHub from what you started with. Nothing is overwritten.
- A terminal that runs in the browser on the open repository: `ls`, `cd`, `cat`, `head`, `tail`, `wc`, `grep`, `find`, `echo`, `touch`, `rm`, `mv`, `cp`, `open`, `save`, and `git status | diff | commit -m | log | branch | switch | restore | pull`, with pipes, `&&`, `;`, `>` and `>>`.
- Python 3.12, 3.13, 3.14 and 3.15 in that terminal, running in the browser tab. See [Python](#python).
- **Go to**: type a few letters of a file's name, or of an action, and pick it. Control+K or Command+K with a keyboard attached.
- A row of extra keys for typing code on a phone, and layouts for a phone, an open iPhone Duo and an iPad Pro. See [Phone and tablet](#phone-and-tablet).

## Phone and tablet

**Extra keys.** When the on-screen keyboard is up in the editor or the terminal, a row above it adds the keys it lacks: Esc, Tab, the four arrows and Ctrl, then three more groups of punctuation that code needs and the iOS keyboard buries (`{ } [ ] ( ) =`, `< > | \ _ # \``, `" ' : ; / - +`). The last key of each group shows the next. Ctrl applies to the next key, typed or tapped: Ctrl then Z undoes. In the terminal, Tab completes, Up and Down walk the history, Ctrl+C stops and Ctrl+L clears. More > Extra keys shows the row all the time, for use with a switch, a pointer or another on-screen keyboard.

**Layouts.** The width decides, and it follows rotation, unfolding and Split View live:

| Width | Devices | What shows |
| --- | --- | --- |
| under 600 points | iPhone; iPhone Duo folded (about 466) | one pane, five tabs |
| 600 to 1099 | iPhone Duo open (about 626, or 890 on its side); iPad upright | the file list beside one pane |
| 1100 and up | iPad Pro on its side (1210 or 1376) | file list, editor, and a third column for terminal, changes or settings |

The iPhone Duo widths are worked out from Apple's published pixel counts at 3x. They are not published as points.

**Edges and the keyboard.** Nothing is drawn under the notch, the rounded corners or the home indicator. When the keyboard opens, the interface moves up to sit above it; on a phone the tabs give way to the extra keys until the keyboard is put away (Hide keyboard, top right). Pinch-zoom magnifies the page and rearranges nothing.

**Swipe.** A quick sideways swipe moves to the next or previous pane, the same as tapping its tab. It can be switched off in More. Slow drags, swipes from the screen's edge and two-finger gestures are ignored.

**Not yet tried on a device.** All of this is tested in desktop Chrome with a phone-sized window, touch events, an emulated notch and emulated zoom. The on-screen keyboard's behaviour in Safari cannot be emulated there, so the part that moves the interface above the keyboard has not run at all outside unit tests of its arithmetic.

## Python

Python here is [Pyodide](https://pyodide.org): the standard interpreter compiled to WebAssembly. It runs in a background worker in the tab, on the files of the open repository, including edits that are not saved yet. Files a program writes show up as unsaved changes.

| Command | What it does |
| --- | --- |
| `python FILE [ARGS]`, `python -c CODE`, `python -m MODULE` | Run a program. `python3` is the same. |
| `uv run FILE` | Run a script, first installing the dependencies it declares in its own header ([inline script metadata](https://packaging.python.org/en/latest/specifications/inline-script-metadata/)). |
| `pip install NAMES`, `pip install -r FILE`, `uv pip install ...` | Install for this session. A reload forgets it. |
| `uv python list`, `uv python pin 3.13` | Show the versions; choose one for the repository (writes `.python-version`). |

The version comes from `.python-version`, then `requires-python` in `pyproject.toml`, then the default, 3.14.

| Python | Pyodide release | Libraries Pyodide has built for it |
| --- | --- | --- |
| 3.12.7 | 0.27.7 | 351, including numpy, pandas, scipy, matplotlib, scikit-learn, polars, pydantic |
| 3.13.2 | 0.29.5 | 379, the same set without polars |
| 3.14.2 | 314.0.8 | 357, the same set with polars |
| 3.15.0rc1 | 315.0.0a2, a pre-release | 31: numpy, scipy, pytest and their dependencies |

What to know:

- `uv` itself is a native program and cannot run in a browser. The `uv` commands above are a small compatible subset built into the terminal. There is no virtual environment, so `uv add`, `uv sync` and `uv venv` say they are not supported.
- The four interpreters are part of the site (about 13 MB each, downloaded the first time that version is used). Libraries are not: the first time a program imports one, Pyodide downloads it from `cdn.jsdelivr.net`. Pure-Python packages that Pyodide has not built are installed from PyPI. So the first use of a library needs a connection.
- Not possible in a browser: sockets, subprocesses, threads, and packages with native code that Pyodide has not built.
- A program gets the repository's text files, up to 400 files or 16 MB. Files that are not text are not given to it, and non-text files it writes (a plot saved as PNG, for example) are reported but not kept yet.
- Output appears when the program ends, not while it runs. Stop ends a running program by discarding the interpreter, which also discards anything installed in the session.
- There is no interactive prompt (`python` with no arguments).

## Accessibility

GPUI draws the interface into a canvas, which assistive technology cannot see into, and GPUI's own accessibility tree is compiled out for the browser.
So the app describes itself: every button, field and piece of text it draws is also written to the page as an invisible HTML element placed exactly over it (`crates/app/src/mirror.rs`).

- A screen reader reads those elements and activates them directly. New terminal output, save states and notices are announced.
- Voice Control and Switch Control tap an element's centre; the tap passes through to the control drawn underneath.
- A hardware keyboard can Tab between them, with a visible focus outline, and press Enter.
- Every button is at least 48 px tall. Nothing needs a drag, a long press or two hands, and nothing times out.
- All text is at least 5.7:1 against its background in both themes. A test checks the theme files (`cargo test -p xtask`).

Checked in Chrome by `e2e/smoke.mjs`, which operates the whole app only through those elements and runs axe on every pane (no WCAG A/AA findings).
**Not yet checked with VoiceOver, Switch Control or Voice Control on an iPhone.** That needs a real device.

Known gaps: a screen reader hears the end of the terminal output and of a diff, not all of it, and reads the file being edited through GPUI's hidden text area, which holds only the text around the cursor.

## What does not work yet

- No code completion.
- Apart from Python, the terminal has no machine behind it. `cargo`, `npm` and other programs answer "not available".
- Local changes are kept in the browser's origin-private file system (Safari 26 or later; older browsers fall back to `localStorage`, about 5 MB). The app asks the browser not to evict it, but the browser may refuse, so the GitHub checkpoint is the durable copy. More > Export writes all changes to a patch file that `git apply` accepts.
- GitHub sign-in with a browser redirect is not possible from a static page: GitHub's login endpoints do not allow cross-origin browser requests. A token is the only way in.
- Only text files up to 2 MB open in the editor.

## Token

Create a fine-grained personal access token at <https://github.com/settings/personal-access-tokens/new> with **Contents: Read and write** on the repositories you want to edit.
The token is kept in this browser's storage on this device and is sent only to `api.github.com`. Sign out removes it.

## Build

The workspace has three crates:

| Crate | What it is | Builds with |
| --- | --- | --- |
| `crates/core` | GitHub client, workspace, save states, terminal commands. No UI. | stable Rust |
| `xtask` | The build tool. | stable Rust |
| `crates/app` | The GPUI user interface. Browser only. | nightly Rust (pinned) |

The app needs a nightly toolchain because GPUI's web crate enables a `parking_lot` feature that stable rejects. The pin is `nightly-2026-10-31`, the same one gpui-kit uses.

One-time setup:

```sh
rustup toolchain install nightly-2026-10-31 --target wasm32-unknown-unknown --component clippy
cargo install wasm-bindgen-cli --version 0.2.130 --locked
```

Node.js and npm are needed too: the build fetches the Python interpreters from npm, at the versions pinned in `web/python/package-lock.json`.

Build the site into `dist/` and serve it:

```sh
cargo xtask dist
python3 -m http.server -d dist 8080
```

Then open <http://localhost:8080>. `dist/` is plain static files, so any static host works, as long as it serves `.mjs` files as JavaScript (Python needs that; GitHub Pages and `python3 -m http.server` do).
For a faster, larger build while developing, use `cargo xtask dist --profile quick`.

## Open it on a phone

`.github/workflows/pages.yml` publishes the site to GitHub Pages each time `main` changes, after the same checks as a pull request.
Pages has to be switched on once: **Settings > Pages > Build and deployment > Source: GitHub Actions**.
The address is then <https://shorewood-analytics.github.io/silver-wombat/>. Until Pages is switched on the workflow does nothing.

## Host on ChatGPT Sites

`dist` is one of the output folders Sites accepts for a static-only site.
Sites can only be created and deployed from ChatGPT on the web or the desktop app.
Open this project there and ask: "Deploy this project with Sites as a static site from the `dist` folder. Check whether it is compatible and give me the URL."
This has not been tried yet.

Sites' terms prohibit processing protected health information. The app sends repository contents only to GitHub, not to the host, but do not use a Sites-hosted copy with repositories that contain PHI without checking that with whoever owns compliance.

## Checks

Run these before opening a PR. CI runs the same ones.

```sh
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
cargo xtask check
```

The first line covers `core` and `xtask`. `cargo xtask check` lints the app for the browser target.

To check the GitHub client against the real API (it creates two branches named `live-test/...` and `autosave/live-test/...` in the repository you name, and touches nothing else):

```sh
SILVER_WOMBAT_LIVE_REPO=owner/name GITHUB_TOKEN=... \
    cargo test -p silver-wombat-core --test live -- --ignored --nocapture
```

To run the built site in a real browser and operate it end to end (needs Chrome and Node):

```sh
cargo xtask dist
npm ci --prefix e2e && node e2e/smoke.mjs
```

The owner (`benpshore`) may push their own work directly to `main` without a PR or passing CI. CI still runs after a push; releases retain their CI gates. Force-push, deletion, and merge commits remain blocked for everyone.

Other accounts must use a squash PR with passing, up-to-date `ci`. Agents must use PRs and CI even when authenticated as the owner (see `AGENTS.md`). GitHub checks the pushing account, so enforcing a human/agent boundary requires separate credentials.

## Fonts

Inter, JetBrains Mono and IBM Plex Sans are bundled under the SIL Open Font License (`crates/app/fonts/OFL.txt`). The browser gives WebAssembly no system fonts, so the app carries its own.
