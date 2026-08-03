// SPDX-License-Identifier: MIT OR Apache-2.0
//! Puffin instrumentation, behind the `profile` cargo feature.
//!
//! ```sh
//! cargo run -p bava --features profile              # live app, profiled
//! cargo install puffin_viewer && puffin_viewer --url 127.0.0.1:8585
//! BAVA_PROFILE_REPORT=300 cargo run -p bava --features profile   # + text report
//! ```
//!
//! Two sources of scopes feed one collector:
//!
//! 1. **Bevy's own spans.** The `profile` feature turns on `bevy/trace`, which
//!    wraps every system run and render-graph node in a `tracing` span.
//!    [`layer`] returns a [`tracing_subscriber::Layer`] that converts those
//!    spans into puffin scopes, so the whole schedule is profiled without
//!    touching a single system. It is installed via `LogPlugin::custom_layer`,
//!    which is the only supported way to add a layer to the subscriber Bevy
//!    owns.
//! 2. **Hand-placed [`profile_scope!`] calls** inside hot code, for detail below
//!    system granularity (the per-bar mesh rebuild, say).
//!
//! [`PuffinPlugin`] marks frame boundaries, serves the frames over TCP for
//! `puffin_viewer`, and — when `BAVA_PROFILE_REPORT=<frames>` is set — prints an
//! aggregated "slowest scopes" table to stderr every `<frames>` frames. The text
//! report is what makes this usable over ssh / in CI, where attaching a viewer
//! isn't an option.

/// Open a puffin scope for the rest of the enclosing block, named `$name`.
///
/// A no-op (and free) without the `profile` feature, so hot paths can be
/// instrumented unconditionally. Mirrors `puffin::profile_scope!`.
#[macro_export]
macro_rules! profile_scope {
    ($name:expr) => {
        #[cfg(feature = "profile")]
        $crate::profiling::puffin::profile_scope!($name);
    };
    ($name:expr, $data:expr) => {
        #[cfg(feature = "profile")]
        $crate::profiling::puffin::profile_scope!($name, $data);
    };
}

#[cfg(feature = "profile")]
mod imp {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use bevy::log::BoxedLayer;
    use bevy::prelude::*;
    pub use puffin;
    use puffin::{GlobalProfiler, ScopeCollection, ScopeDetails, ScopeId, ThreadProfiler};
    use tracing::span::{Attributes, Id};
    use tracing::{Metadata, Subscriber};
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::registry::LookupSpan;
    use tracing_subscriber::Layer;

    /// Port `puffin_viewer --url 127.0.0.1:8585` connects to by default.
    const PUFFIN_PORT: u16 = 8585;

    /// Env var selecting the periodic text report: the number of frames to
    /// aggregate before printing (`BAVA_PROFILE_REPORT=300` ≈ every 5 s at 60 fps).
    const REPORT_ENV: &str = "BAVA_PROFILE_REPORT";

    /// Rows printed per report.
    const REPORT_ROWS: usize = 25;

    /// Set to `off` to keep the instrumentation compiled in but collect nothing
    /// (no scopes, no server). The point is an apples-to-apples baseline: the
    /// same binary, the same `bevy/trace` span machinery, minus puffin — so a
    /// measurement can be checked against the cost of measuring.
    const ENABLE_ENV: &str = "BAVA_PROFILE";

    /// Whether collection is enabled this run.
    fn collection_enabled() -> bool {
        !std::env::var(ENABLE_ENV).is_ok_and(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "off" || v == "0" || v == "false"
        })
    }

    // ---------------------------------------------------------------- layer --

    thread_local! {
        /// Open puffin scopes on this thread, innermost last.
        ///
        /// Puffin requires strictly LIFO begin/end *and* only flushes a thread's
        /// stream once its depth returns to zero. `tracing` does not guarantee
        /// LIFO — a span can be exited while an inner one is still open — so a
        /// naive "only close if it's on top" bridge silently strands scopes, the
        /// depth never returns to zero, and the stream grows without bound
        /// (measured: ~2 MB/s, with frame time degrading in lockstep).
        /// [`close_through`] therefore unwinds *through* the exiting span.
        static SPAN_STACK: RefCell<Vec<(Id, usize)>> = const { RefCell::new(Vec::new()) };
    }

    /// Count of out-of-order exits, reported with the periodic text report — a
    /// nonzero value means scope nesting in the viewer is approximate.
    static OUT_OF_ORDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Spans that stay entered for the whole process, which must **not** become
    /// puffin scopes.
    ///
    /// Puffin flushes a thread's stream only once that thread's scope depth
    /// returns to zero (`ThreadProfiler::end_scope`). Bevy enters both of these
    /// once and holds them until exit — `App::run` wraps everything in
    /// `bevy_app`, and the pipelined renderer's thread body opens `render thread`
    /// before its loop — so bridging them pins those two threads above depth zero
    /// forever and their streams grow without bound. Measured before this
    /// exclusion: ~11 MB/s of RSS growth, with frame time degrading from 1.6 ms
    /// to 12 ms over half a minute.
    ///
    /// Nothing is lost by skipping them: each spans the entire program, so it
    /// carries no attributable time, and everything nested inside still profiles.
    fn is_long_lived(name: &str) -> bool {
        matches!(name, "bevy_app" | "render thread")
    }

    /// End every open scope from the innermost down to and including `id`, so the
    /// thread's depth still unwinds LIFO. Does nothing if `id` isn't open (its
    /// enter happened before profiling started, or was already unwound by an
    /// outer span).
    fn close_through(id: &Id) {
        let offsets = SPAN_STACK.with(|s| {
            let mut stack = s.borrow_mut();
            let Some(pos) = stack.iter().rposition(|(open, _)| open == id) else {
                return Vec::new();
            };
            if pos + 1 != stack.len() {
                OUT_OF_ORDER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // Innermost first: puffin's stream is a nesting stack.
            stack.split_off(pos).into_iter().rev().map(|(_, off)| off).collect()
        });
        if !offsets.is_empty() {
            // `call` takes an `Fn`, so the offsets are borrowed, not consumed.
            ThreadProfiler::call(|tp| {
                for &offset in &offsets {
                    tp.end_scope(offset);
                }
            });
        }
    }

    /// Scope display name → puffin scope id. Puffin wants scopes registered once
    /// up front and referenced by id, so resolved names are cached here.
    ///
    /// The key has to be the *resolved* name rather than the callsite: Bevy
    /// instruments every system from a single callsite — `info_span!("system",
    /// name = &*system.name())` — so keying by callsite would collapse the whole
    /// schedule into one row called "system" (which is exactly what the first
    /// version of this did).
    static SCOPE_IDS: Mutex<Option<HashMap<String, ScopeId>>> = Mutex::new(None);

    /// Look up (or register) the puffin scope id for a resolved scope name.
    fn scope_id_for(name: &str, meta: &'static Metadata<'static>) -> ScopeId {
        let mut guard = SCOPE_IDS.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(HashMap::new);
        if let Some(&id) = map.get(name) {
            return id;
        }
        let details = ScopeDetails::from_scope_name(name.to_owned())
            .with_function_name(meta.target().to_owned())
            .with_file(meta.file().unwrap_or("<unknown>").to_owned())
            .with_line_nr(meta.line().unwrap_or(0));
        let id = GlobalProfiler::lock().register_user_scopes(&[details])[0];
        map.insert(name.to_owned(), id);
        id
    }

    /// The resolved scope name for a span, cached in its extensions so the field
    /// visit and the string build happen once per span rather than once per enter.
    struct ScopeName(String);

    /// Pulls the `name` field out of a span's attributes — the field Bevy carries
    /// the system / render-graph-node identity in.
    #[derive(Default)]
    struct NameVisitor(Option<String>);

    impl tracing::field::Visit for NameVisitor {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "name" {
                self.0 = Some(value.to_owned());
            }
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "name" && self.0.is_none() {
                self.0 = Some(format!("{value:?}").trim_matches('"').to_owned());
            }
        }
    }

    /// Bridges `tracing` spans (Bevy's per-system / render-graph instrumentation
    /// under `bevy/trace`) into puffin scopes.
    struct PuffinLayer;

    impl<S> Layer<S> for PuffinLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        // Deliberately not gated on `are_scopes_on()`: Bevy builds every system's
        // span once, during plugin build, so gating here would drop the name of
        // every system created before profiling was switched on — which is all of
        // them — and the whole schedule would collapse into one "system" row.
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            let Some(span) = ctx.span(id) else { return };
            let mut visitor = NameVisitor::default();
            attrs.record(&mut visitor);
            // `system{name="bava::vis::bars::update_bars"}` → "system: update_bars".
            // Bevy's names are full module paths; the tail is what identifies the
            // system, and the span name keeps the *kind* of work visible.
            let name = match visitor.0 {
                Some(inner) => {
                    let short = inner.rsplit("::").next().unwrap_or(&inner).to_owned();
                    format!("{}: {short}", span.metadata().name())
                }
                None => span.metadata().name().to_owned(),
            };
            span.extensions_mut().insert(ScopeName(name));
        }

        fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
            if !puffin::are_scopes_on() {
                return;
            }
            let Some(span) = ctx.span(id) else { return };
            let meta = span.metadata();
            if is_long_lived(meta.name()) {
                return;
            }
            let scope_id = {
                let extensions = span.extensions();
                let name = extensions
                    .get::<ScopeName>()
                    .map(|n| n.0.as_str())
                    .unwrap_or_else(|| meta.name());
                scope_id_for(name, meta)
            };
            let offset = ThreadProfiler::call(|tp| tp.begin_scope(scope_id, ""));
            SPAN_STACK.with(|s| s.borrow_mut().push((id.clone(), offset)));
        }

        // `close_through` is keyed on the span id, so a span skipped by
        // `on_enter` is simply absent from the stack and closes nothing.
        fn on_exit(&self, id: &Id, _ctx: Context<'_, S>) {
            close_through(id);
        }
    }

    /// The puffin bridge layer, for `LogPlugin::custom_layer`.
    ///
    /// Bevy installs the global `tracing` subscriber itself, so this is the only
    /// hook where an extra layer can be added without fighting it.
    pub fn layer(_app: &mut App) -> Option<BoxedLayer> {
        // Earliest possible point — `LogPlugin` is near the head of
        // `DefaultPlugins`, so scopes are live for everything built after it.
        // [`PuffinPlugin`] repeats this for the case where the layer is used
        // standalone.
        puffin::set_scopes_on(collection_enabled());
        Some(Box::new(PuffinLayer))
    }

    // --------------------------------------------------------------- report --

    /// Running totals for one scope across the frames since the last report.
    #[derive(Default, Clone)]
    struct ScopeStats {
        /// Wall time inside the scope, children included.
        total_ns: i64,
        /// Wall time inside the scope minus time in its children — the number
        /// that actually attributes cost to *this* code.
        self_ns: i64,
        /// Slowest single occurrence, to spot spikes an average hides.
        max_ns: i64,
        calls: u64,
    }

    /// Aggregates the frame stream into a periodic stderr table. Held by the
    /// sink closure registered with the [`GlobalProfiler`], which puffin calls
    /// on the thread that ends the frame (the main thread).
    #[derive(Default)]
    struct Report {
        /// Id → details for every scope announced so far. Puffin ships scope
        /// metadata as a per-frame *delta*, so it has to be accumulated.
        scopes: ScopeCollection,
        stats: HashMap<ScopeId, ScopeStats>,
        frames: u64,
        window_ns: i64,
        every: u64,
        /// Bytes puffin is holding for the frames in this window, and how many
        /// threads reported. A per-frame byte count that *grows* between reports
        /// means some thread's scope stack never unwinds to zero (puffin only
        /// flushes a thread's stream at depth 0), which reads as a memory leak.
        frame_bytes: usize,
        threads: usize,
    }

    impl Report {
        fn ingest(&mut self, frame: &puffin::FrameData) {
            for details in &frame.scope_delta {
                self.scopes.insert(details.clone());
            }
            let Ok(unpacked) = frame.unpacked() else { return };
            for stream_info in unpacked.thread_streams.values() {
                self.walk(&stream_info.stream, 0);
            }
            self.frames += 1;
            self.frame_bytes += frame.bytes_of_ram_used();
            self.threads = self.threads.max(unpacked.thread_streams.len());
            let (from, to) = frame.range_ns();
            self.window_ns += to - from;
        }

        /// Accumulate every scope in `stream` starting at `offset`, recursing
        /// into children so `self_ns` can subtract them.
        fn walk(&mut self, stream: &puffin::Stream, offset: u64) {
            let Ok(reader) = puffin::Reader::with_offset(stream, offset) else {
                return;
            };
            for scope in reader {
                let Ok(scope) = scope else { return };
                let mut children_ns = 0;
                if scope.child_begin_position < scope.child_end_position {
                    children_ns = self.child_total_ns(stream, scope.child_begin_position);
                    self.walk(stream, scope.child_begin_position);
                }
                let entry = self.stats.entry(scope.id).or_default();
                entry.total_ns += scope.record.duration_ns;
                entry.self_ns += scope.record.duration_ns - children_ns;
                entry.max_ns = entry.max_ns.max(scope.record.duration_ns);
                entry.calls += 1;
            }
        }

        /// Summed duration of the direct children at `offset` (not recursive —
        /// grandchildren are already inside each child's own duration).
        fn child_total_ns(&self, stream: &puffin::Stream, offset: u64) -> i64 {
            let Ok(reader) = puffin::Reader::with_offset(stream, offset) else {
                return 0;
            };
            reader
                .flatten()
                .map(|scope| scope.record.duration_ns)
                .sum()
        }

        fn print_and_reset(&mut self) {
            let frames = self.frames.max(1);
            // Merge by *name*, not id: `profile_scope!` registers its scope per
            // thread, so one hand-placed scope inside a system that the task pool
            // migrates across threads yields several ids for the same code.
            let mut by_name: HashMap<&str, ScopeStats> = HashMap::new();
            for (id, s) in &self.stats {
                let name = self
                    .scopes
                    .fetch_by_id(id)
                    .map(|d| d.name().as_ref())
                    .unwrap_or("<unknown>");
                let entry = by_name.entry(name).or_default();
                entry.total_ns += s.total_ns;
                entry.self_ns += s.self_ns;
                entry.max_ns = entry.max_ns.max(s.max_ns);
                entry.calls += s.calls;
            }
            let mut rows: Vec<(&str, ScopeStats)> = by_name.into_iter().collect();
            rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.self_ns));
            rows.truncate(REPORT_ROWS);

            let avg_frame_ms = self.window_ns as f64 / frames as f64 / 1e6;
            let ooo = OUT_OF_ORDER.swap(0, std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "\n─── puffin: {frames} frames, {avg_frame_ms:.2} ms/frame avg \
                 ({:.0} fps), {:.1} KiB/frame over {} threads{} ───",
                1000.0 / avg_frame_ms.max(1e-6),
                self.frame_bytes as f64 / frames as f64 / 1024.0,
                self.threads,
                if ooo > 0 {
                    format!(", {ooo} out-of-order span exits")
                } else {
                    String::new()
                }
            );
            eprintln!(
                "{:<44} {:>9} {:>9} {:>9} {:>8}",
                "scope (self time, descending)", "self/f", "total/f", "max", "calls/f"
            );
            for (name, s) in rows {
                let per_frame = |ns: i64| ns as f64 / frames as f64 / 1e6;
                eprintln!(
                    "{:<44} {:>8.3}ms {:>8.3}ms {:>8.3}ms {:>8.1}",
                    truncate(name, 44),
                    per_frame(s.self_ns),
                    per_frame(s.total_ns),
                    s.max_ns as f64 / 1e6,
                    s.calls as f64 / frames as f64,
                );
            }

            self.stats.clear();
            self.frames = 0;
            self.window_ns = 0;
            self.frame_bytes = 0;
            self.threads = 0;
        }
    }

    /// Truncate to `max` chars, keeping the *tail* — scope names are prefixed by
    /// module paths, and the distinguishing part is at the end.
    fn truncate(s: &str, max: usize) -> String {
        if s.chars().count() <= max {
            return s.to_string();
        }
        let skip = s.chars().count() - (max - 1);
        format!("…{}", s.chars().skip(skip).collect::<String>())
    }

    // --------------------------------------------------------------- plugin --

    /// Turns scopes on, marks frame boundaries, serves frames to `puffin_viewer`
    /// and installs the optional periodic text report.
    pub struct PuffinPlugin;

    impl Plugin for PuffinPlugin {
        fn build(&self, app: &mut App) {
            if !collection_enabled() {
                info!("bava: profiling compiled in but disabled ({ENABLE_ENV}=off)");
                return;
            }
            puffin::set_scopes_on(true);

            match puffin_http::Server::new(&format!("127.0.0.1:{PUFFIN_PORT}")) {
                Ok(server) => {
                    info!(
                        "bava: puffin server on 127.0.0.1:{PUFFIN_PORT} \
                         (`puffin_viewer --url 127.0.0.1:{PUFFIN_PORT}`)"
                    );
                    // The server stops serving when dropped, so it has to outlive
                    // `build`; the app owns it for the process lifetime.
                    app.insert_non_send(server);
                }
                Err(e) => warn!("bava: could not start puffin server: {e}"),
            }

            if let Some(every) = std::env::var(REPORT_ENV)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&n| n > 0)
            {
                info!("bava: puffin text report every {every} frames ({REPORT_ENV})");
                let report = Mutex::new(Report {
                    every,
                    ..Report::default()
                });
                GlobalProfiler::lock().add_sink(Box::new(move |frame| {
                    let mut report = report.lock().unwrap_or_else(|e| e.into_inner());
                    report.ingest(&frame);
                    if report.frames >= report.every {
                        report.print_and_reset();
                    }
                }));
            }

            // `First` runs before anything else each frame, so the boundary lands
            // between frames rather than mid-schedule.
            app.add_systems(First, new_frame).add_systems(Startup, unthrottle);
        }
    }

    /// Close the previous puffin frame and open the next.
    fn new_frame() {
        GlobalProfiler::lock().new_frame();
    }

    /// Render flat out in profiling builds.
    ///
    /// Under vsync every frame that fits the budget costs exactly one refresh
    /// interval, and the surplus shows up as the presenting system blocking —
    /// which hides both how much CPU work a frame really costs and how much
    /// headroom a change bought. Uncapped, the frame time *is* the cost.
    fn unthrottle(mut windows: Query<&mut Window>) {
        for mut window in &mut windows {
            window.present_mode = bevy::window::PresentMode::AutoNoVsync;
        }
    }
}

#[cfg(feature = "profile")]
pub use imp::{layer, puffin, PuffinPlugin};
