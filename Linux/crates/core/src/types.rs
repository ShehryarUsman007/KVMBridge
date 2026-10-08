//! Small shared value types used across the protocol, engine and platform layers.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Stable identifier of a device (generated once on first run).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(pub Uuid);

impl DeviceId {
    pub fn new_random() -> Self {
        DeviceId(Uuid::new_v4())
    }
    pub fn nil() -> Self {
        DeviceId(Uuid::nil())
    }
    pub fn short(&self) -> String {
        self.0.simple().to_string()[..8].to_string()
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Dev({})", self.short())
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for DeviceId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(DeviceId(Uuid::parse_str(s)?))
    }
}

/// A point in a device's global desktop coordinate space (logical pixels).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

/// Axis aligned rectangle in logical pixels. `x`/`y` is the top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    /// Half-open containment: left/top edges inclusive, right/bottom exclusive.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
    /// Nearest point inside the rectangle (the last pixel row/column is `right()-1`).
    pub fn clamp(&self, p: Point) -> Point {
        Point {
            x: p.x.clamp(self.x, (self.right() - 1.0).max(self.x)),
            y: p.y.clamp(self.y, (self.bottom() - 1.0).max(self.y)),
        }
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        let r = self.right().max(o.right());
        let b = self.bottom().max(o.bottom());
        Rect::new(x, y, r - x, b - y)
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// One of the four sides of a screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }

    /// Grid offset (column, row) of the neighbour on this side.
    pub fn grid_delta(self) -> (i32, i32) {
        match self {
            Edge::Left => (-1, 0),
            Edge::Right => (1, 0),
            Edge::Top => (0, -1),
            Edge::Bottom => (0, 1),
        }
    }

    pub fn is_horizontal_neighbor(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// Clipboard content kinds that are synchronised between devices. Platform
/// backends translate between these and their native formats/mime types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipFormat {
    /// UTF-8 text.
    Text,
    /// UTF-8 HTML fragment.
    Html,
    /// PNG encoded image.
    Png,
    /// A list of files (transferred on demand when pasted).
    Files,
}

impl ClipFormat {
    pub fn mime(self) -> &'static str {
        match self {
            ClipFormat::Text => "text/plain;charset=utf-8",
            ClipFormat::Html => "text/html",
            ClipFormat::Png => "image/png",
            ClipFormat::Files => "text/uri-list",
        }
    }
}

/// Mouse buttons, using Linux evdev codes as the canonical wire representation.
pub mod buttons {
    pub const BTN_LEFT: u32 = 0x110;
    pub const BTN_RIGHT: u32 = 0x111;
    pub const BTN_MIDDLE: u32 = 0x112;
    pub const BTN_SIDE: u32 = 0x113;
    pub const BTN_EXTRA: u32 = 0x114;
    pub const BTN_FORWARD: u32 = 0x115;
    pub const BTN_BACK: u32 = 0x116;
}

/// A few evdev key codes the engine needs to know about (hotkeys, defaults).
/// Key events on the wire always carry Linux evdev key codes.
pub mod keys {
    pub const KEY_ESC: u32 = 1;
    pub const KEY_LEFTCTRL: u32 = 29;
    pub const KEY_LEFTSHIFT: u32 = 42;
    pub const KEY_RIGHTSHIFT: u32 = 54;
    pub const KEY_LEFTALT: u32 = 56;
    pub const KEY_SCROLLLOCK: u32 = 70;
    pub const KEY_RIGHTCTRL: u32 = 97;
    pub const KEY_RIGHTALT: u32 = 100;
    pub const KEY_LEFTMETA: u32 = 125;
    pub const KEY_RIGHTMETA: u32 = 126;
    pub const KEY_PAUSE: u32 = 119;

    /// Collapses left/right modifier variants so hotkeys match either side.
    pub fn normalize(code: u32) -> u32 {
        match code {
            KEY_RIGHTCTRL => KEY_LEFTCTRL,
            KEY_RIGHTSHIFT => KEY_LEFTSHIFT,
            KEY_RIGHTALT => KEY_LEFTALT,
            KEY_RIGHTMETA => KEY_LEFTMETA,
            c => c,
        }
    }

    /// Human readable name for the handful of keys shown in the UI.
    pub fn name(code: u32) -> String {
        match code {
            KEY_ESC => "Esc".into(),
            KEY_LEFTCTRL | KEY_RIGHTCTRL => "Ctrl".into(),
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => "Shift".into(),
            KEY_LEFTALT | KEY_RIGHTALT => "Alt".into(),
            KEY_LEFTMETA | KEY_RIGHTMETA => "Super".into(),
            KEY_SCROLLLOCK => "ScrollLock".into(),
            KEY_PAUSE => "Pause".into(),
            59..=68 => format!("F{}", code - 58),
            87 => "F11".into(),
            88 => "F12".into(),
            other => format!("Key{other}"),
        }
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Microseconds from a process-local monotonic clock (for RTT measurement).
pub fn mono_us() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_micros() as u64
}

/// Short operating system tag used in device info.
pub fn os_tag() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    }
}
