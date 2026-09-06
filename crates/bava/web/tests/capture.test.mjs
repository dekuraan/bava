import { readFileSync } from "node:fs";
import vm from "node:vm";
import test from "node:test";
import assert from "node:assert/strict";

const source = readFileSync(new URL("../bava.js", import.meta.url), "utf8").split("const AUDIO_EXT")[0];
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
