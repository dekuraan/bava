// SPDX-License-Identifier: GPL-3.0-or-later
//! Native PipeWire capture backend (Linux).
//!
//! Captures the default sink's monitor directly from the PipeWire graph rather
//! than going through the `pipewire-pulse` compatibility layer. Versus the Pulse
//! backend this buys three things cava's native PipeWire path also gets:
//!
//! * **Follows the default sink for free.** `PW_KEY_STREAM_CAPTURE_SINK` +
//!   `AUTOCONNECT` make the graph route us to whatever sink is currently the
//!   default and re-route on a default-device change — no monitor-name probing.
//! * **Real format negotiation.** We request interleaved `f32` at the plan's
//!   exact rate/channels; PipeWire inserts a converter, so the process callback
//!   delivers precisely that and no resampling is needed on our side.
//! * **A non-blocking RT callback** that only copies samples into a shared queue.
//!
//! PipeWire is event-loop driven and its objects are `!Send`, so the loop runs
//! on a dedicated thread created here; [`read`](PipeWireCapture::read) (on the
//! capture thread) drains the queue. A [`pw::channel`] carries the stop signal
//! back into the loop thread on drop.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::{context::ContextRc, main_loop::MainLoopRc, properties::properties, spa, stream::StreamBox};
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::{format_utils, ParamType};
use spa::pod::{serialize::PodSerializer, Object, Pod, Value};
use spa::utils::{Direction, SpaTypes};

use super::{AudioCapture, CaptureError};

/// How long [`PipeWireCapture::open`] waits for the stream to negotiate a format
/// (or report an error) before giving up so the caller can fall back to Pulse.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long [`PipeWireCapture::read`] waits for a full chunk of *real* samples
/// before giving up and zero-filling. This must comfortably exceed PipeWire's
/// largest plausible quantum (a quantum can be a few tens of ms) so that during
/// active playback `read` always assembles a full real chunk and never injects
/// spurious silence — which would otherwise corrupt cava's autosens/gravity
/// smoothing (the Pulse backend blocks until a full real read is available, so
/// it never does this). Only a genuinely idle source — sink suspended, no
/// buffers arriving — hits this timeout; the bars then decay on the resulting
/// silence, the same path as cava's `reset_output_buffers`. Kept below
/// `feed_cava`'s 200 ms stall window so steady silence reaches the analysis
/// before that net trips.
const IDLE_TIMEOUT: Duration = Duration::from_millis(120);

/// Hand-off between the realtime process callback (producer) and `read`
/// (consumer): interleaved `f32` at the negotiated (target) format.
struct Shared {
    queue: Mutex<VecDeque<f32>>,
    /// Signalled by the process callback after it enqueues a quantum, so `read`
    /// wakes the moment audio lands instead of polling.
    filled: Condvar,
    /// Hard cap (~4 s of target-rate audio) so a stalled consumer can't grow the
    /// queue without bound; oldest samples are dropped.
    cap: usize,
}

/// Startup handshake: the loop thread reports the negotiated format (or a fatal
/// error) back to `open`, which blocks until one arrives or the timeout elapses.
enum Handshake {
    Pending,
    Ready { rate: u32, channels: u32 },
    Failed(String),
}

/// State shared with the stream listener callbacks (one `&mut` per callback).
struct UserData {
    format: AudioInfoRaw,
    shared: Arc<Shared>,
    handshake: Arc<(Mutex<Handshake>, Condvar)>,
    /// Whether the ready handshake has already been signalled (param_changed can
    /// fire more than once; only the first transition matters for startup).
    ready_signalled: bool,
}

/// A native PipeWire capture stream feeding interleaved `f64` at the requested
/// `rate`/`channels`.
pub struct PipeWireCapture {
    shared: Arc<Shared>,
    target_rate: u32,
    target_channels: usize,
    /// Sends the stop signal to the loop thread (on drop); `Sender` is `Send`.
    quit: pw::channel::Sender<()>,
    /// The loop thread, joined on drop.
    thread: Option<JoinHandle<()>>,
}

impl PipeWireCapture {
    /// Connect a capture stream on the default sink's monitor (or `device` if
    /// given), converting to `target_rate`/`target_channels`. `_frame_samples` is
    /// accepted for parity with the Pulse backend but unused — PipeWire drives
    /// its own buffer sizes.
    pub fn open(
        device: Option<&str>,
        target_rate: u32,
        target_channels: u8,
        _frame_samples: usize,
    ) -> Result<Self, CaptureError> {
        let target_channels = target_channels.max(1) as usize;

        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            filled: Condvar::new(),
            cap: (target_rate as usize * target_channels * 4).max(1),
        });
        let handshake = Arc::new((Mutex::new(Handshake::Pending), Condvar::new()));

        // The receiver attaches to the loop (inside the thread); the sender stays
        // here and signals termination on drop.
        let (quit_tx, quit_rx) = pw::channel::channel::<()>();

        let device = device.map(str::to_owned);
        let thread = {
            let shared = shared.clone();
            let handshake = handshake.clone();
            std::thread::Builder::new()
                .name("bava-pw".into())
                .spawn(move || {
                    run_loop(
                        device,
                        target_rate,
                        target_channels as u32,
                        shared,
                        handshake,
                        quit_rx,
                    );
                })
                .map_err(|e| CaptureError::Init(format!("spawn PipeWire thread: {e}")))?
        };

        // Block until the stream negotiates a format, hits an error, or times out.
        let (lock, cv) = &*handshake;
        let mut guard = lock.lock().unwrap();
        let start = Instant::now();
        let outcome = loop {
            match &*guard {
                Handshake::Ready { rate, channels } => break Ok((*rate, *channels)),
                Handshake::Failed(e) => break Err(e.clone()),
                Handshake::Pending => {}
            }
            let remaining = match CONNECT_TIMEOUT.checked_sub(start.elapsed()) {
                Some(r) if !r.is_zero() => r,
                _ => break Err("timed out connecting PipeWire stream".into()),
            };
            let (g, timeout) = cv.wait_timeout(guard, remaining).unwrap();
            guard = g;
            if timeout.timed_out() {
                break Err("timed out connecting PipeWire stream".into());
            }
        };
        drop(guard);

        match outcome {
            Ok((rate, channels)) => Ok(Self {
                shared,
                target_rate: rate,
                target_channels: channels as usize,
                quit: quit_tx,
                thread: Some(thread),
            }),
            Err(e) => {
                // Tear the loop thread down before returning so a failed attempt
                // doesn't leak it; the Pulse fallback then takes over.
                let _ = quit_tx.send(());
                let _ = thread.join();
                Err(CaptureError::Init(format!("pipewire: {e}")))
            }
        }
    }
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl AudioCapture for PipeWireCapture {
    fn read(&mut self, buf: &mut [f64]) -> Result<(), CaptureError> {
        // Block until the queue holds a full chunk of *real* samples, then drain
        // it. PipeWire delivers in quanta that are usually larger than one chunk
        // and arrive tens of ms apart, so returning early with whatever happens
        // to be queued would zero-fill most chunks mid-quantum during active
        // playback and feed cava a stream of real-audio-interspersed-with-silence
        // — visibly different bar smoothing than the Pulse backend, whose
        // `simple.read` blocks until a full real read is available. Only a
        // genuinely idle source (no quanta within `IDLE_TIMEOUT`) short-reads and
        // zero-fills, so the bars decay on silence at a steady cadence.
        let need = buf.len();
        let mut q = self.shared.queue.lock().unwrap();
        let start = Instant::now();
        while q.len() < need {
            let remaining = match IDLE_TIMEOUT.checked_sub(start.elapsed()) {
                Some(r) if !r.is_zero() => r,
                _ => break,
            };
            let (g, res) = self.shared.filled.wait_timeout(q, remaining).unwrap();
            q = g;
            if res.timed_out() {
                break;
            }
        }
        let take = need.min(q.len());
        for slot in buf[..take].iter_mut() {
            *slot = q.pop_front().unwrap() as f64;
        }
        drop(q);
        for slot in &mut buf[take..] {
            *slot = 0.0;
        }
        Ok(())
    }

    fn rate(&self) -> u32 {
        self.target_rate
    }

    fn channels(&self) -> u32 {
        self.target_channels as u32
    }
}

/// Body of the PipeWire loop thread: build the stream, run the loop until the
/// quit channel fires, reporting startup success/failure through `handshake`.
fn run_loop(
    device: Option<String>,
    rate: u32,
    channels: u32,
    shared: Arc<Shared>,
    handshake: Arc<(Mutex<Handshake>, Condvar)>,
    quit_rx: pw::channel::Receiver<()>,
) {
    // Any setup error is reported through the handshake so `open` stops waiting.
    let fail = |handshake: &Arc<(Mutex<Handshake>, Condvar)>, msg: String| {
        let (lock, cv) = &**handshake;
        *lock.lock().unwrap() = Handshake::Failed(msg);
        cv.notify_all();
    };

    pw::init();

    let mainloop = match MainLoopRc::new(None) {
        Ok(m) => m,
        Err(e) => return fail(&handshake, format!("MainLoop::new: {e}")),
    };
    let context = match ContextRc::new(&mainloop, None) {
        Ok(c) => c,
        Err(e) => return fail(&handshake, format!("Context::new: {e}")),
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(e) => return fail(&handshake, format!("connect: {e}")),
    };

    // Capture the sink monitor (playback), following the default sink via the
    // graph. A `*.monitor` device name (Pulse-style) is reduced to its sink node
    // name; "default"/"auto"/none lets AUTOCONNECT pick the default sink.
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::STREAM_CAPTURE_SINK => "true",
    };
    if let Some(dev) = device.as_deref()
        && !dev.is_empty()
        && dev != "default"
        && dev != "auto"
    {
        let target = dev.strip_suffix(".monitor").unwrap_or(dev);
        props.insert(*pw::keys::TARGET_OBJECT, target);
    }

    let stream = match StreamBox::new(&core, "bava", props) {
        Ok(s) => s,
        Err(e) => return fail(&handshake, format!("Stream::new: {e}")),
    };

    let user_data = UserData {
        format: AudioInfoRaw::default(),
        shared,
        handshake: handshake.clone(),
        ready_signalled: false,
    };

    let _listener = stream
        .add_local_listener_with_user_data(user_data)
        .param_changed(|_stream, ud, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            if ud.format.parse(param).is_err() {
                return;
            }
            // Format negotiated: streaming is about to begin. Report it once.
            if !ud.ready_signalled {
                ud.ready_signalled = true;
                let (lock, cv) = &*ud.handshake;
                *lock.lock().unwrap() = Handshake::Ready {
                    rate: ud.format.rate(),
                    channels: ud.format.channels(),
                };
                cv.notify_all();
            }
        })
        .state_changed(|_stream, ud, _old, new| {
            if let pw::stream::StreamState::Error(msg) = new
                && !ud.ready_signalled
            {
                let (lock, cv) = &*ud.handshake;
                *lock.lock().unwrap() = Handshake::Failed(format!("stream error: {msg}"));
                cv.notify_all();
            }
        })
        .process(|stream, ud| {
            while let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else { break };
                let chunk = data.chunk();
                let offset = chunk.offset() as usize;
                let size = chunk.size() as usize;
                let Some(bytes) = data.data() else { continue };
                let start = offset.min(bytes.len());
                let end = (offset + size).min(bytes.len());
                let region = &bytes[start..end];

                // try_lock so this realtime data thread never blocks on the
                // consumer mid-drain (and a poisoned mutex can't panic-unwind
                // through PipeWire's C trampoline). On contention, drop the
                // quantum: the only contended window is `read`'s brief drain,
                // after which it re-checks the queue, so no wakeup is lost.
                let Ok(mut q) = ud.shared.queue.try_lock() else {
                    continue;
                };
                for c in region.chunks_exact(4) {
                    q.push_back(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
                if q.len() > ud.shared.cap {
                    let overflow = q.len() - ud.shared.cap;
                    q.drain(..overflow);
                }
                drop(q);
                // Wake `read`, which is blocked until a full chunk is queued.
                ud.shared.filled.notify_one();
            }
        })
        .register();
    let _listener = match _listener {
        Ok(l) => l,
        Err(e) => return fail(&handshake, format!("register listener: {e}")),
    };

    // Request interleaved f32 at the plan's exact rate/channels; PipeWire inserts
    // a converter so the process callback delivers precisely this format.
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(rate);
    audio_info.set_channels(channels);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values = match PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
    {
        Ok((cursor, _)) => cursor.into_inner(),
        Err(e) => return fail(&handshake, format!("serialize format: {e}")),
    };
    let Some(pod) = Pod::from_bytes(&values) else {
        return fail(&handshake, "build format pod".into());
    };
    let mut params = [pod];

    if let Err(e) = stream.connect(
        Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    ) {
        return fail(&handshake, format!("connect stream: {e}"));
    }

    // Stop the loop when `open`/`Drop` sends on the quit channel.
    let _recv = quit_rx.attach(mainloop.loop_(), {
        let ml = mainloop.clone();
        move |_| ml.quit()
    });

    mainloop.run();
}
