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

## What does not work yet

- **Screen readers, Switch Control and Voice Control cannot operate the app.** GPUI draws to a canvas and its web platform exposes no accessibility tree. Closing this gap is the next piece of work.
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

The owner (`benpshore`) may push their own work directly to `main` without a PR or passing CI. CI still runs after a push; releases retain their CI gates. Force-push, deletion, and merge commits remain blocked for everyone.

Other accounts must use a squash PR with passing, up-to-date `ci`. Agents must use PRs and CI even when authenticated as the owner (see `AGENTS.md`). GitHub checks the pushing account, so enforcing a human/agent boundary requires separate credentials.

## Fonts

Inter, JetBrains Mono and IBM Plex Sans are bundled under the SIL Open Font License (`crates/app/fonts/OFL.txt`). The browser gives WebAssembly no system fonts, so the app carries its own.
