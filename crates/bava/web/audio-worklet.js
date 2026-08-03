// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Taps the captured tab's audio and ships interleaved PCM to the main thread,
// where `bava.js` forwards it into wasm.
//
// Why a worklet rather than an AnalyserNode: cavacore needs a *continuous*,
// gap-free sample stream — its framerate estimate and auto-sensitivity assume a
// steady number of new samples per execute. `getFloatTimeDomainData` polled from
// requestAnimationFrame gives neither: consecutive reads overlap or skip
// depending on how frame timing lands against the audio clock. The worklet runs
// on the audio thread and sees every block exactly once.

// Blocks to coalesce before posting. A worklet block is 128 frames (~2.7 ms at
// 48 kHz); posting each one means ~375 messages/second, which is a lot of
// structured-clone churn for no benefit. Eight blocks is ~21 ms — still well
// under a 60 fps frame, so the visualizer sees fresh audio every frame.
const BLOCKS_PER_POST = 8;

class BavaCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.pending = [];
    this.pendingBlocks = 0;
  }

  process(inputs) {
    const input = inputs[0];
    // No connected source yet, or the track ended: nothing to forward. Return
    // true regardless — returning false would permanently retire the node, and
    // a paused video would kill capture for good.
    if (!input || input.length === 0 || !input[0]) return true;

    const channels = Math.min(input.length, 2);
    const frames = input[0].length;
    const interleaved = new Float32Array(frames * channels);
    if (channels === 1) {
      interleaved.set(input[0]);
    } else {
      const left = input[0];
      const right = input[1];
      for (let i = 0; i < frames; i++) {
        interleaved[i * 2] = left[i];
        interleaved[i * 2 + 1] = right[i];
      }
    }

    this.pending.push({ data: interleaved, channels });
    this.pendingBlocks++;
    if (this.pendingBlocks < BLOCKS_PER_POST) return true;

    // A channel-count change mid-batch would corrupt the interleaving, so the
    // batch carries the count of its last block and is flushed on any change.
    const channelCount = this.pending[this.pending.length - 1].channels;
    let total = 0;
    for (const block of this.pending) {
      if (block.channels === channelCount) total += block.data.length;
    }
    const batch = new Float32Array(total);
    let offset = 0;
    for (const block of this.pending) {
      if (block.channels !== channelCount) continue;
      batch.set(block.data, offset);
      offset += block.data.length;
    }
    this.pending = [];
    this.pendingBlocks = 0;

    // Transfer the buffer rather than copying it: this runs on the audio
    // thread, where an allocation-heavy path risks glitching playback.
    this.port.postMessage(
      { samples: batch, channels: channelCount, rate: sampleRate },
      [batch.buffer],
    );
    return true;
  }
}

registerProcessor("bava-capture", BavaCaptureProcessor);
