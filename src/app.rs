//! The resident app: tray icon, global hotkey and wiring between threads.

use crate::audio::Recorder;
use crate::config::{self, Config};
use crate::engine::{Job, Status, Worker};
use crate::gesture::{Action, Gesture};
use crate::overlay::{Level, Overlay};
use crate::settings::{Landing, Message, Settings};
use crate::updater::{self, Updater};
use crate::{inject, log, models};
use auto_launch::{AutoLaunch, AutoLaunchBuilder, MacOSLaunchMode};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::fs::{self, TryLockError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

enum UserEvent {
    Hotkey(GlobalHotKeyEvent),
    Menu(MenuEvent),
    Status(Status),
    Settings(String),
    Update(updater::Event),
}

#[derive(Clone, Copy, PartialEq)]
enum Light {
    Idle,
    Recording,
    Busy,
}

pub fn run() -> Result<(), String> {
    let root = config::data_dir();
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;

    // The OS releases this lock when the process exits, even after a crash.
    let lock = fs::File::create(root.join("koeboard.lock")).map_err(|e| e.to_string())?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err("KoeBoard is already running".into()),
        Err(TryLockError::Error(e)) => return Err(e.to_string()),
    }

    let config_path = root.join("config.toml");
    let config = Config::load(&config_path)?;
    let mut hotkey: HotKey = config
        .hotkey
        .parse()
        .map_err(|e| format!("Invalid shortcut {:?}: {e}", config.hotkey))?;

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        // Taking activation at launch would pull focus from the app being typed in.
        event_loop.set_activate_ignoring_other_apps(false);
    }
    let proxy = event_loop.create_proxy();

    let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
    manager
        .register(hotkey)
        .map_err(|e| format!("Cannot register shortcut {:?}: {e}", config.hotkey))?;

    let p = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |e| {
        let _ = p.send_event(UserEvent::Hotkey(e));
    }));
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = p.send_event(UserEvent::Menu(e));
    }));

    let level = Level::default();
    let overlay_level = level.clone();
    let (jobs, jobs_rx) = channel();
    let recorder = {
        let p = proxy.clone();
        Recorder::spawn(&config, jobs.clone(), level, move |msg| {
            let _ = p.send_event(UserEvent::Status(Status::Error(msg)));
        })
    };
    let restore_clipboard = Arc::new(AtomicBool::new(config.restore_clipboard));
    let worker = {
        let restore = restore_clipboard.clone();
        let p = proxy.clone();
        Worker {
            root: root.clone(),
            config: config.clone(),
            deliver: Box::new(move |text, enter| inject::insert(text, restore.load(Ordering::Relaxed), enter)),
            status: Box::new(move |s| {
                let _ = p.send_event(UserEvent::Status(s));
            }),
        }
    };
    std::thread::spawn(move || worker.run(jobs_rx));
    let _ = jobs.send(Job::Warm);

    let updater = {
        let p = proxy.clone();
        Updater::new(root.clone(), move |e| {
            let _ = p.send_event(UserEvent::Update(e));
        })
    };
    let auto_update = Arc::new(AtomicBool::new(config.auto_update));
    updater.run_periodic(auto_update.clone());
    let mut welcome = updater::take_welcome(&root);
    let mut settings_tab: Option<&'static str> = None;
    let mut available: Option<updater::Update> = None;
    let mut update_view = updater::idle_view();

    let mut gesture = Gesture::default();
    let mut max_record = Duration::from_secs(config.max_record_seconds);
    let mut settings: Option<Settings> = None;
    let mut want_settings = false;
    let mut ui: Option<Ui> = None;
    let mut overlay: Option<Overlay> = None;
    let mut quit = false;
    let mut record_deadline: Option<Instant> = None;
    let mut pending = 0usize;
    let mut config = config;

    event_loop.run(move |event, target, flow| {
        let _ = (&manager, &lock);
        let now = Instant::now();

        let action = match event {
            Event::NewEvents(StartCause::Init) => {
                inject::request_permission();
                match Ui::new(&config) {
                    Ok(u) => ui = Some(u),
                    Err(e) => {
                        log(&format!("error: {e}"));
                        quit = true;
                    }
                }
                // Nothing is downloaded until the user accepts a model's license.
                // A resident app gives no sign of life otherwise, so greet each new version.
                want_settings = welcome.is_some() || !models::id_is_installed(&root, &config.model);
                settings_tab = welcome.map(|_| "updates");
                // Dictation still works without the panel.
                match Overlay::new(target, overlay_level.clone(), config.panel.clone()) {
                    Ok(o) => overlay = Some(o),
                    Err(e) => log(&format!("error: overlay: {e}")),
                }
                None
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. })
                if record_deadline.is_some_and(|d| now >= d) =>
            {
                gesture.reset();
                Some(Action::Stop)
            }
            Event::UserEvent(UserEvent::Hotkey(e)) if e.id == hotkey.id() => match e.state {
                HotKeyState::Pressed => gesture.press(),
                HotKeyState::Released => gesture.release(),
            },
            Event::UserEvent(UserEvent::Status(s)) => {
                if matches!(s, Status::Done) {
                    pending = pending.saturating_sub(1);
                }
                if let Some(o) = overlay.as_mut().filter(|_| config.overlay) {
                    match &s {
                        Status::Recognized(text, enter) => o.result(text.clone(), *enter),
                        Status::EnterPressed => o.enter_pressed(),
                        Status::Notice(message) | Status::Error(message) => o.result(message.clone(), false),
                        Status::Done if pending == 0 => o.finish_empty(),
                        _ => {}
                    }
                }
                if let Some(u) = ui.as_mut() {
                    u.on_status(&s);
                    if let Some(w) = settings.as_ref() {
                        w.push_status(&u.status.text());
                    }
                }
                let model_event = match &s {
                    Status::Installed { id, selected } => {
                        if *selected {
                            config.model = id.clone();
                            if let Err(e) = config.save(&config_path) {
                                log(&format!("error: {e}"));
                            }
                        }
                        Some(serde_json::json!({ "kind": "installed", "id": id, "selected": selected }))
                    }
                    Status::DownloadFailed(id) => Some(serde_json::json!({ "kind": "failed", "id": id })),
                    Status::Removed(id) => Some(serde_json::json!({ "kind": "removed", "id": id })),
                    _ => None,
                };
                if let (Some(w), Some(event)) = (settings.as_ref(), model_event) {
                    w.push_model_event(&event);
                }
                None
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if let Some(u) = ui.as_mut() {
                    match u.on_menu(&e.id) {
                        MenuOutcome::Quit => quit = true,
                        MenuOutcome::None => {}
                        MenuOutcome::Settings => want_settings = true,
                        MenuOutcome::CheckUpdates => {
                            want_settings = true;
                            settings_tab = Some("updates");
                            updater.check(true);
                        }
                    }
                }
                None
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. }
                if settings.as_ref().is_some_and(|w| w.id() == window_id) =>
            {
                if let Some(w) = settings.take() {
                    w.close();
                }
                None
            }
            Event::UserEvent(UserEvent::Update(e)) => {
                match &e {
                    updater::Event::Available(update) => {
                        let is_new = available.as_ref().map(|a| &a.version) != Some(&update.version);
                        available = Some(update.clone());
                        if let Some(u) = ui.as_mut() {
                            u.set_update_available(Some(&update.version));
                        }
                        // A window would steal focus from the text being dictated into.
                        if is_new && config.overlay && record_deadline.is_none() && pending == 0 {
                            if let Some(o) = overlay.as_mut() {
                                o.result(format!("KoeBoard {} is available. Install it from the menu.", update.version), false);
                            }
                        }
                    }
                    updater::Event::UpToDate => {
                        available = None;
                        if let Some(u) = ui.as_mut() {
                            u.set_update_available(None);
                        }
                    }
                    updater::Event::Restart => quit = true,
                    _ => {}
                }
                update_view = updater::view(&e, available.as_ref());
                if let Some(w) = settings.as_ref() {
                    w.push_update(&update_view);
                }
                None
            }
            Event::UserEvent(UserEvent::Settings(body)) => {
                match serde_json::from_str::<Message>(&body) {
                    Ok(Message::Preview) => {
                        if let Some(o) = overlay.as_mut() {
                            o.preview();
                        }
                    }
                    Ok(Message::OpenConfig) => open_path(&config_path),
                    Ok(Message::OpenLog) => open_path(&root.join("koeboard.log")),
                    Ok(Message::OpenUrl { url }) => {
                        // Only license and model pages listed in the catalog.
                        if models::allows_url(&url) {
                            open_url(&url);
                        }
                    }
                    Ok(Message::Download { id, accepted }) => {
                        if accepted && models::find(&id).is_some() {
                            if !config.accepted_licenses.contains(&id) {
                                config.accepted_licenses.push(id.clone());
                                if let Err(e) = config.save(&config_path) {
                                    log(&format!("error: {e}"));
                                }
                            }
                            let _ = jobs.send(Job::Download(id));
                        }
                    }
                    Ok(Message::CheckUpdates) => updater.check(true),
                    Ok(Message::InstallUpdate) => {
                        if let Some(update) = available.clone() {
                            updater.install(update);
                        }
                    }
                    Ok(Message::Delete { id }) => {
                        let _ = jobs.send(Job::Delete(id));
                    }
                    Ok(Message::Save { config: new, launch_at_login }) => {
                        let mut new = *new;
                        // The page never owns these.
                        new.accepted_licenses = config.accepted_licenses.clone();
                        let mut problem = None;
                        if new.hotkey != config.hotkey {
                            let replaced = new.hotkey.parse::<HotKey>().map_err(|e| e.to_string()).and_then(|k| {
                                let _ = manager.unregister(hotkey);
                                manager.register(k).map(|()| k).map_err(|e| {
                                    let _ = manager.register(hotkey);
                                    e.to_string()
                                })
                            });
                            match replaced {
                                Ok(k) => hotkey = k,
                                Err(e) => {
                                    problem = Some(format!("That shortcut is unavailable: {e}"));
                                    new.hotkey = config.hotkey.clone();
                                }
                            }
                        }
                        max_record = Duration::from_secs(new.max_record_seconds.max(1));
                        if new.microphone != config.microphone {
                            recorder.set_device(new.microphone.clone());
                        }
                        restore_clipboard.store(new.restore_clipboard, Ordering::Relaxed);
                        auto_update.store(new.auto_update, Ordering::Relaxed);
                        if new.panel != config.panel {
                            if let Some(o) = overlay.as_mut() {
                                o.set_style(new.panel.clone());
                                o.preview();
                            }
                        }
                        if let Some(u) = ui.as_mut() {
                            u.set_hotkey_label(&new.hotkey);
                            if let Err(e) = u.set_launch_at_login(launch_at_login) {
                                problem = Some(e);
                            }
                        }
                        let _ = jobs.send(Job::Configure(Box::new(new.clone())));
                        if let Err(e) = new.save(&config_path) {
                            problem = Some(e);
                        }
                        config = new;
                        if let (Some(w), Some(p)) = (settings.as_ref(), problem.as_deref()) {
                            w.push_error(p);
                        }
                    }
                    Err(e) => log(&format!("error: settings message: {e}")),
                }
                None
            }
            _ => None,
        };

        // Recording cannot produce text yet, so go straight to the download step.
        let action = if matches!(action, Some(Action::Start)) && !models::id_is_installed(&root, &config.model) {
            gesture.reset();
            want_settings = true;
            None
        } else {
            action
        };

        if std::mem::take(&mut want_settings) {
            let tab = settings_tab.take();
            if let Some(w) = settings.as_ref() {
                w.focus();
                match tab {
                    Some(t) => w.select_tab(t),
                    None => w.require_model(),
                }
            } else if let Some(u) = ui.as_ref() {
                let p = proxy.clone();
                let opened = Settings::open(
                    target,
                    &root,
                    &config,
                    u.launch_at_login(),
                    &u.status.text(),
                    Landing { tab, welcome: welcome.take() },
                    &update_view,
                    move |body| {
                        let _ = p.send_event(UserEvent::Settings(body));
                    },
                );
                match opened {
                    Ok(w) => settings = Some(w),
                    Err(e) => log(&format!("error: settings: {e}")),
                }
            }
        }

        match action {
            Some(Action::Start) => {
                log("recording started");
                recorder.start();
                let _ = jobs.send(Job::Warm);
                record_deadline = Some(now + max_record);
                if let Some(o) = overlay.as_mut().filter(|_| config.overlay) {
                    // Re-read so hand edits to the panel apply without restarting.
                    match Config::load(&config_path) {
                        Ok(latest) => o.set_style(latest.panel),
                        Err(e) => log(&format!("error: {e}")),
                    }
                    o.listen();
                }
            }
            Some(Action::Stop) => {
                recorder.stop();
                record_deadline = None;
                pending += 1;
                if let Some(o) = overlay.as_mut().filter(|_| config.overlay) {
                    o.recognizing();
                }
            }
            None => {}
        }
        if let Some(u) = ui.as_mut() {
            u.set_light(if record_deadline.is_some() {
                Light::Recording
            } else if pending > 0 {
                Light::Busy
            } else {
                Light::Idle
            });
        }
        if let Some(o) = overlay.as_mut() {
            o.tick(now);
        }
        let wake = [record_deadline, overlay.as_ref().and_then(Overlay::next_wake)]
            .into_iter()
            .flatten()
            .min();
        *flow = if quit {
            ControlFlow::Exit
        } else {
            wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil)
        };
    })
}

/// Shows each overlay state with a simulated voice level, then exits.
pub fn overlay_demo() -> Result<(), String> {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<()>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        // Taking activation at launch would pull focus from the app being typed in.
        event_loop.set_activate_ignoring_other_apps(false);
    }
    let level = Level::default();
    let start = Instant::now();
    let mut overlay: Option<Overlay> = None;
    let mut stage = 0;
    event_loop.run(move |_, target, flow| {
        let now = Instant::now();
        let t = now.duration_since(start).as_secs_f32();
        let o = match overlay.as_mut() {
            Some(o) => o,
            None => {
                let mut o = Overlay::new(target, level.clone(), Default::default()).expect("overlay");
                o.listen();
                overlay.insert(o)
            }
        };
        crate::overlay::store_level(&level, 0.02 + 0.2 * (t * 9.0).sin().abs());
        if stage == 0 && t > 3.0 {
            stage = 1;
            o.recognizing();
        } else if stage == 1 && t > 4.5 {
            stage = 2;
            o.result("The meeting tomorrow has moved to 3 PM.".to_owned(), false);
        }
        o.tick(now);
        *flow = if t > 6.0 {
            ControlFlow::Exit
        } else {
            ControlFlow::WaitUntil(now + Duration::from_millis(16))
        };
    })
}

const CHECK_UPDATES_LABEL: &str = "Check for Updates...";

enum MenuOutcome {
    None,
    Quit,
    Settings,
    CheckUpdates,
}

struct Ui {
    tray: TrayIcon,
    status: MenuItem,
    settings: MenuItem,
    updates: MenuItem,
    auto_launch: Option<AutoLaunch>,
    quit: MenuItem,
    light: Light,
    icons: [Icon; 3],
    idle_text: String,
    /// The status line holds a message the user should still get to read.
    sticky: bool,
}

impl Ui {
    fn new(config: &Config) -> Result<Self, String> {
        let err = |e: tray_icon::menu::Error| e.to_string();
        let status = MenuItem::new("Starting...", false, None);
        let auto_launch = build_auto_launch();
        carry_over_launch_at_login(auto_launch.as_ref());
        let settings = MenuItem::new("Settings...", true, None);
        let updates = MenuItem::new(CHECK_UPDATES_LABEL, true, None);
        let quit = MenuItem::new("Quit KoeBoard", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &settings,
            &updates,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(err)?;

        let icons = [
            // macOS recolors this template icon; the Windows taskbar is usually dark.
            tray_icon(if cfg!(target_os = "macos") { [0, 0, 0] } else { [230, 230, 230] })?,
            tray_icon([235, 60, 50])?,
            tray_icon([240, 150, 30])?,
        ];
        let builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("KoeBoard");
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icons[0].clone());
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icons[0].clone());
        let tray = builder.build().map_err(|e| e.to_string())?;

        Ok(Self {
            tray,
            status,
            settings,
            updates,
            auto_launch,
            quit,
            light: Light::Idle,
            icons,
            idle_text: format!("Ready ({})", config.hotkey),
            sticky: false,
        })
    }

    fn set_light(&mut self, light: Light) {
        if self.light == light {
            return;
        }
        self.light = light;
        if light == Light::Recording {
            self.sticky = false;
            self.status.set_text("Recording...");
        }
        let _ = match light {
            #[cfg(target_os = "macos")]
            Light::Idle => self.tray.set_icon_templated(Some(self.icons[0].clone())),
            #[cfg(not(target_os = "macos"))]
            Light::Idle => self.tray.set_icon(Some(self.icons[0].clone())),
            Light::Recording => self.tray.set_icon(Some(self.icons[1].clone())),
            Light::Busy => self.tray.set_icon(Some(self.icons[2].clone())),
        };
    }

    fn on_status(&mut self, status: &Status) {
        let text = match status {
            Status::Downloading(p) => format!("Downloading model {p}%"),
            Status::Loading => "Loading model...".to_owned(),
            Status::Ready => self.idle_text.clone(),
            Status::Done if self.sticky => return,
            Status::Done => self.idle_text.clone(),
            Status::Recognized(..)
            | Status::EnterPressed
            | Status::Installed { .. }
            | Status::DownloadFailed(_)
            | Status::Removed(_) => return,
            Status::NeedsModel => "No model. Open Settings to download one".to_owned(),
            Status::Notice(m) => {
                self.sticky = true;
                m.clone()
            }
            Status::Error(m) => {
                log(&format!("error: {m}"));
                self.sticky = true;
                format!("Error: {m}")
            }
        };
        self.status.set_text(text);
    }

    fn on_menu(&self, id: &MenuId) -> MenuOutcome {
        if id == self.quit.id() {
            MenuOutcome::Quit
        } else if id == self.settings.id() {
            MenuOutcome::Settings
        } else if id == self.updates.id() {
            MenuOutcome::CheckUpdates
        } else {
            MenuOutcome::None
        }
    }

    /// `None` when launch at login is unavailable.
    fn launch_at_login(&self) -> Option<bool> {
        self.auto_launch.as_ref().map(|a| a.is_enabled().unwrap_or(false))
    }

    fn set_launch_at_login(&self, want: bool) -> Result<(), String> {
        if self.launch_at_login() == Some(want) {
            return Ok(());
        }
        let Some(a) = &self.auto_launch else { return Ok(()) };
        if want { a.enable() } else { a.disable() }.map_err(|e| format!("Launch at login: {e}"))
    }

    fn set_update_available(&self, version: Option<&str>) {
        match version {
            Some(v) => self.updates.set_text(format!("Update to {v}...")),
            None => self.updates.set_text(CHECK_UPDATES_LABEL),
        }
    }

    fn set_hotkey_label(&mut self, hotkey: &str) {
        let was_idle = self.status.text() == self.idle_text;
        self.idle_text = format!("Ready ({hotkey})");
        if was_idle {
            self.status.set_text(&self.idle_text);
        }
    }
}

/// Login items of the app's former names point at a path that no longer exists.
fn carry_over_launch_at_login(auto_launch: Option<&AutoLaunch>) {
    for former in ["Poso", "jVin"] {
        let Some(old) = build_auto_launch_named(former) else { continue };
        if old.is_enabled().unwrap_or(false) && old.disable().is_ok() {
            if let Some(Err(e)) = auto_launch.map(AutoLaunch::enable) {
                log(&format!("error: launch at login: {e}"));
            }
        }
    }
}

fn build_auto_launch() -> Option<AutoLaunch> {
    build_auto_launch_named("KoeBoard")
}

fn build_auto_launch_named(name: &str) -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    AutoLaunchBuilder::new()
        .set_app_name(name)
        .set_app_path(exe.to_str()?)
        .set_macos_launch_mode(MacOSLaunchMode::LaunchAgent)
        .build()
        .ok()
}

fn open_url(url: &str) {
    let result = if cfg!(windows) {
        std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn()
    } else {
        std::process::Command::new("open").arg(url).spawn()
    };
    if let Err(e) = result {
        log(&format!("error: open url: {e}"));
    }
}

fn open_path(path: &std::path::Path) {
    let opener = if cfg!(windows) { "notepad" } else { "open" };
    if let Err(e) = std::process::Command::new(opener).arg(path).spawn() {
        log(&format!("error: open config: {e}"));
    }
}

/// Decodes an embedded 8-bit RGBA PNG into (pixels, width, height).
pub(crate) fn decode_rgba_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .map_err(|e| e.to_string())?;
    let size = reader.output_buffer_size().ok_or("PNG is too large")?;
    let mut px = vec![0u8; size];
    let info = reader.next_frame(&mut px).map_err(|e| e.to_string())?;
    if (info.color_type, info.bit_depth) != (png::ColorType::Rgba, png::BitDepth::Eight) {
        return Err("PNG must be 8-bit RGBA".into());
    }
    px.truncate(info.buffer_size());
    Ok((px, info.width, info.height))
}

/// The monochrome tray glyph, recolored; only its alpha channel is used.
fn tray_icon(rgb: [u8; 3]) -> Result<Icon, String> {
    let (mut px, w, h) = decode_rgba_png(include_bytes!("../assets/icons/tray-icon.png"))?;
    for p in px.chunks_exact_mut(4) {
        p[..3].copy_from_slice(&rgb);
    }
    Icon::from_rgba(px, w, h).map_err(|e| e.to_string())
}
