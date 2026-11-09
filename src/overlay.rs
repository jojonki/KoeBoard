//! Floating dictation panel: live input level, then the recognized text.
//!
//! The window can never take focus, so the paste lands in the app the user was typing in.
//! On macOS the window is transparent and every frame is drawn here with per-pixel alpha.

use crate::config::{PanelStyle, parse_color};
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder};

/// Transparent room around the pill for its shadow and entrance motion.
const PAD: f64 = if cfg!(target_os = "macos") { 14.0 } else { 0.0 };
const ANIMATED: bool = cfg!(target_os = "macos");
const FRAME: Duration = Duration::from_millis(16);
const RESULT_MIN_MS: u64 = 1000;
const RESULT_MS_PER_CHAR: u64 = 45;
const RESULT_MAX_MS: u64 = 8000;
const PREVIEW_TIME: Duration = Duration::from_millis(2500);
const SIDE_MARGIN: f32 = 16.0;
const TEXT_V_MARGIN: f32 = 9.0;
const LINE_HEIGHT: f32 = 1.45;
/// Long text may take this share of the screen height.
const MAX_SCREEN_SHARE: f64 = 0.6;
/// Screens with more logical pixels than this on their short side get the panel enlarged.
const COMFORTABLE_SHORT_SIDE: f64 = 1440.0;
/// Room kept beside the text for the Enter mark.
const ENTER_SLOT: f32 = 20.0;
const ENTER_MARK_RIGHT: f32 = 22.0;
const ENTER_MARK_SECONDS: f32 = 0.7;
const BARS: usize = 21;
/// One extra column is partly off the edge while the waveform scrolls.
const HISTORY: usize = BARS + 1;

/// Time constants, in seconds, of the eased transitions.
const FADE_IN: f32 = 0.055;
const FADE_OUT: f32 = 0.085;
const RESIZE: f32 = 0.07;
const CROSSFADE: f32 = 0.06;
const LEVEL_RISE: f32 = 0.015;
const LEVEL_FALL: f32 = 0.05;
/// Frames for the waveform to scroll by one bar.
const LEVEL_EVERY: u32 = 3;

/// Peak level of the latest audio callback, stored as `f32` bits.
pub type Level = Arc<AtomicU32>;

pub fn store_level(level: &Level, peak: f32) {
    level.store(peak.to_bits(), Ordering::Relaxed);
}

fn read_level(level: &Level) -> f32 {
    f32::from_bits(level.load(Ordering::Relaxed))
}

/// Maps a linear peak to 0..=1 on a perceptual (dB) scale.
fn bar_height(peak: f32) -> f32 {
    const FLOOR_DB: f32 = -50.0;
    if peak <= 0.0 {
        return 0.0;
    }
    ((20.0 * peak.log10() - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

/// Widens the gap between silence and speech so the bars clearly answer the voice.
fn expressive(height: f32) -> f32 {
    (height * 1.35).min(1.0).powf(1.25)
}

fn approach(value: f32, target: f32, dt: f32, tau: f32) -> f32 {
    value + (target - value) * (1.0 - (-dt / tau).exp())
}

/// Extra enlargement for a high-resolution screen the OS leaves at a low scale,
/// such as a 4K display at 100%. Screens the OS already scales stay at 1.
fn display_zoom(width: u32, height: u32, device_scale: f64) -> f64 {
    (width.min(height) as f64 / device_scale / COMFORTABLE_SHORT_SIDE).max(1.0)
}

fn panel_scale(monitor: &tao::monitor::MonitorHandle) -> f64 {
    let (size, device) = (monitor.size(), monitor.scale_factor());
    device * display_zoom(size.width, size.height, device)
}

/// Opacity of the Enter mark: a quick appear, a short hold, then gone.
fn enter_mark_strength(elapsed: f32) -> f32 {
    (elapsed / 0.08).min(1.0) * ((ENTER_MARK_SECONDS - elapsed) / 0.2).clamp(0.0, 1.0)
}

#[derive(Clone, Copy)]
struct Colors {
    background: u32,
    level: u32,
    level_end: u32,
    busy: u32,
    text: u32,
}

impl Colors {
    fn from_style(style: &PanelStyle) -> Self {
        let parse = |value: &str, default: &str| {
            parse_color(value).or_else(|| parse_color(default)).unwrap_or(0)
        };
        let fallback = PanelStyle::default();
        Self {
            background: parse(&style.background, &fallback.background),
            level: parse(&style.level_color, &fallback.level_color),
            level_end: parse(&style.level_color_end, &fallback.level_color_end),
            busy: parse(&style.busy_color, &fallback.busy_color),
            text: parse(&style.text_color, &fallback.text_color),
        }
    }
}

#[derive(PartialEq)]
enum View {
    Hidden,
    Listening,
    Recognizing,
    Result(String),
}

pub struct Overlay {
    window: Rc<Window>,
    presenter: Presenter,
    font: Option<FontVec>,
    level: Level,
    levels: VecDeque<f32>,
    scroll: f32,
    lines: Vec<String>,
    view: View,
    style: PanelStyle,
    colors: Colors,
    target_width: f32,
    target_height: f32,
    pill_width: f32,
    pill_height: f32,
    window_height: f64,
    /// Physical pixels per logical pixel on the panel's screen.
    device_scale: f64,
    zoom: f64,
    alpha: f32,
    text_mix: f32,
    busy_mix: f32,
    leaving: bool,
    enter_slot: f32,
    enter_at: Option<Instant>,
    pixels: Vec<u32>,
    frame_count: u32,
    result_until: Instant,
    next_frame: Instant,
    last_tick: Instant,
    started: Instant,
}

impl Overlay {
    pub fn new<T>(
        target: &EventLoopWindowTarget<T>,
        level: Level,
        style: PanelStyle,
    ) -> Result<Self, String> {
        let style = style.clamped();
        let builder = WindowBuilder::new()
            .with_title("KoeBoard")
            .with_inner_size(LogicalSize::new(style.width, style.height))
            .with_decorations(false)
            .with_resizable(false)
            .with_always_on_top(true)
            .with_visible(false)
            .with_focused(false)
            .with_focusable(false)
            .with_visible_on_all_workspaces(true);
        #[cfg(windows)]
        let builder = {
            use tao::platform::windows::WindowBuilderExtWindows;
            builder.with_skip_taskbar(true).with_undecorated_shadow(false)
        };
        let window = Rc::new(builder.build(target).map_err(|e| e.to_string())?);
        let _ = window.set_ignore_cursor_events(true);
        let presenter = Presenter::new(&window)?;
        let now = Instant::now();
        Ok(Self {
            window,
            presenter,
            font: load_font(),
            level,
            levels: VecDeque::from(vec![0.0; HISTORY]),
            scroll: 0.0,
            lines: Vec::new(),
            view: View::Hidden,
            colors: Colors::from_style(&style),
            target_width: style.width as f32,
            pill_width: style.width as f32,
            target_height: style.height as f32,
            pill_height: style.height as f32,
            window_height: style.height,
            device_scale: 1.0,
            zoom: 1.0,
            style,
            alpha: 0.0,
            text_mix: 0.0,
            busy_mix: 0.0,
            leaving: false,
            enter_slot: 0.0,
            enter_at: None,
            pixels: Vec::new(),
            frame_count: 0,
            result_until: now,
            next_frame: now,
            last_tick: now,
            started: now,
        })
    }

    /// Applies edited settings.
    pub fn set_style(&mut self, style: PanelStyle) {
        let style = style.clamped();
        if style != self.style {
            self.colors = Colors::from_style(&style);
            self.style = style;
        }
    }

    /// The window never needs to change on macOS: it is invisible outside the pill.
    fn window_width(&self) -> f64 {
        if ANIMATED { self.style.max_text_width + 2.0 * PAD } else { self.target_width as f64 }
    }

    fn apply_geometry(&mut self) {
        // The window's own scale is stale until it has moved to this screen.
        if let Some(m) = self.monitor() {
            let size = m.size();
            self.device_scale = m.scale_factor();
            self.zoom = display_zoom(size.width, size.height, self.device_scale);
        }
        self.window_height = self.target_height as f64;
        let (width, height) = (self.window_width(), self.window_height + 2.0 * PAD);
        #[cfg(target_os = "macos")]
        self.presenter.set_anchor(&self.style.position);
        self.resize_window(width, height);
        self.place(width, height);
        // Windows rescales a window that lands on a screen with another DPI.
        if cfg!(windows) {
            self.resize_window(width, height);
        }
    }

    fn resize_window(&self, width: f64, height: f64) {
        if cfg!(windows) {
            let scale = self.device_scale * self.zoom;
            let (w, h) = ((width * scale).ceil() as u32, (height * scale).ceil() as u32);
            self.window.set_inner_size(PhysicalSize::new(w, h));
        } else {
            self.window.set_inner_size(LogicalSize::new(width * self.zoom, height * self.zoom));
        }
    }

    fn monitor(&self) -> Option<tao::monitor::MonitorHandle> {
        self.window
            .cursor_position()
            .ok()
            .and_then(|p| self.window.monitor_from_point(p.x, p.y))
            .or_else(|| self.window.primary_monitor())
    }

    fn place(&self, width: f64, height: f64) {
        let window = &self.window;
        let Some(m) = self.monitor() else { return };
        let scale = panel_scale(&m);
        let (w, h) = ((width * scale) as i32, (height * scale) as i32);
        let pad = (PAD * scale) as i32;
        let gap = (self.style.margin * scale) as i32;
        let screen = m.size().height as i32;
        let y = match self.style.position.as_str() {
            "top" => gap - pad,
            "center" => (screen - h) / 2 + gap,
            _ => screen - h - gap + pad,
        };
        window.set_outer_position(PhysicalPosition::new(
            m.position().x + (m.size().width as i32 - w) / 2 + (self.style.offset_x * scale) as i32,
            m.position().y + y,
        ));
    }

    fn show(&mut self, view: View, width: f64, height: f64) {
        let appearing = self.view == View::Hidden;
        let resized = width as f32 != self.target_width || height as f32 != self.target_height;
        let grows = height > self.window_height;
        self.target_width = width as f32;
        self.target_height = height as f32;
        if appearing {
            self.alpha = 0.0;
            self.pill_width = width as f32 * 0.9;
            self.pill_height = height as f32;
            self.text_mix = if matches!(view, View::Result(_)) { 1.0 } else { 0.0 };
            self.busy_mix = if view == View::Recognizing { 1.0 } else { 0.0 };
            self.last_tick = Instant::now();
        }
        if appearing || grows || (resized && !ANIMATED) {
            self.apply_geometry();
        }
        if appearing {
            self.window.set_visible(true);
        }
        self.leaving = false;
        self.view = view;
        self.next_frame = Instant::now();
    }

    pub fn listen(&mut self) {
        self.enter_at = None;
        self.levels.iter_mut().for_each(|l| *l = 0.0);
        self.scroll = 0.0;
        let (width, height) = (self.style.width, self.style.height);
        self.show(View::Listening, width, height);
    }

    pub fn recognizing(&mut self) {
        let (width, height) = (self.style.width, self.style.height);
        self.show(View::Recognizing, width, height);
    }

    /// `enter_follows` keeps room for the mark that `enter_pressed` shows.
    pub fn result(&mut self, text: String, enter_follows: bool) {
        self.enter_at = None;
        self.enter_slot = if enter_follows { ENTER_SLOT } else { 0.0 };
        let (width, height) = self.layout_text(&text);
        let ms = RESULT_MIN_MS + RESULT_MS_PER_CHAR * text.chars().count() as u64;
        self.result_until = Instant::now() + Duration::from_millis(ms.min(RESULT_MAX_MS));
        self.show(View::Result(text), width, height);
    }

    /// Shows sample text with the current style, for tuning position and color.
    pub fn preview(&mut self) {
        let text = "This is how your dictated text will look.".to_owned();
        self.enter_at = None;
        self.enter_slot = 0.0;
        let (width, height) = self.layout_text(&text);
        self.show(View::Result(text), width, height);
        self.result_until = Instant::now() + PREVIEW_TIME;
        self.apply_geometry();
    }

    /// Flashes the Enter mark, alone in the panel when there was no text to show.
    pub fn enter_pressed(&mut self) {
        let now = Instant::now();
        let mark = Duration::from_secs_f32(ENTER_MARK_SECONDS);
        match self.view {
            View::Result(_) => self.result_until = self.result_until.max(now + mark),
            View::Recognizing => {
                self.lines.clear();
                self.enter_slot = 0.0;
                self.result_until = now + mark;
                let (width, height) = (self.style.width, self.style.height);
                self.show(View::Result(String::new()), width, height);
            }
            View::Hidden | View::Listening => return,
        }
        self.enter_at = Some(now);
    }

    /// Nothing was recognized, so there is no result to wait for.
    pub fn finish_empty(&mut self) {
        if self.view == View::Recognizing {
            self.leaving = true;
        }
    }

    /// Wraps the text and returns the panel size that shows all of it.
    fn layout_text(&mut self, text: &str) -> (f64, f64) {
        let Some(font) = &self.font else {
            self.lines.clear();
            return (self.style.width, self.style.height);
        };
        let size = self.style.text_size;
        let scaled = font.as_scaled(PxScale::from(size));
        let advance = |c: char| scaled.h_advance(font.glyph_id(c));
        let room = self.style.max_text_width as f32 - 2.0 * SIDE_MARGIN - self.enter_slot;
        let mut lines = wrap_lines(text, room, advance);

        let line_height = size * LINE_HEIGHT;
        let screen = self
            .monitor()
            .map_or(800.0, |m| m.size().height as f64 / panel_scale(&m));
        let available = (screen * MAX_SCREEN_SHARE) as f32 - 2.0 * TEXT_V_MARGIN;
        let max_lines = ((available / line_height) as usize).max(1);
        if lines.len() > max_lines {
            // Only for text taller than the screen: the newest words matter most.
            lines.drain(..lines.len() - max_lines);
            lines[0].insert(0, '…');
        }

        let widest = lines
            .iter()
            .map(|l| l.chars().map(advance).sum::<f32>())
            .fold(0.0, f32::max);
        let width = ((widest + 2.0 * SIDE_MARGIN + self.enter_slot) as f64 + 2.0).clamp(self.style.width, self.style.max_text_width);
        let height = (lines.len() as f32 * line_height + 2.0 * TEXT_V_MARGIN) as f64;
        self.lines = lines;
        (width, height.max(self.style.height))
    }

    fn hide(&mut self) {
        self.view = View::Hidden;
        self.leaving = false;
        // Otherwise the last frame flashes when the panel next appears.
        let size = self.window.inner_size();
        let (w, h) = (size.width as usize, size.height as usize);
        if self.pixels.len() == w * h && w * h > 0 {
            self.pixels.fill(0);
            self.presenter.present(&self.pixels, w, h, self.device_scale as f32);
        }
        self.window.set_visible(false);
    }

    pub fn next_wake(&self) -> Option<Instant> {
        (self.view != View::Hidden).then_some(self.next_frame)
    }

    pub fn tick(&mut self, now: Instant) {
        if self.view == View::Hidden || now < self.next_frame {
            return;
        }
        if matches!(self.view, View::Result(_)) && now >= self.result_until {
            self.leaving = true;
        }
        self.next_frame = now + FRAME;
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.05);
        self.last_tick = now;
        self.frame_count = self.frame_count.wrapping_add(1);
        if self.view == View::Listening {
            // The newest column follows the voice every frame, then scrolls away unchanged.
            let target = expressive(bar_height(read_level(&self.level)));
            let front = self.levels.front().copied().unwrap_or(0.0);
            let tau = if target > front { LEVEL_RISE } else { LEVEL_FALL };
            let value = approach(front, target, dt, tau);
            let step = self.frame_count % LEVEL_EVERY;
            if step == 0 {
                self.levels.pop_back();
                self.levels.push_front(value);
            } else if let Some(slot) = self.levels.front_mut() {
                *slot = value;
            }
            self.scroll = step as f32 / LEVEL_EVERY as f32;
        }
        let text_target = if matches!(self.view, View::Result(_)) { 1.0 } else { 0.0 };
        // Stays up during the result, so the waveform cannot reappear before the text.
        let busy_target = if self.view == View::Listening { 0.0 } else { 1.0 };
        if ANIMATED {
            let (goal, tau) = if self.leaving { (0.0, FADE_OUT) } else { (1.0, FADE_IN) };
            self.alpha = approach(self.alpha, goal, dt, tau);
            self.pill_width = approach(self.pill_width, self.target_width, dt, RESIZE);
            self.pill_height = approach(self.pill_height, self.target_height, dt, RESIZE);
            self.text_mix = approach(self.text_mix, text_target, dt, CROSSFADE);
            self.busy_mix = approach(self.busy_mix, busy_target, dt, CROSSFADE);
            if self.leaving && self.alpha < 0.012 {
                self.hide();
                return;
            }
        } else {
            if self.leaving {
                self.hide();
                return;
            }
            self.alpha = 1.0;
            self.pill_width = self.target_width;
            self.pill_height = self.target_height;
            self.text_mix = text_target;
            self.busy_mix = busy_target;
        }
        self.draw(now);
    }

    fn draw(&mut self, now: Instant) {
        let size = self.window.inner_size();
        let (w, h) = (size.width as usize, size.height as usize);
        if w == 0 || h == 0 {
            return;
        }
        self.pixels.resize(w * h, 0);
        let scale = (self.device_scale * self.zoom) as f32;
        let lines: &[String] = if matches!(self.view, View::Result(_)) { &self.lines } else { &[] };
        let levels: Vec<f32> = self.levels.iter().copied().collect();
        let frame = Frame {
            scroll: self.scroll,
            scale,
            pill_width: self.pill_width * scale,
            pill_height: self.pill_height * scale,
            corner: self.style.height as f32 * scale / 2.0,
            anchor: match self.style.position.as_str() {
                "top" => Anchor::Top,
                "center" => Anchor::Center,
                _ => Anchor::Bottom,
            },
            pad: PAD as f32 * scale,
            opacity: self.style.opacity as f32,
            alpha: self.alpha,
            rise: (1.0 - self.alpha) * 6.0 * scale,
            text_mix: self.text_mix,
            busy_mix: self.busy_mix,
            phase: now.duration_since(self.started).as_secs_f32(),
            levels: &levels,
            lines,
            text_px: self.style.text_size * scale,
            text_shift: -self.enter_slot * scale / 2.0,
            enter_mark: self
                .enter_at
                .map_or(0.0, |at| enter_mark_strength(now.duration_since(at).as_secs_f32())),
            colors: self.colors,
        };
        paint(&mut self.pixels, w, h, self.font.as_ref(), &frame);
        self.presenter.present(&self.pixels, w, h, self.device_scale as f32);
    }
}

#[cfg(target_os = "macos")]
struct Presenter {
    layer: *mut objc2::runtime::AnyObject,
}

#[cfg(target_os = "macos")]
impl Presenter {
    fn new(window: &Rc<Window>) -> Result<Self, String> {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        use tao::platform::macos::WindowExtMacOS;
        let ns_window = window.ns_window() as *mut AnyObject;
        let layer: *mut AnyObject = unsafe {
            let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![ns_window, setOpaque: false];
            let _: () = msg_send![ns_window, setBackgroundColor: clear];
            let _: () = msg_send![ns_window, setHasShadow: false];
            // kCGAssistiveTechHighWindowLevel: above Spotlight, the Dock and menus.
            let _: () = msg_send![ns_window, setLevel: 1500isize];
            // canJoinAllSpaces | stationary | ignoresCycle | fullScreenAuxiliary
            let _: () = msg_send![ns_window, setCollectionBehavior: 337usize];
            let layer: *mut AnyObject = msg_send![class!(CALayer), new];
            let view: *mut AnyObject = msg_send![ns_window, contentView];
            // Layer-hosting view: AppKit leaves this layer's contents alone.
            let _: () = msg_send![view, setLayer: layer];
            let _: () = msg_send![view, setWantsLayer: true];
            layer
        };
        Ok(Self { layer })
    }

    /// Keeps the pill still for the one frame after the window changes height.
    fn set_anchor(&self, position: &str) {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let name: &[u8] = match position {
            "top" => b"top\0",
            "center" => b"center\0",
            _ => b"bottom\0",
        };
        unsafe {
            let gravity: *mut AnyObject =
                msg_send![class!(NSString), stringWithUTF8String: name.as_ptr() as *const std::ffi::c_char];
            let _: () = msg_send![self.layer, setContentsGravity: gravity];
        }
    }

    fn present(&mut self, pixels: &[u32], w: usize, h: usize, scale: f32) {
        use core_graphics::base::{kCGBitmapByteOrder32Little, kCGImageAlphaPremultipliedFirst, kCGRenderingIntentDefault};
        use core_graphics::color_space::CGColorSpace;
        use core_graphics::data_provider::CGDataProvider;
        use core_graphics::image::CGImage;
        use foreign_types::ForeignType;
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};

        let bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
        let provider = CGDataProvider::from_buffer(Arc::new(bytes));
        let image = CGImage::new(
            w,
            h,
            8,
            32,
            w * 4,
            &CGColorSpace::create_device_rgb(),
            kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
            &provider,
            false,
            kCGRenderingIntentDefault,
        );
        unsafe {
            let transaction = class!(CATransaction);
            let _: () = msg_send![transaction, begin];
            // Without this, Core Animation cross-fades every frame.
            let _: () = msg_send![transaction, setDisableActions: true];
            let _: () = msg_send![self.layer, setContentsScale: scale as f64];
            let _: () = msg_send![self.layer, setContents: image.as_ptr() as *mut AnyObject];
            let _: () = msg_send![transaction, commit];
        }
    }
}

/// Hands each premultiplied frame to the compositor, so the pill's edges blend
/// with whatever is behind it and no unpainted window surface can show.
#[cfg(windows)]
struct Presenter {
    hwnd: windows_sys::Win32::Foundation::HWND,
    dc: windows_sys::Win32::Graphics::Gdi::HDC,
    bitmap: windows_sys::Win32::Graphics::Gdi::HBITMAP,
    bits: *mut u32,
    size: (usize, usize),
    reported: bool,
}

#[cfg(windows)]
impl Presenter {
    fn new(window: &Rc<Window>) -> Result<Self, String> {
        use tao::platform::windows::WindowExtWindows;
        use windows_sys::Win32::Graphics::Gdi::CreateCompatibleDC;
        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if dc.is_null() {
            return Err(format!("CreateCompatibleDC: {}", std::io::Error::last_os_error()));
        }
        Ok(Self {
            hwnd: window.hwnd() as _,
            dc,
            bitmap: std::ptr::null_mut(),
            bits: std::ptr::null_mut(),
            size: (0, 0),
            reported: false,
        })
    }

    fn resize(&mut self, w: usize, h: usize) -> bool {
        use windows_sys::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, SelectObject,
        };
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            // Negative: rows run top to bottom, like the frame.
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        };
        let mut bits = std::ptr::null_mut();
        let bitmap =
            unsafe { CreateDIBSection(self.dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0) };
        if bitmap.is_null() || bits.is_null() {
            return false;
        }
        unsafe {
            SelectObject(self.dc, bitmap as _);
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap as _);
            }
        }
        self.bitmap = bitmap;
        self.bits = bits.cast();
        self.size = (w, h);
        true
    }

    fn present(&mut self, pixels: &[u32], w: usize, h: usize, _scale: f32) {
        use windows_sys::Win32::Foundation::{POINT, SIZE};
        use windows_sys::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION};
        use windows_sys::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
        if self.size != (w, h) && !self.resize(w, h) {
            return;
        }
        let size = SIZE { cx: w as i32, cy: h as i32 };
        let origin = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let updated = unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), self.bits, w * h);
            UpdateLayeredWindow(
                self.hwnd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
                self.dc,
                &origin,
                0,
                &blend,
                ULW_ALPHA,
            )
        };
        if updated == 0 && !std::mem::replace(&mut self.reported, true) {
            crate::log(&format!("error: UpdateLayeredWindow: {}", std::io::Error::last_os_error()));
        }
    }
}

#[cfg(windows)]
impl Drop for Presenter {
    fn drop(&mut self) {
        use windows_sys::Win32::Graphics::Gdi::{DeleteDC, DeleteObject};
        unsafe {
            DeleteDC(self.dc);
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap as _);
            }
        }
    }
}

/// Covers Japanese and Latin alike, whatever fonts this Windows has installed.
fn bundled_font() -> Option<FontVec> {
    const NOTO_SANS_JP: &[u8] = include_bytes!("../assets/fonts/NotoSansJP-Medium.otf");
    FontVec::try_from_vec(NOTO_SANS_JP.to_vec()).ok()
}

fn load_font() -> Option<FontVec> {
    const CANDIDATES: &[&str] = &[
        "/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc",
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
    ];
    let font = if cfg!(windows) {
        bundled_font()
    } else {
        CANDIDATES.iter().find_map(|path| {
            let bytes = std::fs::read(path).ok()?;
            FontVec::try_from_vec_and_index(bytes, 0).ok()
        })
    };
    if font.is_none() {
        crate::log("error: no suitable font found; the panel shows no text");
    }
    font
}

#[derive(Clone, Copy)]
enum Anchor {
    Top,
    Center,
    Bottom,
}

struct Frame<'a> {
    /// Fraction of a bar the waveform has moved toward the left.
    scroll: f32,
    scale: f32,
    pill_width: f32,
    pill_height: f32,
    corner: f32,
    anchor: Anchor,
    pad: f32,
    opacity: f32,
    alpha: f32,
    rise: f32,
    text_mix: f32,
    busy_mix: f32,
    phase: f32,
    levels: &'a [f32],
    lines: &'a [String],
    text_px: f32,
    /// Horizontal move of the text, making room for the Enter mark.
    text_shift: f32,
    enter_mark: f32,
    colors: Colors,
}

/// Signed distance from a point to a capsule centered at the origin.
fn capsule(x: f32, y: f32, half_w: f32, half_h: f32) -> f32 {
    rounded_box(x, y, half_w, half_h, half_w.min(half_h))
}

fn rounded_box(x: f32, y: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let radius = radius.min(half_w).min(half_h);
    let qx = x.abs() - (half_w - radius);
    let qy = y.abs() - (half_h - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

fn segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// Signed distance to a return-key arrow, in units of about 14 across.
fn enter_arrow(p: (f32, f32)) -> f32 {
    const STROKES: [((f32, f32), (f32, f32)); 4] = [
        ((5.5, -5.0), (5.5, 2.0)),
        ((5.5, 2.0), (-5.5, 2.0)),
        ((-5.5, 2.0), (-2.0, -1.5)),
        ((-5.5, 2.0), (-2.0, 5.5)),
    ];
    STROKES.iter().map(|(a, b)| segment(p, *a, *b)).fold(f32::MAX, f32::min) - 0.9
}

fn channels(rgb: u32) -> [f32; 3] {
    [((rgb >> 16) & 255) as f32 / 255.0, ((rgb >> 8) & 255) as f32 / 255.0, (rgb & 255) as f32 / 255.0]
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Source-over onto a premultiplied RGBA accumulator.
fn over(dst: &mut [f32; 4], rgb: [f32; 3], alpha: f32) {
    let keep = 1.0 - alpha;
    for i in 0..3 {
        dst[i] = rgb[i] * alpha + dst[i] * keep;
    }
    dst[3] = alpha + dst[3] * keep;
}

fn pack(c: [f32; 4]) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    byte(c[3]) << 24 | byte(c[0]) << 16 | byte(c[1]) << 8 | byte(c[2])
}

/// Height (0..=1) of the column `age` steps behind the newest audio.
fn bar_shape(levels: &[f32], age: usize) -> f32 {
    levels.get(age).copied().unwrap_or(0.0).clamp(0.0, 1.0)
}

fn paint(buf: &mut [u32], w: usize, h: usize, font: Option<&FontVec>, f: &Frame) {
    buf.fill(0);
    let (half_w, half_h) = (f.pill_width / 2.0, f.pill_height / 2.0);
    let center_y = match f.anchor {
        Anchor::Top => f.pad + half_h,
        Anchor::Center => h as f32 / 2.0,
        Anchor::Bottom => h as f32 - f.pad - half_h,
    };
    let (cx, cy) = (w as f32 / 2.0, center_y + f.rise);
    let shadow = if ANIMATED { 11.0 * f.scale } else { 0.0 };
    let reach = shadow + 2.0;

    let background = channels(f.colors.background);
    let (start, end) = (channels(f.colors.level), channels(f.colors.level_end));
    let busy_color = channels(f.colors.busy);

    let count = BARS as f32;
    let area = (f.pill_width - 2.0 * SIDE_MARGIN * f.scale).max(1.0);
    let pitch = (area / count).clamp(3.0 * f.scale, 7.0 * f.scale);
    let bar_half = pitch * 0.27;
    let left = -pitch * (count - 1.0) / 2.0;
    let max_half = (half_h - 5.0 * f.scale).max(bar_half);
    let bars_alpha = (1.0 - f.text_mix) * (1.0 - f.busy_mix);
    let dots_alpha = (1.0 - f.text_mix) * f.busy_mix;
    let glow = 3.5 * f.scale;
    let scroll = f.scroll;
    let span = pitch * (count - 1.0);

    let x0 = (cx - half_w - reach).floor().max(0.0) as usize;
    let x1 = ((cx + half_w + reach).ceil() as usize).min(w);
    let y0 = (cy - half_h - reach).floor().max(0.0) as usize;
    let y1 = ((cy + half_h + reach).ceil() as usize).min(h);

    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let d = rounded_box(px, py, half_w, half_h, f.corner);
            let mut c = [0.0f32; 4];
            if d > 0.0 && shadow > 0.0 {
                let t = 1.0 - (d / shadow).min(1.0);
                c[3] = 0.32 * t * t;
            }
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                over(&mut c, background, f.opacity * cover);
                // Hairline edge: reads as glass rather than a flat box.
                let edge = (1.0 - (d + 1.0 * f.scale).abs() / f.scale).clamp(0.0, 1.0);
                over(&mut c, [1.0, 1.0, 1.0], 0.10 * edge * cover);
                if bars_alpha > 0.004 {
                    // Newest audio enters at the right and scrolls left, like a recorder.
                    let right = left + span;
                    let index = ((right - px) / pitch - scroll).round().clamp(0.0, count);
                    let bar_x = right - (index + scroll) * pitch;
                    let shape = bar_shape(f.levels, index as usize);
                    let half = bar_half + (max_half - bar_half) * shape;
                    let bd = capsule(px - bar_x, py, bar_half, half);
                    let t = ((px - left) / span).clamp(0.0, 1.0);
                    let color = lerp3(start, end, t);
                    // Old columns fade at the left; the newest stays fully visible at the right.
                    let edge = ((bar_x - left + pitch * 0.5) / (pitch * 2.5)).clamp(0.0, 1.0);
                    let strength = edge;
                    if bd > 0.0 && bd < glow {
                        let g = 1.0 - bd / glow;
                        over(&mut c, color, 0.22 * g * g * bars_alpha * cover * shape.max(0.25) * strength);
                    }
                    let bar = (0.5 - bd).clamp(0.0, 1.0);
                    over(&mut c, color, bar * bars_alpha * cover * strength);
                }
                if dots_alpha > 0.004 {
                    // Three dots pulsing in turn: working, not listening.
                    for i in 0..3 {
                        let pulse = ((f.phase * 6.0 - i as f32 * 1.0).sin() + 1.0) / 2.0;
                        let radius = (2.2 + 0.9 * pulse) * f.scale;
                        let dot_x = (i as f32 - 1.0) * 11.0 * f.scale;
                        let dot = (0.5 - capsule(px - dot_x, py, radius, radius)).clamp(0.0, 1.0);
                        over(&mut c, busy_color, dot * dots_alpha * cover * (0.4 + 0.6 * pulse));
                    }
                }
                if f.enter_mark > 0.004 {
                    let mark_x = if f.lines.is_empty() { 0.0 } else { half_w - ENTER_MARK_RIGHT * f.scale };
                    let arrow = enter_arrow(((px - mark_x) / f.scale, py / f.scale)) * f.scale;
                    over(&mut c, start, (0.5 - arrow).clamp(0.0, 1.0) * f.enter_mark * cover);
                }
            }
            buf[y * w + x] = pack(c);
        }
    }

    if let Some(font) = font.filter(|_| f.text_mix > 0.01 && !f.lines.is_empty()) {
        let clip = [cx - half_w + 4.0, cx + half_w - 4.0, cy - half_h, cy + half_h];
        draw_text(buf, w, h, font, f, (cx + f.text_shift, cy), clip);
    }

    // Global fade, applied last so the text fades with everything else.
    if f.alpha < 0.999 {
        let scale = f.alpha.clamp(0.0, 1.0);
        for pixel in &mut buf[..] {
            let a = |shift: u32| (((*pixel >> shift) & 255) as f32 * scale) as u32;
            *pixel = a(24) << 24 | a(16) << 16 | a(8) << 8 | a(0);
        }
    }
}

/// Breaks text to fit `max_width`, keeping every character.
/// Closing punctuation stays on the line it follows.
fn wrap_lines(text: &str, max_width: f32, advance: impl Fn(char) -> f32) -> Vec<String> {
    const NO_LINE_START: &str = "、。，．！？!?,.）」』】〕)]}ぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮー";
    let mut lines = vec![String::new()];
    let mut width = 0.0;
    for c in text.chars() {
        let w = advance(c);
        let current_empty = lines.last().is_none_or(String::is_empty);
        if width + w > max_width && !current_empty && !NO_LINE_START.contains(c) {
            lines.push(String::new());
            width = 0.0;
        }
        if let Some(line) = lines.last_mut() {
            line.push(c);
        }
        width += w;
    }
    lines
}

fn draw_text(
    buf: &mut [u32],
    w: usize,
    h: usize,
    font: &FontVec,
    f: &Frame,
    center: (f32, f32),
    clip: [f32; 4],
) {
    let scaled = font.as_scaled(PxScale::from(f.text_px));
    let advance = |c: char| scaled.h_advance(font.glyph_id(c));
    let color = channels(f.colors.text);
    let line_height = f.text_px * LINE_HEIGHT;
    let block = line_height * f.lines.len() as f32;
    let first_center = center.1 - block / 2.0 + line_height / 2.0;
    let widest = f
        .lines
        .iter()
        .map(|l| l.chars().map(advance).sum::<f32>())
        .fold(0.0, f32::max);
    let multiline = f.lines.len() > 1;

    for (row, line) in f.lines.iter().enumerate() {
        let line_width: f32 = line.chars().map(advance).sum();
        // One line is centered; several read better left-aligned as a block.
        let mut x = center.0 - if multiline { widest } else { line_width } / 2.0;
        // A whole-pixel baseline keeps horizontal strokes sharp.
        let baseline = (first_center + row as f32 * line_height + (scaled.ascent() + scaled.descent()) / 2.0).round();
        for c in line.chars() {
            let id = font.glyph_id(c);
            let glyph = id.with_scale_and_position(f.text_px, point(x, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let px_x = bounds.min.x as i32 + gx as i32;
                    let px_y = bounds.min.y as i32 + gy as i32;
                    let inside = (clip[0] as i32..clip[1] as i32).contains(&px_x)
                        && (clip[2] as i32..clip[3] as i32).contains(&px_y)
                        && (0..w as i32).contains(&px_x)
                        && (0..h as i32).contains(&px_y);
                    if inside {
                        let pixel = &mut buf[px_y as usize * w + px_x as usize];
                        let unpack = |shift: u32| ((*pixel >> shift) & 255) as f32 / 255.0;
                        let mut c = [unpack(16), unpack(8), unpack(0), unpack(24)];
                        over(&mut c, color, coverage.clamp(0.0, 1.0) * f.text_mix);
                        *pixel = pack(c);
                    }
                });
            }
            x += scaled.h_advance(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame<'a>(levels: &'a [f32], lines: &'a [String], mix: f32, busy: bool) -> Frame<'a> {
        Frame {
            scroll: 0.0,
            scale: 2.0,
            pill_width: 296.0,
            pill_height: 72.0,
            corner: 36.0,
            anchor: Anchor::Center,
            pad: 28.0,
            opacity: 0.9,
            alpha: 1.0,
            rise: 0.0,
            text_mix: mix,
            busy_mix: if busy { 1.0 } else { 0.0 },
            phase: 1.0,
            levels,
            lines,
            text_px: 26.0,
            text_shift: 0.0,
            enter_mark: 0.0,
            colors: Colors::from_style(&PanelStyle::default()),
        }
    }

    #[test]
    fn level_scale() {
        assert_eq!(bar_height(0.0), 0.0);
        assert_eq!(bar_height(1.0), 1.0);
        assert_eq!(bar_height(1e-4), 0.0);
        let speech = bar_height(0.1);
        assert!((0.55..0.65).contains(&speech), "{speech}");
    }

    #[test]
    fn only_unscaled_large_screens_are_zoomed() {
        assert_eq!(display_zoom(1920, 1080, 1.0), 1.0);
        assert_eq!(display_zoom(3840, 2160, 1.5), 1.0, "4K at 150%");
        assert_eq!(display_zoom(3840, 2160, 2.0), 1.0, "4K at 200%");
        assert_eq!(display_zoom(3840, 2160, 1.0), 1.5, "4K at 100%");
        assert_eq!(display_zoom(2160, 3840, 1.0), 1.5, "portrait");
    }

    #[test]
    fn wrapping_keeps_every_character() {
        let text = "あいうえおかきくけこさしすせそ";
        let lines = wrap_lines(text, 45.0, |_| 10.0);
        assert_eq!(lines.concat(), text);
        assert!(lines.iter().all(|l| l.chars().count() <= 4));
        assert_eq!(wrap_lines("あいう", 100.0, |_| 10.0).len(), 1);
    }

    #[test]
    fn punctuation_never_starts_a_line() {
        let lines = wrap_lines("あいう。えお", 30.0, |_| 10.0);
        assert_eq!(lines[0], "あいう。");
        assert_eq!(lines[1], "えお");
    }

    #[test]
    fn speech_stands_out_from_noise() {
        assert!(expressive(0.1) < 0.1);
        assert!(expressive(0.72) > 0.9);
        assert_eq!(expressive(1.0), 1.0);
    }

    #[test]
    fn bundled_font_covers_japanese_and_latin() {
        let font = bundled_font().expect("bundled font parses");
        for c in "あア漢Aa1。、".chars() {
            assert_ne!(font.glyph_id(c).0, 0, "{c}");
        }
    }

    #[test]
    fn capsule_distance() {
        assert!(capsule(0.0, 0.0, 10.0, 4.0) < 0.0);
        assert!((capsule(11.0, 0.0, 10.0, 4.0) - 1.0).abs() < 1e-4);
        assert!(capsule(10.0, 4.0, 10.0, 4.0) > 0.0, "corners are rounded");
    }

    #[test]
    fn approach_converges_without_overshoot() {
        let mut v = 0.0;
        for _ in 0..200 {
            v = approach(v, 1.0, 0.016, FADE_IN);
            assert!(v <= 1.0);
        }
        assert!(v > 0.999);
    }

    #[test]
    fn corners_stay_transparent_and_center_is_filled() {
        let (w, h) = (360, 128);
        let mut buf = vec![0xFFFF_FFFFu32; w * h];
        paint(&mut buf, w, h, None, &frame(&[0.8; HISTORY], &[], 0.0, false));
        assert_eq!(buf[0], 0, "no box outside the pill");
        assert!(buf[h / 2 * w + w / 2] >> 24 > 200, "pill is drawn");
    }

    #[test]
    fn enter_mark_is_brief_and_visible() {
        assert_eq!(enter_mark_strength(0.0), 0.0);
        assert_eq!(enter_mark_strength(0.3), 1.0);
        assert_eq!(enter_mark_strength(ENTER_MARK_SECONDS), 0.0);

        let (w, h) = (360, 128);
        let (mut plain, mut marked) = (vec![0; w * h], vec![0; w * h]);
        paint(&mut plain, w, h, None, &frame(&[], &[], 1.0, false));
        let mut with_mark = frame(&[], &[], 1.0, false);
        with_mark.enter_mark = 1.0;
        paint(&mut marked, w, h, None, &with_mark);
        let on_stem = h / 2 * w + w / 2 + 11;
        assert_ne!(plain[on_stem], marked[on_stem]);
    }

    #[test]
    fn extreme_levels_stay_in_bounds() {
        let (w, h) = (120, 40);
        let mut buf = vec![0; w * h];
        for level in [-1.0, 0.0, 9.0] {
            paint(&mut buf, w, h, None, &frame(&[level; HISTORY], &[], 0.0, false));
        }
    }

    #[test]
    fn fade_scales_all_channels() {
        let (w, h) = (360, 128);
        let mut full = vec![0; w * h];
        let mut half = vec![0; w * h];
        let levels = [0.5; HISTORY];
        paint(&mut full, w, h, None, &frame(&levels, &[], 0.0, false));
        let mut faded = frame(&levels, &[], 0.0, false);
        faded.alpha = 0.5;
        paint(&mut half, w, h, None, &faded);
        let i = h / 2 * w + 20 * 2;
        let ratio = (half[i] >> 24) as f32 / (full[i] >> 24).max(1) as f32;
        assert!((0.45..0.55).contains(&ratio), "{ratio}");
    }

    /// `OVERLAY_DUMP=dir cargo test` writes BGRA frames for visual checks.
    #[test]
    fn dump_frames() {
        let Ok(dir) = std::env::var("OVERLAY_DUMP") else { return };
        let (w, h) = (560usize, 128usize);
        // OVERLAY_BUNDLED previews the Windows font from any platform.
        let font = if std::env::var_os("OVERLAY_BUNDLED").is_some() { bundled_font() } else { load_font() };
        let levels: Vec<f32> = (0..HISTORY).map(|i| 0.95 - i as f32 * 0.05 - (i as f32 * 1.4).sin().abs() * 0.3).collect();
        let one = vec!["明日の会議は午後3時からです。".to_owned()];
        let many = vec![
            "音声認識の技術は近年急速に進歩しており、パソコンの".to_owned(),
            "処理能力だけでも実用的な速度で文字起こしができるよ".to_owned(),
            "うになりました。".to_owned(),
        ];
        let mut wide = frame(&levels, &one, 1.0, false);
        wide.pill_width = 480.0;
        let mut mixed = frame(&levels, &many, 1.0, false);
        mixed.pill_width = 500.0;
        mixed.pill_height = 120.0;
        let mut sent = frame(&levels, &one, 1.0, false);
        sent.pill_width = 520.0;
        sent.text_shift = -ENTER_SLOT * sent.scale / 2.0;
        sent.enter_mark = 1.0;
        let mut alone = frame(&levels, &[], 1.0, false);
        alone.enter_mark = 1.0;
        let frames = [
            ("sent", sent),
            ("alone", alone),
            ("listen", frame(&levels, &[], 0.0, false)),
            ("busy", frame(&levels, &[], 0.0, true)),
            ("multi", mixed),
            ("text", wide),
        ];
        for (name, f) in frames {
            let mut buf = vec![0u32; w * h];
            paint(&mut buf, w, h, font.as_ref(), &f);
            let bytes: Vec<u8> = buf.iter().flat_map(|p| p.to_le_bytes()).collect();
            std::fs::write(format!("{dir}/{name}.raw"), bytes).unwrap();
        }
    }
}
