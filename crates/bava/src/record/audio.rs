// SPDX-License-Identifier: MIT OR Apache-2.0
//! Offline audio decoding for `--input`, via symphonia (pure Rust).
//!
//! Decodes a whole audio file (mp3/flac/ogg/wav/m4a — whatever the enabled
//! symphonia codecs cover) into interleaved `f64` samples at the file's native
//! rate, plus the tag metadata and embedded cover art that feed the recording's
//! now-playing HUD and dynamic colors.

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag};

use crate::now_playing::OfflineTrack;

/// A fully decoded track, ready to be fed to cavacore frame by frame.
pub struct DecodedTrack {
    /// Sample rate the file decodes at (the cavacore plan is built to match).
    pub rate: u32,
    /// 1 or 2 — sources with more channels are downmixed to stereo.
    pub channels: usize,
    /// Interleaved samples, `channels` per PCM frame.
    pub samples: Vec<f64>,
    /// Tag metadata + embedded cover art for the HUD / dynamic colors.
    pub track: OfflineTrack,
}

impl DecodedTrack {
    /// Track length in PCM frames (samples per channel).
    pub fn pcm_frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    /// Track length in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.pcm_frames() as f64 / self.rate.max(1) as f64
    }
}

/// Decode `path` completely. Errors are stringly typed — this runs once at
/// startup and any failure is fatal for the recording.
pub fn decode(path: &Path) -> Result<DecodedTrack, String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("unrecognized audio format in {}: {e}", path.display()))?;

    let mut tags = TagScratch::default();
    // Tags can live before the container (ID3v2 on mp3) or inside it
    // (FLAC/OGG/MP4); the probe queues every revision it saw onto the reader's
    // metadata log, oldest first. Walk them all, letting later revisions fill
    // gaps rather than overwrite.
    {
        let mut md = format.metadata();
        if let Some(rev) = md.current() {
            merge_metadata(&mut tags, rev);
        }
        while md.pop().is_some() {
            if let Some(rev) = md.current() {
                merge_metadata(&mut tags, rev);
            }
        }
    }
    let track_meta = tags.into_track();

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| format!("{}: no decodable audio track", path.display()))?;
    let track_id = track.id;
    // Known for most formats (from headers / the LAME tag); lets the sample
    // buffer allocate once instead of doubling its way through hundreds of MB.
    let n_frames_hint = track.num_frames;

    let audio_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| format!("{}: no decodable audio track", path.display()))?;

    // Gapless decoding trims mp3 LAME priming/padding, so our sample count
    // matches the duration ffmpeg gives the muxed audio — otherwise the vis
    // would lead the sound by the priming length (tens of ms).
    let dec_opts = AudioDecoderOptions::default().gapless(true);
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(audio_params, &dec_opts)
        .map_err(|e| format!("{}: unsupported codec: {e}", path.display()))?;

    let mut samples: Vec<f64> = Vec::new();
    let mut rate = 0u32;
    let mut src_channels = 0usize;
    let mut scratch: Vec<f64> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            // End of stream.
            Ok(None) => break,
            // A truncated file — decode what we got.
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("{}: read error: {e}", path.display())),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A corrupt frame is skippable; mp3s in the wild have them.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(format!("{}: decode error: {e}", path.display())),
        };

        if rate == 0 {
            let spec = decoded.spec();
            rate = spec.rate();
            src_channels = spec.channels().count();
            if let Some(n) = n_frames_hint {
                samples.reserve_exact((n as usize).saturating_mul(src_channels));
            }
        }

        decoded.copy_to_vec_interleaved(&mut scratch);
        samples.extend_from_slice(&scratch);
    }

    if rate == 0 || samples.is_empty() {
        return Err(format!("{}: no audio decoded", path.display()));
    }

    // cavacore handles 1 or 2 channels; fold anything wider down to stereo.
    // The first two channels are front L/R in every common layout; the rest
    // (center — where 5.1 mixes put lead vocals — LFE, surrounds) are mixed
    // into both sides at -3 dB so the analysis hears everything the muxed
    // full-mix audio track carries. Absolute scale doesn't matter (autosens).
    let channels = if src_channels > 2 {
        samples = samples
            .chunks_exact(src_channels)
            .flat_map(|frame| {
                let rest: f64 = frame[2..].iter().sum::<f64>() * std::f64::consts::FRAC_1_SQRT_2;
                [frame[0] + rest, frame[1] + rest]
            })
            .collect();
        2
    } else {
        src_channels.max(1)
    };

    Ok(DecodedTrack {
        rate,
        channels,
        samples,
        track: track_meta,
    })
}

/// Tag fields gathered across metadata revisions. Artist and album-artist are
/// kept apart until the end: tag order within a file is arbitrary, and folding
/// them into one first-wins field would show "Various Artists" (the album
/// artist) on compilation tracks whenever TPE2 precedes TPE1.
#[derive(Default)]
struct TagScratch {
    title: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    album: Option<String>,
    art: Option<Vec<u8>>,
}

impl TagScratch {
    /// The performing artist wins; the album artist is only a fallback.
    fn into_track(self) -> OfflineTrack {
        OfflineTrack {
            title: self.title,
            artist: self.artist.or(self.album_artist),
            album: self.album,
            art: self.art,
        }
    }
}

/// Fold one metadata revision into `out`, filling only missing fields (called
/// oldest revision first).
fn merge_metadata(out: &mut TagScratch, rev: &MetadataRevision) {
    for tag in &rev.media.tags {
        let (slot, value) = match &tag.std {
            Some(StandardTag::TrackTitle(s)) => (&mut out.title, s),
            Some(StandardTag::Artist(s)) => (&mut out.artist, s),
            Some(StandardTag::AlbumArtist(s)) => (&mut out.album_artist, s),
            Some(StandardTag::Album(s)) => (&mut out.album, s),
            _ => continue,
        };
        if !value.is_empty() {
            slot.get_or_insert_with(|| value.to_string());
        }
    }
    if out.art.is_none()
        && let Some(visual) = rev.media.visuals.first() {
            out.art = Some(visual.data.to_vec());
        }
}
