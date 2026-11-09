//! Microphone capture and the signal helpers that prepare audio for the recognizer.

use crate::config::Config;
use crate::engine::Job;
use crate::overlay::{self, Level};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Keeps capturing briefly after stop so the last syllable is not cut off.
const POST_ROLL: Duration = Duration::from_millis(120);

enum Cmd {
    Start,
    Stop,
    SetDevice(String),
}

pub struct Recorder {
    tx: Sender<Cmd>,
}

impl Recorder {
    /// `on_error` is called with a user-facing message when capture fails.
    pub fn spawn(cfg: &Config, jobs: Sender<Job>, level: Level, on_error: impl Fn(String) + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let keep_open = cfg.keep_mic_open;
        let device = cfg.microphone.clone();
        // cpal streams are not Send on every platform, so one thread owns the stream.
        std::thread::spawn(move || run(rx, jobs, level, keep_open, device, on_error));
        Self { tx }
    }

    pub fn start(&self) {
        let _ = self.tx.send(Cmd::Start);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    /// Applies from the next recording. An empty ID follows the system default.
    pub fn set_device(&self, id: String) {
        let _ = self.tx.send(Cmd::SetDevice(id));
    }
}

/// The input devices as `(id, name)`, skipping any that disappear while listing.
pub fn list_inputs() -> Vec<(String, String)> {
    let devices = match cpal::default_host().input_devices() {
        Ok(devices) => devices,
        Err(e) => {
            crate::log(&format!("error: list microphones: {e}"));
            return Vec::new();
        }
    };
    devices
        .filter_map(|d| Some((d.id().ok()?.to_string(), d.description().ok()?.name().to_owned())))
        .collect()
}

struct Capture {
    stream: cpal::Stream,
    device: String,
    rate: u32,
    channels: usize,
    device_failed: Arc<AtomicBool>,
}

fn run(rx: Receiver<Cmd>, jobs: Sender<Job>, level: Level, keep_open: bool, mut device: String, on_error: impl Fn(String)) {
    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
    let active = Arc::new(AtomicBool::new(false));
    let mut capture: Option<Capture> = None;
    let mut open_error: Option<String> = None;

    for cmd in rx {
        match cmd {
            Cmd::Start => {
                if capture
                    .as_ref()
                    .is_some_and(|c| c.device_failed.load(Ordering::Relaxed) || c.device != device)
                {
                    capture = None;
                }
                if capture.is_none() {
                    match open(&device, buffer.clone(), active.clone(), level.clone()) {
                        Ok(c) => capture = Some(c),
                        Err(e) => {
                            let message = format!("Cannot open the microphone: {e}");
                            on_error(message.clone());
                            open_error = Some(message);
                            continue;
                        }
                    }
                }
                buffer.lock().unwrap().clear();
                active.store(true, Ordering::Release);
            }
            Cmd::Stop => {
                if capture.is_some() {
                    std::thread::sleep(POST_ROLL);
                }
                active.store(false, Ordering::Release);
                overlay::store_level(&level, 0.0);
                // Stopping replaces the panel, so the failure must outlast that.
                if let Some(message) = open_error.take() {
                    on_error(message);
                }
                let rate = capture.as_ref().map_or(16_000, |c| c.rate);
                let capture_channels = capture.as_ref().map_or(1, |c| c.channels);
                if !keep_open {
                    if let Some(c) = capture.take() {
                        let _ = c.stream.pause();
                    }
                }
                let channels = capture_channels;
                let interleaved = std::mem::take(&mut *buffer.lock().unwrap());
                let mut samples = loudest_channel(&interleaved, channels);
                let peak = peak_of(&samples);
                crate::log(&format!("captured channels={channels} rate={rate} peak={peak:.4}"));
                lift_quiet_input(&mut samples);
                let _ = jobs.send(Job::Transcribe { samples, rate });
            }
            Cmd::SetDevice(id) => device = id,
        }
    }
}

fn select_device(id: &str) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    if !id.is_empty() {
        match id.parse::<cpal::DeviceId>().ok().and_then(|id| host.device_by_id(&id)) {
            Some(device) => return Ok(device),
            None => crate::log("selected microphone is unavailable; using the system default"),
        }
    }
    host.default_input_device().ok_or_else(|| "No input device found".to_owned())
}

fn open(device_id: &str, buffer: Arc<Mutex<Vec<f32>>>, active: Arc<AtomicBool>, level: Level) -> Result<Capture, String> {
    let device = select_device(device_id)?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let config = supported.config();
    let rate = config.sample_rate;
    let channels = config.channels as usize;
    let device_failed = Arc::new(AtomicBool::new(false));
    let failed = device_failed.clone();
    let on_err = move |e| {
        crate::log(&format!("error: microphone stream: {e}"));
        failed.store(true, Ordering::Relaxed);
    };
    let name = device.description().map_or_else(|_| "unknown".to_owned(), |d| d.name().to_owned());
    crate::log(&format!(
        "microphone: {name} rate={rate} channels={channels} format={:?}",
        supported.sample_format()
    ));

    macro_rules! build {
        ($t:ty) => {
            device.build_input_stream::<$t, _, _>(
                config,
                callback::<$t>(buffer, active, level),
                on_err,
                None,
            )
        };
    }
    use cpal::SampleFormat as F;
    let stream = match supported.sample_format() {
        F::F32 => build!(f32),
        F::I16 => build!(i16),
        F::U16 => build!(u16),
        F::I32 => build!(i32),
        other => return Err(format!("Unsupported sample format: {other:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Capture { stream, device: device_id.to_owned(), rate, channels, device_failed })
}

fn callback<T>(
    buffer: Arc<Mutex<Vec<f32>>>,
    active: Arc<AtomicBool>,
    level: Level,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo) + Send + 'static
where
    T: SizedSample,
    f32: FromSample<T>,
{
    move |data, _| {
        if !active.load(Ordering::Acquire) {
            return;
        }
        let mut out = buffer.lock().unwrap();
        let start = out.len();
        out.extend(data.iter().map(|s| s.to_sample::<f32>()));
        overlay::store_level(&level, peak_of(&out[start..]));
    }
}

/// Audio interfaces often carry the microphone on one of several channels,
/// so averaging them would dilute or cancel the voice.
fn loudest_channel(interleaved: &[f32], channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    let energy = |c: usize| -> f32 { interleaved.iter().skip(c).step_by(channels).map(|s| s * s).sum() };
    let best = (0..channels).max_by(|a, b| energy(*a).total_cmp(&energy(*b))).unwrap_or(0);
    interleaved.iter().skip(best).step_by(channels).copied().collect()
}

fn peak_of(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0, |m, s| m.max(s.abs()))
}

/// Raises low-gain microphones toward the level the models were trained on.
fn lift_quiet_input(samples: &mut [f32]) {
    const SILENCE_PEAK: f32 = 0.02;
    const TARGET_PEAK: f32 = 0.5;
    const MAX_GAIN: f32 = 10.0;
    let peak = peak_of(samples);
    if peak < SILENCE_PEAK || peak >= TARGET_PEAK {
        return;
    }
    let gain = (TARGET_PEAK / peak).min(MAX_GAIN);
    samples.iter_mut().for_each(|s| *s *= gain);
}

const FRAME_SECONDS: f32 = 0.02;
const SPEECH_RMS: f32 = 0.004;
const MIN_SPEECH_SECONDS: f32 = 0.12;
const TRIM_MARGIN_SECONDS: f32 = 0.25;

fn frame_len(rate: u32) -> usize {
    ((rate as f32 * FRAME_SECONDS) as usize).max(1)
}

fn frame_rms(samples: &[f32], frame: usize) -> Vec<f32> {
    samples
        .chunks(frame)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect()
}

/// The part of `samples` containing speech plus a margin, or `None` for silence.
pub fn speech_range(samples: &[f32], rate: u32) -> Option<Range<usize>> {
    let frame = frame_len(rate);
    let rms = frame_rms(samples, frame);
    let is_speech = |r: &&f32| **r >= SPEECH_RMS;
    let voiced = rms.iter().filter(is_speech).count();
    if (voiced as f32) * FRAME_SECONDS < MIN_SPEECH_SECONDS {
        return None;
    }
    let first = rms.iter().position(|r| *r >= SPEECH_RMS)?;
    let last = rms.iter().rposition(|r| *r >= SPEECH_RMS)?;
    let margin = (rate as f32 * TRIM_MARGIN_SECONDS) as usize;
    let start = (first * frame).saturating_sub(margin);
    let end = ((last + 1) * frame + margin).min(samples.len());
    Some(start..end)
}

/// Splits long audio at its quietest points so no segment exceeds `max_seconds`.
pub fn split_segments(samples: &[f32], rate: u32, max_seconds: f32) -> Vec<Range<usize>> {
    let max_len = (rate as f32 * max_seconds) as usize;
    let frame = frame_len(rate);
    let mut ranges = Vec::new();
    let mut start = 0;
    while samples.len() - start > max_len {
        // Search the last 40% of the window, so segments stay reasonably long.
        let search_from = start + max_len * 6 / 10;
        let window = &samples[search_from..start + max_len];
        let rms = frame_rms(window, frame);
        let quietest = rms
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(i, _)| i);
        let cut = search_from + quietest * frame + frame / 2;
        let cut = cut.min(start + max_len);
        ranges.push(start..cut);
        start = cut;
    }
    if start < samples.len() {
        ranges.push(start..samples.len());
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn tone(seconds: f32, amp: f32) -> Vec<f32> {
        (0..(RATE as f32 * seconds) as usize)
            .map(|i| amp * (i as f32 * 0.2).sin())
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (RATE as f32 * seconds) as usize]
    }

    #[test]
    fn picks_the_channel_carrying_the_voice() {
        // Four channels, the voice on the second only.
        let interleaved: Vec<f32> = (0..100).flat_map(|i| [0.0, i as f32 / 100.0, 0.0, 0.0]).collect();
        let mono = loudest_channel(&interleaved, 4);
        assert_eq!(mono.len(), 100);
        assert_eq!(mono[50], 0.5);
        assert_eq!(loudest_channel(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn lifts_quiet_speech_but_not_silence() {
        let mut quiet = vec![0.05, -0.1];
        lift_quiet_input(&mut quiet);
        assert!((peak_of(&quiet) - 0.5).abs() < 1e-5);
        let mut noise = vec![0.005, -0.01];
        lift_quiet_input(&mut noise);
        assert_eq!(noise, vec![0.005, -0.01]);
        let mut loud = vec![0.9];
        lift_quiet_input(&mut loud);
        assert_eq!(loud, vec![0.9]);
    }

    #[test]
    fn silence_and_blips_are_not_speech() {
        assert_eq!(speech_range(&silence(2.0), RATE), None);
        assert_eq!(speech_range(&[], RATE), None);
        let mut blip = silence(1.0);
        blip.extend(tone(0.04, 0.5));
        assert_eq!(speech_range(&blip, RATE), None);
    }

    #[test]
    fn trims_silence_with_margin() {
        let mut a = silence(2.0);
        a.extend(tone(1.0, 0.3));
        a.extend(silence(2.0));
        let r = speech_range(&a, RATE).unwrap();
        assert!((1.7..1.8).contains(&(r.start as f32 / RATE as f32)), "{r:?}");
        assert!((3.2..3.3).contains(&(r.end as f32 / RATE as f32)), "{r:?}");
    }

    #[test]
    fn short_audio_is_one_segment() {
        let a = tone(5.0, 0.3);
        assert_eq!(split_segments(&a, RATE, 25.0), vec![0..a.len()]);
    }

    #[test]
    fn exact_max_length_is_one_segment() {
        let a = tone(25.0, 0.3);
        assert_eq!(split_segments(&a, RATE, 25.0).len(), 1);
    }

    #[test]
    fn splits_at_silence_and_covers_everything() {
        let mut a = tone(20.0, 0.3);
        a.extend(silence(0.5));
        a.extend(tone(15.0, 0.3));
        let r = split_segments(&a, RATE, 25.0);
        assert_eq!(r.len(), 2);
        let cut = r[0].end as f32 / RATE as f32;
        assert!((20.0..20.5).contains(&cut), "cut at {cut}");
        assert_eq!(r[0].start, 0);
        assert_eq!(r[0].end, r[1].start);
        assert_eq!(r[1].end, a.len());
    }

    #[test]
    fn very_long_audio_without_silence_terminates() {
        let a = tone(130.0, 0.3);
        let r = split_segments(&a, RATE, 25.0);
        assert!(r.iter().all(|x| x.len() <= 25 * RATE as usize));
        assert_eq!(r.last().unwrap().end, a.len());
    }
}
