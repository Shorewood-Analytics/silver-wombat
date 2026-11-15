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
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm" };

const server = createServer(async (request, response) => {
  const path = normalize(decodeURIComponent(new URL(request.url, "http://x").pathname));
  const file = join(dist, path === "/" || path === "\\" ? "index.html" : path);
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

const browser = await chromium.launch({
  executablePath: process.env.CHROME ?? "/usr/bin/google-chrome",
  // No GPU on a CI machine: use the software renderer. The app then takes its WebGL2 path.
  args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist"],
});
// iPhone-sized. The pixel ratio stays 1 because the browser's emulation of a
// higher one does not resize the canvas backing store the way a real device does.
const context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 1, hasTouch: true });
const page = await context.newPage();
const errors = [];
page.on("console", (message) => message.type() === "error" && errors.push(message.text().slice(0, 300)));
page.on("pageerror", (error) => errors.push(`uncaught: ${error.message.slice(0, 300)}`));

let failures = 0;
function check(condition, what) {
  console.log(`${condition ? "ok  " : "FAIL"} ${what}`);
  if (!condition) failures += 1;
}
const layer = page.locator("#a11y");
const button = (name) => layer.getByRole("button", { name, exact: true });
const field = (name) => layer.getByRole("textbox", { name, exact: true });
const settle = (ms = 700) => page.waitForTimeout(ms);
// A screen reader activates an element by sending it a click.
async function press(name) {
  await button(name).dispatchEvent("click");
  await settle();
}
async function typeInto(name, text) {
  await field(name).dispatchEvent("click");
  await settle(300);
  await page.keyboard.type(text, { delay: 10 });
  await settle(300);
}
const heard = (role) => layer.getByRole(role).allTextContents();
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
  check((await field("GitHub token").count()) === 1, "the token field is exposed as a text field");
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
  check((await heard("heading")).some((text) => text.includes("demo/hello-python")), "Enter on the focused button opened the demo repository");

  // --- Files ---
  await scan("files");
  // Voice Control and Switch Control tap the centre of an element; that tap must reach the canvas.
  const box = await button("Folder app").evaluate((e) => e.getBoundingClientRect().toJSON());
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
  await settle(1000);
  check((await heard("heading")).includes("Folder /app"), "a tap at an element's centre reaches the control under it");

  // --- Editor and the three save states ---
  await press("main.py");
  await settle(800);
  check((await field("Code editor, /app/main.py").count()) === 1, "the editor opened /app/main.py");
  check((await heard("status")).every((text) => text === "No changes"), "an unedited file says: No changes");
  await typeInto("Code editor, /app/main.py", "# edited by the smoke test\n");
  check(await waitForStatus("Saved on this device", 5_000), "after typing: Saved on this device");
  check(await waitForStatus("Checkpointed to GitHub", 15_000), "a few seconds later: Checkpointed to GitHub");
  await scan("editor");

  // --- Terminal ---
  await press("Terminal");
  await typeInto("Command", "grep -c 'smoke test' app/main.py && git status");
  await page.keyboard.press("Enter");
  await settle(1500);
  const log = (await heard("log")).join("\n");
  check(/^1$/m.test(log), "the terminal sees the edit (grep -c printed 1)");
  check(log.includes("modified: app/main.py"), "git status lists the file as modified");
  await typeInto("Command", "python app/main.py");
  await press("Run");
  await settle(800);
  check((await heard("log")).join("\n").includes("not available"), "an unsupported program says it is not available");
  await scan("terminal");

  // --- Changes: diff, commit ---
  await press("Changes");
  check((await heard("heading")).includes("1 changed file"), "Changes lists 1 changed file");
  await press("Show diff of /app/main.py");
  await settle(800);
  check((await layer.locator('[data-key="diff-text"]').textContent()).includes("+# edited by the smoke test"), "the diff shows the added line");
  await scan("changes");
  await typeInto("Commit message", "Edit from the smoke test");
  await press("Commit to main");
  await settle(1500);
  check((await heard("status")).some((text) => text.startsWith("Committed 1 file to main")), "the commit is confirmed");
  check((await heard("heading")).includes("Nothing changed since the last commit."), "after the commit nothing is left to commit");

  // --- Recovery: an edit saved on the device survives a reload ---
  await press("Terminal");
  await typeInto("Command", "echo keep me > notes.txt");
  await press("Run");
  check(await waitForStatus("Saved on this device", 5_000), "a file written from the terminal is saved on the device");
  await openDemo();
  await press("Try the demo (no account)");
  await settle(1500);
  check((await heard("status")).some((text) => text.startsWith("Restored 1 unsaved change")), "after a reload the unsaved change is restored and announced");
  check((await button("notes.txt  (new)").count()) === 1, "the restored file is in the file list, marked new");

  // --- Settings ---
  await press("More");
  await press("Theme: Dark. Switch to Midnight");
  check((await button("Theme: Midnight. Switch to Dark").count()) === 1, "the theme switches to Midnight");
  await scan("more");

  check(errors.length === 0, `no errors in the browser console${errors.length ? `: ${errors.slice(0, 3).join(" | ")}` : ""}`);
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
