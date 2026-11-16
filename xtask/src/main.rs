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

    run(app_cargo(&root)
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
    .map_err(|error| format!("{error}\n{}", toolchain_hint()))?;

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
    if profile == "release" {
        shrink(&root, &out.join(format!("{WASM_NAME}_bg.wasm")))?;
    }

    let web = root.join("web");
    let entries = fs::read_dir(&web).map_err(|error| format!("could not read web/: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read web/: {error}"))?;
        if entry.path().is_dir() {
            continue;
        }
        fs::copy(entry.path(), out.join(entry.file_name()))
            .map_err(|error| format!("could not copy {}: {error}", entry.path().display()))?;
    }
    python_runtimes(&root, &out)?;
    ml_runtime(&root, &out)?;

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

/// The files of a Pyodide release that a browser needs. Each release has one
/// of the two `pyodide.asm` scripts.
const PYTHON_FILES: [&str; 4] = [
    "pyodide.mjs",
    "pyodide.asm.wasm",
    "python_stdlib.zip",
    "pyodide-lock.json",
];
const PYTHON_SCRIPTS: [&str; 2] = ["pyodide.asm.js", "pyodide.asm.mjs"];

/// The Python versions in `web/python/package.json`, as `(npm alias, folder)`:
/// the alias `py312` is served from `python/3.12/`.
fn python_versions(manifest: &str) -> Result<Vec<(String, String)>, String> {
    let manifest: serde_json::Value = serde_json::from_str(manifest)
        .map_err(|error| format!("web/python/package.json is not valid JSON: {error}"))?;
    let dependencies = manifest["dependencies"]
        .as_object()
        .ok_or("web/python/package.json has no dependencies")?;
    dependencies
        .keys()
        .map(|alias| {
            alias
                .strip_prefix("py3")
                .filter(|minor| !minor.is_empty() && minor.chars().all(|c| c.is_ascii_digit()))
                .map(|minor| (alias.clone(), format!("3.{minor}")))
                .ok_or_else(|| {
                    format!("web/python/package.json: {alias:?} is not named like py312")
                })
        })
        .collect()
}

/// Copies the Python interpreters into `dist/python/<version>/`, installing
/// them from npm first if they are not there yet.
fn python_runtimes(root: &Path, out: &Path) -> Result<(), String> {
    let source = root.join("web").join("python");
    let manifest = fs::read_to_string(source.join("package.json"))
        .map_err(|error| format!("could not read web/python/package.json: {error}"))?;
    let versions = python_versions(&manifest)?;
    let modules = source.join("node_modules");
    if versions
        .iter()
        .any(|(alias, _)| !modules.join(alias).join("pyodide.mjs").exists())
    {
        run(Command::new("npm")
            .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund", "--prefix"])
            .arg(&source))
        .map_err(|error| {
            format!("{error}\nThe Python interpreters come from npm (web/python/package.json); Node.js and npm are needed to build the site.")
        })?;
    }
    for (alias, version) in versions {
        let from = modules.join(&alias);
        let to = out.join("python").join(&version);
        fs::create_dir_all(&to)
            .map_err(|error| format!("could not create {}: {error}", to.display()))?;
        let scripts = PYTHON_SCRIPTS
            .into_iter()
            .filter(|name| from.join(name).exists());
        let mut bytes = 0;
        for name in PYTHON_FILES.into_iter().chain(scripts) {
            bytes += fs::copy(from.join(name), to.join(name))
                .map_err(|error| format!("could not copy {alias}/{name}: {error}"))?;
        }
        println!("  {bytes:>10} bytes  python/{version}/");
    }
    Ok(())
}

/// The files of the runtime that runs the code-suggestion model, as paths
/// under `web/ml/node_modules/`: Transformers.js bundled for a browser, and
/// the ONNX Runtime build it loads.
const ML_FILES: [&str; 3] = [
    "@huggingface/transformers/dist/transformers.min.js",
    "onnxruntime-web/dist/ort-wasm-simd-threaded.asyncify.mjs",
    "onnxruntime-web/dist/ort-wasm-simd-threaded.asyncify.wasm",
];

/// Copies that runtime into `dist/ml/`, installing it from npm first if it is
/// not there yet. The model itself is not part of the site.
fn ml_runtime(root: &Path, out: &Path) -> Result<(), String> {
    let source = root.join("web").join("ml");
    let modules = source.join("node_modules");
    if ML_FILES.iter().any(|file| !modules.join(file).exists()) {
        run(Command::new("npm")
            .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund", "--prefix"])
            .arg(&source))
        .map_err(|error| {
            format!("{error}\nThe suggestion model's runtime comes from npm (web/ml/package.json); Node.js and npm are needed to build the site.")
        })?;
    }
    let to = out.join("ml");
    fs::create_dir_all(&to)
        .map_err(|error| format!("could not create {}: {error}", to.display()))?;
    let mut bytes = 0;
    for file in ML_FILES {
        let name = Path::new(file).file_name().ok_or("a file with no name")?;
        bytes += fs::copy(modules.join(file), to.join(name))
            .map_err(|error| format!("could not copy {file}: {error}"))?;
    }
    println!("  {bytes:>10} bytes  ml/");
    Ok(())
}

/// Makes the app's WebAssembly smaller with Binaryen's `wasm-opt`, installing
/// it from npm first if it is not there yet. Only the release build is shrunk:
/// it takes minutes, and the result is what a phone has to download.
fn shrink(root: &Path, wasm: &Path) -> Result<(), String> {
    let source = root.join("xtask").join("wasm-opt");
    let tool = source.join("node_modules/binaryen/bin/wasm-opt");
    if !tool.exists() {
        run(Command::new("npm")
            .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund", "--prefix"])
            .arg(&source))
        .map_err(|error| {
            format!("{error}\nwasm-opt comes from npm (xtask/wasm-opt/package.json); Node.js and npm are needed to build the site.")
        })?;
    }
    let size = |path: &Path| {
        fs::metadata(path)
            .map(|meta| meta.len())
            .map_err(|error| format!("could not read {}: {error}", path.display()))
    };
    let before = size(wasm)?;
    let smaller = wasm.with_extension("wasm.small");
    println!("Shrinking the app with wasm-opt (a few minutes)...");
    run(Command::new("node")
        .arg(&tool)
        .arg("-Os")
        .arg(wasm)
        .arg("-o")
        .arg(&smaller))?;
    let after = size(&smaller)?;
    // An empty or missing result must not replace a working file.
    if after == 0 {
        return Err("wasm-opt wrote an empty file".to_owned());
    }
    fs::rename(&smaller, wasm)
        .map_err(|error| format!("could not replace {}: {error}", wasm.display()))?;
    println!("  wasm-opt: {before} bytes -> {after} bytes");
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

    fn luminance(hex: &str) -> f64 {
        let channel = |index: usize| {
            let value = f64::from(u8::from_str_radix(&hex[index..index + 2], 16).unwrap()) / 255.0;
            if value <= 0.039_28 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
    }

    /// The WCAG contrast ratio of two `#rrggbb` colours.
    fn contrast(a: &str, b: &str) -> f64 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// The app draws its own text, so no browser tool can measure its
    /// contrast. This checks the theme files instead: text needs 4.5:1 and
    /// the outline of a text field needs 3:1 (WCAG 1.4.3 and 1.4.11).
    #[test]
    fn theme_colours_have_enough_contrast() {
        let themes = fs::read_to_string(root().join("crates/app/src/themes.json")).unwrap();
        let themes: serde_json::Value = serde_json::from_str(&themes).unwrap();
        for theme in themes["themes"].as_array().unwrap() {
            let name = theme["name"].as_str().unwrap();
            let colour = |group: &str, key: &str| theme[group][key].as_str().unwrap().to_owned();
            let background = colour("colors", "background");
            let editor = colour("highlight", "editor.background");
            let mut text = vec![
                ("text", colour("colors", "foreground"), background.clone()),
                (
                    "muted text",
                    colour("colors", "muted.foreground"),
                    background.clone(),
                ),
                (
                    "muted text on the title bar",
                    colour("colors", "muted.foreground"),
                    colour("colors", "title_bar.background"),
                ),
                (
                    "primary button",
                    colour("colors", "primary.foreground"),
                    colour("colors", "primary.background"),
                ),
                (
                    "button",
                    colour("colors", "secondary.foreground"),
                    colour("colors", "secondary.background"),
                ),
                (
                    "notice",
                    colour("colors", "foreground"),
                    colour("colors", "muted.background"),
                ),
                (
                    "error text",
                    colour("colors", "base.red"),
                    background.clone(),
                ),
                (
                    "line numbers",
                    colour("highlight", "editor.line_number"),
                    editor.clone(),
                ),
            ];
            for (_, style) in theme["highlight"]["syntax"].as_object().unwrap() {
                if let Some(code) = style["color"].as_str() {
                    text.push(("code", code.to_owned(), editor.clone()));
                }
            }
            for (what, foreground, behind) in text {
                let ratio = contrast(&foreground, &behind);
                assert!(
                    ratio >= 4.5,
                    "{name}: {what} {foreground} on {behind} is {ratio:.2}:1"
                );
            }
            let outline = contrast(&colour("colors", "input.border"), &background);
            assert!(
                outline >= 3.0,
                "{name}: text field outline is {outline:.2}:1"
            );
            let ring = contrast(&colour("colors", "ring"), &background);
            assert!(ring >= 3.0, "{name}: focus ring is {ring:.2}:1");
        }
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
