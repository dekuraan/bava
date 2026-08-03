// SPDX-License-Identifier: MIT OR Apache-2.0
//
// The page half of the web build: it gets audio into the wasm module and
// metadata out of whatever is playing.
//
// There are two ways in, because browsers have no loopback device:
//
//  1. **Tab share.** `getDisplayMedia({ audio: true })` over a tab surface
//     yields a `MediaStreamTrack` carrying that tab's output — including our
//     own tab, which is what makes the embedded YouTube player work. This is
//     the good path, and it is Chromium-only in practice: Firefox and every
//     mobile browser either omit `getDisplayMedia` or refuse to carry audio.
//  2. **A local file.** An `<audio>` element through
//     `createMediaElementSource` needs no permission prompt at all, works on
//     mobile, and is where the capability check routes when tab audio is
//     unavailable. `local.html` is a direct entry point to it.
//
// Both end in the same place: an `AudioWorklet` (`audio-worklet.js`) pushing
// interleaved blocks into `bava_push_audio`, the wasm export that stands in for
// a capture backend.
//
// Metadata: a page cannot read across an `<iframe>` origin boundary, so the
// YouTube IFrame Player API is the only channel for the title, and the cover is
// the video's thumbnail fetched separately. File sources use the file name.

// --- capability detection ---------------------------------------------------
//
// Decided once, up front, so the UI can present only what will actually work
// rather than letting someone click through a picker to reach a dead end.

/** Tab-audio capture. Chromium-only in practice; absent on all mobile. */
const CAN_CAPTURE = typeof navigator.mediaDevices?.getDisplayMedia === "function";
/** The worklet is how audio reaches wasm; without it neither source works. */
const CAN_WORKLET =
  typeof AudioWorkletNode === "function" &&
  typeof (window.AudioContext || window.webkitAudioContext) === "function";

/** Bevy needs WebGL2. Only called on the slow-boot path — a context isn't free. */
function hasWebGL2() {
  try {
    return !!document.createElement("canvas").getContext("webgl2");
  } catch {
    return false;
  }
}

const PARAMS = new URLSearchParams(location.search);
/** `?source=file` (what `local.html` redirects to) forces the no-prompt path. */
const FILE_ONLY = PARAMS.get("source") === "file" || !CAN_CAPTURE;

const CAPTURE_STATUS = document.getElementById("capture-status");
const BTN_START = document.getElementById("start");
const BTN_OTHER_TAB = document.getElementById("capture-other");
const BTN_FILE = document.getElementById("pick-file");
const BTN_STOP = document.getElementById("capture-stop");
const BTN_RETRY = document.getElementById("capture-retry");
const FILE_INPUT = document.getElementById("file-input");
const FILE_SECTION = document.getElementById("file-section");
const START_SUB = document.getElementById("start-sub");
const YT_SECTION = document.getElementById("yt-section");
const STAGE = document.getElementById("stage");

/** Resolved once trunk's loader has instantiated the wasm module. */
let wasm = null;
/** The live source: `{ stop() }`, or null. */
let capture = null;
/** The YT.Player instance, once the IFrame API has finished loading. */
let player = null;
/** Video id whose metadata we last pushed, so we only push on real changes. */
let lastVideoId = null;
/** How to repeat the last start, for the Retry button. Null if unrepeatable. */
let lastIntent = null;
/** The <audio> element for file playback, rebuilt per file (see startFile). */
let audioEl = document.getElementById("file-audio");

// --- wasm handshake ---------------------------------------------------------

// Trunk's generated loader publishes the wasm-bindgen exports on
// `window.wasmBindings` and fires this event once `main()` has run.
const wasmReady = new Promise((resolve) => {
  if (window.wasmBindings) {
    resolve(window.wasmBindings);
    return;
  }
  addEventListener(
    "TrunkApplicationStarted",
    () => resolve(window.wasmBindings),
    { once: true },
  );
});

wasmReady.then((bindings) => {
  wasm = bindings;
  document.getElementById("boot")?.remove();
});

// The wasm is tens of megabytes, so a slow connection legitimately sits on the
// splash for a while — but a WebGL2-less browser or a failed fetch would sit
// there forever with nothing but "loading". Say something useful instead.
setTimeout(() => {
  const boot = document.getElementById("boot");
  if (!boot) return;
  boot.querySelector(".loading").textContent = hasWebGL2()
    ? "Still loading — a few tens of MB have to arrive before the visualizer " +
      "starts. Check the browser console if this doesn't clear."
    : "This browser has no WebGL2, which the visualizer requires. Try a " +
      "current Chrome, Edge, Firefox or Safari.";
}, 20000);

// --- status -----------------------------------------------------------------

function setStatus(text, kind = "", { retry = false } = {}) {
  CAPTURE_STATUS.textContent = text;
  CAPTURE_STATUS.className = `status ${kind}`;
  if (BTN_RETRY) BTN_RETRY.hidden = !retry || !lastIntent;
}

// --- the shared tail of both sources ----------------------------------------
//
// Everything from "an AudioContext with a source node" onwards is identical for
// a tab share and a file, so it lives here once.

/**
 * Wire `sourceNode` into the capture worklet and start pushing to wasm.
 *
 * @param {AudioContext} context
 * @param {AudioNode} sourceNode
 * @param {object} opts
 * @param {boolean} opts.audible Whether the source should also reach the
 *   speakers. A tab share must *not* — the tab is already playing it, and
 *   re-emitting would double it. A file must, or nothing is heard.
 * @param {() => void} opts.onStop Extra teardown for the owning source.
 * @param {string} opts.label Shown in the status line.
 */
async function attachWorklet(context, sourceNode, { audible, onStop, label }) {
  try {
    await context.audioWorklet.addModule("./audio-worklet.js");
  } catch (err) {
    onStop?.();
    await context.close();
    setStatus(`Could not load the audio worklet: ${err}`, "error", { retry: true });
    return false;
  }

  const worklet = new AudioWorkletNode(context, "bava-capture");

  // A silent source is the most confusing failure here: everything reports
  // success, the bars sit at zero, and it reads as a broken build. Watch the
  // opening seconds and name the likely cause instead.
  let peak = 0;
  let settled = false;
  const deadline = performance.now() + 3500;

  worklet.port.onmessage = (event) => {
    const { samples, channels, rate } = event.data;
    wasm?.bava_push_audio(samples, channels, rate);

    if (settled) return;
    // Sparse scan: enough to notice signal, cheap enough for every block.
    for (let i = 0; i < samples.length; i += 32) {
      const v = Math.abs(samples[i]);
      if (v > peak) peak = v;
    }
    if (peak > 1e-4) {
      settled = true;
    } else if (performance.now() > deadline) {
      settled = true;
      setStatus(
        audible
          ? "That file decoded, but it is silent."
          : "Capturing, but that tab is silent — is it muted, or was " +
              "“Also share tab audio” left off?",
        "error",
        { retry: true },
      );
    }
  };

  // An AudioWorkletNode is only pulled if it lies on a path to the context's
  // destination — a dangling node never runs. Route it through a zero gain so
  // it is scheduled without being heard; anything that *should* be audible gets
  // its own direct connection instead.
  const mute = context.createGain();
  mute.gain.value = 0;
  sourceNode.connect(worklet);
  worklet.connect(mute);
  mute.connect(context.destination);
  if (audible) sourceNode.connect(context.destination);

  capture = {
    async stop() {
      worklet.port.onmessage = null;
      try {
        sourceNode.disconnect();
      } catch {
        // Already torn down with its context; nothing to undo.
      }
      worklet.disconnect();
      mute.disconnect();
      onStop?.();
      await context.close();
      // Drop whatever was buffered, so restarting doesn't replay the old tail.
      wasm?.bava_reset_audio();
    },
  };

  BTN_STOP.hidden = false;
  setStatus(`${label} at ${context.sampleRate} Hz.`, "ok");
  // The panel has done its job; the visualizer is the point.
  setPanelOpen(false);
  document.getElementById("bava-canvas")?.focus();
  return true;
}

async function stopCapture() {
  const current = capture;
  capture = null;
  BTN_STOP.hidden = true;
  await current?.stop();
}

// --- source 1: tab share ----------------------------------------------------

/**
 * Start capturing a tab's audio.
 *
 * @param {boolean} currentTab Ask the browser to offer *this* tab, which is the
 *   path for the embedded player. Otherwise the full picker is shown and this
 *   tab is excluded from it, so the obvious choice is some other tab.
 */
async function startCapture(currentTab) {
  lastIntent = () => startCapture(currentTab);
  if (!CAN_CAPTURE) {
    setStatus(
      "This browser can't capture tab audio. Play an audio file instead.",
      "error",
    );
    return false;
  }
  // Deliberately *not* `await stopCapture()` here. Tearing the old source down
  // awaits `AudioContext.close()`, which is real async work, and transient
  // activation does not survive it — so re-entering while a source is already
  // live (paste-anywhere, a second "Start visualizing", Retry) would reach
  // `getDisplayMedia` gestureless and fail with `NotAllowedError`, reported as
  // a cancelled picker. The old source keeps playing behind the picker and is
  // stopped once a new stream is actually granted; if the user dismisses the
  // picker, it keeps running, which is what "cancel" should mean anyway.

  let stream;
  try {
    // Must be the first await after the user gesture: transient activation is
    // consumed by awaiting, and a gestureless getDisplayMedia is rejected.
    stream = await navigator.mediaDevices.getDisplayMedia({
      // Chrome refuses audio-only display capture, so a video track has to be
      // requested even though nothing here draws it. Ask for the cheapest one
      // the browser will give us — it is decoded and thrown away.
      video: { frameRate: { max: 5 } },
      audio: {
        // All three are on by default for capture streams, and all three would
        // wreck the analysis: AGC continuously renormalises level (so quiet
        // passages pump up and the bars stop tracking dynamics), and the
        // speech-tuned denoiser/echo canceller chew holes in music.
        autoGainControl: false,
        echoCancellation: false,
        noiseSuppression: false,
      },
      preferCurrentTab: currentTab,
      selfBrowserSurface: currentTab ? "include" : "exclude",
      // Let the user re-target the share without going through the picker again.
      surfaceSwitching: "include",
      systemAudio: "include",
    });
  } catch (err) {
    // A user dismissing the picker is not an error worth shouting about.
    const dismissed = err?.name === "NotAllowedError";
    setStatus(
      dismissed ? "Capture cancelled." : `Capture failed: ${err}`,
      dismissed ? "" : "error",
      { retry: !dismissed },
    );
    return false;
  }

  const [audioTrack] = stream.getAudioTracks();
  if (!audioTrack) {
    stream.getTracks().forEach((t) => t.stop());
    setStatus(
      "That share had no audio. Pick a tab and tick “Also share tab audio”.",
      "error",
      { retry: true },
    );
    return false;
  }

  // Nothing draws the video track, but the browser still captures and encodes
  // it. Disabling blanks it at the source; *stopping* it would end the share.
  for (const track of stream.getVideoTracks()) track.enabled = false;

  // The gesture has done its job — now it is safe to tear down whatever was
  // playing before, ahead of building this stream's graph.
  await stopCapture();

  const context = new AudioContext();
  // Chrome starts an AudioContext suspended unless it can attribute it to a
  // gesture; the picker interaction counts, but resume() is cheap insurance.
  if (context.state === "suspended") await context.resume();

  // Ending the share from the browser's own "Stop sharing" bar fires this.
  audioTrack.addEventListener("ended", () => {
    stopCapture();
    setStatus("Sharing ended.");
  });

  return attachWorklet(context, context.createMediaStreamSource(stream), {
    audible: false,
    onStop: () => stream.getTracks().forEach((t) => t.stop()),
    label: `Capturing ${audioTrack.label || "tab audio"}`,
  });
}

// --- source 2: a local file -------------------------------------------------

/**
 * Play `file` through the visualizer. No permission prompt, and unlike tab
 * capture this works on phones.
 */
async function startFile(file) {
  await stopCapture();

  // `createMediaElementSource` binds an element to one AudioContext for the
  // element's lifetime, and we close the context on every stop. Replacing the
  // element keeps each file's graph independent instead of resurrecting a dead
  // context on the second file.
  const fresh = audioEl.cloneNode(false);
  audioEl.replaceWith(fresh);
  audioEl = fresh;

  const url = URL.createObjectURL(file);
  // CodeQL's js/xss-through-dom treats `file` (from an <input type=file>) as
  // tainted DOM text and `createObjectURL` as taint-preserving, so it reads
  // this as user text reaching a URL sink. `createObjectURL` only ever returns
  // a same-origin `blob:` URL — none of the file's own bytes or name survive
  // into it — and `<audio>.src` doesn't interpret HTML regardless. Code
  // scanning does not honour inline `codeql[...]` suppression comments, so the
  // alert is dismissed in the security tab instead; this comment is the record
  // of why, for whoever sees it re-raised if the line ever moves.
  audioEl.src = url;
  audioEl.loop = true;
  if (FILE_SECTION) FILE_SECTION.hidden = false;
  // Re-picking the same File isn't possible programmatically, but re-opening
  // the picker is the right retry for a file source.
  lastIntent = () => FILE_INPUT.click();

  const context = new AudioContext();
  if (context.state === "suspended") await context.resume();

  let source;
  try {
    source = context.createMediaElementSource(audioEl);
  } catch (err) {
    await context.close();
    URL.revokeObjectURL(url);
    setStatus(`Could not read that file: ${err}`, "error", { retry: true });
    return false;
  }

  // Title from the file name: there is no tag reader on this side, since the
  // desktop build's symphonia path isn't compiled for wasm.
  const name = file.name.replace(/\.[^.]+$/, "");
  wasm?.bava_set_now_playing(name || undefined, undefined, undefined);
  wasm?.bava_set_album_art(new Uint8Array());

  const ok = await attachWorklet(context, source, {
    audible: true,
    onStop: () => {
      audioEl.pause();
      URL.revokeObjectURL(url);
    },
    label: `Playing ${file.name}`,
  });

  if (ok) {
    try {
      await audioEl.play();
    } catch {
      // Autoplay refused — the element has controls, so say so rather than
      // leaving a silent visualizer.
      setStatus(`Loaded ${file.name} — press play below.`, "ok");
    }
  }
  return ok;
}

const AUDIO_EXT = /\.(mp3|flac|ogg|oga|wav|m4a|aac|opus|weba|webm)$/i;
const isAudioFile = (file) =>
  file.type.startsWith("audio/") || AUDIO_EXT.test(file.name);

// --- the one button ---------------------------------------------------------
//
// The old flow was two numbered steps (load a video, then capture), which made
// the user work out the ordering — and getting it wrong played the intro
// unvisualized. One action now does both, in the right order.

async function startEverything() {
  if (!CAN_WORKLET) return;
  if (FILE_ONLY) {
    FILE_INPUT.click();
    return;
  }
  const field = document.getElementById("yt-url");
  const typed = field?.value?.trim();
  if (typed) {
    const id = parseVideoId(typed);
    if (!id) {
      setStatus("That doesn't look like a YouTube URL or video id.", "error");
      return;
    }
    // Cue, don't play: the picker is about to open, and anything playing behind
    // it is audible but unvisualized.
    player?.cueVideoById(id);
    field.blur();
  }
  // startCapture must be reached without an intervening await, or the gesture
  // is spent — cueVideoById above is synchronous, which is why it can precede.
  const ok = await startCapture(true);
  if (ok) player?.playVideo();
}

// --- wiring -----------------------------------------------------------------

// Blur after clicking: a focused button swallows Space and Enter (re-firing
// itself instead of cycling the visualizer mode), and the canvas is where the
// keyboard should go once something is running.
function onClick(button, handler) {
  button?.addEventListener("click", () => {
    button.blur();
    handler();
  });
}

onClick(BTN_START, startEverything);
onClick(BTN_OTHER_TAB, () => startCapture(false));
onClick(BTN_FILE, () => FILE_INPUT.click());
onClick(BTN_RETRY, () => lastIntent?.());
onClick(BTN_STOP, async () => {
  await stopCapture();
  setStatus("Not capturing.");
});

FILE_INPUT?.addEventListener("change", () => {
  const [file] = FILE_INPUT.files ?? [];
  if (file) startFile(file);
});

// Drop an audio file anywhere on the page.
addEventListener("dragover", (event) => {
  if (event.dataTransfer?.types?.includes("Files")) event.preventDefault();
});
addEventListener("drop", (event) => {
  const [file] = event.dataTransfer?.files ?? [];
  if (!file) return;
  event.preventDefault();
  if (isAudioFile(file)) startFile(file);
  else setStatus(`${file.name} isn't an audio file.`, "error");
});

// Paste a YouTube URL anywhere on the page and it just goes.
addEventListener("paste", (event) => {
  if (FILE_ONLY || event.target instanceof HTMLInputElement) return;
  const text = event.clipboardData?.getData("text");
  const id = text && parseVideoId(text);
  if (!id) return;
  event.preventDefault();
  const field = document.getElementById("yt-url");
  if (field) field.value = text.trim();
  startEverything();
});

// Clicking the big dark rectangle is what people try first. Before anything is
// running that starts the show; afterwards the canvas owns its clicks, which
// spawn physics balls.
STAGE?.addEventListener("pointerdown", (event) => {
  if (capture || event.button !== 0) return;
  startEverything();
});

// --- embedded player --------------------------------------------------------

/** Pull a video id out of anything a user is likely to paste. */
function parseVideoId(input) {
  const text = input.trim();
  if (!text) return null;
  // A bare id: 11 chars of the YouTube alphabet.
  if (/^[\w-]{11}$/.test(text)) return text;
  try {
    const url = new URL(text.includes("//") ? text : `https://${text}`);
    if (url.hostname === "youtu.be") {
      return url.pathname.slice(1).split("/")[0] || null;
    }
    if (url.searchParams.has("v")) return url.searchParams.get("v");
    // /embed/<id>, /shorts/<id>, /live/<id>
    const match = url.pathname.match(/\/(?:embed|shorts|live|v)\/([\w-]+)/);
    if (match) return match[1];
  } catch {
    // Not a URL; fall through.
  }
  return null;
}

// Enter in the URL field runs the whole flow, not just "load the video" — a
// paste-and-Enter user shouldn't then have to hunt for a second button.
document.getElementById("yt-form")?.addEventListener("submit", (event) => {
  event.preventDefault();
  startEverything();
});

if (!FILE_ONLY) {
  // The IFrame Player API calls this global once it has loaded.
  window.onYouTubeIframeAPIReady = () => {
    player = new YT.Player("yt-player", {
      // A regular upload, deliberately not one of the 24/7 lofi *live* streams:
      // when those go offline YouTube refuses to embed the archive and the
      // player shows "this live stream recording is not available" instead.
      // Override per-visit with `?video=<id>`.
      videoId: PARAMS.get("video") || "EAxr8tqvw1A",
      playerVars: { playsinline: 1, modestbranding: 1 },
      events: {
        onStateChange: pushNowPlaying,
        onReady: pushNowPlaying,
      },
    });
  };

  const ytApi = document.createElement("script");
  ytApi.src = "https://www.youtube.com/iframe_api";
  document.head.append(ytApi);
}

/**
 * Push the embedded video's title and thumbnail into the visualizer, which
 * shows them in the HUD and — with dynamic colors on — derives its palette from
 * the cover.
 */
async function pushNowPlaying() {
  if (!wasm || !player?.getVideoData) return;
  const data = player.getVideoData();
  const videoId = data?.video_id;
  if (!videoId || videoId === lastVideoId) return;
  lastVideoId = videoId;

  wasm.bava_set_now_playing(data.title || undefined, data.author || undefined, undefined);

  // maxres exists only for videos uploaded with a big enough thumbnail; hq is
  // always generated. Both are served with `access-control-allow-origin: *`,
  // which is what lets us read the bytes rather than just display them.
  for (const name of ["maxresdefault", "hqdefault"]) {
    try {
      const response = await fetch(
        `https://i.ytimg.com/vi/${videoId}/${name}.jpg`,
        { mode: "cors" },
      );
      if (!response.ok) continue;
      const bytes = new Uint8Array(await response.arrayBuffer());
      // Bail if another video was loaded while this fetch was in flight.
      if (lastVideoId !== videoId) return;
      wasm.bava_set_album_art(bytes);
      return;
    } catch {
      // CORS or network; try the next size, then give up quietly — the
      // visualizer just keeps its configured colors.
    }
  }
  wasm.bava_set_album_art(new Uint8Array());
}

// --- panel ------------------------------------------------------------------

const PANEL = document.getElementById("panel");
const BTN_SHOW = document.getElementById("panel-show");

function setPanelOpen(open) {
  PANEL.classList.toggle("open", open);
  BTN_SHOW.hidden = open;
}

document
  .getElementById("panel-toggle")
  .addEventListener("click", () => setPanelOpen(false));
BTN_SHOW.addEventListener("click", () => setPanelOpen(true));

addEventListener("keydown", (event) => {
  // Not while typing in the URL field, and not when the visualizer's own
  // egui editor has the keyboard.
  const typing = event.target instanceof HTMLInputElement;
  if (event.key === "h" && !typing && !event.metaKey && !event.ctrlKey) {
    setPanelOpen(!PANEL.classList.contains("open"));
  }
});

// --- capability-driven UI ---------------------------------------------------
//
// Never offer a control that leads somewhere this browser can't go.

if (!CAN_WORKLET) {
  setStatus(
    "This browser has no AudioWorklet, which is how audio reaches the " +
      "visualizer. Try a current Chrome, Edge, Firefox or Safari.",
    "error",
  );
  BTN_START.disabled = true;
  BTN_OTHER_TAB.hidden = true;
  BTN_FILE.hidden = true;
} else if (FILE_ONLY) {
  // No tab capture here: present the file path as the whole story rather than
  // offering buttons that open a picker which cannot deliver audio.
  BTN_START.textContent = "Choose an audio file";
  START_SUB.textContent =
    "Plays a file from this device — no permission prompt, and it works " +
    "on phones. You can also drop a file anywhere on this page.";
  BTN_OTHER_TAB.hidden = true;
  BTN_FILE.hidden = true;
  if (YT_SECTION) YT_SECTION.hidden = true;
  if (!CAN_CAPTURE) {
    setStatus("This browser can't capture tab audio — play a file instead.");
  }
}

// --- service worker ---------------------------------------------------------
//
// The wasm blob is tens of megabytes and trunk content-hashes its filename, so
// it is safe to cache indefinitely: a rebuild produces a new name rather than
// new bytes under the old one. That makes every visit after the first instant,
// and the whole app usable offline.
//
// Skipped on localhost, where a stale cache during `trunk serve` is pure
// confusion.
const LOCAL_HOST = ["localhost", "127.0.0.1", "[::1]", ""].includes(location.hostname);
if ("serviceWorker" in navigator && !LOCAL_HOST) {
  addEventListener("load", () => {
    navigator.serviceWorker.register("./sw.js").catch(() => {
      // A failed registration costs only the cache; never let it block boot.
    });
  });
}
