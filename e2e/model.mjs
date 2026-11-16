// Runs the real code-suggestion model in a browser, once on WebGPU and once
// on the CPU, and prints what it suggests and how long it took.
//
// This is the only check that downloads the real model (about 320 MB from
// Hugging Face), so it is separate from smoke.mjs, which uses a stand-in. It
// needs the network; without it, it says so and passes.
//
//   node model.mjs            (after `cargo xtask dist`)
//
// A CI machine has no GPU, so "WebGPU" here is Chrome's software version:
// this shows that the model loads and runs through the WebGPU code path, not
// how fast it is on a real device.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { dirname, extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const MODEL = { repo: "Xenova/tiny_starcoder_py", host: "https://huggingface.co/", gpu_f16: "q4f16", gpu: "q4", cpu: "q4", processor: "any" };
const PROMPT = "<fim_prefix>def add(a, b):\n    return <fim_suffix>\n\nprint(add(1, 2))\n<fim_middle>";

const reachable = await fetch(`${MODEL.host}${MODEL.repo}/resolve/main/config.json`, { signal: AbortSignal.timeout(10_000) }).then((reply) => reply.ok).catch(() => false);
if (!reachable) {
  console.log("Hugging Face is not reachable from here, so the real model was not tried.");
  process.exit(0);
}

const dist = join(dirname(fileURLToPath(import.meta.url)), "..", "dist");
const types = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm" };
const server = createServer(async (request, response) => {
  const file = join(dist, normalize(decodeURIComponent(new URL(request.url, "http://x").pathname)));
  if (!file.startsWith(dist)) return response.writeHead(403).end();
  try {
    const body = await readFile(file);
    response.writeHead(200, { "Content-Type": types[extname(file)] ?? "application/octet-stream" }).end(body);
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const url = `http://127.0.0.1:${server.address().port}/`;

const software = ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"];
const sessions = { "WebGPU (software)": [...software, "--enable-unsafe-webgpu", "--enable-features=Vulkan", "--use-vulkan=swiftshader"], CPU: software };
let failures = 0;
for (const [name, args] of Object.entries(sessions)) {
  const browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/google-chrome", args });
  try {
    const page = await browser.newPage();
    page.on("pageerror", (error) => console.log(`   page error: ${error.message.slice(0, 300)}`));
    await page.goto(`${url}app.css`);
    const result = await page.evaluate(async ([model, prompt]) => {
      const worker = new Worker("/complete-worker.js", { type: "module" });
      const ask = (id, body) => new Promise((resolve) => {
        const started = performance.now();
        const listen = (event) => {
          const message = JSON.parse(event.data);
          if (message.id !== id || message.progress) return;
          worker.removeEventListener("message", listen);
          resolve({ seconds: Math.round((performance.now() - started) / 100) / 10, ...message });
        };
        worker.addEventListener("message", listen);
        worker.addEventListener("error", (event) => resolve({ error: `the worker failed: ${event.message}` }));
        worker.postMessage(JSON.stringify({ id, ...body }));
      });
      const loaded = await ask(1, { load: model });
      if (loaded.error) return { loaded };
      const first = await ask(2, { complete: { prompt, max_new_tokens: 12 } });
      const second = await ask(3, { complete: { prompt, max_new_tokens: 12 } });
      return { loaded, first, second };
    }, [MODEL, PROMPT]);
    const { loaded, first, second } = result;
    if (loaded.error) {
      failures += 1;
      console.log(`FAIL ${name}: the model did not load: ${loaded.error}`);
      continue;
    }
    console.log(`ok   ${name}: loaded on ${loaded.ready.device} (${loaded.ready.dtype}, ${Math.round(loaded.ready.bytes / 1e6)} MB) in ${loaded.seconds} s`);
    for (const [which, reply] of [["first", first], ["second", second]]) {
      const good = !reply.error && reply.text.trim().length > 0;
      if (!good) failures += 1;
      console.log(`${good ? "ok  " : "FAIL"} ${name}: ${which} suggestion in ${reply.seconds} s: ${JSON.stringify(reply.error ?? reply.text)}`);
    }
  } catch (error) {
    failures += 1;
    console.log(`FAIL ${name}: ${error.message.split("\n")[0]}`);
  } finally {
    await browser.close();
  }
}
server.close();
console.log(failures === 0 ? "\nThe real model ran." : `\n${failures} check(s) failed.`);
process.exit(failures === 0 ? 0 : 1);
