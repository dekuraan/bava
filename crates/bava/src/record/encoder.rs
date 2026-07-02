// SPDX-License-Identifier: MIT OR Apache-2.0
//! A [`bevy_capture::Encoder`] that streams raw frames to an ffmpeg subprocess.
//!
//! One pass, no intermediate files: rendered RGBA frames go down ffmpeg's
//! stdin as rawvideo while ffmpeg encodes H.264 and muxes the original audio
//! file alongside — so video encoding runs in a separate process, in parallel
//! with Bevy's rendering, and the output is ready the moment the last frame
//! lands. Encode settings follow YouTube's upload recommendations: H.264 high
//! profile, yuv420p, closed GOP of half the framerate, AAC-LC 384 kbps stereo,
//! and `+faststart`.

use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use bevy::prelude::Image;
use bevy_capture::encoder::{Encoder, Result};

/// Where the encoder ended up, shared with the record driver (the encoder
/// itself is consumed by `Capture::start` and finishes on the render thread).
pub struct EncoderStatus(AtomicU8);

const RUNNING: u8 = 0;
const FINISHED_OK: u8 = 1;
const FAILED: u8 = 2;

impl EncoderStatus {
    /// `Some(true)` = finished cleanly, `Some(false)` = ffmpeg failed,
    /// `None` = still running.
    pub fn finished(&self) -> Option<bool> {
        match self.0.load(Ordering::Acquire) {
            RUNNING => None,
            FINISHED_OK => Some(true),
            _ => Some(false),
        }
    }
}

/// Streams RGBA frames into `ffmpeg`, which encodes and muxes the audio track.
pub struct FfmpegEncoder {
    child: Child,
    /// Taken on the first write error / on finish, so a dead ffmpeg isn't
    /// written to repeatedly.
    stdin: Option<ChildStdin>,
    status: Arc<EncoderStatus>,
    frames: u64,
}

impl FfmpegEncoder {
    /// Spawn ffmpeg reading `width`×`height` RGBA rawvideo at `fps` from stdin
    /// (input 0) and the original `audio` file (input 1), writing `out` with
    /// exactly `duration_secs` of output.
    pub fn spawn(
        out: &Path,
        audio: &Path,
        width: u32,
        height: u32,
        fps: u32,
        duration_secs: f64,
    ) -> std::io::Result<(Self, Arc<EncoderStatus>)> {
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            // Input 0: our rendered frames.
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-r", &fps.to_string()])
            .args(["-i", "pipe:0"])
            // Input 1: the source audio file.
            .arg("-i")
            .arg(audio)
            .args(["-map", "0:v:0", "-map", "1:a:0"])
            // Video per YouTube's recommendations. CRF 18 is visually lossless
            // territory; veryfast keeps encoding well ahead of the renderer.
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"])
            .args(["-profile:v", "high", "-pix_fmt", "yuv420p"])
            .args(["-g", &(fps / 2).max(1).to_string(), "-bf", "2"])
            .args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"])
            // Audio: AAC-LC 384 kbps stereo (YouTube's recommended maximum).
            .args(["-c:a", "aac", "-b:a", "384k"])
            // Exact output length (the video's frame count / fps, known up
            // front). NOT `-shortest`: that makes ffmpeg exit as soon as the
            // audio stream ends, racing the last in-flight piped frames into a
            // broken pipe. `-t` trims the audio when --duration cut the video,
            // and ffmpeg always waits for stdin EOF.
            .args(["-t", &format!("{duration_secs:.6}")])
            .args(["-movflags", "+faststart"])
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            // Inherit stderr so encoder errors surface in our output.
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child.stdin.take();
        let status = Arc::new(EncoderStatus(AtomicU8::new(RUNNING)));
        Ok((
            Self {
                child,
                stdin,
                status: status.clone(),
                frames: 0,
            },
            status,
        ))
    }
}

impl Encoder for FfmpegEncoder {
    fn encode(&mut self, image: &Image) -> Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            // ffmpeg already stopped reading. If it *succeeded* (it had every
            // frame `-t` asked for and closed the pipe first), the remaining
            // in-flight frames are expected and silently dropped; only a real
            // failure is worth surfacing.
            return match self.status.finished() {
                Some(true) => Ok(()),
                _ => Err("ffmpeg stdin already closed".into()),
            };
        };
        let data = image
            .data
            .as_ref()
            .ok_or("captured frame has no CPU-side data")?;
        if let Err(e) = stdin.write_all(data) {
            // ffmpeg stopped reading. With output trimming (`-t`) it
            // legitimately closes the pipe the moment it has every frame it
            // needs — often mid-write of the next one — so a broken pipe from
            // a *successfully exited* ffmpeg is a clean finish, not an error.
            self.stdin = None;
            let ok = self.child.wait().is_ok_and(|st| st.success());
            self.status
                .0
                .store(if ok { FINISHED_OK } else { FAILED }, Ordering::Release);
            return if ok {
                Ok(())
            } else {
                Err(format!("ffmpeg stopped accepting frames: {e}").into())
            };
        }
        self.frames += 1;
        Ok(())
    }

    fn finish(mut self: Box<Self>) {
        // Closing stdin is ffmpeg's EOF; it then finalizes the mp4 (including
        // the +faststart moov relocation) and exits.
        drop(self.stdin.take());
        let outcome = match self.child.wait() {
            Ok(st) if st.success() => FINISHED_OK,
            Ok(st) => {
                bevy::log::error!("bava: ffmpeg exited with {st}");
                FAILED
            }
            Err(e) => {
                bevy::log::error!("bava: could not wait for ffmpeg: {e}");
                FAILED
            }
        };
        // Don't overwrite an earlier FAILED from a write error.
        let _ = self.status.0.compare_exchange(
            RUNNING,
            outcome,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// `true` if an `ffmpeg` binary is runnable — checked before the slow GPU init
/// so a missing encoder fails in milliseconds with a clear message.
pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
