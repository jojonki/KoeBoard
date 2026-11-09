//! Inserts text into the focused control via the clipboard.
//!
//! Pasting is used rather than synthesizing keystrokes because per-character
//! input is intercepted by the Japanese IME.

use arboard::{Clipboard, ImageData};
use std::thread::sleep;
use std::time::Duration;

const SETTLE: Duration = Duration::from_millis(30);
/// Target apps read the clipboard asynchronously after receiving paste.
const PASTE_GRACE: Duration = Duration::from_millis(250);

enum Saved {
    Empty,
    Text(String),
    Image(ImageData<'static>),
}

fn save(cb: &mut Clipboard) -> Saved {
    if let Ok(t) = cb.get_text() {
        Saved::Text(t)
    } else if let Ok(i) = cb.get_image() {
        Saved::Image(i)
    } else {
        Saved::Empty
    }
}

/// Returns a notice when the text could not be pasted but is on the clipboard.
/// `enter` presses the Enter key once the text has landed. `text` may be empty.
pub fn insert(text: &str, restore: bool, enter: bool) -> Result<Option<String>, String> {
    let mut cb = Clipboard::new().map_err(|e| e.to_string())?;
    if !platform::can_paste() {
        crate::log("paste skipped: accessibility permission is not granted");
        if text.is_empty() {
            return Ok(Some("No Accessibility access: Enter was not pressed".into()));
        }
        cb.set_text(text).map_err(|e| e.to_string())?;
        return Ok(Some(
            "No Accessibility access: copied only".into(),
        ));
    }
    if text.is_empty() {
        return if enter { platform::send_enter().map(|()| None) } else { Ok(None) };
    }
    let saved = restore.then(|| save(&mut cb));
    cb.set_text(text).map_err(|e| e.to_string())?;
    sleep(SETTLE);
    platform::send_paste()?;
    if enter || saved.is_some() {
        sleep(PASTE_GRACE);
    }
    // Enter before restoring: the app may still read the clipboard until the text lands.
    let entered = if enter { platform::send_enter() } else { Ok(()) };
    if let Some(saved) = saved {
        // Leave the clipboard alone if the user copied something meanwhile.
        if cb.get_text().is_ok_and(|t| t == text) {
            let _ = match saved {
                Saved::Empty => cb.clear(),
                Saved::Text(t) => cb.set_text(t),
                Saved::Image(i) => cb.set_image(i),
            };
        }
    }
    entered.map(|()| None)
}

#[cfg(target_os = "macos")]
mod platform {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    const KEY_V: u16 = 9;
    const KEY_RETURN: u16 = 36;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        static kAXTrustedCheckOptionPrompt: CFStringRef;
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
        fn CGPreflightPostEventAccess() -> bool;
        fn CGRequestPostEventAccess() -> bool;
    }

    pub fn can_paste() -> bool {
        let (ax, post) = unsafe { (AXIsProcessTrusted(), CGPreflightPostEventAccess()) };
        crate::log(&format!("paste access: accessibility={ax} post_event={post}"));
        ax && post
    }

    /// Shows the system permission dialog when access has not been granted.
    pub fn request_permission() {
        unsafe {
            let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
            let options = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
            AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef());
            CGRequestPostEventAccess();
        }
    }

    pub fn send_paste() -> Result<(), String> {
        send_key(KEY_V, CGEventFlags::CGEventFlagCommand)
    }

    pub fn send_enter() -> Result<(), String> {
        send_key(KEY_RETURN, CGEventFlags::CGEventFlagNull)
    }

    fn send_key(code: u16, flags: CGEventFlags) -> Result<(), String> {
        let source = CGEventSource::new(CGEventSourceStateID::Private)
            .map_err(|_| "CGEventSource failed")?;
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(source.clone(), code, down)
                .map_err(|_| "CGEvent failed")?;
            // Explicit flags, so a still-held Option key cannot alter the shortcut.
            event.set_flags(flags);
            event.post(CGEventTapLocation::HID);
        }
        Ok(())
    }
}

#[cfg(windows)]
mod platform {
    use std::mem::size_of;
    use std::thread::sleep;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };

    const VK_V: u16 = 0x56;
    const VK_RETURN: u16 = 0x0D;

    pub fn can_paste() -> bool {
        true
    }

    pub fn request_permission() {}

    fn key(vk: u16, flags: u32) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    /// Ctrl+Alt+V is not paste, so wait until the hotkey's modifiers are up.
    fn wait_for_modifiers_up() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let held = || {
            [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
                .iter()
                .any(|vk| unsafe { GetAsyncKeyState(*vk as i32) } as u16 & 0x8000 != 0)
        };
        while held() && Instant::now() < deadline {
            sleep(Duration::from_millis(10));
        }
    }

    pub fn send_paste() -> Result<(), String> {
        send(&[
            key(VK_CONTROL, 0),
            key(VK_V, 0),
            key(VK_V, KEYEVENTF_KEYUP),
            key(VK_CONTROL, KEYEVENTF_KEYUP),
        ])
    }

    pub fn send_enter() -> Result<(), String> {
        send(&[key(VK_RETURN, 0), key(VK_RETURN, KEYEVENTF_KEYUP)])
    }

    fn send(inputs: &[INPUT]) -> Result<(), String> {
        wait_for_modifiers_up();
        let sent = unsafe {
            SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32)
        };
        if sent == inputs.len() as u32 {
            Ok(())
        } else {
            Err("SendInput was blocked".into())
        }
    }
}

pub use platform::request_permission;
