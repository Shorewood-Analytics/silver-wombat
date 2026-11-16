//! The browser APIs the app touches directly: `fetch`, the origin-private
//! file system, `localStorage` and file download.

use std::fmt::Write as _;

use silver_wombat_core::http::{Http, LocalBoxFuture, Request, Response};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

const API: &str = "https://api.github.com";

/// Set to "webgl" by `web/boot.js` when WebGPU drawing has failed on this
/// device, so that the app starts on WebGL2 from then on.
pub const KEY_RENDERER: &str = "silver-wombat.renderer";

/// Forgets that WebGPU drawing failed on this device and reloads the page to
/// try it again. If it fails again, `web/boot.js` falls back as before.
pub fn retry_webgpu() {
    let Some(window) = web_sys::window() else {
        return;
    };
    forget(KEY_RENDERER);
    if let Ok(Some(session)) = window.session_storage() {
        let _ = session.remove_item(KEY_RENDERER);
    }
    let _ = window.location().reload();
}

/// Deletes the suggestion model's downloaded files from the browser's cache.
/// `Ok(true)` if there was something to delete.
pub async fn forget_model() -> Result<bool, String> {
    let caches = web_sys::window()
        .ok_or("no window")?
        .caches()
        .map_err(|error| describe(&error))?;
    // The name Transformers.js gives its cache (`env.cacheKey`).
    let deleted = JsFuture::from(caches.delete("transformers-cache"))
        .await
        .map_err(|error| describe(&error))?;
    Ok(deleted.as_bool().unwrap_or(false))
}

/// Whether the person has pinch-zoomed the page. Zooming and an on-screen
/// keyboard both shrink what the browser calls the visual viewport, and only
/// the keyboard should make the interface rearrange itself: someone who zooms
/// in to read wants the page bigger, not re-fitted to the smaller view.
pub fn zoomed() -> bool {
    web_sys::window()
        .and_then(|window| window.visual_viewport())
        .is_some_and(|viewport| viewport.scale() > 1.01)
}

/// A touch in progress: which finger, and where and when it went down.
#[derive(Clone, Copy)]
struct Touch {
    id: i32,
    x: f64,
    y: f64,
    at: f64,
}

/// Calls `on_touch` for every single-finger touch that ends, with where it
/// started, how far it moved and how long it took: `(start_x, dx, dy,
/// milliseconds)`. Deciding whether that was a swipe is the caller's job.
/// The listeners only watch; the touch still reaches the canvas.
pub fn watch_touches(on_touch: impl Fn(f32, f32, f32, f32) + 'static) {
    use std::cell::Cell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use web_sys::PointerEvent;

    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let down: Rc<Cell<Option<Touch>>> = Rc::new(Cell::new(None));
    let options = web_sys::AddEventListenerOptions::new();
    options.set_capture(true);
    options.set_passive(true);
    let listen = |name: &str, handler: Box<dyn FnMut(PointerEvent)>| {
        let handler = Closure::wrap(handler);
        let _ = document.add_event_listener_with_callback_and_add_event_listener_options(
            name,
            handler.as_ref().unchecked_ref(),
            &options,
        );
        // Lives as long as the page.
        handler.forget();
    };

    let state = down.clone();
    listen(
        "pointerdown",
        Box::new(move |event: PointerEvent| {
            if event.pointer_type() != "touch" {
                return;
            }
            // A second finger makes it a pinch, not a swipe.
            state.set(match state.get() {
                None => Some(Touch {
                    id: event.pointer_id(),
                    x: f64::from(event.client_x()),
                    y: f64::from(event.client_y()),
                    at: event.time_stamp(),
                }),
                Some(_) => None,
            });
        }),
    );
    let state = down.clone();
    listen(
        "pointerup",
        Box::new(move |event: PointerEvent| {
            let Some(touch) = state.take() else {
                return;
            };
            if event.pointer_id() != touch.id {
                return;
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "screen distances and short times fit"
            )]
            on_touch(
                touch.x as f32,
                (f64::from(event.client_x()) - touch.x) as f32,
                (f64::from(event.client_y()) - touch.y) as f32,
                (event.time_stamp() - touch.at) as f32,
            );
        }),
    );
    listen(
        "pointercancel",
        Box::new(move |_: PointerEvent| down.set(None)),
    );
}

/// While `held` is set, catches the next single character typed, by an
/// on-screen or an attached keyboard, before it reaches a text field, and
/// gives it to `on_character` instead. Used for the extra keys' Control: the
/// character becomes a Control key and is never typed.
///
/// The browser announces text about to be inserted with a `beforeinput`
/// event, which can be cancelled; that is what this listens for.
pub fn watch_typing(
    held: std::rc::Rc<std::cell::Cell<bool>>,
    on_character: impl Fn(String) + 'static,
) {
    use wasm_bindgen::closure::Closure;
    use web_sys::InputEvent;

    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let handler = Closure::<dyn FnMut(InputEvent)>::new(move |event: InputEvent| {
        // Text being composed (an input method, dictation) is left alone.
        if !held.get() || event.is_composing() {
            return;
        }
        let Some(typed) = event.data() else { return };
        if typed.chars().count() != 1 {
            return;
        }
        event.prevent_default();
        event.stop_propagation();
        held.set(false);
        on_character(typed);
    });
    let _ = document.add_event_listener_with_callback_and_bool(
        "beforeinput",
        handler.as_ref().unchecked_ref(),
        true,
    );
    // Lives as long as the page.
    handler.forget();
}

/// Calls `on_shortcut` when Control+K or Command+K is pressed, whatever has
/// the keyboard's focus. The key goes no further: browsers give it no
/// meaning of their own inside a page.
pub fn watch_finder_shortcut(on_shortcut: impl Fn() + 'static) {
    use wasm_bindgen::closure::Closure;
    use web_sys::KeyboardEvent;

    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let handler = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        let chord = (event.ctrl_key() || event.meta_key()) && !event.alt_key();
        if chord && event.key().eq_ignore_ascii_case("k") {
            event.prevent_default();
            event.stop_propagation();
            on_shortcut();
        }
    });
    let _ = document.add_event_listener_with_callback_and_bool(
        "keydown",
        handler.as_ref().unchecked_ref(),
        true,
    );
    // Lives as long as the page.
    handler.forget();
}

fn describe(value: &JsValue) -> String {
    value
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.message()))
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}

/// Talks to GitHub with the person's token.
pub struct FetchHttp {
    token: String,
}

impl FetchHttp {
    pub fn new(token: String) -> Self {
        Self { token }
    }

    async fn fetch(&self, request: Request) -> Result<Response, JsValue> {
        let headers = web_sys::Headers::new()?;
        // A browser asks GitHub's permission before sending any header outside
        // a short list. GitHub's CORS documentation names the ones it permits:
        // Authorization and Content-Type are on it, X-GitHub-Api-Version is
        // not. So no version header is sent; GitHub then uses its default
        // version, 2022-11-28, which is the one this client was written for.
        headers.set("Accept", "application/vnd.github+json")?;
        headers.set("Authorization", &format!("Bearer {}", self.token))?;
        let init = web_sys::RequestInit::new();
        init.set_method(request.method.as_str());
        init.set_mode(web_sys::RequestMode::Cors);
        // GitHub marks answers cacheable for 60 seconds. A stale branch head
        // would make a fresh commit look like a conflict, so never reuse one.
        init.set_cache(web_sys::RequestCache::NoStore);
        if let Some(body) = &request.body {
            headers.set("Content-Type", "application/json")?;
            init.set_body(&JsValue::from_str(body));
        }
        init.set_headers(&headers);
        let url = format!("{API}{}", request.path);
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
        let response: web_sys::Response = JsFuture::from(
            window.fetch_with_request(&web_sys::Request::new_with_str_and_init(&url, &init)?),
        )
        .await?
        .dyn_into()?;
        let number = |name: &str| {
            response
                .headers()
                .get(name)
                .ok()
                .flatten()
                .and_then(|value| value.parse::<u64>().ok())
        };
        let rate_limit_remaining = number("x-ratelimit-remaining");
        let rate_limit_reset = number("x-ratelimit-reset");
        let body = JsFuture::from(response.text()?)
            .await?
            .as_string()
            .unwrap_or_default();
        Ok(Response {
            status: response.status(),
            body,
            rate_limit_remaining,
            rate_limit_reset,
        })
    }
}

impl Http for FetchHttp {
    fn send(&self, request: Request) -> LocalBoxFuture<'_, Result<Response, String>> {
        Box::pin(async move { self.fetch(request).await.map_err(|error| describe(&error)) })
    }
}

fn storage() -> Result<web_sys::Storage, String> {
    web_sys::window()
        .ok_or_else(|| "no window".to_owned())?
        .local_storage()
        .map_err(|error| describe(&error))?
        .ok_or_else(|| "this browser has storage turned off".to_owned())
}

/// Reads a small setting saved on this device.
pub fn load(key: &str) -> Option<String> {
    storage().ok()?.get_item(key).ok().flatten()
}

/// Saves a small setting on this device. `Err` carries the browser's reason.
pub fn save(key: &str, value: &str) -> Result<(), String> {
    storage()?
        .set_item(key, value)
        .map_err(|error| describe(&error))
}

/// Removes a setting saved on this device.
pub fn forget(key: &str) {
    if let Ok(storage) = storage() {
        let _ = storage.remove_item(key);
    }
}

// ----- Unsaved work -----
//
// Local changes go to the origin-private file system (OPFS): it has room for
// real projects, and a write only replaces the old file when it is closed, so
// a tab killed mid-write leaves the previous version intact. Browsers without
// it (Safari before 26, some private modes) fall back to `localStorage`,
// which is limited to about 5 MB.

/// Where unsaved work is being kept on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// The origin-private file system.
    Files,
    /// `localStorage`, the small fallback, and why file storage was not used.
    Small(String),
}

/// A browser error with its name, which for storage errors says more than
/// the message does (`NotAllowedError`, `QuotaExceededError`).
fn describe_named(value: &JsValue) -> String {
    match value.dyn_ref::<js_sys::Error>() {
        Some(error) => format!("{}: {}", error.name(), error.message()),
        None => describe(value),
    }
}

fn file_name(key: &str) -> String {
    // File names may not contain `/`; keep them readable and unambiguous.
    let mut name = String::with_capacity(key.len());
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'@') {
            name.push(char::from(byte));
        } else {
            let _ = write!(name, "%{byte:02X}");
        }
    }
    name
}

async fn directory() -> Result<web_sys::FileSystemDirectoryHandle, JsValue> {
    let manager = web_sys::window()
        .ok_or_else(|| JsValue::from_str("no window"))?
        .navigator()
        .storage();
    JsFuture::from(manager.get_directory()).await?.dyn_into()
}

async fn write_file(key: &str, text: &str) -> Result<(), JsValue> {
    let options = web_sys::FileSystemGetFileOptions::new();
    options.set_create(true);
    let handle: web_sys::FileSystemFileHandle = JsFuture::from(
        directory()
            .await?
            .get_file_handle_with_options(&file_name(key), &options),
    )
    .await?
    .dyn_into()?;
    let stream: web_sys::FileSystemWritableFileStream =
        JsFuture::from(handle.create_writable()).await?.dyn_into()?;
    JsFuture::from(stream.write_with_str(text)?).await?;
    // Closing is the moment the new contents replace the old.
    JsFuture::from(stream.close()).await?;
    Ok(())
}

async fn read_file(key: &str) -> Result<String, JsValue> {
    let handle: web_sys::FileSystemFileHandle =
        JsFuture::from(directory().await?.get_file_handle(&file_name(key)))
            .await?
            .dyn_into()?;
    let file: web_sys::File = JsFuture::from(handle.get_file()).await?.dyn_into()?;
    Ok(JsFuture::from(file.text())
        .await?
        .as_string()
        .unwrap_or_default())
}

/// Saves unsaved work on this device and says where it went. `Err` means it
/// is not saved anywhere on the device; the caller must say so.
pub async fn keep(key: &str, text: &str) -> Result<Kept, String> {
    match write_file(key, text).await {
        Ok(()) => {
            // Older copies in the fallback store would be stale now.
            forget(key);
            forget(&last_moment_key(key));
            Ok(Kept::Files)
        }
        Err(files_error) => {
            let why = describe_named(&files_error);
            match save(key, text) {
                Ok(()) => {
                    forget(&last_moment_key(key));
                    Ok(Kept::Small(why))
                }
                Err(small_error) => Err(format!(
                    "file storage: {why}; fallback storage: {small_error}"
                )),
            }
        }
    }
}

fn last_moment_key(key: &str) -> String {
    format!("{key}.last-moment")
}

/// Keeps unsaved work at once, without waiting, for the moment the page is
/// being hidden or closed and there may be no time for [`keep`] to finish.
/// It goes to the small fallback store, the only one that can be written
/// without waiting, and is read back in preference to anything older.
pub fn keep_now(key: &str, text: &str) -> Result<(), String> {
    save(&last_moment_key(key), text)
}

/// Calls `on_hidden` when the page is hidden or about to be unloaded: the
/// person switched app or tab, locked the phone, or closed the page. On a
/// phone this is often the last thing a page gets to do.
pub fn watch_hidden(on_hidden: impl Fn() + 'static) {
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let on_hidden = Rc::new(on_hidden);
    let hidden = on_hidden.clone();
    let watched = document.clone();
    let on_visibility = Closure::<dyn FnMut()>::new(move || {
        if watched.hidden() {
            hidden();
        }
    });
    let _ = document.add_event_listener_with_callback(
        "visibilitychange",
        on_visibility.as_ref().unchecked_ref(),
    );
    on_visibility.forget();
    let on_page_hide = Closure::<dyn FnMut()>::new(move || on_hidden());
    let _ =
        window.add_event_listener_with_callback("pagehide", on_page_hide.as_ref().unchecked_ref());
    on_page_hide.forget();
}

/// One file the person picked from their device.
pub struct Picked {
    pub name: String,
    /// The file's text, or `None` if it is not text or is too big.
    pub text: Option<String>,
}

/// Opens the browser's file picker, now. Browsers only allow that while a
/// tap or key press is being handled, so this part does not wait for
/// anything; the reading is done by [`read_picked`].
pub fn open_file_picker() -> Option<(web_sys::HtmlInputElement, js_sys::Promise)> {
    use wasm_bindgen::closure::Closure;

    let document = web_sys::window()?.document()?;
    let input: web_sys::HtmlInputElement =
        document.create_element("input").ok()?.dyn_into().ok()?;
    input.set_type("file");
    input.set_multiple(true);
    let picked = js_sys::Promise::new(&mut |resolve, _| {
        // Whichever comes first: files chosen, or the picker dismissed.
        for event in ["change", "cancel"] {
            let resolve = resolve.clone();
            let done = Closure::once_into_js(move || {
                let _ = resolve.call0(&JsValue::NULL);
            });
            let _ = input.add_event_listener_with_callback(event, done.unchecked_ref());
        }
    });
    input.click();
    Some((input, picked))
}

/// Waits for the person to pick files in the picker [`open_file_picker`]
/// opened, and reads them. Files that are not valid UTF-8 text, or are over
/// `max_bytes`, come back without text. Empty if they pick nothing.
pub async fn read_picked(
    (input, picked): (web_sys::HtmlInputElement, js_sys::Promise),
    max_bytes: u64,
) -> Vec<Picked> {
    let _ = JsFuture::from(picked).await;
    let Some(files) = input.files() else {
        return Vec::new();
    };
    let mut read = Vec::new();
    for index in 0..files.length() {
        let Some(file) = files.get(index) else {
            continue;
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "a file size, compared with a limit"
        )]
        let small_enough = file.size() <= max_bytes as f64;
        let text = if small_enough {
            match JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => String::from_utf8(js_sys::Uint8Array::new(&buffer).to_vec()).ok(),
                Err(_) => None,
            }
        } else {
            None
        };
        read.push(Picked {
            name: file.name(),
            text,
        });
    }
    read
}

/// Reads back unsaved work kept by [`keep`] or [`keep_now`]. Also says where
/// saves go, when finding it showed that: work found in file storage means
/// file storage works. Work found in the small store does not say why it was
/// there, so the next save has to.
pub async fn recall(key: &str) -> Option<(String, Option<Kept>)> {
    if let Some(text) = load(&last_moment_key(key)) {
        return Some((text, None));
    }
    match read_file(key).await {
        Ok(text) => Some((text, Some(Kept::Files))),
        Err(_) => load(key).map(|text| (text, None)),
    }
}

/// Asks the browser not to evict this site's storage under pressure. Returns
/// `Some(true)` if granted, `Some(false)` if refused, `None` if the browser
/// cannot say.
pub async fn request_persistence() -> Option<bool> {
    let manager = web_sys::window()?.navigator().storage();
    JsFuture::from(manager.persist().ok()?)
        .await
        .ok()?
        .as_bool()
}

/// Hands the person a text file to keep, through the browser's download.
pub fn download(name: &str, text: &str) -> Result<(), String> {
    let run = || -> Result<(), JsValue> {
        let parts = js_sys::Array::of1(&JsValue::from_str(text));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("text/plain;charset=utf-8");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)?;
        let sent = download_from(name, &url);
        web_sys::Url::revoke_object_url(&url)?;
        sent
    };
    run().map_err(|error| describe(&error))
}

fn download_from(name: &str, url: &str) -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let link: web_sys::HtmlAnchorElement = document.create_element("a")?.dyn_into()?;
    link.set_href(url);
    link.set_download(name);
    link.click();
    Ok(())
}

/// Hands the person the file behind a [`file_address`] to keep.
pub fn download_address(name: &str, address: &str) -> Result<(), String> {
    download_from(name, address).map_err(|error| describe(&error))
}

/// Holds a file's bytes in the browser and returns an address for them that
/// an `<img>` can show and a download can save. Give the address back to
/// [`forget_address`] when it is no longer needed, to free the bytes.
pub fn file_address(bytes: &[u8], media_type: &str) -> Result<String, String> {
    let run = || -> Result<String, JsValue> {
        let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type(media_type);
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)?;
        web_sys::Url::create_object_url_with_blob(&blob)
    };
    run().map_err(|error| describe(&error))
}

pub fn forget_address(address: &str) {
    let _ = web_sys::Url::revoke_object_url(address);
}
