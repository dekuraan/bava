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
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

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
            .args([
                "-colorspace",
                "bt709",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
            ])
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

/// Exactly one frame of RGBA — `width × height × 4` bytes — from a captured
/// image, discarding any trailing readback padding.
///
/// **The padding is not cosmetic.** The GPU→CPU copy behind `bevy_capture` can
/// hand back a buffer larger than the frame (at 960×540 it is 2 211 840 bytes,
/// 36 rows past the 2 073 600 the frame needs), with the slack after the last
/// row. ffmpeg reads *fixed-size* frames off the pipe, so writing the slack
/// desynchronises the stream: frame 0 is correct, and every frame after it is
/// shifted down by `padding / row_bytes` rows with the padding showing through
/// as a black band at the wrap. Whether there is slack depends on the
/// resolution — 1280×720 and 800×600 come back exact while 640×360, 960×540 and
/// 1920×1080 do not — which is why this went unnoticed.
///
/// Rows themselves are contiguous (a *row* stride mismatch would shear the
/// picture, and frame 0 is pixel-perfect), so truncating is the whole fix.
fn frame_payload(image: &Image) -> Result<&[u8]> {
    let data = image
        .data
        .as_ref()
        .ok_or("captured frame has no CPU-side data")?;
    let needed = image.width() as usize * image.height() as usize * 4;
    data.get(..needed).ok_or_else(|| {
        format!(
            "captured frame is {} bytes, short of the {needed} a {}x{} RGBA frame needs",
            data.len(),
            image.width(),
            image.height(),
        )
        .into()
    })
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
        let data = frame_payload(image)?;
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
        let _ =
            self.status
                .0
                .compare_exchange(RUNNING, outcome, Ordering::AcqRel, Ordering::Acquire);
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

#[cfg(test)]
mod tests {
    use bevy::asset::RenderAssetUsages;
    use bevy::image::Image;
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

    use super::frame_payload;

    /// A `w`×`h` RGBA image whose buffer carries `extra_rows` of trailing
    /// readback padding, as the GPU copy hands it back at some resolutions.
    fn padded(w: u32, h: u32, extra_rows: u32) -> Image {
        let row = w as usize * 4;
        let mut data = vec![0xABu8; row * h as usize];
        data.extend(std::iter::repeat_n(0u8, row * extra_rows as usize));
        let mut image = Image::new(
            Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            vec![0; row * h as usize],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.data = Some(data);
        image
    }

    #[test]
    fn trailing_readback_padding_is_stripped() {
        // The regression: piping the padding to ffmpeg desynchronises its
        // fixed-size frame reads, rolling every frame after the first.
        let image = padded(960, 540, 36);
        let out = frame_payload(&image).unwrap();
        assert_eq!(out.len(), 960 * 540 * 4);
        assert!(out.iter().all(|&b| b == 0xAB), "kept only real frame rows");
    }

    #[test]
    fn an_exact_buffer_is_passed_through() {
        let image = padded(1280, 720, 0);
        assert_eq!(frame_payload(&image).unwrap().len(), 1280 * 720 * 4);
    }

    #[test]
    fn a_short_buffer_is_an_error_not_a_truncated_frame() {
        let mut image = padded(64, 64, 0);
        image.data.as_mut().unwrap().truncate(64 * 63 * 4);
        assert!(frame_payload(&image).is_err());
    }
}
