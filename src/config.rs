use crate::models;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// When to press Enter after the text. One setting, so the modes cannot combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnterMode {
    Off,
    Always,
    /// Only when the speech ends with `enter_keyword`, which is removed from the text.
    Keyword,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub model: String,
    pub hotkey: String,
    /// 0 picks half of the logical CPUs, up to 6.
    pub threads: usize,
    /// Frees the model after this many idle minutes. 0 keeps it loaded.
    pub idle_unload_minutes: u64,
    /// Keeps the microphone stream open for instant capture.
    pub keep_mic_open: bool,
    /// Input device ID. Empty follows the system default.
    pub microphone: String,
    pub max_record_seconds: u64,
    pub restore_clipboard: bool,
    pub enter_mode: EnterMode,
    pub enter_keyword: String,
    /// Shows the input-level and result panel.
    pub overlay: bool,
    pub panel: PanelStyle,
    /// Models whose license the user accepted before downloading.
    pub accepted_licenses: Vec<String>,
}

/// Appearance of the overlay panel. Re-read at the start of every recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelStyle {
    /// `bottom`, `top` or `center` of the screen under the mouse.
    pub position: String,
    /// Distance in points from the chosen screen edge; shifts down from `center`.
    pub margin: f64,
    /// Horizontal shift from the screen center, in points.
    pub offset_x: f64,
    pub width: f64,
    /// Widest the panel may grow to fit recognized text.
    pub max_text_width: f64,
    pub height: f64,
    /// 0.1 to 1.0.
    pub opacity: f64,
    pub text_size: f32,
    pub background: String,
    pub level_color: String,
    /// Other end of the recording level gradient.
    pub level_color_end: String,
    pub busy_color: String,
    pub text_color: String,
}

impl Default for PanelStyle {
    fn default() -> Self {
        Self {
            position: "bottom".into(),
            margin: 160.0,
            offset_x: 0.0,
            width: 156.0,
            max_text_width: 460.0,
            height: 36.0,
            opacity: 0.88,
            text_size: 13.0,
            background: "#15161B".into(),
            level_color: "#6FE3FF".into(),
            level_color_end: "#B69CFF".into(),
            busy_color: "#9AA0B5".into(),
            text_color: "#F2F2F7".into(),
        }
    }
}

impl PanelStyle {
    /// Keeps a mistyped value from producing an invisible or screen-filling panel.
    pub fn clamped(mut self) -> Self {
        self.width = self.width.clamp(60.0, 2000.0);
        self.max_text_width = self.max_text_width.clamp(self.width, 4000.0);
        self.height = self.height.clamp(16.0, 200.0);
        self.opacity = self.opacity.clamp(0.1, 1.0);
        self.text_size = self.text_size.clamp(8.0, 48.0);
        self
    }
}

/// Parses `#RRGGBB` to 0RGB.
pub fn parse_color(text: &str) -> Option<u32> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: models::DEFAULT_MODEL.into(),
            hotkey: default_hotkey().into(),
            threads: 0,
            idle_unload_minutes: 10,
            keep_mic_open: false,
            microphone: String::new(),
            max_record_seconds: 300,
            restore_clipboard: true,
            enter_mode: EnterMode::Off,
            enter_keyword: "オーバー".into(),
            overlay: true,
            panel: PanelStyle::default(),
            accepted_licenses: Vec::new(),
        }
    }
}

fn default_hotkey() -> &'static str {
    "control+shift+KeyK"
}

impl Config {
    pub fn thread_count(&self) -> i32 {
        if self.threads > 0 {
            return self.threads as i32;
        }
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        (cpus / 2).clamp(1, 6) as i32
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|e| format!("config.toml: {e}")),
            Err(_) => {
                let cfg = Self::default();
                cfg.save(path)?;
                Ok(cfg)
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let s = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, s).map_err(|e| e.to_string())
    }
}

/// `~/Library/Application Support/KoeBoard` on macOS, `%APPDATA%\KoeBoard` on Windows.
pub fn data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA is not set"))
    } else {
        PathBuf::from(std::env::var_os("HOME").expect("HOME is not set")).join("Library/Application Support")
    };
    base.join("KoeBoard")
}

/// Data folder and file stem of the app's earlier names, newest first.
const FORMER_NAMES: [(&str, &str); 2] = [("Poso", "poso"), ("jVin", "jvin")];

/// Moves settings and models from a former name of the app on first launch.
pub fn migrate_former_name() {
    let new = data_dir();
    let Some(base) = new.parent() else { return };
    for (folder, stem) in FORMER_NAMES {
        let old = base.join(folder);
        if old.is_dir() && !new.exists() && fs::rename(&old, &new).is_ok() {
            let _ = fs::remove_file(new.join(format!("{stem}.lock")));
            let _ = fs::rename(new.join(format!("{stem}.log")), new.join("koeboard.log"));
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::HotKey;

    #[test]
    fn default_hotkey_parses() {
        assert!(default_hotkey().parse::<HotKey>().is_ok());
    }

    #[test]
    fn partial_config_uses_defaults() {
        let c: Config = toml::from_str("hotkey = \"F9\"").unwrap();
        assert_eq!(c.hotkey, "F9");
        assert_eq!(c.model, models::DEFAULT_MODEL);
    }

    #[test]
    fn removed_trigger_setting_is_ignored() {
        assert!(toml::from_str::<Config>("mode = \"toggle\"").is_ok());
    }

    #[test]
    fn enter_mode_defaults_off_and_parses() {
        let c = Config::default();
        assert_eq!((c.enter_mode, c.enter_keyword.as_str()), (EnterMode::Off, "オーバー"));
        let c: Config = toml::from_str("enter_mode = \"keyword\"\nenter_keyword = \"送信\"").unwrap();
        assert_eq!((c.enter_mode, c.enter_keyword.as_str()), (EnterMode::Keyword, "送信"));
    }

    #[test]
    fn colors_parse() {
        assert_eq!(parse_color("#A8A8AE"), Some(0xA8A8AE));
        assert_eq!(parse_color("A8A8AE"), None);
        assert_eq!(parse_color("#FFF"), None);
        assert_eq!(parse_color("#GGGGGG"), None);
    }

    #[test]
    fn partial_panel_uses_defaults() {
        let c: Config = toml::from_str("[panel]\nmargin = 300.0\nposition = \"top\"").unwrap();
        assert_eq!(c.panel.margin, 300.0);
        assert_eq!(c.panel.position, "top");
        assert_eq!(c.panel.width, PanelStyle::default().width);
    }

    #[test]
    fn panel_limits() {
        let p = PanelStyle { width: 1.0, height: 9999.0, opacity: 0.0, ..PanelStyle::default() }.clamped();
        assert_eq!((p.width, p.height, p.opacity), (60.0, 200.0, 0.1));
        assert!(p.max_text_width >= p.width);
    }

    #[test]
    fn roundtrip() {
        let s = toml::to_string_pretty(&Config::default()).unwrap();
        let c: Config = toml::from_str(&s).unwrap();
        assert_eq!(c.hotkey, default_hotkey());
    }

    #[test]
    fn threads_are_bounded() {
        let n = Config::default().thread_count();
        assert!((1..=6).contains(&n));
    }
}
