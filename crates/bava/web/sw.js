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
//    bytes under the old one. Cache-first, with at most two builds retained per asset.
//  - **Everything else** (the HTML entry points, this file, the hand-written
//    JS trunk copies verbatim) can change in place. Network-first with a cache
//    fallback, so a deploy is picked up on the next load but a dead network
//    still boots the app.
//
// Cross-origin requests — the YouTube IFrame API, i.ytimg.com thumbnails — are
// left entirely alone: they are opaque to us, they change, and caching them
// would only risk serving a stale player.

const PREFIX = `bava:${self.registration.scope}:`;
const CACHE = `${PREFIX}v2`;
let writes = Promise.resolve();

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
        if (name.startsWith(PREFIX) && name !== CACHE) await caches.delete(name);
      }
      // The previous worker used one origin-wide Bava cache. Remove only
      // entries under this installation; another Bava scope may still use it.
      if ((await caches.keys()).includes("bava-v1")) {
        const legacy = await caches.open("bava-v1");
        for (const key of await legacy.keys()) {
          if (key.url.startsWith(self.registration.scope)) await legacy.delete(key);
        }
        if ((await legacy.keys()).length === 0) await caches.delete("bava-v1");
      }
      await self.clients.claim();
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  if (request.method !== "GET") return;

  const url = new URL(request.url);
  if (!url.href.startsWith(self.registration.scope)) return;

  event.respondWith(
    IMMUTABLE.test(url.pathname) ? cacheFirst(request) : networkFirst(request),
  );
});

async function cacheFirst(request) {
  const hit = await cached(request);
  if (hit) return hit;
  const response = await fetch(request);
  if (response.ok) {
    await remember(request, response.clone());
  }
  return response;
}

async function networkFirst(request) {
  try {
    const response = await fetch(request);
    if (response.ok) {
      await remember(request, response.clone());
    }
    return response;
  } catch (err) {
    const hit = await cached(request);
    if (hit) return hit;
    throw err;
  }
}

// Serialize insertion and eviction so simultaneous fetches obey the same limit.
function remember(request, response) {
  writes = writes.then(async () => {
    const cache = await caches.open(CACHE);
    await cache.put(request, response);
    const keys = await cache.keys();
    const family = (url) => new URL(url).pathname.replace(/-[0-9a-f]{8,}(?=(_bg)?\.)/, "");
    const siblings = keys.filter((key) => family(key.url) === family(request.url));
    for (const key of siblings.slice(0, -2)) await cache.delete(key);
    // Also bound snippets and stable URLs that have no fingerprint family.
    const remaining = await cache.keys();
    for (const key of remaining.slice(0, -64)) await cache.delete(key);
  }).catch(() => {
    // Storage denial or quota exhaustion must not fail a successful download.
  });
  return writes;
}

async function cached(request) {
  try {
    return await (await caches.open(CACHE)).match(request);
  } catch {
    return undefined;
  }
}
