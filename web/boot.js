// The only hand-written JavaScript in the app: load the WASM module and start it.
// Everything else is Rust. A failure is shown on the page, not only in the console.
const status = document.getElementById("status");

let crashed = false;

function fail(error) {
  console.error(error);
  // A Rust panic has already written its own, more useful message.
  if (crashed || status.textContent.startsWith("The editor stopped")) {
    crashed = true;
    status.hidden = false;
    return;
  }
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
  if (!status.textContent.startsWith("The editor stopped")) status.hidden = true;
} catch (error) {
  fail(error);
}
