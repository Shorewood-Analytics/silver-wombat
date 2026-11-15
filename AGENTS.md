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
- The app must never report a change as saved before the write that makes it so has succeeded (see `crates/core/src/workspace.rs`).
