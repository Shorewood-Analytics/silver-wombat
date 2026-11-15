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

- No Python, no code completion, no OPFS storage, no keyboard accessory row.
- The terminal has no machine behind it. `python`, `cargo`, `npm` and other programs answer "not available".
- Local changes are kept in `localStorage`, which Safari may evict and which holds about 5 MB. The GitHub checkpoint is the durable copy.
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

Build the site into `dist/` and serve it:

```sh
cargo xtask dist
python3 -m http.server -d dist 8080
```

Then open <http://localhost:8080>. `dist/` is plain static files, so any static host works.
For a faster, larger build while developing, use `cargo xtask dist --profile quick`.

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
