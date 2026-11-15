//! The browser APIs the app touches directly: `fetch`, the origin-private
//! file system, `localStorage` and file download.

use std::fmt::Write as _;

use silver_wombat_core::http::{Http, LocalBoxFuture, Request, Response};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

const API: &str = "https://api.github.com";

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// The origin-private file system.
    Files,
    /// `localStorage`, the small fallback.
    Small,
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
            // An older copy in the fallback store would be stale now.
            forget(key);
            Ok(Kept::Files)
        }
        Err(files_error) => save(key, text)
            .map(|()| Kept::Small)
            .map_err(|small_error| {
                format!(
                    "file storage: {}; fallback storage: {small_error}",
                    describe(&files_error)
                )
            }),
    }
}

/// Reads back unsaved work kept by [`keep`], and says where it was found.
pub async fn recall(key: &str) -> Option<(String, Kept)> {
    match read_file(key).await {
        Ok(text) => Some((text, Kept::Files)),
        Err(_) => load(key).map(|text| (text, Kept::Small)),
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
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or_else(|| JsValue::from_str("no document"))?;
        let link: web_sys::HtmlAnchorElement = document.create_element("a")?.dyn_into()?;
        link.set_href(&url);
        link.set_download(name);
        link.click();
        web_sys::Url::revoke_object_url(&url)
    };
    run().map_err(|error| describe(&error))
}
