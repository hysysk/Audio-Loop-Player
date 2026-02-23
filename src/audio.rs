use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleRate, Stream, StreamConfig};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{DecoderOptions, CODEC_TYPE_NULL},
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};
use symphonia::default::{get_codecs, get_probe};

// ─── Audio data ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AudioData {
    pub samples: Arc<Vec<f32>>, // interleaved: [L0, R0, L1, R1, ...]
    pub channels: usize,
    pub sample_rate: u32,
    pub total_frames: u64,
    pub duration_secs: f64,
}

// ─── Waveform peaks ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
pub struct Peak {
    pub min: f32,
    pub max: f32,
}

/// Compute `num_peaks` (min, max) pairs from interleaved samples.
/// Mixes all channels to mono for display.
pub fn compute_waveform_peaks(
    samples: &[f32],
    channels: usize,
    num_peaks: usize,
) -> Vec<Peak> {
    if samples.is_empty() || channels == 0 || num_peaks == 0 {
        return vec![Peak::default(); num_peaks];
    }
    let total_frames = samples.len() / channels;
    let frames_per_peak = ((total_frames as f64 / num_peaks as f64).ceil() as usize).max(1);

    (0..num_peaks)
        .map(|i| {
            let start = i * frames_per_peak;
            let end = ((i + 1) * frames_per_peak).min(total_frames);
            if start >= total_frames {
                return Peak::default();
            }
            let mut min_v = f32::INFINITY;
            let mut max_v = f32::NEG_INFINITY;
            for frame in start..end {
                let mut mixed = 0.0f32;
                for ch in 0..channels {
                    mixed += samples[frame * channels + ch];
                }
                mixed /= channels as f32;
                min_v = min_v.min(mixed);
                max_v = max_v.max(mixed);
            }
            if min_v == f32::INFINITY {
                Peak::default()
            } else {
                Peak { min: min_v, max: max_v }
            }
        })
        .collect()
}

// ─── Shared playback state (atomics, no mutex in audio hot path) ─────────────

pub struct PlaybackAtomics {
    /// Current frame position encoded as f64 bits in u64
    pub position: AtomicU64,
    pub playing: AtomicBool,
    pub loop_enabled: AtomicBool,
    /// Loop start frame (f64 bits)
    pub loop_start: AtomicU64,
    /// Loop end frame (f64 bits)
    pub loop_end: AtomicU64,
    /// Volume: f32 bits in u32 (range 0.0–2.0)
    pub volume: AtomicU32,
    /// Playback speed: f32 bits in u32 (range 0.25–2.0)
    pub speed: AtomicU32,
    /// Pending seek: f64 bits, u64::MAX = no pending seek
    pub seek_to: AtomicU64,
}

impl PlaybackAtomics {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            position: AtomicU64::new(0f64.to_bits()),
            playing: AtomicBool::new(false),
            loop_enabled: AtomicBool::new(false),
            loop_start: AtomicU64::new(0f64.to_bits()),
            loop_end: AtomicU64::new(0f64.to_bits()),
            volume: AtomicU32::new(1.0f32.to_bits()),
            speed: AtomicU32::new(1.0f32.to_bits()),
            seek_to: AtomicU64::new(u64::MAX),
        })
    }
}

// ─── Buffer info protected by Mutex (only accessed briefly in callback) ──────

struct BufferInfo {
    samples: Arc<Vec<f32>>,
    channels: usize,
    sample_rate: u32,
    total_frames: u64,
}

// ─── Public engine state snapshot ────────────────────────────────────────────

pub struct EngineState {
    pub position_secs: f64,
    pub is_playing: bool,
}

// ─── AudioEngine ─────────────────────────────────────────────────────────────

pub struct AudioEngine {
    pub atomics: Arc<PlaybackAtomics>,
    buffer: Arc<Mutex<Option<BufferInfo>>>,
    // Keep stream alive; dropping it stops audio
    _stream: Stream,
}

impl AudioEngine {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("No output audio device found"))?;

        let supported = device.default_output_config()?;
        let out_sample_rate = supported.sample_rate().0;
        let out_channels = supported.channels() as usize;

        let config = StreamConfig {
            channels: supported.channels(),
            sample_rate: SampleRate(out_sample_rate),
            buffer_size: BufferSize::Default,
        };

        let atomics = PlaybackAtomics::new();
        let buffer: Arc<Mutex<Option<BufferInfo>>> = Arc::new(Mutex::new(None));

        let atomics_cb = Arc::clone(&atomics);
        let buffer_cb = Arc::clone(&buffer);
        let out_sr = out_sample_rate;
        let out_ch = out_channels;

        let stream = device.build_output_stream(
            &config,
            move |output: &mut [f32], _info| {
                audio_callback(output, out_ch, out_sr, &atomics_cb, &buffer_cb);
            },
            |err| eprintln!("Audio stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            atomics,
            buffer,
            _stream: stream,
        })
    }

    /// Load decoded audio into the engine and reset playback.
    pub fn load(&self, data: &AudioData) {
        // Stop playback first
        self.atomics.playing.store(false, Ordering::SeqCst);
        self.atomics.position.store(0f64.to_bits(), Ordering::SeqCst);
        self.atomics.loop_enabled.store(false, Ordering::SeqCst);
        self.atomics.loop_start.store(0f64.to_bits(), Ordering::SeqCst);
        self.atomics
            .loop_end
            .store((data.total_frames as f64).to_bits(), Ordering::SeqCst);
        self.atomics.seek_to.store(u64::MAX, Ordering::SeqCst);

        let mut guard = self.buffer.lock().unwrap();
        *guard = Some(BufferInfo {
            samples: Arc::clone(&data.samples),
            channels: data.channels,
            sample_rate: data.sample_rate,
            total_frames: data.total_frames,
        });
    }

    pub fn play(&self) {
        self.atomics.playing.store(true, Ordering::SeqCst);
    }

    pub fn pause(&self) {
        self.atomics.playing.store(false, Ordering::SeqCst);
    }

    pub fn stop(&self) {
        self.atomics.playing.store(false, Ordering::SeqCst);
        self.atomics.position.store(0f64.to_bits(), Ordering::SeqCst);
    }

    /// Seek to a position in seconds.
    pub fn seek(&self, secs: f64) {
        let guard = self.buffer.lock().unwrap();
        if let Some(info) = &*guard {
            let frame = (secs * info.sample_rate as f64)
                .max(0.0)
                .min(info.total_frames as f64);
            // Write seek request; audio callback applies it on the next buffer
            self.atomics.seek_to.store(frame.to_bits(), Ordering::SeqCst);
        }
    }

    pub fn set_loop(&self, enabled: bool, start_secs: f64, end_secs: f64) {
        let guard = self.buffer.lock().unwrap();
        if let Some(info) = &*guard {
            let sr = info.sample_rate as f64;
            let total = info.total_frames as f64;
            let start = (start_secs * sr).clamp(0.0, total);
            let end = (end_secs * sr).clamp(0.0, total);
            self.atomics.loop_start.store(start.to_bits(), Ordering::Release);
            self.atomics.loop_end.store(end.to_bits(), Ordering::Release);
            self.atomics.loop_enabled.store(enabled, Ordering::Release);
        }
    }

    pub fn set_volume(&self, vol: f32) {
        self.atomics
            .volume
            .store(vol.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_speed(&self, speed: f32) {
        self.atomics
            .speed
            .store(speed.clamp(0.25, 2.0).to_bits(), Ordering::Relaxed);
    }

    /// Snapshot current state for the UI.
    pub fn state(&self) -> EngineState {
        let guard = self.buffer.lock().unwrap();
        let sr = guard
            .as_ref()
            .map(|b| b.sample_rate as f64)
            .unwrap_or(44100.0);

        let pos_frame = f64::from_bits(self.atomics.position.load(Ordering::Relaxed));

        EngineState {
            position_secs: pos_frame / sr,
            is_playing: self.atomics.playing.load(Ordering::Relaxed),
        }
    }
}

// ─── Audio callback (runs in cpal real-time thread) ──────────────────────────

fn audio_callback(
    output: &mut [f32],
    out_channels: usize,
    out_sr: u32,
    atomics: &Arc<PlaybackAtomics>,
    buffer: &Arc<Mutex<Option<BufferInfo>>>,
) {
    use Ordering::Relaxed;

    // Handle pending seek
    let seek_bits = atomics.seek_to.load(Relaxed);
    if seek_bits != u64::MAX {
        atomics.position.store(seek_bits, Relaxed);
        // Clear seek request (compare_exchange to avoid races)
        let _ = atomics
            .seek_to
            .compare_exchange(seek_bits, u64::MAX, Relaxed, Relaxed);
    }

    if !atomics.playing.load(Relaxed) {
        output.fill(0.0);
        return;
    }

    // Quickly clone the Arc<Vec<f32>> (just a refcount bump, no copy)
    let (samples_arc, buf_channels, buf_sr, total_frames) = {
        let guard = match buffer.try_lock() {
            Ok(g) => g,
            Err(_) => {
                // Main thread is loading a new file; output silence this buffer
                output.fill(0.0);
                return;
            }
        };
        match &*guard {
            Some(info) => (
                Arc::clone(&info.samples),
                info.channels,
                info.sample_rate,
                info.total_frames,
            ),
            None => {
                output.fill(0.0);
                return;
            }
        }
        // MutexGuard drops here, releasing the lock
    };

    let samples = samples_arc.as_slice();
    let volume = f32::from_bits(atomics.volume.load(Relaxed));
    let speed = f32::from_bits(atomics.speed.load(Relaxed)) as f64;
    let loop_enabled = atomics.loop_enabled.load(Relaxed);
    let loop_start = f64::from_bits(atomics.loop_start.load(Relaxed));
    let loop_end_v = f64::from_bits(atomics.loop_end.load(Relaxed));

    // Rate: how many input frames to advance per output frame
    let rate = speed * buf_sr as f64 / out_sr as f64;

    let effective_end = if loop_enabled && loop_end_v > loop_start {
        loop_end_v
    } else {
        total_frames as f64
    };

    let mut frac_pos = f64::from_bits(atomics.position.load(Relaxed));
    let out_frames = output.len() / out_channels;
    let mut stopped_at = None;

    for frame_idx in 0..out_frames {
        // Loop boundary
        if frac_pos >= effective_end {
            if loop_enabled && loop_end_v > loop_start {
                frac_pos = loop_start;
            } else {
                // End of file
                output[frame_idx * out_channels..].fill(0.0);
                stopped_at = Some(frame_idx);
                break;
            }
        }

        let pos = frac_pos as usize;
        let frac = (frac_pos - pos as f64) as f32;

        for ch in 0..out_channels {
            let in_ch = ch % buf_channels;
            let idx0 = pos * buf_channels + in_ch;
            let idx1 = (pos + 1) * buf_channels + in_ch;
            let s0 = samples.get(idx0).copied().unwrap_or(0.0);
            let s1 = samples.get(idx1).copied().unwrap_or(0.0);
            output[frame_idx * out_channels + ch] =
                (s0 * (1.0 - frac) + s1 * frac) * volume;
        }

        frac_pos += rate;
    }

    atomics.position.store(frac_pos.to_bits(), Relaxed);
    if stopped_at.is_some() {
        atomics.playing.store(false, Relaxed);
        atomics.position.store(0f64.to_bits(), Relaxed);
    }
}

// ─── Symphonia decoder ────────────────────────────────────────────────────────

pub fn decode_audio(path: &std::path::Path) -> Result<AudioData> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;

    let mut format = probed.format;

    // Find the first real audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow::anyhow!("No supported audio track found"))?;

    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(2)
        .max(1);

    let mut decoder = get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut all_samples: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let audio_buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };

        // Lazily allocate SampleBuffer on first decoded packet
        let sb = sample_buf.get_or_insert_with(|| {
            SampleBuffer::<f32>::new(audio_buf.capacity() as u64, *audio_buf.spec())
        });

        sb.copy_interleaved_ref(audio_buf);
        all_samples.extend_from_slice(sb.samples());
    }

    let total_frames = (all_samples.len() / channels) as u64;
    let duration_secs = total_frames as f64 / sample_rate as f64;

    Ok(AudioData {
        samples: Arc::new(all_samples),
        channels,
        sample_rate,
        total_frames,
        duration_secs,
    })
}
