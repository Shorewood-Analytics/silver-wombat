// Draws web/icon.svg as the PNG files a home screen asks for (web/icon-*.png).
// Run it again after changing the SVG:   node e2e/make_icons.mjs
import { readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";

const web = join(dirname(fileURLToPath(import.meta.url)), "..", "web");
const svg = await readFile(`${web}/icon.svg`, "utf8");
const browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/google-chrome" });
for (const size of [180, 192, 512]) {
  const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
  await page.setContent(`<style>html,body{margin:0}svg{display:block;width:${size}px;height:${size}px}</style>${svg}`);
  await writeFile(`${web}/icon-${size}.png`, await page.screenshot({ type: "png" }));
  await page.close();
}
await browser.close();
