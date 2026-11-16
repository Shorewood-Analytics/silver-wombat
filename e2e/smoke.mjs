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

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, "..", "dist");
const axeSource = await readFile(createRequire(import.meta.url).resolve("axe-core/axe.min.js"), "utf8");
const types = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".json": "application/json" };

// Python's libraries are downloaded from Pyodide's CDN when a program first
// imports one. Where the CDN can be reached (CI), that is tested for real.
// Where it cannot, the test checks that the terminal says so plainly.
const CDN = "https://cdn.jsdelivr.net/pyodide/";
const online = await fetch(`${CDN}v314.0.8/full/pyodide-lock.json`, { signal: AbortSignal.timeout(8_000) }).then((reply) => reply.ok).catch(() => false);

// Code suggestions are tested with a stand-in model of a few kilobytes (see
// fixtures/make_tiny_model.py), served here the way Hugging Face serves a
// real one: <host>/<repository>/resolve/<revision>/<file>.
const fixtures = join(here, "fixtures");
const server = createServer(async (request, response) => {
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
let browser, context, page, layer;
async function launch(args, settings = {}) {
  browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/google-chrome", args });
  // iPhone-sized. The pixel ratio stays 1 because the browser's emulation of a
  // higher one does not resize the canvas backing store the way a real device does.
  context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, hasTouch: true });
  await context.addInitScript((entries) => {
    for (const [key, value] of entries) if (localStorage.getItem(key) === null) localStorage.setItem(key, value);
  }, Object.entries({ "silver-wombat.suggest-model": JSON.stringify(tinyModel), ...settings }));
  page = await context.newPage();
  layer = page.locator("#a11y");
  page.on("console", (message) => {
    // A library download that fails is reported in the terminal and tested there.
    if (message.type() === "error" && !message.location().url.startsWith(CDN)) errors.push(message.text().slice(0, 300));
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
  check(await hears("paragraph", (text) => text.includes("kept in this browser's file storage")), "unsaved work is kept in the browser's file storage (OPFS)");
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
  const cdp = await context.newCDPSession(page);
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

  check(errors.length === 0, `no errors in the browser console${errors.length ? `: ${errors.slice(0, 3).join(" | ")}` : ""}`);

  // --- The same app on WebGPU: drawing, and the model on the GPU ---
  await browser.close();
  errors.length = 0;
  await launch(softwareWebGpu, { "silver-wombat.suggest": "on" });
  await openDemo();
  // Chrome's software WebGPU loses its device as soon as the app starts
  // drawing. That makes this a test of the recovery: the page notes that
  // WebGPU drawing failed, reloads once, and draws with WebGL2.
  check(await eventually(async () => (await page.evaluate(() => localStorage.getItem("silver-wombat.renderer"))) === "webgl", 20_000), "when the WebGPU device is lost, the page switches to WebGL2 and reloads");
  await button("Try the demo (no account)").waitFor({ state: "attached", timeout: 60_000 });
  await settle(1500);
  errors.length = 0;
  check(await page.evaluate(async () => (await navigator.gpu?.requestAdapter()) != null), "this browser session has WebGPU (a software one)");
  check((await page.screenshot()).length > 12_000, "after that the app is drawn (the screenshot is not a blank page)");
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
  check((await button("Try WebGPU drawing again (reloads the page)").count()) === 1, "the settings say drawing fell back to WebGL2 and offer to try WebGPU again");
  await scan("more, on WebGPU");
  check(errors.length === 0, `no errors in the browser console after the reload${errors.length ? `: ${errors.slice(0, 3).join(" | ")}` : ""}`);
} catch (error) {
  failures += 1;
  console.log(`FAIL the test stopped: ${error.message.split("\n")[0]}`);
  await page.screenshot({ path: join(here, "failure.png") }).catch(() => {});
} finally {
  await browser.close();
  server.close();
}

console.log(failures === 0 ? "\nAll checks passed." : `\n${failures} check(s) failed.`);
process.exit(failures === 0 ? 0 : 1);
