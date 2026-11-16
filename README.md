# silver-wombat

A code editor for GitHub repositories that runs entirely in the browser, built for iPhone and iPad Safari.
It is written in Rust, compiled to WebAssembly, and drawn with [GPUI](https://www.gpui.rs) through [gpui-kit](https://gpui-kit.com).
There is no server: the page talks straight to the GitHub API with your token.

## What works today

- Sign in with a GitHub personal access token, or try the built-in demo with no account.
- Open any repository the token can reach, browse folders and open text files. Files > Add files from this device brings in text files from the phone or tablet as unsaved changes.
- Edit with syntax highlighting. Dark and Midnight themes, sans font, 1.5 line spacing, optional fixed-width code font.
- Three save states, always visible: **Not yet saved**, **Saved on this device**, **Checkpointed to GitHub**.
  A state is only shown after the write that earns it has succeeded.
- Continuous save: a pause in typing saves to the device, and a few seconds later checkpoints to the `autosave/<branch>` branch on GitHub. Your real branch is not touched until you commit.
  When the page is hidden or closed (switching app, locking the phone), anything not yet saved is written to the device at once. If the device cannot store it, the state reads "Not saved on this device" until it can.
- Commit all changes as one commit. Before writing, the app fetches the branch and refuses if any changed file differs on GitHub from what you started with. Nothing is overwritten. Each such file then offers a comparison with GitHub's version and a choice: keep mine, or use GitHub's. (The demo has a button in More that changes a file "as someone else", to try this.)
- A terminal that runs in the browser on the open repository: `ls`, `cd`, `cat`, `head`, `tail`, `wc`, `grep`, `find`, `echo`, `touch`, `rm`, `mv`, `cp`, `open`, `save`, and `git status | diff | commit -m | log | branch | switch | restore | pull`, with pipes, `&&`, `;`, `>` and `>>`.
- Python 3.12, 3.13, 3.14 and 3.15 in that terminal, running in the browser tab. See [Python](#python).
- **Go to**: type a few letters of a file's name, or of an action, and pick it. Control+K or Command+K with a keyboard attached.
- A row of extra keys for typing code on a phone, and layouts for a phone, an open iPhone Duo and an iPad Pro. See [Phone and tablet](#phone-and-tablet).
- Code suggestions from a small language model that runs on the device, on the GPU through WebGPU. Off until switched on. See [Code suggestions](#code-suggestions).

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

**Tried in Safari's engine, not in Safari.** CI also runs the browser test in WebKit 27.2, the engine Safari is built on, on Linux. There the app starts, draws with WebGL2 and passes every check that does not need Chrome's own controls (117 of them): editing and the save states, the terminal, all four Python versions with numpy and pip, the finder, the extra keys, the three layouts, suggestions on the CPU, a commit conflict, full storage, and leaving the page mid-edit. That build has no WebGPU, so WebGPU in WebKit has not run. The emulated notch, zoom and swipe are not run there.

## Code suggestions

More > Turn code suggestions on. After a pause in typing, a suggested end of the line appears under the editor with two buttons, Insert and Dismiss. A screen reader hears the suggestion when it arrives.

- **The model** is [Tiny StarCoder](https://huggingface.co/bigcode/tiny_starcoder_py): 164 million parameters, trained on Python, able to use the code after the cursor as well as before it. Suggestions are offered in `.py` and `.pyi` files only. It is small enough for a phone, and it is a weak model: expect it to finish an obvious line, not to write a function. Its licence is BigCode OpenRAIL-M, which restricts some uses.
- **It runs on the device**, in a background worker, through [Transformers.js](https://huggingface.co/docs/transformers.js) and ONNX Runtime. On a browser with WebGPU it runs on the GPU. Without WebGPU the settings say so and offer the CPU, which is slower. Code is never sent anywhere.
- **The download** happens once, when suggestions are switched on: 224 MB on a GPU with 16-bit floats, 320 MB otherwise, from `huggingface.co`. The browser keeps it. The settings show the size before and after, and progress during.
- **Memory.** The model is loaded the first time it is needed, not when the app opens. Turn code suggestions off ends the worker and gives the memory back. Remove the downloaded model deletes the files.
- **When it fails.** If the model cannot start, the settings say why and offer three choices: try again, use the CPU instead, or switch suggestions off. If the page closes while the model is loading (a phone's browser ends a page that uses too much memory), the app notes it and does not load the model again until asked, so a model too big for the device cannot end the page on every visit.
- **Another model.** Put a description in `localStorage` under `silver-wombat.suggest-model` (the fields of `Model` in `crates/core/src/complete.rs`, as JSON) to use any text-generation model in Transformers.js's ONNX layout, from any host.

What has been run: the whole path (settings, download, the worker, generation on the CPU and on WebGPU, the offer, Insert) in Chrome with a stand-in model of a few kilobytes. `e2e/model.mjs` loads and runs the real model in Chrome, on software WebGPU and on the CPU; CI runs it and prints what the model suggested and how long it took, without failing the build if the download does.
The real model has run in CI's Chrome (on a GitHub runner, which has no GPU). Given `def add(a, b):` and `return ` it suggested `a + b` both ways. On the CPU it loaded in 8 seconds and answered in about 1 second. On software WebGPU it loaded in 31 seconds and answered in 13 to 15 seconds; that is the GPU being imitated in software, and says nothing about a real GPU.
**Not run: the real model on an iPhone or iPad.** Whether 224 MB of model fits in Safari's memory on a given phone, and how fast it is there, is unknown until it is tried on one.

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
- Output appears line by line while the program runs (when it goes to the terminal; into a pipe or a file it is handed over at the end). Stop ends a running program by discarding the interpreter, which also discards anything installed in the session.
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

A screen reader can read back what is typed in the single-line fields (command, commit message, repository, finder).

Known gaps: a screen reader hears the end of the terminal output and of a diff, not all of it, and reads the file being edited through GPUI's hidden text area, which holds only the text around the cursor.

## What does not work yet

- Apart from Python, the terminal has no machine behind it. `cargo`, `npm` and other programs answer "not available".
- Local changes are kept in the browser's origin-private file system (Safari 26 or later; older browsers fall back to `localStorage`, about 5 MB). WebKit refuses that file system to a window that keeps nothing on disk, which is what a private window is, so there the small store is used too. More says which store is in use and, for the small one, the browser's reason. The app asks the browser not to evict it, but the browser may refuse, so the GitHub checkpoint is the durable copy. More > Export writes all changes to a patch file that `git apply` accepts.
- GitHub sign-in with a browser redirect is not possible from a static page: GitHub's login endpoints do not allow cross-origin browser requests. A token is the only way in.
- Only text files up to 2 MB open in the editor.

## Token

Create a fine-grained personal access token at <https://github.com/settings/personal-access-tokens/new> with **Contents: Read and write** on the repositories you want to edit.
The token is kept in this browser's storage on this device and is sent only to `api.github.com`. Sign out removes it.
The field it is typed into is a real HTML password field, so the browser treats it as a password: it is not shown, not read aloud by a screen reader, and not learned by the keyboard.

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

Node.js and npm are needed too: the build fetches the Python interpreters, the suggestion model's runtime and `wasm-opt` from npm, at the versions pinned in `web/python/package-lock.json`, `web/ml/package-lock.json` and `xtask/wasm-opt/package-lock.json`.

Build the site into `dist/` and serve it:

```sh
cargo xtask dist
python3 -m http.server -d dist 8080
```

Then open <http://localhost:8080>. `dist/` is plain static files, so any static host works, as long as it serves `.mjs` files as JavaScript (Python needs that; GitHub Pages and `python3 -m http.server` do).
The last step of the build shrinks the app with Binaryen's `wasm-opt`, from about 29 MB to 15.5 MB (6.4 MB once the host compresses it). It takes a few minutes.
For a faster, larger build while developing, use `cargo xtask dist --profile quick`, which skips that step.

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
node e2e/model.mjs    # the real suggestion model; downloads about 320 MB
```

The same test runs in WebKit, the engine Safari is built on (CI does this in its `webkit` job; the result is shown on a pull request and does not gate a merge):

```sh
(cd e2e && npx playwright-core install --with-deps webkit)
BROWSER=webkit node e2e/smoke.mjs
```

**Drawing.** GPUI draws with WebGPU where the browser has it and WebGL2 where it does not. Both have been seen working, in different places: with Chrome's software WebGPU on GitHub's runners the app draws with WebGPU; with the same switches on Chrome 153 the WebGPU device is lost as soon as drawing starts, which leaves a blank canvas. The app guards against that: if the WebGPU device is lost, it remembers to use WebGL2 on that device and reloads once (unsaved work is already on the device and comes back). More then says so and offers to try WebGPU again. The test covers whichever of the two happens where it runs. Neither has been seen on real GPU hardware.

The owner (`benpshore`) may push their own work directly to `main` without a PR or passing CI. CI still runs after a push; releases retain their CI gates. Force-push, deletion, and merge commits remain blocked for everyone.

Other accounts must use a squash PR with passing, up-to-date `ci`. Agents must use PRs and CI even when authenticated as the owner (see `AGENTS.md`). GitHub checks the pushing account, so enforcing a human/agent boundary requires separate credentials.

## Fonts

Inter, JetBrains Mono and IBM Plex Sans are bundled under the SIL Open Font License (`crates/app/fonts/OFL.txt`). The browser gives WebAssembly no system fonts, so the app carries its own.
