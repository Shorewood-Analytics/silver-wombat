//! Python for the terminal: a web worker running Pyodide.
//!
//! The worker is `web/python-worker.js`. It holds one Python version at a
//! time, so asking for another version replaces it, and stopping a program
//! ends it: a running interpreter cannot be interrupted any other way without
//! cross-origin isolation, which a plain static host does not provide.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use silver_wombat_core::http::LocalBoxFuture;
use silver_wombat_core::python::{Job, Outcome, PythonHost};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{MessageEvent, Worker, WorkerOptions, WorkerType};

const WORKER: &str = "./python-worker.js";

struct Running {
    python: String,
    worker: Worker,
    // Kept so the browser can keep calling them; dropped with the worker.
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::Event)>,
}

struct State {
    running: RefCell<Option<Running>>,
    /// The request being waited on, and the function that ends the wait.
    pending: RefCell<Option<(u32, js_sys::Function)>>,
    next_id: Cell<u32>,
    progress: Rc<dyn Fn(&str)>,
}

impl State {
    /// Ends the wait for request `id` with this reply.
    fn settle(&self, id: u32, reply: &str) {
        let waiting = self
            .pending
            .borrow_mut()
            .take_if(|(pending, _)| *pending == id);
        if let Some((_, resolve)) = waiting {
            let _ = resolve.call1(&JsValue::NULL, &JsValue::from_str(reply));
        }
    }

    fn end_worker(&self) {
        if let Some(running) = self.running.borrow_mut().take() {
            running.worker.terminate();
        }
    }

    fn pending_id(&self) -> Option<u32> {
        self.pending.borrow().as_ref().map(|(id, _)| *id)
    }

    fn receive(&self, text: &str) {
        let Ok(reply) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let Some(id) = reply["id"].as_u64().and_then(|id| u32::try_from(id).ok()) else {
            return;
        };
        if self.pending_id() != Some(id) {
            return;
        }
        if let Some(note) = reply["progress"].as_str() {
            (self.progress)(note);
        } else {
            self.settle(id, text);
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.end_worker();
    }
}

fn failure(id: u32, message: &str) -> String {
    serde_json::json!({ "id": id, "error": message }).to_string()
}

/// Runs Python in a web worker.
pub struct WorkerPython {
    state: Rc<State>,
}

impl WorkerPython {
    /// `progress` receives short notes while Python starts and loads
    /// libraries, for showing in the terminal.
    pub fn new(progress: Rc<dyn Fn(&str)>) -> Self {
        Self {
            state: Rc::new(State {
                running: RefCell::new(None),
                pending: RefCell::new(None),
                next_id: Cell::new(1),
                progress,
            }),
        }
    }

    /// The worker for this Python version, started if need be.
    fn worker(&self, python: &str) -> Result<Worker, String> {
        if let Some(running) = self.state.running.borrow().as_ref()
            && running.python == python
        {
            return Ok(running.worker.clone());
        }
        self.state.end_worker();
        let options = WorkerOptions::new();
        options.set_type(WorkerType::Module);
        let worker = Worker::new_with_options(WORKER, &options)
            .map_err(|error| format!("this browser could not start a worker: {error:?}"))?;

        let state = Rc::downgrade(&self.state);
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let (Some(state), Some(text)) = (state.upgrade(), event.data().as_string()) {
                state.receive(&text);
            }
        });
        worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let state = Rc::downgrade(&self.state);
        let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
            let Some(state) = state.upgrade() else { return };
            // The worker is unusable; the next run starts a new one.
            state.end_worker();
            if let Some(id) = state.pending_id() {
                state.settle(
                    id,
                    &failure(
                        id,
                        "the Python worker stopped unexpectedly (it may have run out of memory)",
                    ),
                );
            }
        });
        worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        *self.state.running.borrow_mut() = Some(Running {
            python: python.to_owned(),
            worker: worker.clone(),
            _on_message: on_message,
            _on_error: on_error,
        });
        Ok(worker)
    }
}

impl PythonHost for WorkerPython {
    fn run(&self, job: Job) -> LocalBoxFuture<'_, Result<Outcome, String>> {
        Box::pin(async move {
            if self.state.pending.borrow().is_some() {
                return Err("a Python program is already running".to_owned());
            }
            let worker = self.worker(&job.python)?;
            let id = self.state.next_id.get();
            self.state.next_id.set(id.wrapping_add(1));
            let request = serde_json::json!({ "id": id, "job": job }).to_string();

            let state = self.state.clone();
            let reply = js_sys::Promise::new(&mut |resolve, _| {
                *state.pending.borrow_mut() = Some((id, resolve));
            });
            if let Err(error) = worker.post_message(&JsValue::from_str(&request)) {
                self.state.pending.borrow_mut().take();
                return Err(format!("could not reach the Python worker: {error:?}"));
            }
            let reply = JsFuture::from(reply)
                .await
                .ok()
                .and_then(|reply| reply.as_string())
                .ok_or("the Python worker gave no answer")?;
            let mut reply: serde_json::Value =
                serde_json::from_str(&reply).map_err(|error| error.to_string())?;
            if let Some(message) = reply["error"].as_str() {
                return Err(message.to_owned());
            }
            serde_json::from_value(reply["outcome"].take())
                .map_err(|error| format!("the Python worker's answer could not be read: {error}"))
        })
    }

    fn stop(&self) {
        let Some(id) = self.state.pending_id() else {
            return;
        };
        // Ending the worker is the only way to stop the interpreter. Files
        // the program had written so far are lost with it.
        self.state.end_worker();
        let stopped = serde_json::json!({
            "id": id,
            "outcome": { "output": "", "status": 130, "changed": [], "binary": [] },
        });
        self.state.settle(id, &stopped.to_string());
    }
}
