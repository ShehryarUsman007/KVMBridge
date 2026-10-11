//! Input emulation (another computer controls this one) through SendInput.
//!
//! Every injected event carries [`TAG`], so the input hooks can tell it
//! apart from this computer's own keyboard and mouse.

use crate::keymap::{self, Inject};
use kvmbridge_core::platform::{BackendSide, BackendStatus, EmulationCommand, EmulationEvent};
use kvmbridge_core::types::{buttons, keys, Point};
use std::collections::HashSet;
use windows_sys::Win32::Foundation::{GetLastError, POINT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    WHEEL_DELTA, XBUTTON1, XBUTTON2,
};

/// `dwExtraInfo` of every event KVM Bridge injects ("KVMB").
pub const TAG: usize = 0x4B56_4D42;

/// Wheel units per logical pixel of smooth scrolling (a wheel notch is
/// 120 units, and scrolls about 15 pixels on GNOME).
const WHEEL_PER_PIXEL: f64 = WHEEL_DELTA as f64 / 15.0;

type Side = BackendSide<EmulationCommand, EmulationEvent>;

pub async fn run(mut side: Side) {
    let _ = side.events.send(EmulationEvent::Status(BackendStatus::Ready("SendInput".into())));
    let mut emu = Emulator::default();
    while let Some(cmd) = side.commands.recv().await {
        emu.apply(cmd);
    }
    emu.release_all();
}

#[derive(Default)]
struct Emulator {
    keys: HashSet<u32>,
    buttons: HashSet<u32>,
    /// Where the pointer was last put.
    pos: Option<Point>,
    /// Smooth scrolling not sent yet (less than one wheel unit).
    wheel: (f64, f64),
    /// Windows refused injected input last time (lock screen, UAC prompt).
    blocked: bool,
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: TAG } },
    }
}

fn key(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: TAG } },
    }
}

/// Normalised absolute coordinate (0..65535 across the virtual screen) that
/// lands on pixel `v` (the rounding AutoHotkey uses).
fn to_abs(v: i32, origin: i32, size: i32) -> i32 {
    let size = size.max(1) as i64;
    (((v - origin) as i64 * 65536) / size + 1) as i32
}

impl Emulator {
    fn send(&mut self, inputs: &[INPUT]) {
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        if (sent as usize) < inputs.len() {
            if !self.blocked {
                let err = unsafe { GetLastError() };
                tracing::warn!(
                    "Windows did not take the injected input (error {err}): the lock screen, a UAC prompt or another secure screen is probably showing"
                );
            }
            self.blocked = true;
        } else if self.blocked {
            tracing::info!("injected input reaches Windows again");
            self.blocked = false;
        }
    }

    fn move_to(&mut self, p: Point) {
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let (x, y) = (p.x.round() as i32, p.y.round() as i32);
        let flags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
        self.send(&[mouse(to_abs(x, vx, vw), to_abs(y, vy, vh), 0, flags)]);
        self.pos = Some(p);
    }

    fn current(&self) -> Point {
        self.pos.unwrap_or_else(|| {
            let mut p = POINT { x: 0, y: 0 };
            unsafe { GetCursorPos(&mut p) };
            Point::new(p.x as f64, p.y as f64)
        })
    }

    fn button(&mut self, button: u32, pressed: bool) {
        let (flags, data) = match (button, pressed) {
            (buttons::BTN_LEFT, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (buttons::BTN_LEFT, false) => (MOUSEEVENTF_LEFTUP, 0),
            (buttons::BTN_RIGHT, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (buttons::BTN_RIGHT, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (buttons::BTN_MIDDLE, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (buttons::BTN_MIDDLE, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (buttons::BTN_SIDE | buttons::BTN_BACK, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as u32),
            (buttons::BTN_SIDE | buttons::BTN_BACK, false) => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
            (buttons::BTN_EXTRA | buttons::BTN_FORWARD, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as u32),
            (buttons::BTN_EXTRA | buttons::BTN_FORWARD, false) => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
            _ => {
                tracing::debug!("no Windows mouse button for {button:#x}");
                return;
            }
        };
        self.send(&[mouse(0, 0, data, flags)]);
        if pressed {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
    }

    fn key(&mut self, code: u32, pressed: bool) {
        let up = if pressed { 0 } else { KEYEVENTF_KEYUP };
        let input = match keymap::from_evdev(code) {
            Some(Inject::Scan { scan, extended }) => {
                let ext = if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
                key(0, scan, KEYEVENTF_SCANCODE | ext | up)
            }
            Some(Inject::Vk(vk)) => {
                let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
                key(vk, scan, up)
            }
            None => {
                tracing::debug!("no Windows key for evdev code {code}");
                return;
            }
        };
        self.send(&[input]);
        if pressed {
            self.keys.insert(code);
        } else {
            self.keys.remove(&code);
        }
    }

    fn wheel(&mut self, dx: i32, dy: i32) {
        // Windows: positive is away from the user / to the right; on the wire
        // positive is down / to the right.
        if dy != 0 {
            self.send(&[mouse(0, 0, (-dy) as u32, MOUSEEVENTF_WHEEL)]);
        }
        if dx != 0 {
            self.send(&[mouse(0, 0, dx as u32, MOUSEEVENTF_HWHEEL)]);
        }
    }

    fn apply(&mut self, cmd: EmulationCommand) {
        match cmd {
            EmulationCommand::Enter { pos } | EmulationCommand::MoveAbs { pos } => self.move_to(pos),
            EmulationCommand::MoveRel { dx, dy } => {
                let p = self.current();
                self.move_to(Point::new(p.x + dx, p.y + dy));
            }
            EmulationCommand::Button { button, pressed } => self.button(button, pressed),
            EmulationCommand::Key { code, pressed } => self.key(code, pressed),
            EmulationCommand::Scroll { dx, dy } => {
                self.wheel.0 += dx * WHEEL_PER_PIXEL;
                self.wheel.1 += dy * WHEEL_PER_PIXEL;
                let (wx, wy) = (self.wheel.0.trunc(), self.wheel.1.trunc());
                self.wheel.0 -= wx;
                self.wheel.1 -= wy;
                self.wheel(wx as i32, wy as i32);
            }
            EmulationCommand::ScrollDiscrete { dx, dy } => self.wheel(dx, dy),
            EmulationCommand::Leave { park } => {
                // A button still held may be dragging something: Escape
                // cancels the drag, where letting go would drop it here.
                if !self.buttons.is_empty() {
                    self.key(keys::KEY_ESC, true);
                    self.key(keys::KEY_ESC, false);
                }
                self.release_all();
                if let Some(p) = park {
                    self.move_to(p);
                }
                self.pos = None;
                self.wheel = (0.0, 0.0);
            }
            EmulationCommand::Restart => {}
        }
    }

    /// Let go of every key and button pressed through here.
    fn release_all(&mut self) {
        for code in std::mem::take(&mut self.keys) {
            self.key(code, false);
        }
        for b in std::mem::take(&mut self.buttons) {
            self.button(b, false);
        }
    }
}
