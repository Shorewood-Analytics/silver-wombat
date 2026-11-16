// Keeps a copy of the app on the device, so that it opens with no connection
// and so that the files of one version are never mixed with another's.
//
// `cargo xtask dist` fills in the two values below: a version worked out from
// the contents of every file of the site, and the files the app needs to start.
const VERSION = "__VERSION__";
const SHELL = __SHELL__;
const CACHE = `silver-wombat-${VERSION}`;

// "no-cache" makes the browser check each file with the server before using
// its own stored copy. A file the page has just downloaded is then not
// downloaded a second time, and a stale stored copy from an older build can
// never be filed under this version.
const FRESH = { cache: "no-cache" };

// Installing: fetch the files the app starts from. If any is missing the
// install fails and is tried again on the next visit.
self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(SHELL.map((path) => new Request(path, FRESH)))));
});

// A new version waits until no page is using the old one (there is no
// skipWaiting here), so a running app is never handed files from a newer
// build. Once it takes over, the older versions' files are deleted.
self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      for (const name of await caches.keys()) {
        if (name.startsWith("silver-wombat-") && name !== CACHE) await caches.delete(name);
      }
      await self.clients.claim();
    })(),
  );
});

// This site's own files come from the copy on the device. One not copied yet
// (a Python interpreter, the suggestion model's runtime) is fetched and kept.
// Requests to other sites (GitHub, the library and model hosts) are left alone.
self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET" || new URL(request.url).origin !== self.location.origin) return;
  event.respondWith(
    (async () => {
      const cache = await caches.open(CACHE);
      // Whatever address the app was opened with, the page is the same page.
      const kept = request.mode === "navigate" ? await cache.match(new URL("./", self.registration.scope).href) : await cache.match(request);
      if (kept) return kept;
      let reply;
      try {
        reply = await fetch(request, request.mode === "navigate" ? undefined : FRESH);
      } catch {
        // No connection: the page is told the request failed, as it would be
        // without this script.
        return Response.error();
      }
      if (reply.ok && reply.type === "basic") {
        // A full device must not stop the file being used.
        cache.put(request, reply.clone()).catch(() => {});
      }
      return reply;
    })(),
  );
});
