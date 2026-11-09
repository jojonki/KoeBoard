//! Recognizer lifecycle: runs on one worker thread, so loading never blocks the UI.

use crate::audio;
use crate::config::{Config, EnterMode};
use crate::models::{self, ModelSpec};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Offline models are validated on clips of up to about 30 s.
const MAX_SEGMENT_SECONDS: f32 = 25.0;
/// Keeps the first and last phonemes away from the edge of the input.
const EDGE_PADDING_SECONDS: f32 = 0.15;

fn without_trailing_punctuation(text: &str) -> &str {
    text.trim_end_matches(|c: char| c.is_whitespace() || "。．.、，,！!？?…".contains(c))
}

/// `text` without its last characters when they equal `suffix`, ignoring case.
fn strip_suffix_ignoring_case<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let mut end = text.len();
    let mut from_end = text.char_indices().rev();
    for expected in suffix.chars().rev() {
        let (index, actual) = from_end.next()?;
        if !actual.to_lowercase().eq(expected.to_lowercase()) {
            return None;
        }
        end = index;
    }
    Some(&text[..end])
}

/// Returns the text to insert and whether to press Enter after it.
fn apply_enter_mode<'a>(text: &'a str, mode: EnterMode, keyword: &str) -> (&'a str, bool) {
    match mode {
        EnterMode::Off => (text, false),
        EnterMode::Always => (text, true),
        EnterMode::Keyword => {
            let keyword = without_trailing_punctuation(keyword.trim());
            if keyword.is_empty() {
                return (text, false);
            }
            match strip_suffix_ignoring_case(without_trailing_punctuation(text), keyword) {
                // Keeps a sentence's "。" but not a comma left dangling before the keyword.
                Some(body) => (body.trim_end_matches(|c: char| c.is_whitespace() || "、，,".contains(c)), true),
                None => (text, false),
            }
        }
    }
}

pub enum Job {
    Warm,
    Transcribe { samples: Vec<f32>, rate: u32 },
    /// New settings; reloads the model only if the model or thread count changed.
    Configure(Box<Config>),
    Download(String),
    Delete(String),
}

#[derive(Debug)]
pub enum Status {
    Downloading(u8),
    Loading,
    Ready,
    /// Sent just before pasting. Display only; never log it.
    /// The flag says Enter will follow.
    Recognized(String, bool),
    EnterPressed,
    /// One transcription finished; `Notice` or `Error` may have preceded it.
    Done,
    Notice(String),
    Error(String),
    NeedsModel,
    Installed { id: String, selected: bool },
    DownloadFailed(String),
    Removed(String),
}

pub struct Engine {
    recognizer: OfflineRecognizer,
    joiner: &'static str,
}

impl Engine {
    pub fn load(dir: &Path, spec: &ModelSpec, threads: i32) -> Result<Self, String> {
        let path = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());
        let mut cfg = OfflineRecognizerConfig::default();
        cfg.feat_config.sample_rate = 16_000;
        cfg.feat_config.feature_dim = spec.feature_dim;
        cfg.model_config.transducer.encoder = path(spec.encoder);
        cfg.model_config.transducer.decoder = path(spec.decoder);
        cfg.model_config.transducer.joiner = path(spec.joiner);
        cfg.model_config.tokens = path(spec.tokens);
        cfg.model_config.num_threads = threads;
        cfg.model_config.provider = Some("cpu".into());
        if !spec.model_type.is_empty() {
            cfg.model_config.model_type = Some(spec.model_type.into());
        }
        cfg.decoding_method = Some("greedy_search".into());
        let recognizer = OfflineRecognizer::create(&cfg)
            .ok_or_else(|| format!("Cannot load model: {}", spec.id))?;
        // The first decode is much slower; absorb that here rather than on the user's first words.
        let warmup = recognizer.create_stream();
        warmup.accept_waveform(16_000, &vec![0.0; 16_000]);
        recognizer.decode(&warmup);
        Ok(Self { recognizer, joiner: spec.segment_joiner })
    }

    /// Returns the recognized text, empty when the audio holds no speech.
    pub fn transcribe(&self, samples: &[f32], rate: u32) -> String {
        let Some(range) = audio::speech_range(samples, rate) else {
            return String::new();
        };
        let speech = &samples[range];
        let padding = vec![0.0f32; (rate as f32 * EDGE_PADDING_SECONDS) as usize];
        let mut parts = Vec::new();
        for segment in audio::split_segments(speech, rate, MAX_SEGMENT_SECONDS) {
            let stream = self.recognizer.create_stream();
            let mut input = Vec::with_capacity(segment.len() + 2 * padding.len());
            input.extend_from_slice(&padding);
            input.extend_from_slice(&speech[segment]);
            input.extend_from_slice(&padding);
            stream.accept_waveform(rate as i32, &input);
            self.recognizer.decode(&stream);
            if let Some(r) = stream.get_result() {
                let text = r.text.trim();
                if !text.is_empty() {
                    parts.push(text.to_owned());
                }
            }
        }
        parts.join(self.joiner)
    }
}

pub struct Worker {
    pub root: PathBuf,
    pub config: Config,
    /// Delivers recognized text to the focused app.
    pub deliver: Box<dyn Fn(&str, bool) -> Result<Option<String>, String> + Send>,
    pub status: Box<dyn Fn(Status) + Send>,
}

impl Worker {
    pub fn run(mut self, jobs: Receiver<Job>) {
        let mut engine: Option<(String, Engine)> = None;
        loop {
            let idle = match self.config.idle_unload_minutes {
                0 => None,
                m => Some(Duration::from_secs(m * 60)),
            };
            let job = match (idle, engine.is_some()) {
                (Some(limit), true) => match jobs.recv_timeout(limit) {
                    Ok(j) => j,
                    Err(RecvTimeoutError::Timeout) => {
                        engine = None;
                        crate::log("model unloaded after idle timeout");
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                _ => match jobs.recv() {
                    Ok(j) => j,
                    Err(_) => return,
                },
            };
            match job {
                Job::Warm => {
                    if !self.current_model_installed() {
                        (self.status)(Status::NeedsModel);
                    } else if let Err(e) = self.ensure_engine(&mut engine) {
                        (self.status)(Status::Error(e));
                    }
                }
                Job::Download(id) => self.download(&id, &mut engine),
                Job::Delete(id) => self.delete(&id, &mut engine),
                Job::Configure(config) => {
                    let reload = config.model != self.config.model || config.threads != self.config.threads;
                    self.config = *config;
                    if !reload {
                        continue;
                    }
                    engine = None;
                    if !self.current_model_installed() {
                        (self.status)(Status::NeedsModel);
                        continue;
                    }
                    if let Err(e) = self.ensure_engine(&mut engine) {
                        (self.status)(Status::Error(e));
                    }
                }
                Job::Transcribe { samples, rate } => {
                    self.transcribe(&mut engine, &samples, rate);
                    (self.status)(Status::Done);
                }
            }
        }
    }

    fn current_model_installed(&self) -> bool {
        models::id_is_installed(&self.root, &self.config.model)
    }

    fn download(&mut self, id: &str, engine: &mut Option<(String, Engine)>) {
        let Some(spec) = models::find(id) else { return };
        let status = &self.status;
        if let Err(e) = models::ensure_installed(&self.root, spec, |p| status(Status::Downloading(p))) {
            status(Status::Error(e));
            status(Status::DownloadFailed(id.to_owned()));
            return;
        }
        let selected = !self.current_model_installed();
        if selected {
            self.config.model = id.to_owned();
        }
        (self.status)(Status::Installed { id: id.to_owned(), selected });
        if selected {
            if let Err(e) = self.ensure_engine(engine) {
                (self.status)(Status::Error(e));
            }
        } else {
            (self.status)(Status::Ready);
        }
    }

    fn delete(&self, id: &str, engine: &mut Option<(String, Engine)>) {
        let Some(spec) = models::find(id) else { return };
        if engine.as_ref().is_some_and(|(loaded, _)| loaded == id) {
            *engine = None;
        }
        if let Err(e) = models::remove(&self.root, spec) {
            (self.status)(Status::Error(e));
            return;
        }
        (self.status)(Status::Removed(id.to_owned()));
        if !self.current_model_installed() {
            (self.status)(Status::NeedsModel);
        }
    }

    fn ensure_engine(&self, slot: &mut Option<(String, Engine)>) -> Result<(), String> {
        if slot.as_ref().is_some_and(|(id, _)| *id == self.config.model) {
            return Ok(());
        }
        *slot = None;
        let spec = models::find(&self.config.model)
            .ok_or_else(|| format!("Unknown model: {}", self.config.model))?;
        let status = &self.status;
        let dir = models::model_dir(&self.root, spec);
        status(Status::Loading);
        let started = Instant::now();
        let engine = Engine::load(&dir, spec, self.config.thread_count())?;
        crate::log(&format!("loaded {} in {:?}", spec.id, started.elapsed()));
        *slot = Some((spec.id.to_owned(), engine));
        status(Status::Ready);
        Ok(())
    }

    fn transcribe(&self, slot: &mut Option<(String, Engine)>, samples: &[f32], rate: u32) {
        if samples.is_empty() {
            return;
        }
        if !self.current_model_installed() {
            (self.status)(Status::NeedsModel);
            (self.status)(Status::Notice("Download a model in Settings".into()));
            return;
        }
        if let Err(e) = self.ensure_engine(slot) {
            (self.status)(Status::Error(e));
            return;
        }
        let Some((_, engine)) = slot.as_ref() else { return };
        let started = Instant::now();
        let text = engine.transcribe(samples, rate);
        // Timing only: recognized text never goes to the log.
        crate::log(&format!(
            "transcribed {:.1}s audio in {:?}",
            samples.len() as f32 / rate as f32,
            started.elapsed()
        ));
        if text.is_empty() {
            (self.status)(Status::Notice("No speech detected. Check the microphone in Settings".into()));
            return;
        }
        let (body, enter) = apply_enter_mode(&text, self.config.enter_mode, &self.config.enter_keyword);
        // Saying only the keyword presses Enter with nothing to show.
        if !body.is_empty() {
            (self.status)(Status::Recognized(body.to_owned(), enter));
        }
        match (self.deliver)(body, enter) {
            Ok(Some(notice)) => (self.status)(Status::Notice(notice)),
            Ok(None) if enter => (self.status)(Status::EnterPressed),
            Ok(None) => {}
            Err(e) => (self.status)(Status::Error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyword(text: &str) -> (&str, bool) {
        apply_enter_mode(text, EnterMode::Keyword, "オーバー")
    }

    #[test]
    fn off_and_always_keep_the_text() {
        assert_eq!(apply_enter_mode("了解オーバー", EnterMode::Off, "オーバー"), ("了解オーバー", false));
        assert_eq!(apply_enter_mode("了解オーバー", EnterMode::Always, "オーバー"), ("了解オーバー", true));
    }

    #[test]
    fn keyword_at_the_end_becomes_enter() {
        assert_eq!(keyword("了解です。オーバー"), ("了解です。", true));
        assert_eq!(keyword("了解です。オーバー。"), ("了解です。", true));
        assert_eq!(keyword("了解です、オーバー。 "), ("了解です", true));
        assert_eq!(keyword("オーバー"), ("", true));
    }

    #[test]
    fn keyword_elsewhere_is_left_alone() {
        assert_eq!(keyword("オーバーは着ません"), ("オーバーは着ません", false));
        assert_eq!(keyword("了解"), ("了解", false));
    }

    #[test]
    fn custom_keywords() {
        assert_eq!(apply_enter_mode("hello Send.", EnterMode::Keyword, "send"), ("hello", true));
        assert_eq!(apply_enter_mode("お願い送信", EnterMode::Keyword, " 送信。"), ("お願い", true));
        assert_eq!(apply_enter_mode("お願い", EnterMode::Keyword, "  "), ("お願い", false));
    }
}
