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

// The app draws with WebGPU where the browser has it, and with WebGL2 where
// it does not. If WebGPU is there but its device is lost while the app runs,
// drawing stops for good. So: watch the devices this page creates, and if one
// is lost, remember to use WebGL2 on this device and reload once. Unsaved
// work is on the device already and comes back after the reload. More > the
// drawing setting switches back.
const RENDERER = "silver-wombat.renderer";
if (globalThis.GPUAdapter && localStorage.getItem(RENDERER) !== "webgl") {
  const requestDevice = GPUAdapter.prototype.requestDevice;
  GPUAdapter.prototype.requestDevice = async function (...options) {
    const device = await requestDevice.apply(this, options);
    device.lost.then((lost) => {
      if (lost.reason === "destroyed" || crashed) return;
      localStorage.setItem(RENDERER, "webgl");
      if (sessionStorage.getItem(RENDERER) === null) {
        sessionStorage.setItem(RENDERER, "reloaded");
        location.reload();
      }
    });
    return device;
  };
}

try {
  const app = await import("./silver_wombat_app.js");
  await app.default();
  app.run();
  if (!status.textContent.startsWith("The editor stopped")) status.hidden = true;
} catch (error) {
  fail(error);
}
