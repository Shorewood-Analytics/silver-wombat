// Code suggestions: a small language model, run in this worker by ONNX
// Runtime through Transformers.js. On a browser with WebGPU it runs on the
// GPU. The page never waits on it: everything here is off the main thread.
//
// Messages in (JSON text):
//   {id, load: {repo, host, gpu_f16, gpu, cpu, allow_cpu}}
//   {id, complete: {prompt, max_new_tokens}}
//   {id, forget: true}          delete the downloaded model files
// Messages out (JSON text):
//   {id, progress: {loaded, total}}       bytes, while downloading
//   {id, ready: {device, dtype, bytes}}
//   {id, text, stopped}                   a suggestion ("" if there is none)
//   {id, done: true}
//   {id, error}
//
// To unload the model the page ends this worker, which frees its memory.
//
// The runtime's own files are served by this site from ml/. The model's files
// are downloaded from `host` (Hugging Face unless set otherwise) the first
// time, and kept by the browser's cache after that.

const RUNTIME = new URL("./ml/", self.location.href).href;

let library = null;
let tokenizer = null;
let model = null;
// The generation in progress, so that a newer request can stop it.
let running = null;
let queue = Promise.resolve();

async function runtime(host) {
  if (library === null) {
    library = await import(`${RUNTIME}transformers.min.js`);
    library.env.backends.onnx.wasm.wasmPaths = {
      mjs: `${RUNTIME}ort-wasm-simd-threaded.asyncify.mjs`,
      wasm: `${RUNTIME}ort-wasm-simd-threaded.asyncify.wasm`,
    };
    library.env.allowLocalModels = false;
  }
  library.env.remoteHost = host;
  return library;
}

// Which processor to use, and which of the model's files suits it. Models
// with 16-bit floats are the smallest, but need a GPU feature not every
// device has.
async function choose(load) {
  let adapter = null;
  try {
    adapter = (await self.navigator.gpu?.requestAdapter()) ?? null;
  } catch {
    adapter = null;
  }
  if (adapter !== null) {
    return { device: "webgpu", dtype: adapter.features.has("shader-f16") ? load.gpu_f16 : load.gpu };
  }
  if (load.allow_cpu) {
    return { device: "wasm", dtype: load.cpu };
  }
  throw new Error("no-webgpu");
}

async function load(id, request) {
  const { AutoTokenizer, AutoModelForCausalLM } = await runtime(request.host);
  const { device, dtype } = await choose(request);
  const files = new Map();
  let last = 0;
  const progress_callback = (info) => {
    if (info.status !== "progress" || !Number.isFinite(info.total)) return;
    files.set(info.file, [info.loaded, info.total]);
    const now = Date.now();
    if (now - last < 250) return;
    last = now;
    let loaded = 0;
    let total = 0;
    for (const [fileLoaded, fileTotal] of files.values()) {
      loaded += fileLoaded;
      total += fileTotal;
    }
    self.postMessage(JSON.stringify({ id, progress: { loaded, total } }));
  };
  tokenizer = await AutoTokenizer.from_pretrained(request.repo, { progress_callback });
  model = await AutoModelForCausalLM.from_pretrained(request.repo, { device, dtype, progress_callback });
  let bytes = 0;
  for (const [, total] of files.values()) bytes += total;
  return { ready: { device, dtype, bytes } };
}

async function complete(request) {
  if (model === null || tokenizer === null) throw new Error("the model is not loaded");
  const { InterruptableStoppingCriteria } = library;
  const inputs = tokenizer(request.prompt);
  const given = inputs.input_ids.dims[1];
  const stop = new InterruptableStoppingCriteria();
  running = stop;
  try {
    const output = await model.generate({
      ...inputs,
      max_new_tokens: request.max_new_tokens,
      do_sample: false,
      stopping_criteria: stop,
    });
    const fresh = output.slice(null, [given, null]);
    const [text] = tokenizer.batch_decode(fresh, { skip_special_tokens: true });
    return { text, stopped: stop.interrupted };
  } finally {
    running = null;
  }
}

async function forget() {
  const name = library?.env.cacheKey ?? "transformers-cache";
  await self.caches.delete(name);
  return { done: true };
}

function explain(error) {
  const message = String(error?.message ?? error);
  if (message === "no-webgpu") return "no-webgpu";
  return message;
}

self.onmessage = (event) => {
  const message = JSON.parse(event.data);
  // A newer request makes the suggestion being worked out worthless.
  running?.interrupt();
  queue = queue.then(async () => {
    try {
      let reply;
      if (message.load) reply = await load(message.id, message.load);
      else if (message.complete) reply = await complete(message.complete);
      else if (message.forget) reply = await forget();
      else throw new Error("unknown request");
      self.postMessage(JSON.stringify({ id: message.id, ...reply }));
    } catch (error) {
      self.postMessage(JSON.stringify({ id: message.id, error: explain(error) }));
    }
  });
};
