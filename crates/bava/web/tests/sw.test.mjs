import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
import assert from "node:assert/strict";

const source = readFileSync(new URL("../sw.js", import.meta.url), "utf8");
const scope = "https://example.com/bava/";
const cacheName = `bava:${scope}:v2`;
function setup() {
  const stores = new Map(), handlers = {};
  const caches = {
    async keys() { return [...stores.keys()]; },
    async delete(name) { return stores.delete(name); },
    async open(name) {
      if (!stores.has(name)) stores.set(name, new Map());
      const store = stores.get(name);
      return {
        async put(request, response) { store.set(request.url, response); },
        async match(request) { return store.get(request.url)?.clone(); },
        async keys() { return [...store.keys()].map((url) => new Request(url)); },
        async delete(request) { return store.delete(request.url); },
      };
    },
  };
  let offline = false, downloads = 0;
  vm.runInNewContext(source, {
    self: { registration: { scope }, clients: { async claim() {} }, addEventListener: (name, fn) => { handlers[name] = fn; } },
    caches, URL,
    fetch: async () => { downloads++; if (offline) throw Error("offline"); return new Response("app"); },
  });
  return { stores, caches, handlers, offline: () => { offline = true; }, downloads: () => downloads,
    request(path) {
      let response;
      handlers.fetch({ request: new Request(new URL(path, scope)), respondWith(value) { response = value; } });
      return response;
    },
  };
}

test("activation preserves other apps and removes only obsolete scoped caches", async () => {
  const s = setup();
  const other = "another-app-v1", otherBava = "bava:https://example.com/demo/:v1";
  for (const name of [other, otherBava, `bava:${scope}:v1`, cacheName]) await s.caches.open(name);
  let completion;
  s.handlers.activate({ waitUntil(value) { completion = value; } });
  await completion;
  assert.deepEqual([...s.stores.keys()], [other, otherBava, cacheName]);
  assert.equal(s.request("https://example.com/other/app.js"), undefined);
});

test("concurrent downloads retain only two hashed builds per asset", async () => {
  const s = setup();
  await Promise.all(["11111111", "22222222", "33333333"].map((hash) => s.request(`bava-${hash}_bg.wasm`)));
  const keys = [...s.stores.get(cacheName).keys()];
  assert.equal(keys.length, 2);
  assert.ok(keys.every((url) => !url.includes("11111111")));
  const downloads = s.downloads();
  await s.request("bava-33333333_bg.wasm");
  assert.equal(s.downloads(), downloads);
});

test("offline fallback uses this app's cache only", async () => {
  const s = setup();
  await s.request("index.html");
  const other = await s.caches.open("another-app");
  await other.put(new Request(`${scope}missing.html`), new Response("wrong app"));
  s.offline();
  assert.equal(await (await s.request("index.html")).text(), "app");
  await assert.rejects(s.request("missing.html"), /offline/);
});

test("cache growth is bounded for snippets and stable URLs too", async () => {
  const s = setup();
  for (let i = 0; i < 70; i++) await s.request(`snippets/build-${i}/inline.js`);
  assert.equal(s.stores.get(cacheName).size, 64);
});

test("legacy migration removes only this installation's entries", async () => {
  const s = setup();
  const legacy = await s.caches.open("bava-v1");
  await legacy.put(new Request(`${scope}old.wasm`), new Response("old"));
  await legacy.put(new Request("https://example.com/demo/old.wasm"), new Response("other"));
  let completion;
  s.handlers.activate({ waitUntil(value) { completion = value; } });
  await completion;
  assert.deepEqual([...s.stores.get("bava-v1").keys()], ["https://example.com/demo/old.wasm"]);
});

test("denied cache storage does not block successful network responses", async () => {
  const s = setup();
  s.caches.open = async () => { throw Error("denied"); };
  assert.equal(await (await s.request("bava-11111111_bg.wasm")).text(), "app");
  assert.equal(await (await s.request("index.html")).text(), "app");
});
