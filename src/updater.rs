//! Self-update from GitHub Releases, and first-launch-of-a-version detection.
//!
//! A release is tagged `vX.Y.Z` and carries one zip per platform (see `ASSET`).
//! The running app cannot replace itself, so a detached helper waits for it to
//! exit, swaps the files and starts the new version.

use crate::log;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "jojonki/KoeBoard";
const CHECK_INTERVAL_SECS: u64 = 86_400;
const NOTES_LIMIT: usize = 4_000;

const ASSET: &str = if cfg!(windows) {
    "koeboard-windows-x64.zip"
} else if cfg!(target_arch = "aarch64") {
    "KoeBoard-macos-arm64.zip"
} else {
    "KoeBoard-macos-x64.zip"
};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    last_seen_version: String,
    last_check: u64,
}

fn state_path(root: &Path) -> PathBuf {
    root.join("update-state.toml")
}

fn load_state(root: &Path) -> State {
    fs::read_to_string(state_path(root)).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

fn save_state(root: &Path, state: &State) {
    let saved = toml::to_string(state)
        .map_err(|e| e.to_string())
        .and_then(|s| fs::write(state_path(root), s).map_err(|e| e.to_string()));
    if let Err(e) = saved {
        log(&format!("error: update state: {e}"));
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `"installed"` or `"updated"` the first time this version runs, then `None`.
pub fn take_welcome(root: &Path) -> Option<&'static str> {
    let mut state = load_state(root);
    if state.last_seen_version == VERSION {
        return None;
    }
    let welcome = if state.last_seen_version.is_empty() { "installed" } else { "updated" };
    state.last_seen_version = VERSION.to_owned();
    save_state(root, &state);
    Some(welcome)
}

/// Compares dotted versions numerically; missing parts count as 0.
fn is_newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|p| {
                let digits = p.find(|c: char| !c.is_ascii_digit()).unwrap_or(p.len());
                p[..digits].parse().unwrap_or(0)
            })
            .collect()
    };
    let (a, b) = (parse(a), parse(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    body: Option<String>,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub version: String,
    pub notes: String,
    url: String,
    sha256: Option<String>,
}

/// The update for this platform, if `json` describes a newer release.
fn select_update(json: &str, current: &str) -> Result<Option<Update>, String> {
    let release: ApiRelease = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let version = release.tag_name.trim_start_matches(['v', 'V']).to_owned();
    if !is_newer(&version, current) {
        return Ok(None);
    }
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == ASSET)
        .ok_or_else(|| format!("Release {version} has no download for this computer"))?;
    // Only this repository's release files are ever installed.
    if !asset.browser_download_url.starts_with(&format!("https://github.com/{REPO}/releases/download/")) {
        return Err("Release download is not hosted by this project".into());
    }
    Ok(Some(Update {
        version,
        notes: release.body.unwrap_or_default().chars().take(NOTES_LIMIT).collect(),
        url: asset.browser_download_url,
        sha256: asset.digest.and_then(|d| d.strip_prefix("sha256:").map(str::to_owned)),
    }))
}

fn user_agent() -> String {
    format!("KoeBoard/{VERSION}")
}

fn fetch_latest() -> Result<Option<Update>, String> {
    let response = ureq::get(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", user_agent())
        .config()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .call()
        .map_err(|e| e.to_string())?;
    let body = response.into_body().read_to_string().map_err(|e| e.to_string())?;
    select_update(&body, VERSION)
}

pub enum Event {
    Checking,
    UpToDate,
    Available(Update),
    Downloading(u8),
    Installing,
    Restart,
    Failed(String),
}

/// What the settings page shows. `available` keeps the Install button after a failure.
pub fn view(event: &Event, available: Option<&Update>) -> serde_json::Value {
    let (state, percent, message) = match event {
        Event::Checking => ("checking", 0, ""),
        Event::UpToDate => ("current", 0, ""),
        Event::Available(_) => ("available", 0, ""),
        Event::Downloading(p) => ("downloading", *p, ""),
        Event::Installing | Event::Restart => ("installing", 100, ""),
        Event::Failed(m) => ("failed", 0, m.as_str()),
    };
    json!({
        "state": state,
        "percent": percent,
        "message": message,
        "version": available.map(|u| u.version.as_str()),
        "notes": available.map(|u| u.notes.as_str()),
    })
}

pub fn idle_view() -> serde_json::Value {
    json!({ "state": "idle", "percent": 0, "message": "", "version": null, "notes": null })
}

#[derive(Clone)]
pub struct Updater {
    root: PathBuf,
    busy: Arc<AtomicBool>,
    notify: Arc<dyn Fn(Event) + Send + Sync>,
}

impl Updater {
    pub fn new(root: PathBuf, notify: impl Fn(Event) + Send + Sync + 'static) -> Self {
        Self { root, busy: Arc::new(AtomicBool::new(false)), notify: Arc::new(notify) }
    }

    /// Runs `work` on its own thread unless another check or install is running.
    fn run_exclusive(&self, work: impl FnOnce(&Self) + Send + 'static) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        std::thread::spawn(move || {
            work(&this);
            this.busy.store(false, Ordering::SeqCst);
        });
    }

    /// Only a manual check reports "up to date" and errors to the user.
    pub fn check(&self, manual: bool) {
        self.run_exclusive(move |this| {
            let mut state = load_state(&this.root);
            state.last_check = now_secs();
            save_state(&this.root, &state);
            if manual {
                (this.notify)(Event::Checking);
            }
            match fetch_latest() {
                Ok(Some(update)) => (this.notify)(Event::Available(update)),
                Ok(None) if manual => (this.notify)(Event::UpToDate),
                Ok(None) => {}
                Err(e) => {
                    log(&format!("error: update check: {e}"));
                    if manual {
                        (this.notify)(Event::Failed("Couldn't reach the update server.".into()));
                    }
                }
            }
        });
    }

    /// The app stays resident for weeks, so a check at launch alone is not enough.
    pub fn run_periodic(&self, enabled: Arc<AtomicBool>) {
        let this = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(5));
            loop {
                let due = now_secs().saturating_sub(load_state(&this.root).last_check) >= CHECK_INTERVAL_SECS;
                if due && enabled.load(Ordering::Relaxed) {
                    this.check(false);
                }
                std::thread::sleep(Duration::from_secs(3600));
            }
        });
    }

    pub fn install(&self, update: Update) {
        self.run_exclusive(move |this| match this.download_and_swap(&update) {
            Ok(()) => (this.notify)(Event::Restart),
            Err(e) => {
                log(&format!("error: update install: {e}"));
                (this.notify)(Event::Failed(e));
            }
        });
    }

    fn download_and_swap(&self, update: &Update) -> Result<(), String> {
        // Before the download, so an unwritable install fails right away.
        let target = platform::install_target()?;
        let work = std::env::temp_dir().join(format!("koeboard-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let result = self.prepare(update, &work, &target);
        if result.is_err() {
            let _ = fs::remove_dir_all(&work);
        }
        result
    }

    fn prepare(&self, update: &Update, work: &Path, target: &Path) -> Result<(), String> {
        let zip = work.join("update.zip");
        download(update, &zip, |p| (self.notify)(Event::Downloading(p)))?;
        (self.notify)(Event::Installing);
        let unpacked = platform::unpack(&zip, work)?;
        platform::swap_after_exit(&unpacked, target, work)
    }
}

fn download(update: &Update, to: &Path, progress: impl Fn(u8)) -> Result<(), String> {
    let response = ureq::get(&update.url)
        .header("User-Agent", user_agent())
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let total = response.body().content_length().unwrap_or(0);
    let mut reader = response.into_body().into_reader();
    let mut file = fs::File::create(to).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let (mut written, mut last) = (0u64, 0u8);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Download failed: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        written += n as u64;
        let percent = if total > 0 { (written * 100 / total).min(100) as u8 } else { 0 };
        if percent != last {
            last = percent;
            progress(percent);
        }
    }
    if let Some(expected) = &update.sha256 {
        let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if !actual.eq_ignore_ascii_case(expected) {
            return Err("The download was corrupted. Please try again.".into());
        }
    }
    Ok(())
}

fn check_writable(dir: &Path, hint: &str) -> Result<(), String> {
    let probe = dir.join(format!(".koeboard-update-{}", std::process::id()));
    fs::File::create(&probe).map_err(|_| hint.to_owned())?;
    let _ = fs::remove_file(probe);
    Ok(())
}

#[cfg(target_os = "macos")]
mod platform {
    use super::check_writable;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const MOVE_HINT: &str = "KoeBoard can't update itself here. Move KoeBoard.app to Applications and try again.";

    /// Args: pid, installed app, new app, work dir. Restores the old app if the move fails.
    pub(super) const SWAP: &str = r#"
while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
backup="$2.previous"
rm -rf "$backup"
if mv "$2" "$backup"; then
    if mv "$3" "$2"; then
        rm -rf "$backup"
    else
        rm -rf "$2"
        mv "$backup" "$2"
    fi
fi
open "$2"
rm -rf "$4"
"#;

    pub fn install_target() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let app = exe
            .ancestors()
            .nth(3)
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or("Updates are only available in KoeBoard.app")?;
        // A quarantined app run from Downloads executes from a read-only mount.
        if app.to_string_lossy().contains("/AppTranslocation/") {
            return Err(MOVE_HINT.into());
        }
        check_writable(app.parent().ok_or(MOVE_HINT)?, MOVE_HINT)?;
        Ok(app.to_owned())
    }

    pub fn unpack(zip: &Path, work: &Path) -> Result<PathBuf, String> {
        let status = Command::new("/usr/bin/ditto").arg("-xk").arg(zip).arg(work).status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("The update could not be unpacked.".into());
        }
        fs::read_dir(work)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or_else(|| "The update does not contain KoeBoard.app.".to_owned())
    }

    pub fn swap_after_exit(new: &Path, target: &Path, work: &Path) -> Result<(), String> {
        Command::new("/bin/sh")
            .args(["-c", SWAP, "sh", &std::process::id().to_string()])
            .args([target, new, work])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
mod platform {
    use super::check_writable;
    use std::fs;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const MOVE_HINT: &str = "KoeBoard can't update itself here. Move its folder somewhere you can write to and try again.";

    /// Loaded DLLs stay locked until the app exits, hence the wait and retries.
    const SWAP: &str = r#"param($WaitPid, $Source, $Target, $Work)
Wait-Process -Id $WaitPid -ErrorAction SilentlyContinue
for ($i = 0; $i -lt 20; $i++) {
    try {
        Copy-Item -Path (Join-Path $Source '*') -Destination $Target -Recurse -Force -ErrorAction Stop
        break
    } catch { Start-Sleep -Milliseconds 500 }
}
Start-Process (Join-Path $Target 'koeboard.exe')
Set-Location $env:TEMP
Remove-Item -LiteralPath $Work -Recurse -Force -ErrorAction SilentlyContinue
"#;

    fn system32(tool: &str) -> PathBuf {
        let windows = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(windows).join("System32").join(tool)
    }

    pub fn install_target() -> Result<PathBuf, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = exe.parent().ok_or(MOVE_HINT)?;
        check_writable(dir, MOVE_HINT)?;
        Ok(dir.to_owned())
    }

    pub fn unpack(zip: &Path, work: &Path) -> Result<PathBuf, String> {
        let status = Command::new(system32("tar.exe"))
            .arg("-xf")
            .arg(zip)
            .arg("-C")
            .arg(work)
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("The update could not be unpacked.".into());
        }
        fs::read_dir(work)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.join("koeboard.exe").is_file())
            .ok_or_else(|| "The update does not contain koeboard.exe.".to_owned())
    }

    pub fn swap_after_exit(new: &Path, target: &Path, work: &Path) -> Result<(), String> {
        let script = work.join("swap.ps1");
        fs::write(&script, SWAP).map_err(|e| e.to_string())?;
        Command::new(system32("WindowsPowerShell\\v1.0\\powershell.exe"))
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .arg(std::process::id().to_string())
            .args([new, target, work])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, url: &str) -> String {
        json!({
            "tag_name": tag,
            "body": "notes",
            "assets": [
                { "name": "other.zip", "browser_download_url": "https://example.com/other.zip", "digest": null },
                { "name": ASSET, "browser_download_url": url, "digest": "sha256:abc" },
            ],
        })
        .to_string()
    }

    fn url() -> String {
        format!("https://github.com/{REPO}/releases/download/v9.0.0/{ASSET}")
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.1", "1.0"));
        assert!(!is_newer("1.0.0", "1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn selects_this_platforms_asset_from_newer_release() {
        let update = select_update(&release("v9.0.0", &url()), "1.0.0").unwrap().unwrap();
        assert_eq!((update.version.as_str(), update.url.as_str()), ("9.0.0", url().as_str()));
        assert_eq!(update.sha256.as_deref(), Some("abc"));
    }

    #[test]
    fn same_version_is_not_an_update() {
        assert_eq!(select_update(&release("v1.0.0", &url()), "1.0.0").unwrap(), None);
    }

    #[test]
    fn refuses_downloads_from_elsewhere() {
        assert!(select_update(&release("v9.0.0", "https://example.com/x.zip"), "1.0.0").is_err());
    }

    #[test]
    fn reports_missing_platform_asset() {
        let json = json!({ "tag_name": "v9.0.0", "body": null, "assets": [] }).to_string();
        assert!(select_update(&json, "1.0.0").is_err());
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("koeboard-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn welcome_appears_once_per_version() {
        let root = temp("welcome");
        assert_eq!(take_welcome(&root), Some("installed"));
        assert_eq!(take_welcome(&root), None);
        save_state(&root, &State { last_seen_version: "0.0.1".into(), last_check: 7 });
        assert_eq!(take_welcome(&root), Some("updated"));
        assert_eq!(load_state(&root).last_check, 7);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changelog_covers_this_version() {
        assert!(crate::settings::CHANGELOG.contains(&format!("## {VERSION}")));
    }

    #[cfg(target_os = "macos")]
    fn run_swap(root: &Path, app: &Path, new: &Path, work: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let opened = root.join("opened");
        let stub = bin.join("open");
        fs::write(&stub, format!("#!/bin/sh\necho \"$1\" > '{}'\n", opened.display())).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = gone.id().to_string();
        gone.wait().unwrap();
        let status = std::process::Command::new("/bin/sh")
            .env("PATH", format!("{}:/bin:/usr/bin", bin.display()))
            .args(["-c", platform::SWAP, "sh", &pid])
            .args([app, new, work])
            .status()
            .unwrap();
        assert!(status.success());
        fs::read_to_string(opened).unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swap_replaces_app_and_relaunches() {
        let root = temp("swap");
        let (app, work) = (root.join("KoeBoard.app"), root.join("work"));
        let new = work.join("KoeBoard.app");
        fs::create_dir_all(&app).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(app.join("v"), "old").unwrap();
        fs::write(new.join("v"), "new").unwrap();
        let opened = run_swap(&root, &app, &new, &work);
        assert_eq!(fs::read_to_string(app.join("v")).unwrap(), "new");
        assert_eq!(opened.trim(), app.to_str().unwrap());
        assert!(!work.exists() && !root.join("KoeBoard.app.previous").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swap_keeps_old_app_when_new_one_is_missing() {
        let root = temp("swap-fail");
        let (app, work) = (root.join("KoeBoard.app"), root.join("work"));
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("v"), "old").unwrap();
        run_swap(&root, &app, &work.join("missing.app"), &work);
        assert_eq!(fs::read_to_string(app.join("v")).unwrap(), "old");
        fs::remove_dir_all(root).unwrap();
    }
}
