// SPDX-License-Identifier: MIT OR Apache-2.0
//
// The page half of the web build: it gets audio into the wasm module and
// metadata out of the embedded player.
//
// Audio: browsers have no loopback device, so the only way to hear what another
// page is playing is to *screen-share* it. `getDisplayMedia({ audio: true })`
// over a tab surface yields a `MediaStreamTrack` carrying that tab's output —
// including our own tab, which is what makes the embedded YouTube player work.
// That track goes through an `AudioWorklet` (`audio-worklet.js`) and lands in
// `bava_push_audio`, the wasm export that stands in for a capture backend.
//
// Metadata: a page cannot read across an `<iframe>` origin boundary, so the
// YouTube IFrame Player API is the only channel for the title, and the cover is
// the video's thumbnail fetched separately.

const CAPTURE_STATUS = document.getElementById("capture-status");
const BTN_THIS_TAB = document.getElementById("capture-this");
const BTN_OTHER_TAB = document.getElementById("capture-other");
const BTN_STOP = document.getElementById("capture-stop");

/** Resolved once trunk's loader has instantiated the wasm module. */
let wasm = null;
/** The live capture: `{ stream, context, worklet, source }`, or null. */
let capture = null;
/** The YT.Player instance, once the IFrame API has finished loading. */
let player = null;
/** Video id whose metadata we last pushed, so we only push on real changes. */
let lastVideoId = null;

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
  boot.querySelector(".loading").textContent =
    "Still loading. If this doesn't clear, check the browser console — " +
    "the visualizer needs WebGL2 and a few tens of MB of download.";
}, 20000);

// --- audio capture ----------------------------------------------------------

function setStatus(text, kind = "") {
  CAPTURE_STATUS.textContent = text;
  CAPTURE_STATUS.className = `status ${kind}`;
}

/**
 * Start capturing a tab's audio.
 *
 * @param {boolean} currentTab Ask the browser to offer *this* tab, which is the
 *   path for the embedded player. Otherwise the full picker is shown and this
 *   tab is excluded from it, so the obvious choice is some other tab.
 */
async function startCapture(currentTab) {
  if (!navigator.mediaDevices?.getDisplayMedia) {
    setStatus("This browser has no getDisplayMedia — try Chrome.", "error");
    return;
  }
  await stopCapture();

  let stream;
  try {
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
    );
    return;
  }

  const [audioTrack] = stream.getAudioTracks();
  if (!audioTrack) {
    stream.getTracks().forEach((t) => t.stop());
    setStatus(
      "That share had no audio. Pick a tab and tick “Also share tab audio”.",
      "error",
    );
    return;
  }

  const context = new AudioContext();
  // Chrome starts an AudioContext suspended unless it can attribute it to a
  // gesture; the picker interaction counts, but resume() is cheap insurance.
  if (context.state === "suspended") await context.resume();

  try {
    await context.audioWorklet.addModule("./audio-worklet.js");
  } catch (err) {
    stream.getTracks().forEach((t) => t.stop());
    await context.close();
    setStatus(`Could not load the audio worklet: ${err}`, "error");
    return;
  }

  const source = context.createMediaStreamSource(stream);
  const worklet = new AudioWorkletNode(context, "bava-capture");
  worklet.port.onmessage = (event) => {
    const { samples, channels, rate } = event.data;
    wasm?.bava_push_audio(samples, channels, rate);
  };

  // An AudioWorkletNode is only pulled if it lies on a path to the context's
  // destination — a dangling node never runs. Route it through a silent gain
  // so it is scheduled without the captured audio being played a second time
  // on top of the tab that is already playing it.
  const mute = context.createGain();
  mute.gain.value = 0;
  source.connect(worklet);
  worklet.connect(mute);
  mute.connect(context.destination);

  // Ending the share from Chrome's own "Stop sharing" bar fires this.
  audioTrack.addEventListener("ended", () => {
    stopCapture();
    setStatus("Sharing ended.");
  });

  capture = { stream, context, worklet, source, mute };
  BTN_STOP.hidden = false;
  setStatus(
    `Capturing ${audioTrack.label || "tab audio"} · ${context.sampleRate} Hz.`,
    "ok",
  );
}

async function stopCapture() {
  if (!capture) return;
  const { stream, context, worklet, source, mute } = capture;
  capture = null;
  BTN_STOP.hidden = true;
  worklet.port.onmessage = null;
  source.disconnect();
  worklet.disconnect();
  mute.disconnect();
  stream.getTracks().forEach((track) => track.stop());
  await context.close();
  // Drop whatever was buffered, so restarting doesn't replay the old tail.
  wasm?.bava_reset_audio();
}

// Blur after clicking: a focused button swallows Space and Enter (re-firing
// itself instead of cycling the visualizer mode), and the canvas is where the
// keyboard should go once a capture is running.
function onClick(button, handler) {
  button.addEventListener("click", () => {
    button.blur();
    document.getElementById("bava-canvas")?.focus();
    handler();
  });
}

onClick(BTN_THIS_TAB, () => startCapture(true));
onClick(BTN_OTHER_TAB, () => startCapture(false));
onClick(BTN_STOP, async () => {
  await stopCapture();
  setStatus("Not capturing.");
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

document.getElementById("yt-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const field = document.getElementById("yt-url");
  const id = parseVideoId(field.value);
  if (!id) {
    setStatus("That doesn't look like a YouTube URL or video id.", "error");
    return;
  }
  if (player) player.loadVideoById(id);
  field.blur();
});

// The IFrame Player API calls this global once it has loaded.
window.onYouTubeIframeAPIReady = () => {
  player = new YT.Player("yt-player", {
    // A regular upload, deliberately not one of the 24/7 lofi *live* streams:
    // when those go offline YouTube refuses to embed the archive and the player
    // shows "this live stream recording is not available" instead of playing.
    videoId: new URLSearchParams(location.search).get("video") || "n61ULEU7CO0",
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
