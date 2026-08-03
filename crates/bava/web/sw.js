// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Cache the app shell so repeat visits are instant and the whole thing works
// offline. The prize is the wasm blob: tens of megabytes, and re-downloading it
// on every visit is most of what "the web build feels slow" means.
//
// Two strategies, split on what can change under a stable URL:
//
//  - **Hashed build output** (`*.wasm`, `*-<hash>.js`, the CSS trunk emits) is
//    immutable by construction — a rebuild produces a *new name*, never new
//    bytes under the old one. Cache-first, kept forever, never revalidated.
//  - **Everything else** (the HTML entry points, this file, the hand-written
//    JS trunk copies verbatim) can change in place. Network-first with a cache
//    fallback, so a deploy is picked up on the next load but a dead network
//    still boots the app.
//
// Cross-origin requests — the YouTube IFrame API, i.ytimg.com thumbnails — are
// left entirely alone: they are opaque to us, they change, and caching them
// would only risk serving a stale player.

const CACHE = "bava-v1";

// Trunk's fingerprinted output, and the snippets/ directory it emits for
// wasm-bindgen's inline JS.
//
// The `_bg` is load-bearing: wasm-bindgen names the module `bava-<hash>_bg.wasm`
// — the hash is *not* the last thing before the extension. Requiring it to be
// dropped the 100+ MB wasm out of this branch and into network-first, which is
// the one file the whole cache exists for. Verified against real dist output:
//   bava-76843c393f5a7095_bg.wasm   ✓
//   bava-76843c393f5a7095.js        ✓
//   style-f986d5765087296b.css      ✓
//   bava.js / audio-worklet.js      ✗  (hand-written, must stay revalidated)
const IMMUTABLE = /-[0-9a-f]{8,}(_bg)?\.(js|wasm|css)$|\/snippets\//;

self.addEventListener("install", (event) => {
  // Take over as soon as the new worker is ready rather than waiting for every
  // tab to close; there is no cross-version state to corrupt here.
  event.waitUntil(self.skipWaiting());
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      for (const name of await caches.keys()) {
        if (name !== CACHE) await caches.delete(name);
      }
      await self.clients.claim();
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  if (request.method !== "GET") return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return; // YouTube, ytimg: not ours

  event.respondWith(
    IMMUTABLE.test(url.pathname) ? cacheFirst(request) : networkFirst(request),
  );
});

async function cacheFirst(request) {
  const hit = await caches.match(request);
  if (hit) return hit;
  const response = await fetch(request);
  if (response.ok) {
    const cache = await caches.open(CACHE);
    cache.put(request, response.clone());
  }
  return response;
}

async function networkFirst(request) {
  try {
    const response = await fetch(request);
    if (response.ok) {
      const cache = await caches.open(CACHE);
      cache.put(request, response.clone());
    }
    return response;
  } catch (err) {
    const hit = await caches.match(request);
    if (hit) return hit;
    throw err;
  }
}
