// End-to-end check of the built site (dist/) in a real browser.
//
// The app draws to a canvas, so this test operates it the way assistive
// technology does: it finds the invisible HTML elements that mirror the UI by
// role and name, and activates them. That checks the app and its
// accessibility layer in one go. It uses the demo repository, so it needs no
// network and no GitHub account.
//
//   npm ci && node smoke.mjs            (after `cargo xtask dist`)
//
// CHROME sets the browser binary (default: /usr/bin/google-chrome).
//
// BROWSER=webkit runs the same checks in Playwright's WebKit, the engine
// Safari is built on (`npx playwright-core install --with-deps webkit` first).
// It is not Safari and not iOS, but it is the nearest thing that runs on a
// Linux machine. The parts that need Chrome's own controls are left out there:
// the emulated notch and zoom, the swipe gesture, and the WebGPU session.

import { createServer } from "node:http";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { createRequire } from "node:module";
import { dirname, extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, webkit } from "playwright-core";

const here = dirname(fileURLToPath(import.meta.url));
const inWebKit = process.env.BROWSER === "webkit";
const dist = join(here, "..", "dist");
const axeSource = await readFile(createRequire(import.meta.url).resolve("axe-core/axe.min.js"), "utf8");
const types = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".json": "application/json", ".webmanifest": "application/manifest+json", ".svg": "image/svg+xml", ".png": "image/png" };

// Python's libraries are downloaded from Pyodide's CDN when a program first
// imports one. Where the CDN can be reached (CI), that is tested for real.
// Where it cannot, the test checks that the terminal says so plainly.
const CDN = "https://cdn.jsdelivr.net/pyodide/";
const online = await fetch(`${CDN}v314.0.8/full/pyodide-lock.json`, { signal: AbortSignal.timeout(8_000) }).then((reply) => reply.ok).catch(() => false);

// Where GitHub can be reached (CI), the test also checks that a browser is
// allowed to talk to it from this page. It uses a made-up token, so nothing
// is read or written: GitHub's answer is "401, bad token", and getting that
// answer at all is the point.
const reachesGitHub = await fetch("https://api.github.com/zen", { signal: AbortSignal.timeout(8_000) }).then((reply) => reply.ok).catch(() => false);

// Code suggestions are tested with a stand-in model of a few kilobytes (see
// fixtures/make_tiny_model.py), served here the way Hugging Face serves a
// real one: <host>/<repository>/resolve/<revision>/<file>.
const fixtures = join(here, "fixtures");
// Set to make the site unreachable, as with no connection: every request is cut off.
let siteDown = false;
const server = createServer(async (request, response) => {
  if (siteDown) return request.socket.destroy();
  const path = normalize(decodeURIComponent(new URL(request.url, "http://x").pathname));
  const fixture = path.match(/^\/fixtures\/([^/]+)\/resolve\/[^/]+\/(.+)$/);
  const root = fixture ? fixtures : dist;
  const file = fixture ? join(fixtures, fixture[1], fixture[2]) : join(dist, path === "/" || path === "\\" ? "index.html" : path);
  if (!file.startsWith(root)) return response.writeHead(403).end();
  try {
    const body = await readFile(file);
    response.writeHead(200, { "Content-Type": types[extname(file)] ?? "application/octet-stream" }).end(body);
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const url = `http://127.0.0.1:${server.address().port}/`;

// The stand-in suggestion model: after a letter it suggests the letters that follow.
const tinyModel = {
  name: "Test model", repo: "tiny-model", host: `${url}fixtures/`, gpu_f16: "fp32", gpu: "fp32", cpu: "fp32",
  megabytes_gpu_f16: 1, megabytes_gpu: 1, megabytes_cpu: 1, template: "prefix", extensions: ["txt"],
};
// No GPU on a CI machine: Chrome's software renderer stands in. Without the
// WebGPU switches the app takes its WebGL2 path and has no WebGPU at all.
const software = ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"];
const softwareWebGpu = [...software, "--enable-unsafe-webgpu", "--enable-features=Vulkan", "--use-vulkan=swiftshader"];
const errors = [];
// Writes a file to the browser's own file storage the way the app does, and
// says which step fails, if one does.
const probeFiles = () => page.evaluate(async () => {
  let step = "navigator.storage.getDirectory()";
  try {
    const root = await navigator.storage.getDirectory();
    step = "getFileHandle(create)";
    const handle = await root.getFileHandle("probe.txt", { create: true });
    step = "createWritable()";
    const stream = await handle.createWritable();
    step = "write()";
    await stream.write("probe");
    step = "close()";
    await stream.close();
    step = "reading it back";
    const text = await (await handle.getFile()).text();
    await root.removeEntry("probe.txt");
    return text === "probe" ? "works" : `wrote "probe", read back ${JSON.stringify(text)}`;
  } catch (error) {
    return `${step} fails with ${error?.name}: ${error?.message}`;
  }
});
let browser, context, page, layer;
async function launch(args, settings = {}) {
  // iPhone-sized. The pixel ratio stays 1 because the browser's emulation of a
  // higher one does not resize the canvas backing store the way a real device does.
  const screen = { viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, hasTouch: true };
  if (inWebKit) {
    // A browser started by a test keeps nothing on disk, like a private
    // window. Say what file storage does there, then run the test the way a
    // normal window is: with a profile kept on disk.
    browser = await webkit.launch();
    page = await browser.newPage();
    await page.goto(url, { waitUntil: "commit" });
    console.log(`     (file storage in a private window: ${await probeFiles()})`);
    await browser.close();
    context = await webkit.launchPersistentContext(await mkdtemp(join(tmpdir(), "silver-wombat-webkit-")), screen);
    // Closing this context closes its browser.
    browser = context;
  } else {
    browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/google-chrome", args });
    context = await browser.newContext(screen);
  }
  await context.addInitScript((entries) => {
    for (const [key, value] of entries) if (localStorage.getItem(key) === null) localStorage.setItem(key, value);
  }, Object.entries({ "silver-wombat.suggest-model": JSON.stringify(tinyModel), ...settings }));
  page = await context.newPage();
  layer = page.locator("#a11y");
  page.on("console", (message) => {
    // A library download that fails is reported in the terminal and tested
    // there. So is GitHub's refusal of the made-up token.
    const from = message.location().url;
    // And while the test has cut the site off, requests are meant to fail.
    if (message.type() === "error" && !siteDown && !from.startsWith(CDN) && !from.startsWith("https://api.github.com/")) errors.push(message.text().slice(0, 300));
  });
  page.on("pageerror", (error) => errors.push(`uncaught: ${error.message.slice(0, 300)}`));
}
await launch(software);

let failures = 0;
function check(condition, what) {
  console.log(`${condition ? "ok  " : "FAIL"} ${what}`);
  if (!condition) {
    failures += 1;
    // What the terminal last said often explains a failure.
    page
      .locator('#a11y [role="log"]')
      .first()
      .textContent({ timeout: 2_000 })
      .then((text) => text && console.log(`     after "${what}", the terminal's last lines were:\n${text.trimEnd().split("\n").slice(-14).map((line) => `       | ${line}`).join("\n")}`))
      .catch(() => {});
  }
}
const button = (name) => layer.getByRole("button", { name, exact: true });
const field = (name) => layer.getByRole("textbox", { name, exact: true });
const settle = (ms = 700) => page.waitForTimeout(ms);
// A screen reader activates an element by sending it a click.
async function press(name) {
  // Wait for the button to exist and be enabled: a previous action may still be running.
  await eventually(async () => (await button(name).count()) === 1 && !(await button(name).isDisabled()));
  await button(name).dispatchEvent("click");
  await settle(300);
}
async function typeInto(name, text) {
  await eventually(async () => (await field(name).count()) === 1);
  await field(name).dispatchEvent("click");
  // Typing goes to the hidden text area GPUI focuses; wait until it has the focus.
  await eventually(() => page.evaluate(() => document.activeElement?.tagName === "TEXTAREA"));
  await settle(300);
  await page.keyboard.type(text, { delay: 10 });
  await settle(300);
  // A single-line field reports what is in it. On a slow machine the first
  // key can arrive before the field is ready, so check, and type again if not.
  if ((await field(name).getAttribute("aria-multiline")) === "true" || text === "") return;
  for (let attempt = 0; attempt < 3 && (await field(name).textContent()) !== text; attempt += 1) {
    await page.keyboard.press("Control+a");
    await page.keyboard.type(text, { delay: 25 });
    await settle(400);
  }
}
const heard = (role) => layer.getByRole(role).allTextContents();
// Waits for something to become true instead of sleeping a fixed time, so a
// slow machine does not fail the test.
async function eventually(probe, timeout = 15_000) {
  const end = Date.now() + timeout;
  for (;;) {
    try {
      if (await probe()) return true;
    } catch {
      // The element may not exist yet; try again.
    }
    if (Date.now() > end) return false;
    await page.waitForTimeout(200);
  }
}
const hears = (role, test) => eventually(async () => (await heard(role)).some(test));
async function waitForStatus(text, timeout) {
  try {
    await layer.getByRole("status").filter({ hasText: text }).first().waitFor({ state: "attached", timeout });
    return true;
  } catch {
    return false;
  }
}
async function scan(where) {
  await page.evaluate(axeSource);
  const result = await page.evaluate(() => axe.run(document, { runOnly: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"] }));
  for (const violation of result.violations) console.log(`     ${violation.id}: ${violation.help} (${violation.nodes.length})`);
  check(result.violations.length === 0, `no WCAG A/AA violations in the accessibility layer: ${where}`);
  const small = await layer.locator("button").evaluateAll((all) =>
    all.map((e) => [e.getAttribute("aria-label"), e.getBoundingClientRect()]).filter(([, r]) => r.width < 44 || r.height < 44).map(([name]) => name));
  check(small.length === 0, `every button is at least 44x44: ${where}${small.length ? ` (too small: ${small.join(", ")})` : ""}`);
}

async function openDemo() {
  await page.goto(url, { waitUntil: "load" });
  await button("Try the demo (no account)").waitFor({ state: "attached", timeout: 60_000 });
}

try {
  // --- Start-up and sign-in screen ---
  await page.goto(url, { waitUntil: "load" });
  // What this browser has, for reading the results that follow.
  const has = await page.evaluate(async () => ({
    agent: navigator.userAgent,
    webgpu: (await navigator.gpu?.requestAdapter().catch(() => null)) != null,
    webgl2: document.createElement("canvas").getContext("webgl2") != null,
    files: typeof navigator.storage?.getDirectory === "function" && typeof globalThis.FileSystemFileHandle?.prototype.createWritable === "function",
  }));
  console.log(`     (${has.agent})`);
  console.log(`     (WebGPU: ${has.webgpu ? "yes" : "no"}; WebGL2: ${has.webgl2 ? "yes" : "no"}; file storage: ${has.files ? await probeFiles() : "not there"})`);
  await openDemo();
  check(await page.locator("#status").isHidden(), "the app started (no failure message)");
  check((await page.locator("body > canvas").getAttribute("aria-hidden")) === "true", "the canvas is hidden from assistive technology");
  const token = layer.locator('input[type="password"]');
  check((await token.count()) === 1 && (await token.getAttribute("aria-label")) === "GitHub token", "the token field is a real password field, named GitHub token");
  await token.click();
  await page.keyboard.type("not-a-real-token", { delay: 10 });
  await settle(500);
  const exposed = await page.evaluate(() => document.documentElement.outerHTML.includes("not-a-real-token") || [...document.querySelectorAll("textarea")].some((entry) => entry.value.includes("not-a-real-token")));
  check((await token.inputValue()) === "not-a-real-token" && !exposed, "what is typed there is in the password field and nowhere else on the page");
  if (reachesGitHub) {
    await press("Sign in");
    const refused = await eventually(async () => (await heard("status")).some((text) => text.includes("GitHub did not accept the token")), 30_000);
    check(refused, `signing in with a made-up token reaches the real GitHub, and its refusal is reported${refused ? "" : ` (the app said: ${(await heard("status")).join(" | ") || "nothing"})`}`);
    // The requests that save work are POST and PATCH with a token and a JSON
    // body. A browser asks GitHub's permission before sending those from
    // another site. A 401 means permission was given and the request was sent.
    const writes = await page.evaluate(async () => {
      const answers = [];
      for (const [method, path] of [["POST", "git/blobs"], ["PATCH", "git/refs/heads/main"]]) {
        try {
          const reply = await fetch(`https://api.github.com/repos/octocat/Hello-World/${path}`, {
            method,
            mode: "cors",
            cache: "no-store",
            headers: { Accept: "application/vnd.github+json", Authorization: "Bearer not-a-real-token", "Content-Type": "application/json" },
            body: "{}",
          });
          answers.push(`${method} ${reply.status}`);
        } catch (error) {
          answers.push(`${method} blocked: ${error.message}`);
        }
      }
      return answers;
    });
    check(writes.join(", ") === "POST 401, PATCH 401", `GitHub lets this page send the requests that save work (${writes.join(", ")}; 401 is its answer to the made-up token)`);
  } else {
    console.log("     (GitHub is not reachable from here: the checks that talk to the real GitHub are not run)");
  }
  await token.fill("");
  // Give the keyboard back to the page for the next check.
  await page.evaluate(() => document.activeElement?.blur());
  await scan("sign-in");

  // Keyboard only: Tab to the demo button, see a focus ring, press Enter.
  for (let i = 0; i < 8; i += 1) {
    await page.keyboard.press("Tab");
    if ((await page.evaluate(() => document.activeElement?.getAttribute("aria-label"))) === "Try the demo (no account)") break;
  }
  const outline = await page.evaluate(() => getComputedStyle(document.activeElement).outlineStyle);
  check(outline === "solid", "keyboard focus has a visible outline");
  await page.keyboard.press("Enter");
  await settle(1500);
  check(await hears("heading", (text) => text.includes("demo/hello-python")), "Enter on the focused button opened the demo repository");

  // --- Files ---
  await scan("files");
  // Voice Control and Switch Control tap the centre of an element; that tap must reach the canvas.
  const box = await button("Folder app").evaluate((e) => e.getBoundingClientRect().toJSON());
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
  await settle(1000);
  check(await hears("heading", (text) => text === "Folder /app"), "a tap at an element's centre reaches the control under it");

  // --- Editor and the three save states ---
  await press("main.py");
  await settle(800);
  check(await eventually(async () => (await field("Code editor, /app/main.py").count()) === 1), "the editor opened /app/main.py");
  check((await heard("status")).every((text) => text === "No changes"), "an unedited file says: No changes");
  await typeInto("Code editor, /app/main.py", "# edited by the smoke test\n");
  check(await waitForStatus("Saved on this device", 5_000), "after typing: Saved on this device");
  check(await waitForStatus("Checkpointed to GitHub", 15_000), "a few seconds later: Checkpointed to GitHub");
  await scan("editor");

  // --- Terminal ---
  await press("Terminal");
  await typeInto("Command", "grep -c 'smoke test' app/main.py && git status");
  check((await field("Command").textContent()) === "grep -c 'smoke test' app/main.py && git status", "a screen reader can read back what is typed in a field");
  await page.keyboard.press("Enter");
  await settle(1500);
  check(await hears("log", (text) => /^1$/m.test(text)), "the terminal sees the edit (grep -c printed 1)");
  check(await hears("log", (text) => text.includes("modified: app/main.py")), "git status lists the file as modified");
  await typeInto("Command", "cargo build");
  await press("Run");
  check(await hears("log", (text) => text.includes("cargo: not available")), "an unsupported program says it is not available");
  await scan("terminal");

  // --- Changes: diff, commit ---
  await press("Changes");
  check(await hears("heading", (text) => text === "1 changed file"), "Changes lists 1 changed file");
  await press("Show diff of /app/main.py");
  check(await eventually(async () => (await layer.locator('[data-key="diff-text"]').textContent()).includes("+# edited by the smoke test")), "the diff shows the added line");
  await scan("changes");
  await typeInto("Commit message", "Edit from the smoke test");
  await press("Commit to main");
  check(await hears("status", (text) => text.startsWith("Committed 1 file to main")), "the commit is confirmed");
  check(await hears("heading", (text) => text === "Nothing changed since the last commit."), "after the commit nothing is left to commit");

  // --- Recovery: an edit saved on the device survives a reload ---
  await press("Terminal");
  await typeInto("Command", "echo keep me > notes.txt");
  await press("Run");
  check(await waitForStatus("Saved on this device", 5_000), "a file written from the terminal is saved on the device");
  await openDemo();
  await press("Try the demo (no account)");
  check(await hears("status", (text) => text.startsWith("Restored 1 unsaved change")), "after a reload the unsaved change is restored and announced");
  check(await eventually(async () => (await button("notes.txt  (new)").count()) === 1), "the restored file is in the file list, marked new");

  // --- Settings, storage and export ---
  await press("More");
  await hears("paragraph", (text) => text.includes("nsaved changes are kept"));
  const kept = (await heard("paragraph")).find((text) => text.includes("nsaved changes are kept")) ?? "";
  check(kept.includes("kept in this browser's file storage"), `unsaved work is kept in the browser's file storage (OPFS)${kept.includes("file storage on") ? "" : `; the settings say: ${kept}`}`);
  const download = page.waitForEvent("download", { timeout: 10_000 });
  await press("Export changes as a patch file");
  const patch = await readFile(await (await download).path(), "utf8");
  check(patch.includes("diff --git a/notes.txt b/notes.txt") && patch.includes("+keep me"), "the exported patch file contains the unsaved change");
  await press("Theme: Dark. Switch to Midnight");
  check(await eventually(async () => (await button("Theme: Midnight. Switch to Dark").count()) === 1), "the theme switches to Midnight");
  await scan("more");

  // --- Python ---
  await press("Terminal");
  const runLine = async (line) => {
    await typeInto("Command", line);
    await press("Run");
  };
  const says = (needle, timeout = 90_000) => eventually(async () => (await heard("log")).some((text) => text.includes(needle)), timeout);
  await runLine("clear");
  await runLine("python -m app.main");
  check(await says("Starting Python 3.14"), "the terminal says that Python is starting");
  check(await says("Hello, world"), "python -m app.main runs the demo program");
  await runLine(`python -c "import sys; open('made.txt', 'w').write('by python'); print('runs', sys.version.split()[0])" && cat made.txt`);
  check(await says("runs 3.14.2"), "Python 3.14 runs (3.14.2)");
  check((await says("by python")) && (await says("changed 1 file")), "a file written by a Python program is in the workspace and announced");
  check(await waitForStatus("Saved on this device", 10_000), "and it is saved on the device like any other edit");
  for (const [version, exact] of [["3.12", "3.12.7"], ["3.13", "3.13.2"], ["3.15", "3.15.0rc1"]]) {
    await runLine(`uv python pin ${version} && python -c "import sys; print('runs', sys.version.split()[0])"`);
    check(await says(`runs ${exact}`), `after uv python pin ${version}, Python ${version} runs (${exact})`);
  }
  await runLine("clear");
  // The numbers are worked out by the program, so that the command line
  // echoed in the terminal does not contain what is being looked for.
  await runLine(`python -c "import time; print(1111 * 3); time.sleep(5); print(2222 * 4)"`);
  check((await says("3333", 30_000)) && (await heard("log")).every((text) => !text.includes("8888")), "what a program prints shows while it is still running");
  check(await says("8888", 30_000), "and the rest when it gets there");
  await runLine(`python -c "import time; [print(n * 1001) for n in range(1, 13)]; time.sleep(4); print(7 * 1111)"`);
  check((await says("12012", 3_000)) && (await heard("log")).every((text) => !text.includes("7777")), "a burst of lines is all shown before the program's next wait, not after it");
  await says("7777", 30_000);
  // What a terminal does with the codes programs print: colour codes are
  // not text, and a carriage return starts the same line again.
  await runLine("clear");
  await runLine(`python -c "print(chr(27) + '[31mred text' + chr(27) + '[0m here')"`);
  check((await eventually(async () => (await heard("log")).some((text) => /^red text here$/m.test(text)), 30_000)) && (await heard("log")).every((text) => !text.includes("\u001b")), "colour codes in a program's output are not shown as text");
  await runLine(`python -c "import sys, time; [(sys.stderr.write(chr(13) + 'step %d of 3' % n), sys.stderr.flush(), time.sleep(0.4)) for n in (1, 2, 3)]; print(chr(13) + 'finished %d steps' % 3)"`);
  check((await says("finished 3 steps", 30_000)) && (await heard("log")).every((text) => !text.includes("step 1 of 3") && !text.includes("step 3 of 3")), "a progress line that redraws itself stays one line");
  // A picture the program writes can be looked at and saved to the device.
  const dot = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
  await runLine(`python -c "import base64; open('dot.png', 'wb').write(base64.b64decode('${dot}')); print('wrote', 'the', 'dot')"`);
  check((await says("wrote the dot", 30_000)) && (await says("pictures written: dot.png")), "a picture written by a program is reported in the terminal");
  await press("Show dot.png");
  const picture = layer.locator("img");
  check(await eventually(async () => (await picture.count()) === 1 && (await picture.evaluate((img) => img.complete && img.naturalWidth === 1))), "Show opens it: the browser decoded the picture (1 pixel wide, as written)");
  check((await picture.getAttribute("alt")) === "dot.png, a picture the program wrote", "a screen reader is told what the picture is");
  await scan("terminal with a picture");
  const saved = page.waitForEvent("download", { timeout: 10_000 });
  await press("Save dot.png to this device");
  const file = await saved;
  check(file.suggestedFilename() === "dot.png" && (await readFile(await file.path())).equals(Buffer.from(dot, "base64")), "Save hands over the same file, byte for byte");
  await press("Close the picture");
  check(await eventually(async () => (await picture.count()) === 0 && (await layer.getByRole("log").count()) === 1), "Close brings the terminal's output back");
  await runLine("python -c 'print(5 * 1111)'");
  check((await says("5555", 30_000)) && (await eventually(async () => (await button("Show dot.png").count()) === 0)), "the next program's pictures replace it (here: none)");
  // Typing the next command while the last one is still running.
  await runLine(`python -c "import time; time.sleep(3); print(3030 * 3)"`);
  await field("Command").dispatchEvent("click");
  await settle(300);
  const typedAhead = "echo typed while the last command was running";
  await page.keyboard.type(typedAhead, { delay: 110 });
  await says("9090", 30_000);
  await settle(500);
  check((await field("Command").textContent()) === typedAhead, "keys typed while a command runs are not lost when it finishes and prints");
  await press("Run");
  check(await says("typed while the last command was running\n", 15_000) || (await heard("log")).some((text) => text.split("typed while the last command was running").length >= 3), "and the command typed ahead runs");
  await runLine("python -c 'while True: pass'");
  await settle(2000);
  await press("Stop");
  check(await says("Stopped."), "Stop ends a Python program that would never finish");
  await runLine("echo still here");
  check(await says("still here"), "the terminal works after the stop");
  if (online) {
    for (const [version, numpy] of [["3.15", "2.4.3"], ["3.12", "2.0.2"], ["3.14", "2.4.6"]]) {
      await runLine(`uv python pin ${version} && python -c "import numpy; print('numpy', numpy.__version__)"`);
      check(await says(`numpy ${numpy}`, 180_000), `Python ${version} downloads numpy from the CDN on import (numpy ${numpy})`);
    }
    await runLine(`uv python pin 3.14 && python -c "import matplotlib.pyplot as plt; plt.plot([1, 4, 9]); plt.savefig('plot.png'); plt.show(); print('plot', 'saved')"`);
    check((await says("plot saved", 240_000)) && (await eventually(async () => (await button("Show plot.png").count()) === 1)), "matplotlib downloads from the CDN and saves a plot");
    check((await button("Show figure-1.png").count()) === 1, "plt.show() hands its figure over as a picture too");
    await press("Show figure-1.png");
    const plot = layer.locator("img");
    const width = await eventually(async () => (await plot.count()) === 1 && (await plot.evaluate((img) => img.complete && img.naturalWidth > 100))) ? await plot.evaluate((img) => img.naturalWidth) : 0;
    check(width > 100, `and the plot can be looked at (${width} pixels wide)`);
    await press("Close the picture");
    await runLine(`pip install cowsay && python -c "import cowsay; print('cowsay', 'imported')"`);
    check(await says("cowsay imported", 180_000), "pip install fetches a pure-Python package from PyPI and Python imports it");
  } else {
    console.log("     (the Pyodide CDN is not reachable from here: library downloads are not tested, only the message)");
    await runLine("python -c 'import numpy'");
    check(await says("A library could not be downloaded"), "with no connection, a missing library is reported plainly");
  }
  await scan("terminal after Python");

  // --- Extra keys: the row an on-screen keyboard lacks ---
  await press("More");
  await press("Extra keys: with the keyboard. Switch to always shown");
  await press("Terminal");
  check(await eventually(async () => (await button("Escape").count()) === 1), "the extra keys show in the terminal once switched on");
  check((await button("Complete").count()) === 0 && (await button("Stop").count()) === 1, "beside them the terminal drops its own Complete, Previous and Next, and keeps Stop");
  await runLine("clear");
  await typeInto("Command", "ech");
  await press("Tab");
  await page.keyboard.type("from the keys", { delay: 10 });
  await press("Run");
  check(await says("from the keys", 15_000), "the Tab key completes a command (ech became echo)");
  await press("Up");
  await press("Run");
  check(await eventually(async () => (await heard("log")).some((text) => text.split("from the keys").length >= 5)), "the Up key brings back the last command");
  await press("Control");
  check(await eventually(async () => (await button("Control, on").count()) === 1), "Control says that it is on");
  await page.keyboard.type("l");
  check(await eventually(async () => (await heard("log")).every((text) => !text.includes("from the keys"))), "Control then L clears the terminal");
  check((await field("Command").count()) === 1 && (await button("Control").count()) === 1, "and Control is off again");

  await runLine("echo abc > keys.txt && open keys.txt");
  await typeInto("Code editor, /keys.txt", "12");
  await press("Left");
  await page.keyboard.type("X");
  await press("More keys");
  await press("Left brace");
  await settle(1500);
  await press("Terminal");
  await runLine("cat keys.txt");
  check(await says("1X{2abc", 15_000), "in the editor the Left key moves the cursor and a symbol key types its symbol");
  await press("Editor");
  await typeInto("Code editor, /keys.txt", "");
  for (let i = 0; i < 3; i += 1) await press("More keys");
  await press("Control");
  await page.keyboard.type("a");
  await page.keyboard.type("Z");
  await settle(1500);
  await press("Terminal");
  await runLine("clear");
  await runLine("cat keys.txt");
  check(await eventually(async () => (await heard("log")).some((text) => /^Z$/m.test(text) && !text.includes("abc"))), "Control then A selects everything (typing Z replaced the file's text)");
  await press("Editor");
  await typeInto("Code editor, /keys.txt", "");
  await press("Control");
  await page.keyboard.type("z");
  await settle(1500);
  await press("Terminal");
  await runLine("clear");
  await runLine("cat keys.txt");
  check(await says("1X{2abc", 15_000), "Control then Z undoes (the replaced text is back)");
  await scan("terminal with the extra keys");

  // --- Wider screens: an open iPhone Duo, then an iPad Pro on its side ---
  const names = () => layer.locator("button").evaluateAll((all) => all.map((e) => e.getAttribute("aria-label") ?? e.textContent));
  await page.setViewportSize({ width: 626, height: 890 });
  check(await eventually(async () => !(await names()).includes("Files")), "at 626 points wide the Files tab goes away");
  check((await button("Up one folder").count()) + (await button("Folder app").count()) >= 1 && (await field("Command").count()) === 1, "because the file list now sits beside the open pane");
  await scan("626 wide");
  await page.setViewportSize({ width: 1376, height: 1032 });
  check(await eventually(async () => !(await names()).includes("Editor")), "at 1376 points wide the Editor tab goes away too");
  check((await field("Code editor, /keys.txt").count()) === 1 && (await field("Command").count()) === 1, "because the editor and the terminal are both in view");
  await press("Changes");
  check(await eventually(async () => (await field("Code editor, /keys.txt").count()) === 1 && (await field("Commit message").count()) === 1), "choosing Changes swaps the third column and leaves the editor");
  await scan("1376 wide");
  await page.setViewportSize({ width: 390, height: 844 });
  check(await eventually(async () => (await names()).includes("Files")), "back at phone width the Files tab returns");

  // --- The notch, the home indicator, and pinch-zoom ---
  const edges = () => page.evaluate(() => {
    const all = [...document.querySelectorAll("#a11y > *")].map((e) => e.getBoundingClientRect()).filter((r) => r.width > 0);
    return { top: Math.min(...all.map((r) => r.top)), bottom: Math.max(...all.map((r) => r.bottom)), right: Math.max(...all.map((r) => r.right)) };
  });
  const cdp = inWebKit ? null : await context.newCDPSession(page);
  if (cdp) {
    await cdp.send("Emulation.setSafeAreaInsetsOverride", { insets: { top: 59, bottom: 34, left: 0, right: 0 } });
    check(await eventually(async () => { const e = await edges(); return e.top >= 59 && e.bottom <= 844 - 34; }), "with a 59-point notch and a 34-point home indicator, nothing is drawn under either");
    await cdp.send("Emulation.setSafeAreaInsetsOverride", { insets: {} });
    check(await eventually(async () => (await edges()).top < 59), "and the space is used again when they are gone");
    const before = await edges();
    await cdp.send("Emulation.setPageScaleFactor", { pageScaleFactor: 2 });
    await settle(1500);
    const zoomed = await page.evaluate(() => visualViewport.scale);
    const after = await edges();
    check(zoomed === 2 && after.right === before.right && after.bottom === before.bottom && (await button("Hide keyboard").count()) === 0, "pinch-zoom magnifies the page and the interface does not rearrange itself");
    await cdp.send("Emulation.setPageScaleFactor", { pageScaleFactor: 1 });
  } else {
    console.log("     (not run in WebKit: the emulated notch, home indicator and pinch-zoom)");
  }

  // --- Finder: reach any file or action by typing a few letters ---
  const finder = () => field("Find a file or action").count();
  await press("Go to a file or action");
  check(await eventually(async () => (await finder()) === 1), "Go to opens the finder");
  await page.keyboard.type("grpy", { delay: 20 });
  await press("Open /app/greet.py");
  check(await eventually(async () => (await field("Code editor, /app/greet.py").count()) === 1), "typing grpy finds app/greet.py, and choosing it opens the file");
  await page.keyboard.press("Control+k");
  check(await eventually(async () => (await finder()) === 1), "Control+K opens the finder from a keyboard");
  await page.keyboard.type("term", { delay: 20 });
  await settle(500);
  await page.keyboard.press("Enter");
  check(await eventually(async () => (await field("Command").count()) === 1 && (await finder()) === 0), "Enter takes the first match (Go to Terminal)");
  await page.keyboard.press("Control+k");
  await eventually(async () => (await finder()) === 1);
  await page.keyboard.press("Escape");
  check(await eventually(async () => (await finder()) === 0), "Escape closes the finder");
  await runLine("clear");
  await press("Go to a file or action");
  await page.keyboard.type("run open", { delay: 20 });
  await press("Run the open file with Python");
  check((await says("python '/app/greet.py'", 15_000)) && (await eventually(async () => !(await button("Run").isDisabled()), 60_000)) && (await heard("log")).every((text) => !text.includes("can't open")), "an action can be found the same way (Run the open file with Python)");
  await scan("finder and header");

  // --- Swipe between panes (a touch gesture; the tabs do the same) ---
  // The touch carries its own clock, so how long it "took" does not depend
  // on how fast this machine is.
  const swipe = async (fromX, toX, seconds = 0.15) => {
    const start = Date.now() / 1000;
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: fromX, y: 300 }], timestamp: start });
    for (const part of [0.5, 1]) {
      await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: fromX + (toX - fromX) * part, y: 300 }], timestamp: start + seconds * part * 0.9 });
    }
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [], timestamp: start + seconds });
    await settle(600);
  };
  if (cdp) {
    await swipe(320, 120);
    check(await eventually(async () => (await field("Commit message").count()) + (await layer.getByRole("heading", { name: "Nothing changed since the last commit." }).count()) >= 1 && (await field("Command").count()) === 0, 5_000), "a quick swipe to the left goes from Terminal to Changes");
    await swipe(120, 320);
    check(await eventually(async () => (await field("Command").count()) === 1, 5_000), "a swipe to the right goes back to Terminal");
    await swipe(320, 120, 1.5);
    check((await field("Command").count()) === 1, "a slow drag is not a swipe");
    await swipe(8, 240);
    check((await field("Command").count()) === 1, "nor is a swipe from the screen's edge, which is Safari's Back");
    await press("More");
    await press("Swipe to change pane: on. Turn off");
    await swipe(120, 320);
    check((await button("Swipe to change pane: off. Turn on").count()) === 1, "swiping can be switched off, and then a swipe does nothing");
  } else {
    console.log("     (not run in WebKit: the swipe gesture)");
    await press("More");
  }

  // --- Code suggestions, with the stand-in model ---
  const status = (needle, timeout = 30_000) => eventually(async () => (await heard("status")).some((text) => text.includes(needle)), timeout);
  check(await status("Code suggestions are off") && await status("downloads it once"), "suggestions start switched off, and the settings say what switching them on costs");
  await press("Turn code suggestions on");
  check(await status("This browser has no WebGPU"), "with no WebGPU the settings say so, and offer the CPU");
  await press("Run suggestions on the CPU");
  check(await status("is running on the CPU"), "the model loads and reports that it runs on the CPU");
  await press("Terminal");
  await runLine("echo > letters.txt && open letters.txt");
  await typeInto("Code editor, /letters.txt", "abc");
  check(await status("Suggestion: defghij"), "a pause in typing brings a suggestion (after abc: defghij...)");
  await page.keyboard.press("ArrowLeft");
  check(await eventually(async () => (await button("Insert suggestion").count()) === 0), "moving the cursor withdraws it");
  await page.keyboard.press("ArrowRight");
  await scan("editor with a suggestion");
  await press("Insert suggestion");
  await settle(1500);
  await press("Terminal");
  await runLine("clear");
  await runLine("cat letters.txt");
  check(await says("abcdefghijklmnopqrstuvwxyz", 15_000), "Insert puts the suggestion into the file");
  await press("More");
  await press("Turn code suggestions off and free their memory");
  check(await status("Code suggestions are off"), "suggestions can be switched off again");
  await press("Remove the downloaded model for code suggestions");
  check(await status("was removed from this device"), "and the downloaded model can be removed");

  // --- A conflict: a file changed here was also changed on GitHub ---
  await press("More");
  await press("Demo: someone else changes a file on GitHub");
  await press("Terminal");
  await runLine("echo '# my note' >> app/greet.py");
  await press("Changes");
  await typeInto("Commit message", "My note");
  await press("Commit to main");
  check(await status("nothing on GitHub was changed") && await status("also changed on GitHub since you started"), "a commit is refused when a changed file also changed on GitHub, and says nothing was written");
  check((await button("Keep my version of /app/greet.py").count()) === 1 && (await button("Use GitHub's version of /app/greet.py").count()) === 1, "that file offers two choices: keep mine, or use GitHub's");
  await press("Compare /app/greet.py with GitHub's version");
  check(await eventually(async () => { const text = await layer.locator('[data-key="diff-text"]').textContent(); return text.includes("Hello there") && text.includes("+# my note"); }), "and a comparison of my version with GitHub's new one");
  await press("Keep my version of /app/greet.py");
  check(await status("Keeping your version of /app/greet.py"), "choosing mine is confirmed");
  await typeInto("Commit message", "My note");
  await press("Commit to main");
  check(await hears("status", (text) => text.startsWith("Committed")), "after choosing, the commit goes through");

  // --- Files from this device ---
  await press("Files");
  const importButton = await button("Add files from this device").evaluate((e) => e.getBoundingClientRect().toJSON());
  const chooser = page.waitForEvent("filechooser", { timeout: 15_000 });
  // A real tap: browsers open a file picker only for one.
  await page.touchscreen.tap(importButton.x + importButton.width / 2, importButton.y + importButton.height / 2);
  await (await chooser).setFiles([
    { name: "from-device.py", mimeType: "text/x-python", buffer: Buffer.from("print('from the device')\n") },
    { name: "photo.bin", mimeType: "application/octet-stream", buffer: Buffer.from([0xff, 0xfe, 0x00, 0x80]) },
  ]);
  check((await status("Added 1 file to")) && (await status("photo.bin")), "a text file picked from the device is added, and one that is not text is named as left out");
  check(await eventually(async () => (await button("from-device.py  (new)").count()) === 1), "the added file is in the file list, marked new");

  // --- When the device cannot store the work, the app must not say it did ---
  const saveState = () => layer.locator('[data-key="save-state"]').textContent();
  await page.evaluate(() => {
    const full = () => {
      throw new DOMException("The quota has been exceeded.", "QuotaExceededError");
    };
    globalThis.realStorage = [FileSystemFileHandle.prototype.createWritable, Storage.prototype.setItem];
    FileSystemFileHandle.prototype.createWritable = async () => full();
    Storage.prototype.setItem = full;
  });
  await press("Terminal");
  await runLine("echo more >> letters.txt");
  check(await eventually(async () => (await saveState()) === "Not saved on this device", 10_000), "with storage full, the state reads: Not saved on this device");
  await settle(1500);
  check((await saveState()) === "Not saved on this device", "and it keeps saying so");
  await page.evaluate(() => {
    [FileSystemFileHandle.prototype.createWritable, Storage.prototype.setItem] = globalThis.realStorage;
  });
  await runLine("echo again >> letters.txt");
  check(await waitForStatus("Saved on this device", 10_000), "once storage works again, the next change is saved");

  // --- Leaving the page mid-edit: the work is kept at once ---
  await page.evaluate(() => {
    // The normal save never finishes, as if the phone froze the page first.
    FileSystemFileHandle.prototype.createWritable = () => new Promise(() => {});
  });
  await runLine("echo last words >> letters.txt");
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { get: () => true, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  check(await page.evaluate(() => Object.keys(localStorage).some((key) => key.endsWith(".last-moment") && localStorage.getItem(key).includes("last words"))), "when the page is hidden with work not yet saved, it is written to the device immediately");
  check(await eventually(async () => (await saveState()) === "Saved on this device", 5_000), "and only then does the state read: Saved on this device");
  await openDemo();
  await press("Try the demo (no account)");
  check(await hears("status", (text) => text.startsWith("Restored")), "after reopening, that work is restored");
  await press("Terminal");
  await runLine("tail -n 1 letters.txt");
  check(await says("last words", 15_000), "including the last change made before leaving");

  // --- With no connection: the app opens from the copy kept on the device ---
  const icons = await page.evaluate(async () => {
    const manifest = await (await fetch(document.querySelector('link[rel="manifest"]').href)).json();
    const sources = [...manifest.icons.map((icon) => icon.src), document.querySelector('link[rel="apple-touch-icon"]').getAttribute("href")];
    const found = await Promise.all(sources.map(async (source) => (await fetch(source)).ok));
    return manifest.display === "standalone" && found.length >= 3 && found.every(Boolean);
  });
  check(icons, "the page can be added to a home screen: it names itself, opens without browser bars, and its icons exist");
  const controlled = await eventually(() => page.evaluate(() => navigator.serviceWorker?.controller != null), 20_000);
  check(controlled, "the app's files are kept on the device (a service worker serves this page)");
  if (controlled) {
    await press("More");
    check(await hears("paragraph", (text) => text.includes("The app itself is kept on this device")), "the settings say that the app opens with no connection");
    siteDown = true;
    const unreachable = await page.evaluate(() => fetch(`not-kept-${Date.now()}.txt`).then(() => false, () => true));
    let opened = false;
    try {
      await openDemo();
      await press("Try the demo (no account)");
      opened = await hears("heading", (text) => text.includes("demo/hello-python"));
    } catch {
      opened = false;
    }
    check(unreachable && opened, `with the site unreachable, the app still opens and the work kept on the device comes back${unreachable ? "" : " (the site could still be reached, so this showed nothing)"}`);
    await press("Terminal");
    await runLine("clear");
    await runLine("python -c 'print(6 * 1111)'");
    check(await says("6666", 60_000), "and Python, used before, runs with the site unreachable");
    siteDown = false;
  }

  // --- A model load that never finished is not walked into again ---
  // What is left behind when the page closes, or the browser ends it for
  // using too much memory, while the suggestion model is loading.
  await page.evaluate(() => {
    localStorage.setItem("silver-wombat.suggest", "gpu");
    localStorage.setItem("silver-wombat.suggest-loading", "1");
  });
  await openDemo();
  await press("Try the demo (no account)");
  await press("Terminal");
  await runLine("echo > letters.txt && open letters.txt");
  await typeInto("Code editor, /letters.txt", "abc");
  await settle(2500);
  await press("More");
  check(await status("was not started. Last time, the page closed while it was loading", 5_000), "after a model load that never finished, editing does not load the model again, and the settings say why");
  check((await button("Try again").count()) === 1 && (await button("Use the CPU instead for code suggestions").count()) === 1 && (await button("Turn code suggestions off and free their memory").count()) === 1, "the settings then offer: try again, use the CPU instead, or switch suggestions off");
  await press("Use the CPU instead for code suggestions");
  check(await status("is running on the CPU"), "choosing the CPU loads the model there");
  check((await page.evaluate(() => localStorage.getItem("silver-wombat.suggest-loading"))) === null, "and a load that finished leaves nothing behind");
  await press("Turn code suggestions off and free their memory");
  check(await status("Code suggestions are off"), "suggestions can be switched off from there");

  check(errors.length === 0, `no errors in the browser console${errors.length ? `: ${errors.slice(0, 3).join(" | ")}` : ""}`);

  // --- The same app on WebGPU: drawing, and the model on the GPU ---
  if (!inWebKit) {
    await browser.close();
    errors.length = 0;
    await launch(softwareWebGpu, { "silver-wombat.suggest": "gpu" });
    await openDemo();
    // Two things can happen here, and both are right. Where software WebGPU
    // works (GitHub's runners), the app draws with it. Where its device is lost
    // as soon as drawing starts (seen with Chrome 153), the page notes that
    // WebGPU drawing failed, reloads once, and draws with WebGL2.
    const fellBack = await eventually(async () => (await page.evaluate(() => localStorage.getItem("silver-wombat.renderer"))) === "webgl", 8_000);
    console.log(`     (WebGPU drawing ${fellBack ? "lost its device here, so this tests the fall-back to WebGL2" : "works here, so this tests drawing with WebGPU"})`);
    await button("Try the demo (no account)").waitFor({ state: "attached", timeout: 60_000 });
    await settle(1500);
    if (fellBack) errors.length = 0;
    check(await page.evaluate(async () => (await navigator.gpu?.requestAdapter()) != null), "this browser session has WebGPU (a software one)");
    check((await page.screenshot()).length > 12_000, `the app is drawn, ${fellBack ? "with WebGL2 after the reload" : "with WebGPU"} (the screenshot is not a blank page)`);
    await press("Try the demo (no account)");
    await press("Terminal");
    const typeLine = async (line) => {
      await typeInto("Command", line);
      await press("Run");
    };
    await typeLine("echo > letters.txt && open letters.txt");
    await typeInto("Code editor, /letters.txt", "klm");
    check(await eventually(async () => (await heard("status")).some((text) => text.includes("Suggestion: nopqrst")), 60_000), "a suggestion arrives (after klm: nopqrst...)");
    await press("More");
    check(await eventually(async () => (await heard("status")).some((text) => text.includes("is running on the GPU (WebGPU)"))), "the model itself runs on WebGPU, in its worker, and the settings say so");
    check((await button("Try WebGPU drawing again (reloads the page)").count()) === (fellBack ? 1 : 0), fellBack ? "the settings say drawing fell back to WebGL2 and offer to try WebGPU again" : "the settings do not mention a fall-back, because there was none");
    await scan("more, on WebGPU");
    check(errors.length === 0, `no errors in the browser console in this session${errors.length ? `: ${errors.slice(0, 3).join(" | ")}` : ""}`);

    // The model cannot start on the GPU (here, because its GPU file is
    // missing). The settings must say so and offer the CPU.
    await page.evaluate((model) => localStorage.setItem("silver-wombat.suggest-model", JSON.stringify(model)), { ...tinyModel, gpu_f16: "q8", gpu: "q8" });
    await openDemo();
    await press("Try the demo (no account)");
    await press("Terminal");
    await typeLine("echo > letters.txt && open letters.txt");
    await typeInto("Code editor, /letters.txt", "klm");
    await press("More");
    const told = (needle) => eventually(async () => (await heard("status")).some((text) => text.includes(needle)), 60_000);
    check(await told("The suggestion model could not be started"), "when the model cannot start on the GPU, the settings say so");
    await press("Use the CPU instead for code suggestions");
    check(await told("is running on the CPU"), "and the CPU, offered instead, runs it");
  }
} catch (error) {
  failures += 1;
  console.log(`FAIL the test stopped: ${error.message.split("\n")[0]}`);
  if (errors.length) console.log(`     errors in the browser console so far: ${errors.slice(0, 5).join(" | ")}`);
  const notice = await page.locator("#status").textContent({ timeout: 2_000 }).catch(() => "");
  if (notice?.trim()) console.log(`     the page's own failure message: ${notice.trim().slice(0, 300)}`);
  await page.screenshot({ path: join(here, "failure.png") }).catch(() => {});
} finally {
  await browser.close();
  server.close();
}

console.log(failures === 0 ? "\nAll checks passed." : `\n${failures} check(s) failed.`);
process.exit(failures === 0 ? 0 : 1);
