//! Code suggestions from a small language model that runs in the browser.
//!
//! This module is the part with no browser in it: which model, what to send
//! it for a given cursor position, and what to make of what comes back. The
//! model itself runs in a web worker (`web/complete-worker.js`).

use serde::{Deserialize, Serialize};

/// How a model is asked to continue code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    /// Fill in the middle: the model sees the text before and after the
    /// cursor, marked with the `StarCoder` family's tokens.
    Fim,
    /// The model sees only the text before the cursor.
    Prefix,
}

/// A model that can make suggestions, and where its files are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    /// What to call it on screen.
    pub name: String,
    /// The repository holding its ONNX files, as `owner/name`.
    pub repo: String,
    /// Where repositories are fetched from. Ends with `/`.
    pub host: String,
    /// The variant of the weights for a GPU with 16-bit floats, for a GPU
    /// without, and for the CPU: `q4f16`, `q4`, `fp32` and so on.
    pub gpu_f16: String,
    pub gpu: String,
    pub cpu: String,
    /// Download sizes of those variants, in megabytes, to show before asking.
    pub megabytes_gpu_f16: u32,
    pub megabytes_gpu: u32,
    pub megabytes_cpu: u32,
    pub template: Template,
    /// File extensions it is worth asking about. Empty means any.
    pub extensions: Vec<String>,
}

/// Which processor the person has agreed the model may run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Processor {
    /// The GPU only. Without WebGPU the model is not loaded.
    Gpu,
    /// The GPU if the browser has WebGPU, the CPU if not.
    GpuOrCpu,
    /// The CPU, even where there is a GPU: for when the GPU failed.
    Cpu,
}

impl Processor {
    /// The word for this in a load request and in the saved setting.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::GpuOrCpu => "any",
            Self::Cpu => "cpu",
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        [Self::Gpu, Self::GpuOrCpu, Self::Cpu]
            .into_iter()
            .find(|processor| processor.name() == name)
    }
}

impl Model {
    /// The model used unless another is configured: Tiny `StarCoder`, 164
    /// million parameters, trained on Python and able to fill in the middle.
    /// Sizes are those of the files in the repository on 2026-11-15.
    #[must_use]
    pub fn tiny_starcoder() -> Self {
        Self {
            name: "Tiny StarCoder (Python)".to_owned(),
            repo: "Xenova/tiny_starcoder_py".to_owned(),
            host: "https://huggingface.co/".to_owned(),
            gpu_f16: "q4f16".to_owned(),
            gpu: "q4".to_owned(),
            cpu: "q4".to_owned(),
            megabytes_gpu_f16: 224,
            megabytes_gpu: 320,
            megabytes_cpu: 320,
            template: Template::Fim,
            extensions: vec!["py".to_owned(), "pyi".to_owned()],
        }
    }

    /// A model described in JSON, for someone hosting their own (and for
    /// tests). `None` if the text is not a complete description.
    #[must_use]
    pub fn from_json(text: &str) -> Option<Self> {
        let model: Self = serde_json::from_str(text).ok()?;
        (model.host.ends_with('/') && !model.repo.is_empty()).then_some(model)
    }

    /// Whether to offer suggestions in this file.
    #[must_use]
    pub fn suits(&self, file: &str) -> bool {
        let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
        self.extensions.is_empty() || self.extensions.iter().any(|known| known == extension)
    }

    /// The request that loads this model, as JSON for the worker.
    #[must_use]
    pub fn load_request(&self, id: u32, processor: Processor) -> String {
        serde_json::json!({
            "id": id,
            "load": {
                "repo": self.repo,
                "host": self.host,
                "gpu_f16": self.gpu_f16,
                "gpu": self.gpu,
                "cpu": self.cpu,
                "processor": processor.name(),
            },
        })
        .to_string()
    }

    /// What to send the model for the cursor at byte `cursor` of `text`, or
    /// `None` when there is nothing before the cursor to go on.
    #[must_use]
    pub fn prompt(&self, text: &str, cursor: usize) -> Option<String> {
        let (before, after) = window(text, cursor)?;
        if before.trim().is_empty() {
            return None;
        }
        Some(match self.template {
            Template::Fim => format!("<fim_prefix>{before}<fim_suffix>{after}<fim_middle>"),
            Template::Prefix => before.to_owned(),
        })
    }
}

/// Most text given to the model from before and after the cursor, in bytes.
/// A small model reads slowly and gains little from more.
pub const MAX_BEFORE: usize = 1500;
pub const MAX_AFTER: usize = 400;
/// Most tokens to generate for one suggestion: about a line of code.
pub const MAX_NEW_TOKENS: u32 = 32;
/// The longest suggestion shown, in characters.
pub const MAX_SUGGESTION: usize = 160;

/// The text just before and just after the cursor. The part before starts at
/// the beginning of a line where it can, so the model does not see half a
/// statement. `None` if `cursor` is not a position in `text`.
#[must_use]
pub fn window(text: &str, cursor: usize) -> Option<(&str, &str)> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let (before, after) = text.split_at(cursor);
    let mut start = before.len().saturating_sub(MAX_BEFORE);
    while !before.is_char_boundary(start) {
        start += 1;
    }
    if start > 0
        && let Some(line) = before[start..].find('\n')
        && start + line + 1 < before.len()
    {
        start += line + 1;
    }
    let mut end = after.len().min(MAX_AFTER);
    while !after.is_char_boundary(end) {
        end -= 1;
    }
    Some((&before[start..], &after[..end]))
}

/// The request for one suggestion, as JSON for the worker.
#[must_use]
pub fn complete_request(id: u32, prompt: &str) -> String {
    serde_json::json!({
        "id": id,
        "complete": { "prompt": prompt, "max_new_tokens": MAX_NEW_TOKENS },
    })
    .to_string()
}

/// Turns what the model wrote into something worth offering, or `None`.
///
/// One line is offered at a time: the rest of the current line, or, when the
/// model starts by breaking the line, the line after. `after` is the text
/// following the cursor; a suggestion that only repeats it is dropped.
#[must_use]
pub fn suggestion(raw: &str, after: &str) -> Option<String> {
    let raw = raw.split("<|endoftext|>").next().unwrap_or("");
    let line = match raw.strip_prefix('\n') {
        // A new line: keep the break and the first line that has something on it.
        Some(rest) => {
            let rest = rest.trim_start_matches('\n');
            let line = rest.split('\n').next().unwrap_or("");
            if line.trim().is_empty() {
                return None;
            }
            format!("\n{line}")
        }
        None => raw.split('\n').next().unwrap_or("").to_owned(),
    };
    let line = line.trim_end();
    if line.trim().is_empty() || after.starts_with(line) {
        return None;
    }
    Some(line.chars().take(MAX_SUGGESTION).collect())
}

/// What the worker said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// Bytes downloaded so far, and in all.
    Progress {
        loaded: u64,
        total: u64,
    },
    /// The model is loaded. `gpu` is false when it runs on the CPU.
    Ready {
        gpu: bool,
        bytes: u64,
    },
    /// The answer to request `id`. `stopped` means a newer request cut it short.
    Text {
        id: u32,
        text: String,
        stopped: bool,
    },
    /// The downloaded files were removed.
    Done,
    /// This browser has no WebGPU, and the CPU was not allowed.
    NoGpu,
    Failed(String),
}

impl Reply {
    /// Reads one message from the worker. `None` if it is not one.
    #[must_use]
    pub fn parse(message: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(message).ok()?;
        if let Some(error) = value["error"].as_str() {
            return Some(if error == "no-webgpu" {
                Self::NoGpu
            } else {
                Self::Failed(error.to_owned())
            });
        }
        if let Some(progress) = value.get("progress") {
            return Some(Self::Progress {
                loaded: progress["loaded"].as_f64().map(to_bytes)?,
                total: progress["total"].as_f64().map(to_bytes)?,
            });
        }
        if let Some(ready) = value.get("ready") {
            return Some(Self::Ready {
                gpu: ready["device"].as_str()? == "webgpu",
                bytes: ready["bytes"].as_f64().map(to_bytes)?,
            });
        }
        if let Some(text) = value["text"].as_str() {
            return Some(Self::Text {
                id: u32::try_from(value["id"].as_u64()?).ok()?,
                text: text.to_owned(),
                stopped: value["stopped"].as_bool().unwrap_or(false),
            });
        }
        value["done"].as_bool().map(|_| Self::Done)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "byte counts from the browser: whole, not negative, far below the limit"
)]
fn to_bytes(number: f64) -> u64 {
    number.max(0.0) as u64
}

/// `bytes` in megabytes, rounded, for showing: at least 1 for anything there.
#[must_use]
pub fn megabytes(bytes: u64) -> u64 {
    if bytes == 0 {
        0
    } else {
        ((bytes + 500_000) / 1_000_000).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_marks_what_is_before_and_after_the_cursor() {
        let model = Model::tiny_starcoder();
        let text = "def add(a, b):\n    return \n\nprint(add(1, 2))\n";
        let cursor = text.find("return ").unwrap() + "return ".len();
        assert_eq!(
            model.prompt(text, cursor).unwrap(),
            "<fim_prefix>def add(a, b):\n    return <fim_suffix>\n\nprint(add(1, 2))\n<fim_middle>"
        );
        // Nothing typed yet: nothing to go on.
        assert_eq!(model.prompt("\n  \nx = 1\n", 3), None);
        // Not a character boundary.
        assert_eq!(model.prompt("é", 1), None);

        let mut plain = model;
        plain.template = Template::Prefix;
        assert_eq!(plain.prompt("ab cd", 2).unwrap(), "ab");
    }

    #[test]
    fn a_long_file_is_cut_to_whole_lines_near_the_cursor() {
        let line = "value = compute(1, 2, 3)\n";
        let text = line.repeat(400);
        let cursor = line.len() * 200 + 8;
        let (before, after) = window(&text, cursor).unwrap();
        assert!(before.len() <= MAX_BEFORE && before.len() > MAX_BEFORE - 2 * line.len());
        assert!(before.starts_with("value = compute"), "{before:?}");
        assert!(before.ends_with("value = "));
        assert_eq!(after.len(), MAX_AFTER);
        // One very long line is cut by length alone, on a character boundary.
        let long = "é".repeat(3000);
        let (before, after) = window(&long, 3000).unwrap();
        assert!(before.len() <= MAX_BEFORE && after.len() <= MAX_AFTER);
    }

    #[test]
    fn one_line_is_offered_at_a_time() {
        assert_eq!(
            suggestion("a + b\n\nprint(1)", "").as_deref(),
            Some("a + b")
        );
        assert_eq!(
            suggestion("a + b<|endoftext|>junk", "").as_deref(),
            Some("a + b")
        );
        assert_eq!(suggestion("a + b   ", "").as_deref(), Some("a + b"));
        // The model broke the line first: offer the break and the next line.
        assert_eq!(
            suggestion("\n    return total\n\n", "").as_deref(),
            Some("\n    return total")
        );
        // Nothing useful, or only what is already there.
        assert_eq!(suggestion("", ""), None);
        assert_eq!(suggestion("   \n", ""), None);
        assert_eq!(suggestion("\n\n", ""), None);
        assert_eq!(suggestion("<|endoftext|>", ""), None);
        assert_eq!(suggestion("a + b", "a + b\nprint(1)"), None);
        assert_eq!(
            suggestion(&"x".repeat(500), "").unwrap().len(),
            MAX_SUGGESTION
        );
    }

    #[test]
    fn suggestions_are_offered_only_where_the_model_was_trained() {
        let model = Model::tiny_starcoder();
        assert!(model.suits("app/main.py"));
        assert!(model.suits("types.pyi"));
        assert!(!model.suits("README.md"));
        assert!(!model.suits("Makefile"));
        let mut any = model;
        any.extensions.clear();
        assert!(any.suits("README.md"));
    }

    #[test]
    fn a_model_can_be_described_in_json() {
        let text = serde_json::to_string(&Model::tiny_starcoder()).unwrap();
        assert_eq!(Model::from_json(&text), Some(Model::tiny_starcoder()));
        assert_eq!(Model::from_json("{}"), None);
        assert_eq!(Model::from_json("not json"), None);
        // The host has to end with a slash: file paths are appended to it.
        let bad = text.replace("https://huggingface.co/", "https://example.org");
        assert_eq!(Model::from_json(&bad), None);
    }

    #[test]
    fn a_processor_is_saved_and_read_back_by_name() {
        for processor in [Processor::Gpu, Processor::GpuOrCpu, Processor::Cpu] {
            assert_eq!(Processor::named(processor.name()), Some(processor));
        }
        assert_eq!(Processor::named("on"), None);
        let request: serde_json::Value =
            serde_json::from_str(&Model::tiny_starcoder().load_request(1, Processor::Cpu)).unwrap();
        assert_eq!(request["load"]["processor"], "cpu");
    }

    #[test]
    fn requests_and_replies() {
        let request: serde_json::Value =
            serde_json::from_str(&Model::tiny_starcoder().load_request(7, Processor::Gpu)).unwrap();
        assert_eq!(request["id"], 7);
        assert_eq!(request["load"]["repo"], "Xenova/tiny_starcoder_py");
        assert_eq!(request["load"]["gpu_f16"], "q4f16");
        assert_eq!(request["load"]["processor"], "gpu");
        let request: serde_json::Value =
            serde_json::from_str(&complete_request(8, "x = ")).unwrap();
        assert_eq!(request["complete"]["prompt"], "x = ");
        assert_eq!(request["complete"]["max_new_tokens"], MAX_NEW_TOKENS);

        assert_eq!(
            Reply::parse(r#"{"id":1,"progress":{"loaded":1048576,"total":234567890}}"#),
            Some(Reply::Progress {
                loaded: 1_048_576,
                total: 234_567_890
            })
        );
        assert_eq!(
            Reply::parse(
                r#"{"id":1,"ready":{"device":"webgpu","dtype":"q4f16","bytes":234567890}}"#
            ),
            Some(Reply::Ready {
                gpu: true,
                bytes: 234_567_890
            })
        );
        assert_eq!(
            Reply::parse(r#"{"id":1,"ready":{"device":"wasm","dtype":"q4","bytes":5}}"#),
            Some(Reply::Ready {
                gpu: false,
                bytes: 5
            })
        );
        assert_eq!(
            Reply::parse(r#"{"id":9,"text":"a + b","stopped":false}"#),
            Some(Reply::Text {
                id: 9,
                text: "a + b".to_owned(),
                stopped: false
            })
        );
        assert_eq!(
            Reply::parse(r#"{"id":2,"error":"no-webgpu"}"#),
            Some(Reply::NoGpu)
        );
        assert_eq!(
            Reply::parse(r#"{"id":2,"error":"Failed to fetch"}"#),
            Some(Reply::Failed("Failed to fetch".to_owned()))
        );
        assert_eq!(Reply::parse(r#"{"id":3,"done":true}"#), Some(Reply::Done));
        assert_eq!(Reply::parse("nonsense"), None);
        assert_eq!(megabytes(234_567_890), 235);
        assert_eq!(megabytes(5144), 1);
        assert_eq!(megabytes(0), 0);
    }
}
