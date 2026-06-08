//! Media handling: hover-to-play audio and image thumbnail previews.
//!
//! Audio: when the user rests on a sound file, we decode it to raw PCM with
//! Symphonia (pure Rust; handles mp3/m4a/aac/flac/ogg/wav), wrap that PCM in a
//! plain WAV in memory, and hand the WAV to macroquad's backend — which can
//! only decode WAV/OGG itself. This lets us play the formats people actually
//! have. Playback loops; moving away stops it; one clip at a time.
//!
//! Images: a small thumbnail is decoded from the file bytes and shown in the
//! info card. Decoding is synchronous and the result is cached by path so we
//! don't re-decode every frame.

use macroquad::audio::{load_sound_from_bytes, play_sound, stop_sound, PlaySoundParams, Sound};
use macroquad::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Audio extensions we will auto-play. We decode these ourselves via Symphonia,
/// so the list reflects what Symphonia (with our enabled features) supports.
const AUDIO_EXT: &[&str] = &[
    "wav", "ogg", "oga", "mp3", "m4a", "aac", "flac", "alac", "mp4",
];
/// Recognized image extensions we will preview (matches the `image` crate
/// features we enabled, so a hover actually produces a thumbnail).
const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp"];

pub fn is_audio(path: &Path) -> bool {
    has_ext(path, AUDIO_EXT)
}
pub fn is_image(path: &Path) -> bool {
    has_ext(path, IMAGE_EXT)
}

fn has_ext(path: &Path, list: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .map(|e| list.contains(&e.as_str()))
        .unwrap_or(false)
}

pub struct MediaState {
    /// Currently playing sound + the path it came from.
    current: Option<(PathBuf, Sound)>,
    /// Decoded image thumbnails, cached by path. `None` = tried and failed.
    thumbs: HashMap<PathBuf, Option<Texture2D>>,
    /// In-flight audio decode (background thread), if any.
    pending_audio: Option<PendingAudio>,
}

/// A background audio decode: the worker decodes to WAV bytes and sends them
/// back; the main thread (which owns the audio context) does the actual play.
struct PendingAudio {
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<Result<Vec<u8>, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for PendingAudio {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Default for MediaState {
    fn default() -> Self {
        MediaState {
            current: None,
            thumbs: HashMap::new(),
            pending_audio: None,
        }
    }
}

impl MediaState {
    /// True if the given path is the clip currently playing.
    pub fn is_playing(&self, path: &Path) -> bool {
        self.current
            .as_ref()
            .map(|(p, _)| p.as_path() == path)
            .unwrap_or(false)
    }

    /// True if we're already playing OR decoding the given path — so the
    /// caller knows not to request it again.
    pub fn is_active(&self, path: &Path) -> bool {
        self.is_playing(path)
            || self
                .pending_audio
                .as_ref()
                .map(|p| p.path.as_path() == path)
                .unwrap_or(false)
    }

    /// Request that `path` start playing. Decoding happens on a background
    /// thread so the UI never stalls; the sound actually starts once
    /// `poll_audio` (called each frame) receives the decoded data. Requesting
    /// a new path cancels any in-flight decode and stops current playback.
    pub fn request_audio(&mut self, path: &Path) {
        if self.is_active(path) {
            return;
        }
        self.stop(); // also drops any pending decode (cancels its thread)

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_thread = cancel.clone();
        let path_owned = path.to_path_buf();
        let path_for_thread = path_owned.clone();

        std::thread::spawn(move || {
            // Bail early if already cancelled (rapid hover changes).
            if cancel_thread.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let result = decode_to_wav(&path_for_thread);
            let _ = tx.send(result); // ignore if receiver gone
        });

        self.pending_audio = Some(PendingAudio {
            path: path_owned,
            rx,
            cancel,
        });
    }

    /// Check whether a background audio decode has finished; if so, play it.
    /// Call once per frame. This runs on the main thread, so the audio-context
    /// calls (`load_sound_from_bytes`, `play_sound`) are safe here.
    pub async fn poll_audio(&mut self) {
        let (path, result) = match &self.pending_audio {
            Some(p) => match p.rx.try_recv() {
                Ok(res) => (p.path.clone(), res),
                Err(std::sync::mpsc::TryRecvError::Empty) => return, // still decoding
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending_audio = None;
                    return;
                }
            },
            None => return,
        };
        self.pending_audio = None;

        match result {
            Ok(wav) => match load_sound_from_bytes(&wav).await {
                Ok(sound) => {
                    play_sound(
                        &sound,
                        PlaySoundParams {
                            looped: true,
                            volume: 0.7,
                        },
                    );
                    eprintln!("[audio] playing {}", path.display());
                    self.current = Some((path, sound));
                }
                Err(e) => eprintln!("[audio] backend rejected WAV for {}: {e:?}", path.display()),
            },
            Err(e) => eprintln!("[audio] cannot play {}: {e}", path.display()),
        }
    }

    /// Stop whatever is playing, and cancel any in-flight decode.
    pub fn stop(&mut self) {
        if let Some((_, sound)) = self.current.take() {
            stop_sound(&sound);
        }
        self.pending_audio = None; // Drop cancels the worker
    }

    /// Get (decoding + caching if needed) a thumbnail texture for an image.
    /// Returns `None` if the file isn't a decodable image. Image decode is
    /// fast and cached, so it stays on the main thread; the cache means we
    /// only ever pay the cost once per file.
    pub fn thumbnail(&mut self, path: &Path) -> Option<Texture2D> {
        if let Some(cached) = self.thumbs.get(path) {
            return cached.clone();
        }
        let tex = decode_thumbnail(path);
        self.thumbs.insert(path.to_path_buf(), tex.clone());
        tex
    }
}

/// Decode any supported audio file to a 16-bit PCM WAV held in memory.
///
/// Uses Symphonia for the format/codec work, then serializes the interleaved
/// samples as a canonical RIFF/WAVE so macroquad's simpler decoder can play it.
fn decode_to_wav(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Give the prober a hint from the file extension (optional but helps).
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported format: {e}"))?;
    let mut format = probed.format;

    // Pick the first track with a real codec.
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no decodable audio track".to_string())?;
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("unsupported codec: {e}"))?;

    let mut sample_rate: u32 = track.codec_params.sample_rate.unwrap_or(44_100);
    let mut channels: u16 = track
        .codec_params
        .channels
        .map(|c| c.count() as u16)
        .unwrap_or(2);

    // Accumulate interleaved i16 samples.
    let mut pcm: Vec<i16> = Vec::new();
    // A reusable interleaving buffer, allocated once we know the spec.
    let mut sample_buf: Option<SampleBuffer<i16>> = None;
    // Stop runaway memory on very long files (~12 min stereo @ 44.1k).
    const MAX_SAMPLES: usize = 64_000_000;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(_) => break, // end of stream (or read error) — stop decoding
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                sample_rate = spec.rate;
                channels = spec.channels.count() as u16;

                // (Re)create the conversion buffer if needed, sized to capacity.
                if sample_buf.is_none() {
                    let dur = decoded.capacity() as u64;
                    sample_buf = Some(SampleBuffer::<i16>::new(dur, spec));
                }
                if let Some(buf) = sample_buf.as_mut() {
                    // Converts any sample format to interleaved i16 for us.
                    buf.copy_interleaved_ref(decoded);
                    pcm.extend_from_slice(buf.samples());
                }
                if pcm.len() > MAX_SAMPLES {
                    break;
                }
            }
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue, // skip bad packet
            Err(_) => break,
        }
    }

    if pcm.is_empty() {
        return Err("produced no audio samples".into());
    }
    Ok(encode_wav(&pcm, sample_rate, channels))
}

/// Serialize interleaved 16-bit PCM as a canonical RIFF/WAVE byte stream.
fn encode_wav(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let data_len = (samples.len() * 2) as u32;
    let riff_len = 36 + data_len;

    let mut v = Vec::with_capacity(44 + samples.len() * 2);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&riff_len.to_le_bytes());
    v.extend_from_slice(b"WAVE");
    // fmt chunk
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    v.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&byte_rate.to_le_bytes());
    v.extend_from_slice(&block_align.to_le_bytes());
    v.extend_from_slice(&bits_per_sample.to_le_bytes());
    // data chunk
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// Decode an image file into a GPU texture, set to smooth filtering. Returns
/// `None` on any failure (unsupported format, unreadable, etc.).
/// Decode an image file into a GPU texture, downscaled to a sane preview size.
/// Returns `None` on any failure. We decode with the `image` crate (not
/// macroquad's `from_file_with_format`, which *panics* on formats like JPEG
/// that its bundled decoder doesn't support), then upload raw RGBA via
/// `from_rgba8`, which does no format detection and can't panic.
fn decode_thumbnail(path: &Path) -> Option<Texture2D> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 64 * 1024 * 1024 {
        return None;
    }

    // Decode to a dynamic image; returns Err (not panic) on unsupported/corrupt.
    let img = image::load_from_memory(&bytes).ok()?;

    // Downscale so a huge photo doesn't become a huge GPU texture. The info
    // card shows it at <=260x200, so a 512px-max thumbnail is plenty.
    const MAX_DIM: u32 = 512;
    let img = if img.width() > MAX_DIM || img.height() > MAX_DIM {
        img.thumbnail(MAX_DIM, MAX_DIM) // fast, preserves aspect ratio
    } else {
        img
    };

    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
        return None;
    }

    let tex = Texture2D::from_rgba8(w as u16, h as u16, &rgba.into_raw());
    tex.set_filter(FilterMode::Linear);
    Some(tex)
}
