//! The browser APIs the app touches directly: `fetch` and `localStorage`.

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
        headers.set("Accept", "application/vnd.github+json")?;
        headers.set("X-GitHub-Api-Version", "2022-11-28")?;
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

/// Reads a value saved on this device.
pub fn load(key: &str) -> Option<String> {
    storage().ok()?.get_item(key).ok().flatten()
}

/// Saves a value on this device. `Err` carries the browser's reason, for
/// example a full quota; the caller must not report the data as saved.
pub fn save(key: &str, value: &str) -> Result<(), String> {
    storage()?
        .set_item(key, value)
        .map_err(|error| describe(&error))
}

/// Removes a value saved on this device.
pub fn forget(key: &str) {
    if let Ok(storage) = storage() {
        let _ = storage.remove_item(key);
    }
}
