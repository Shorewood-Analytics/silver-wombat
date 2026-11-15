// The only hand-written JavaScript in the app: load the WASM module and start it.
// Everything else is Rust. A failure is shown on the page, not only in the console.
const status = document.getElementById("status");

function fail(error) {
  console.error(error);
  status.hidden = false;
  status.textContent =
    "The editor could not start.\n\n" +
    (error && error.message ? error.message : String(error)) +
    "\n\nReload the page to try again.";
}

window.addEventListener("error", (event) => fail(event.error ?? event.message));
window.addEventListener("unhandledrejection", (event) => fail(event.reason));

try {
  const app = await import("./silver_wombat_app.js");
  await app.default();
  app.run();
  status.hidden = true;
} catch (error) {
  fail(error);
}
