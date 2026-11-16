//! The web worker that runs the code-suggestion model.
//!
//! The worker is `web/complete-worker.js`; what is said to it and what it
//! says back are defined in `silver_wombat_core::complete`. Dropping this
//! ends the worker, which is how the model's memory is given back.

use std::rc::Rc;

use silver_wombat_core::complete::Reply;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::{MessageEvent, Worker, WorkerOptions, WorkerType};

const WORKER: &str = "./complete-worker.js";

pub struct SuggestWorker {
    worker: Worker,
    // Kept so the browser can keep calling them.
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::Event)>,
}

impl SuggestWorker {
    /// Starts the worker. `on_reply` is called with everything it says.
    pub fn start(on_reply: Rc<dyn Fn(Reply)>) -> Result<Self, String> {
        let options = WorkerOptions::new();
        options.set_type(WorkerType::Module);
        let worker = Worker::new_with_options(WORKER, &options)
            .map_err(|error| format!("this browser could not start a worker: {error:?}"))?;

        let reply = on_reply.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Some(message) = event.data().as_string().as_deref().and_then(Reply::parse) {
                reply(message);
            }
        });
        worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
            on_reply(Reply::Failed(
                "the suggestion model stopped unexpectedly (it may have run out of memory)"
                    .to_owned(),
            ));
        });
        worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        Ok(Self {
            worker,
            _on_message: on_message,
            _on_error: on_error,
        })
    }

    /// Sends one request (see `complete::Model::load_request` and
    /// `complete::complete_request`).
    pub fn send(&self, request: &str) {
        let _ = self.worker.post_message(&JsValue::from_str(request));
    }
}

impl Drop for SuggestWorker {
    fn drop(&mut self) {
        self.worker.terminate();
    }
}
