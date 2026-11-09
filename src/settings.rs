//! Tabbed settings window, created on demand so the resident app stays light.

use crate::config::{Config, PanelStyle};
use crate::{audio, models};
use serde::Deserialize;
use std::path::Path;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebView, WebViewBuilder};

const PAGE: &str = include_str!("settings.html");

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Save { config: Box<Config>, launch_at_login: bool },
    Preview,
    OpenConfig,
    OpenLog,
    OpenUrl { url: String },
    Download { id: String, accepted: bool },
    Delete { id: String },
}

pub struct Settings {
    _webview_owner: Window,
    webview: WebView,
    previous_app: Option<i32>,
}

impl Settings {
    pub fn open<T: 'static>(
        target: &EventLoopWindowTarget<T>,
        root: &Path,
        config: &Config,
        launch_at_login: Option<bool>,
        status: &str,
        on_message: impl Fn(String) + 'static,
    ) -> Result<Self, String> {
        let previous_app = platform::frontmost_app();
        let window = WindowBuilder::new()
            .with_title("KoeBoard Settings")
            .with_inner_size(LogicalSize::new(700.0, 540.0))
            .with_min_inner_size(LogicalSize::new(600.0, 420.0))
            .build(target)
            .map_err(|e| e.to_string())?;

        let list: Vec<_> = models::CATALOG
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "label": m.label,
                    "language": m.language,
                    "license": m.license,
                    "licenseUrl": m.license_url,
                    "modelUrl": m.model_url,
                    "licenseNote": m.license_note,
                    "sizeMb": models::download_size(m) / 1_000_000,
                    "installed": models::is_installed(&models::model_dir(root, m), m),
                })
            })
            .collect();
        let microphones: Vec<_> = audio::list_inputs()
            .into_iter()
            .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
            .collect();
        let init = serde_json::json!({
            "microphones": microphones,
            "config": config,
            "models": list,
            "launchAtLogin": launch_at_login,
            "status": status,
            "panelDefaults": PanelStyle::default(),
        });

        let webview = WebViewBuilder::new()
            .with_initialization_script(format!("window.__KOEBOARD__ = {init};"))
            .with_html(PAGE)
            .with_ipc_handler(move |request| on_message(request.into_body()))
            .build(&window)
            .map_err(|e| e.to_string())?;

        platform::activate_self();
        window.set_focus();
        Ok(Self { _webview_owner: window, webview, previous_app })
    }

    pub fn id(&self) -> WindowId {
        self._webview_owner.id()
    }

    pub fn focus(&self) {
        platform::activate_self();
        self._webview_owner.set_focus();
    }

    /// Brings the user to the download step if dictation cannot run yet.
    pub fn require_model(&self) {
        let _ = self.webview.evaluate_script("window.koeboardRequireModel()");
    }

    pub fn push_status(&self, text: &str) {
        self.call("koeboardStatus", text);
    }

    pub fn push_error(&self, text: &str) {
        self.call("koeboardError", text);
    }

    pub fn push_model_event(&self, event: &serde_json::Value) {
        let _ = self.webview.evaluate_script(&format!("window.koeboardModelEvent({event})"));
    }

    fn call(&self, function: &str, text: &str) {
        let argument = serde_json::Value::from(text);
        let _ = self.webview.evaluate_script(&format!("window.{function}({argument})"));
    }

    /// Hands focus back, or the next dictation would paste into nothing.
    pub fn close(self) {
        let previous = self.previous_app;
        drop(self);
        platform::activate_app(previous);
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2::class;

    pub fn frontmost_app() -> Option<i32> {
        unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
            if app.is_null() {
                return None;
            }
            let pid: i32 = msg_send![app, processIdentifier];
            Some(pid)
        }
    }

    pub fn activate_self() {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
        }
    }

    pub fn activate_app(pid: Option<i32>) {
        let Some(pid) = pid else { return };
        unsafe {
            let app: *mut AnyObject =
                msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
            if !app.is_null() {
                let _: bool = msg_send![app, activateWithOptions: 0usize];
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    // Windows returns focus to the previous window when this one closes.
    pub fn frontmost_app() -> Option<i32> {
        None
    }
    pub fn activate_self() {}
    pub fn activate_app(_pid: Option<i32>) {}
}
