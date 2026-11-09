#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod audio;
mod config;
mod engine;
mod gesture;
mod inject;
mod models;
mod overlay;
mod settings;

use std::fs::OpenOptions;
use std::io::Write;
use std::process::ExitCode;
use std::time::Instant;

/// Appends one diagnostic line. Never pass recognized text to this.
pub fn log(message: &str) {
    eprintln!("{message}");
    let path = config::data_dir().join("koeboard.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{message}");
    }
}

const USAGE: &str = "koeboard                            Run dictation from the tray
koeboard --transcribe <wav> [model] Transcribe a WAV file
koeboard --download <model> --accept-license
                                Download a model after reviewing its license
koeboard --test-mic                 Record 4 seconds and transcribe
koeboard --overlay-demo             Preview the panel
koeboard --models                   List models";

fn main() -> ExitCode {
    // Release builds on Windows have no console, so a dead thread would be silent.
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        // Location only: a panic message could quote recognized text.
        log(&format!("error: panic on thread {name} at {:?}", info.location()));
    }));
    config::migrate_former_name();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        [] => app::run(),
        ["--transcribe", wav] => transcribe(wav, None),
        ["--transcribe", wav, id] => transcribe(wav, Some(id)),
        ["--download", id] => show_license(id),
        ["--download", id, "--accept-license"] => download(id),
        ["--test-mic"] => test_mic(),
        ["--overlay-demo"] => app::overlay_demo(),
        ["--models"] => {
            for m in models::CATALOG {
                println!("{}\t{}\t{}\t{}", m.id, m.language, m.license, m.label);
            }
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(&format!("error: {e}"));
            ExitCode::FAILURE
        }
    }
}

fn load_config() -> Result<config::Config, String> {
    let root = config::data_dir();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    config::Config::load(&root.join("config.toml"))
}

fn resolve(id: Option<&str>, cfg: &config::Config) -> Result<&'static models::ModelSpec, String> {
    let id = id.unwrap_or(&cfg.model);
    models::find(id).ok_or_else(|| format!("Unknown model: {id}"))
}

fn installed_dir(spec: &models::ModelSpec) -> Result<std::path::PathBuf, String> {
    let dir = models::model_dir(&config::data_dir(), spec);
    if models::is_installed(&dir, spec) {
        Ok(dir)
    } else {
        Err(format!("Model {} is not downloaded. Run: koeboard --download {}", spec.id, spec.id))
    }
}

fn show_license(id: &str) -> Result<(), String> {
    let spec = models::find(id).ok_or_else(|| format!("Unknown model: {id}"))?;
    println!("{}\nLicense: {} ({})\nModel: {}\n{}", spec.label, spec.license, spec.license_url, spec.model_url, spec.license_note);
    println!("To download after reviewing the license, add --accept-license.");
    Ok(())
}

fn download(id: &str) -> Result<(), String> {
    let spec = models::find(id).ok_or_else(|| format!("Unknown model: {id}"))?;
    let mut last = 255;
    let dir = models::ensure_installed(&config::data_dir(), spec, |p| {
        if p != last && p % 10 == 0 {
            eprintln!("{p}%");
            last = p;
        }
    })?;
    println!("{}", dir.display());
    Ok(())
}

fn test_mic() -> Result<(), String> {
    let cfg = load_config()?;
    let spec = resolve(None, &cfg)?;
    let dir = installed_dir(spec)?;
    let engine = engine::Engine::load(&dir, spec, cfg.thread_count())?;

    let (tx, rx) = std::sync::mpsc::channel();
    let recorder = audio::Recorder::spawn(&cfg, tx, overlay::Level::default(), |e| eprintln!("{e}"));
    let t = Instant::now();
    recorder.start();
    eprintln!("Recording for 4 seconds...");
    std::thread::sleep(std::time::Duration::from_secs(4));
    recorder.stop();
    let engine::Job::Transcribe { samples, rate } = rx.recv().map_err(|e| e.to_string())? else {
        return Err("unexpected job".into());
    };
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    eprintln!(
        "captured {:.2}s at {rate}Hz in {:?}, peak={peak:.3}",
        samples.len() as f32 / rate as f32,
        t.elapsed()
    );
    println!("{}", engine.transcribe(&samples, rate));
    Ok(())
}

fn transcribe(wav: &str, id: Option<&str>) -> Result<(), String> {
    let cfg = load_config()?;
    let spec = resolve(id, &cfg)?;
    let dir = installed_dir(spec)?;

    let mut reader = hound::WavReader::open(wav).map_err(|e| e.to_string())?;
    let wav_spec = reader.spec();
    let raw: Vec<f32> = match wav_spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = (1i64 << (wav_spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap_or(0) as f32 / scale).collect()
        }
    };
    let channels = wav_spec.channels as usize;
    let mono: Vec<f32> = raw
        .chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect();

    let t = Instant::now();
    let engine = engine::Engine::load(&dir, spec, cfg.thread_count())?;
    let loaded = t.elapsed();
    let t = Instant::now();
    let text = engine.transcribe(&mono, wav_spec.sample_rate);
    eprintln!(
        "model={} load={loaded:?} decode={:?} audio={:.1}s",
        spec.id,
        t.elapsed(),
        mono.len() as f32 / wav_spec.sample_rate as f32
    );
    println!("{text}");
    Ok(())
}
