# silver-wombat

- Agents must work on a branch and open a PR (`gh pr create --fill`); never push directly to `main` or use the owner's bypass, even when authenticated as the owner.
- The `ci` check must pass for agent-authored work. Only the human owner's own work may use the direct-push exception.
- Never commit secrets, `.env` files, databases or `.DS_Store` (see `.gitignore`; CI rejects them).
- Before committing, run:

```sh
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
cargo xtask check
```

- The first line covers `crates/core` and `xtask` on stable Rust. `cargo xtask check` lints `crates/app` for the browser with the pinned nightly toolchain (see `README.md`).
- `crates/core` has no UI and no browser dependency. Put logic there and test it natively; keep `crates/app` to drawing and wiring.
- The Python versions are listed twice and must agree: `RUNTIMES` in `crates/core/src/python.rs` and `web/python/package.json` (a test checks). `web/python-worker.js` is the only JavaScript that runs Python; `e2e/smoke.mjs` tests it in a browser.
- Code suggestions: the rules are in `crates/core/src/complete.rs`; `web/complete-worker.js` runs the model. The browser test uses the stand-in model in `e2e/fixtures/tiny-model/`, built by `e2e/fixtures/make_tiny_model.py`.
- GPUI must not hold keyboard focus when no text field is being edited: it takes Tab away from the page, which breaks keyboard navigation of the accessibility layer.
- A secret is typed into `mirror::Role::Secret` (a real password field), never into a GPUI input: GPUI keeps what is typed in a hidden text area that a screen reader reads.
- The app must never report a change as saved before the write that makes it so has succeeded (see `crates/core/src/workspace.rs`).
