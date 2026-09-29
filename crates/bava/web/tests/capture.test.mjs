import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
import assert from "node:assert/strict";

const script = readFileSync(new URL("../bava.js", import.meta.url), "utf8");
const source = script.split("const AUDIO_EXT")[0]
  + script.slice(script.indexOf("async function pushNowPlaying()"), script.indexOf("const PANEL ="));
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };

function setup() {
  const contexts = [], pickers = [], revoked = [], elements = [];
  let resets = 0;
  function element() {
    const el = { pauseCount: 0, cloneNode: element, replaceWith() {}, pause() { this.pauseCount++; }, play: async () => {}, focus() {} };
    elements.push(el);
    return el;
  }
  const node = () => ({ connect() {}, disconnect() {}, gain: {}, port: {} });
  class AudioContext {
    state = "running";
    sampleRate = 48000;
    destination = {};
    module = deferred();
    closed = false;
    audioWorklet = { addModule: () => this.module.promise };
    constructor() { contexts.push(this); }
    createGain = node;
    createMediaStreamSource = node;
    createMediaElementSource = node;
    async close() { this.closed = true; }
  }
  const sandbox = vm.createContext({
    navigator: { mediaDevices: { getDisplayMedia() { const picker = deferred(); pickers.push(picker); return picker.promise; } } },
    AudioContext, AudioWorkletNode: class { constructor() { Object.assign(this, node()); } },
    window: { AudioContext }, location: { search: "" },
    document: { getElementById: element }, URLSearchParams, Uint8Array,
    URL: { createObjectURL: (file) => `blob:${file.name}`, revokeObjectURL: (url) => revoked.push(url) },
    performance: { now: () => 0 }, setPanelOpen() {}, setTimeout() {}, addEventListener() {},
  });
  vm.runInContext(source + '\nwasm = { bava_reset_audio() { reset(); }, bava_set_now_playing() {}, bava_set_album_art() {} };', Object.assign(sandbox, { reset: () => resets++ }));
  return { contexts, pickers, revoked, elements, run: (code) => vm.runInContext(code, sandbox), resets: () => resets };
}
function stream(label) {
  const track = { label, stopped: false, stop() { this.stopped = true; }, addEventListener(_, cb) { this.ended = cb; } };
  return { track, getAudioTracks: () => [track], getVideoTracks: () => [], getTracks: () => [track] };
}

test("latest file wins when worklet loads finish in reverse order", async () => {
  const s = setup();
  const old = s.run('startFile({name:"old.mp3"})');
  await flush();
  const next = s.run('startFile({name:"new.mp3"})');
  await flush();
  s.contexts[1].module.resolve();
  assert.equal(await next, true);
  const liveElement = s.elements.at(-1);
  s.contexts[0].module.resolve();
  assert.equal(await old, false);
  assert.equal(s.contexts[0].closed, true);
  assert.equal(s.contexts[1].closed, false);
  assert.equal(liveElement.pauseCount, 0);
  assert.deepEqual(s.revoked, ["blob:old.mp3"]);
});

test("stop cancels a pending picker and releases its eventual stream", async () => {
  const s = setup();
  const pending = s.run('startCapture(true)');
  await s.run('stopCapture()');
  const shared = stream("old");
  s.pickers[0].resolve(shared);
  assert.equal(await pending, false);
  assert.equal(shared.track.stopped, true);
  assert.equal(s.contexts.length, 0);
});

test("out-of-order pickers cannot replace a newer capture", async () => {
  const s = setup();
  const old = s.run('startCapture(true)');
  const next = s.run('startCapture(false)');
  const newer = stream("new"), older = stream("old");
  s.pickers[1].resolve(newer);
  await flush();
  s.contexts[0].module.resolve();
  assert.equal(await next, true);
  s.pickers[0].resolve(older);
  assert.equal(await old, false);
  assert.equal(older.track.stopped, true);
  assert.equal(newer.track.stopped, false);
});

test("stopping resets audio before an asynchronous context close finishes", async () => {
  const s = setup();
  const start = s.run('startFile({name:"first.mp3"})');
  await flush();
  s.contexts[0].module.resolve();
  await start;
  const close = deferred();
  s.contexts[0].close = () => close.promise;
  const stopping = s.run('stopCapture()');
  assert.equal(s.resets(), 1);
  const next = s.run('startFile({name:"next.mp3"})');
  await flush();
  s.contexts[1].module.resolve();
  await next;
  close.resolve();
  await stopping;
  assert.equal(s.resets(), 1);
});

test("an old share's ended event cannot stop its replacement", async () => {
  const s = setup();
  const old = s.run('startCapture(true)');
  const shared = stream("old");
  s.pickers[0].resolve(shared);
  await flush();
  s.contexts[0].module.resolve();
  await old;
  const next = s.run('startFile({name:"new.mp3"})');
  await flush();
  s.contexts[1].module.resolve();
  await next;
  shared.track.ended();
  assert.equal(s.contexts[1].closed, false);
});

test("failed worklet loading releases the stream and context", async () => {
  const s = setup();
  const start = s.run('startCapture(true)');
  const shared = stream("broken");
  s.pickers[0].resolve(shared);
  await flush();
  s.contexts[0].module.reject(Error("network failure"));
  assert.equal(await start, false);
  assert.equal(shared.track.stopped, true);
  assert.equal(s.contexts[0].closed, true);
});

test("a file pauses YouTube and keeps its title after a late player event", async () => {
  const s = setup();
  s.run(`globalThis.pauses = 0; globalThis.titles = [];
    player = { pauseVideo() { pauses++; }, getVideoData() {
      return { video_id: "abcdefghijk", title: "Old YouTube song" };
    } };
    wasm.bava_set_now_playing = (title) => titles.push(title);`);
  const start = s.run('startFile({name:"local.mp3"})');
  await flush();
  s.contexts[0].module.resolve();
  assert.equal(await start, true);
  await s.run('pushNowPlaying()');
  assert.equal(s.run('pauses'), 1);
  assert.equal(s.run('titles.at(-1)'), "local");
});

test("a thumbnail finishing after switching to a file cannot replace its art", async () => {
  const s = setup();
  const sharing = s.run('startCapture(true)');
  s.pickers[0].resolve(stream("this tab"));
  await flush();
  s.contexts[0].module.resolve();
  await sharing;
  s.run(`globalThis.art = [];
    player = { pauseVideo() {}, getVideoData() { return { video_id: "abcdefghijk" }; } };
    wasm.bava_set_album_art = (bytes) => art.push(bytes.length);
    globalThis.fetch = () => new Promise(resolve => {
      globalThis.finishThumbnail = () => resolve({ ok: true, arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer });
    });`);
  const metadata = s.run('pushNowPlaying()');
  const start = s.run('startFile({name:"local.mp3"})');
  await flush();
  s.run('finishThumbnail()');
  await metadata;
  assert.equal(s.run('art.includes(3)'), false);
  s.contexts[1].module.resolve();
  assert.equal(await start, true);
});

test("switching back to this tab restores YouTube metadata", async () => {
  const s = setup();
  s.run(`globalThis.titles = [];
    player = { pauseVideo() {}, getVideoData() {
      return { video_id: "abcdefghijk", title: "YouTube song" };
    } };
    wasm.bava_set_now_playing = (title) => titles.push(title);`);
  const file = s.run('startFile({name:"local.mp3"})');
  await flush();
  s.contexts[0].module.resolve();
  await file;
  assert.equal(s.run('titles.at(-1)'), "local");
  const sharing = s.run('startCapture(true)');
  s.pickers[0].resolve(stream("this tab"));
  await flush();
  s.contexts[1].module.resolve();
  assert.equal(await sharing, true);
  assert.equal(s.run('titles.at(-1)'), "YouTube song");
});

test("another tab cannot inherit the embedded player's metadata", async () => {
  const s = setup();
  s.run(`globalThis.titles = [];
    player = { pauseVideo() {}, getVideoData() {
      return { video_id: "abcdefghijk", title: "Wrong tab" };
    } };
    wasm.bava_set_now_playing = (title) => titles.push(title);`);
  const sharing = s.run('startCapture(false)');
  s.pickers[0].resolve(stream("other tab"));
  await flush();
  s.contexts[0].module.resolve();
  assert.equal(await sharing, true);
  await s.run('pushNowPlaying()');
  assert.equal(s.run('titles.at(-1)'), undefined);
});
