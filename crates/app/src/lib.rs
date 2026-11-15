//! The browser app. Everything here targets WebAssembly; on any other target
//! this crate is deliberately empty (see `Cargo.toml`).
#![cfg(target_family = "wasm")]

mod ide;
mod mirror;
mod platform;
mod python;
mod theme;

use std::borrow::Cow;
use std::cell::RefCell;

use gpui_kit::{App, ApplicationHandle, WindowOptions};
use wasm_bindgen::prelude::wasm_bindgen;

thread_local! {
    /// Keeps the application alive for as long as the page is open.
    static APPLICATION: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}

/// Runs `f` inside the app, from a browser event that GPUI did not deliver.
/// Must not be called while GPUI is already running app code.
pub(crate) fn with_app(f: impl FnOnce(&mut App)) {
    APPLICATION.with(|application| {
        if let Some(handle) = application.borrow().as_ref() {
            handle.update(f);
        }
    });
}

fn launch(cx: &mut App) {
    gpui_kit::init(cx);
    // The browser gives WASM no system fonts, so the three the UI needs are
    // bundled. IBM Plex Sans is what GPUI's web platform uses as its default.
    let fonts = vec![
        Cow::Borrowed(include_bytes!("../fonts/Inter-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../fonts/JetBrainsMono-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../fonts/IBMPlexSans-Regular.ttf").as_slice()),
    ];
    cx.text_system()
        .add_fonts(fonts)
        .expect("the bundled fonts are valid");
    gpui_kit::open_window(WindowOptions::default(), cx, ide::build)
        .expect("the browser window opens");
    cx.activate(true);
}

/// Puts a crash on the page as well as in the console. A phone has no
/// console to open, so the page is the only place the reason can be read.
fn show_crashes_on_the_page() {
    std::panic::set_hook(Box::new(|info| {
        console_error_panic_hook::hook(info);
        let shown = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("status"));
        if let Some(status) = shown {
            status.set_text_content(Some(&format!(
                "The editor stopped because of a bug.\n\n{info}\n\nReload the page to continue. Changes already marked as saved on this device are kept."
            )));
            let _ = status.remove_attribute("hidden");
        }
    }));
}

/// Starts the app. `boot.js` calls this once the module has loaded.
#[wasm_bindgen]
pub fn run() {
    show_crashes_on_the_page();
    gpui_kit::platform::web_init();
    let handle = gpui_kit::platform::single_threaded_web().run_embedded(launch);
    APPLICATION.with(|application| *application.borrow_mut() = Some(handle));
}
