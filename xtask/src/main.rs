//! Build tasks, run as `cargo xtask <task>`.
//!
//! * `dist [--profile NAME] [-- CARGO_ARGS...]` builds the app to WebAssembly
//!   and writes a static site to `dist/`. Any static host can serve it.
//! * `check [-- CARGO_ARGS...]` runs clippy on the app for the browser target.
//!
//! The app is built with a pinned nightly toolchain ([`APP_TOOLCHAIN`]): GPUI's
//! web crate turns on a `parking_lot` feature that stable Rust rejects. The
//! rest of the workspace builds on stable. Also needed: the `wasm-bindgen`
//! command at the version in `Cargo.lock`. The error messages say how to
//! install whatever is missing.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const TARGET: &str = "wasm32-unknown-unknown";
/// The toolchain the app is built with. gpui-kit pins the same one.
const APP_TOOLCHAIN: &str = "nightly-2026-10-31";
/// Set this to build the app with another installed toolchain.
const TOOLCHAIN_OVERRIDE: &str = "XTASK_APP_TOOLCHAIN";
const APP: &str = "silver-wombat-app";
const WASM_NAME: &str = "silver_wombat_app";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

fn run(command: &mut Command) -> Result<(), String> {
    let shown = format!("{command:?}");
    let status = command
        .status()
        .map_err(|error| format!("could not start {shown}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{shown} failed with {status}"))
    }
}

/// The `wasm-bindgen` version pinned in `Cargo.lock`.
fn locked_bindgen_version(root: &Path) -> Result<String, String> {
    let lock = fs::read_to_string(root.join("Cargo.lock"))
        .map_err(|error| format!("could not read Cargo.lock: {error}"))?;
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line == "name = \"wasm-bindgen\""
            && let Some(version) = lines.next().and_then(|next| {
                next.strip_prefix("version = \"")
                    .and_then(|rest| rest.strip_suffix('"'))
            })
        {
            return Ok(version.to_owned());
        }
    }
    Err("Cargo.lock has no wasm-bindgen entry".to_owned())
}

fn check_bindgen(root: &Path) -> Result<(), String> {
    let wanted = locked_bindgen_version(root)?;
    let install = format!("cargo install wasm-bindgen-cli --version {wanted} --locked");
    let output = Command::new("wasm-bindgen")
        .arg("--version")
        .output()
        .map_err(|_| format!("wasm-bindgen is not installed. Run: {install}"))?;
    let found = String::from_utf8_lossy(&output.stdout);
    if found.split_whitespace().nth(1) == Some(wanted.as_str()) {
        Ok(())
    } else {
        Err(format!(
            "wasm-bindgen is {}, but Cargo.lock needs {wanted}. Run: {install} --force",
            found.trim()
        ))
    }
}

fn app_toolchain() -> String {
    env::var(TOOLCHAIN_OVERRIDE).unwrap_or_else(|_| APP_TOOLCHAIN.to_owned())
}

/// `cargo` from the app's toolchain, run in the workspace root.
fn app_cargo(root: &Path) -> Command {
    let mut command = Command::new("rustup");
    command
        .current_dir(root)
        .args(["run", &app_toolchain(), "cargo"]);
    command
}

fn toolchain_hint() -> String {
    format!(
        "If the toolchain or target is missing, run: rustup toolchain install {} --target {TARGET} --component clippy",
        app_toolchain()
    )
}

fn target_dir(root: &Path) -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

fn split_args(args: &[String]) -> (&[String], &[String]) {
    match args.iter().position(|arg| arg == "--") {
        Some(index) => (&args[..index], &args[index + 1..]),
        None => (args, &[]),
    }
}

fn dist(args: &[String]) -> Result<(), String> {
    let (own, cargo_args) = split_args(args);
    let profile = match own {
        [] => "release",
        [flag, name] if flag == "--profile" => name.as_str(),
        _ => return Err("usage: cargo xtask dist [--profile NAME] [-- CARGO_ARGS...]".to_owned()),
    };
    let root = root();
    check_bindgen(&root)?;

    run(Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "build",
            "--package",
            APP,
            "--target",
            TARGET,
            "--profile",
            profile,
        ])
        .args(cargo_args))
    .map_err(|error| {
        format!("{error}\nIf the target is missing, run: rustup target add {TARGET}")
    })?;

    // Cargo names the output folder "debug" for the dev profile.
    let folder = if profile == "dev" { "debug" } else { profile };
    let wasm = target_dir(&root)
        .join(TARGET)
        .join(folder)
        .join(format!("{WASM_NAME}.wasm"));
    let out = root.join("dist");
    if out.exists() {
        fs::remove_dir_all(&out).map_err(|error| format!("could not clear dist/: {error}"))?;
    }
    fs::create_dir_all(&out).map_err(|error| format!("could not create dist/: {error}"))?;

    run(Command::new("wasm-bindgen")
        .arg(&wasm)
        .arg("--out-dir")
        .arg(&out)
        .args(["--target", "web", "--no-typescript"]))?;

    let web = root.join("web");
    let entries = fs::read_dir(&web).map_err(|error| format!("could not read web/: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read web/: {error}"))?;
        fs::copy(entry.path(), out.join(entry.file_name()))
            .map_err(|error| format!("could not copy {}: {error}", entry.path().display()))?;
    }

    println!("dist/ is ready ({profile} profile):");
    let mut names: Vec<_> = fs::read_dir(&out)
        .map_err(|error| format!("could not read dist/: {error}"))?
        .filter_map(Result::ok)
        .collect();
    names.sort_by_key(fs::DirEntry::file_name);
    for entry in names {
        let bytes = entry.metadata().map_or(0, |meta| meta.len());
        println!(
            "  {:>10} bytes  {}",
            bytes,
            entry.file_name().to_string_lossy()
        );
    }
    println!(
        "Serve it with any static file server, for example: python3 -m http.server -d dist 8080"
    );
    Ok(())
}

fn check(args: &[String]) -> Result<(), String> {
    let (own, cargo_args) = split_args(args);
    if !own.is_empty() {
        return Err("usage: cargo xtask check [-- CARGO_ARGS...]".to_owned());
    }
    run(app_cargo(&root())
        .args(["clippy", "--package", APP, "--target", TARGET])
        .args(cargo_args)
        .args(["--", "-D", "warnings"]))
    .map_err(|error| {
        format!(
            "{error}
{}",
            toolchain_hint()
        )
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((task, rest)) if task == "dist" => dist(rest),
        Some((task, rest)) if task == "check" => check(rest),
        _ => Err("usage: cargo xtask <dist|check>".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_locked_bindgen_version() {
        let version = locked_bindgen_version(&root()).unwrap();
        assert!(version.starts_with("0.2."), "{version}");
    }

    #[test]
    fn splits_passthrough_arguments() {
        let args: Vec<String> = ["--profile", "quick", "--", "-Zbuild-std"]
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect();
        let (own, cargo) = split_args(&args);
        assert_eq!(own, ["--profile", "quick"]);
        assert_eq!(cargo, ["-Zbuild-std"]);
        assert_eq!(split_args(&args[..2]).1.len(), 0);
    }
}
