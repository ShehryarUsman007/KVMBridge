//! Keyboard scan codes (set 1, as Windows reports them) to and from Linux
//! evdev key codes, which is what key events carry on the wire.
//!
//! evdev numbers the main block of a PC keyboard exactly like set 1 scan
//! codes, so most keys map to themselves; the tables list the rest.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

/// Keys sent with an E0 prefix (`LLKHF_EXTENDED`): (evdev, scan code).
const EXTENDED: &[(u32, u16)] = &[
    (96, 0x1C),  // KP Enter
    (97, 0x1D),  // Right Ctrl
    (98, 0x35),  // KP /
    (99, 0x37),  // Print Screen
    (100, 0x38), // Right Alt / AltGr
    (119, 0x46), // Ctrl+Break
    (102, 0x47), // Home
    (103, 0x48), // Up
    (104, 0x49), // Page Up
    (105, 0x4B), // Left
    (106, 0x4D), // Right
    (107, 0x4F), // End
    (108, 0x50), // Down
    (109, 0x51), // Page Down
    (110, 0x52), // Insert
    (111, 0x53), // Delete
    (125, 0x5B), // Left Windows
    (126, 0x5C), // Right Windows
    (127, 0x5D), // Menu
    (142, 0x5F), // Sleep
    (165, 0x10), // Previous track
    (163, 0x19), // Next track
    (113, 0x20), // Mute
    (140, 0x21), // Calculator
    (164, 0x22), // Play/Pause
    (166, 0x24), // Stop
    (114, 0x2E), // Volume down
    (115, 0x30), // Volume up
    (172, 0x32), // Browser home
    (217, 0x65), // Search
    (156, 0x66), // Favourites
    (173, 0x67), // Refresh
    (128, 0x68), // Browser stop
    (159, 0x69), // Forward
    (158, 0x6A), // Back
    (157, 0x6B), // My Computer
    (155, 0x6C), // Mail
    (226, 0x6D), // Media select
];

/// Plain scan codes whose evdev code is not the scan code itself.
const PLAIN: &[(u32, u16)] = &[
    (99, 0x54),  // Alt+Print Screen
    (117, 0x59), // KP =
    (183, 0x64), // F13
    (184, 0x65), // F14
    (185, 0x66), // F15
    (186, 0x67), // F16
    (187, 0x68), // F17
    (188, 0x69), // F18
    (189, 0x6A), // F19
    (190, 0x6B), // F20
    (191, 0x6C), // F21
    (192, 0x6D), // F22
    (193, 0x6E), // F23
    (194, 0x76), // F24
    (93, 0x70),  // Katakana/Hiragana
    (89, 0x73),  // Ro
    (92, 0x79),  // Henkan
    (94, 0x7B),  // Muhenkan
    (124, 0x7D), // Yen
    (121, 0x7E), // KP , (Brazilian)
];

/// Keys that are injected by virtual key code, because their scan codes are
/// ambiguous (Pause and Num Lock share 0x45) or not mapped reliably by every
/// keyboard layout (media keys).
const BY_VK: &[(u32, VIRTUAL_KEY)] = &[
    (119, VK_PAUSE),
    (69, VK_NUMLOCK),
    (113, VK_VOLUME_MUTE),
    (114, VK_VOLUME_DOWN),
    (115, VK_VOLUME_UP),
    (163, VK_MEDIA_NEXT_TRACK),
    (165, VK_MEDIA_PREV_TRACK),
    (166, VK_MEDIA_STOP),
    (164, VK_MEDIA_PLAY_PAUSE),
    (155, VK_LAUNCH_MAIL),
    (226, VK_LAUNCH_MEDIA_SELECT),
    (157, VK_LAUNCH_APP1),
    (140, VK_LAUNCH_APP2),
    (158, VK_BROWSER_BACK),
    (159, VK_BROWSER_FORWARD),
    (173, VK_BROWSER_REFRESH),
    (128, VK_BROWSER_STOP),
    (217, VK_BROWSER_SEARCH),
    (156, VK_BROWSER_FAVORITES),
    (172, VK_BROWSER_HOME),
    (142, VK_SLEEP),
];

/// Keys whose evdev code equals their plain scan code.
fn is_identity(code: u32) -> bool {
    matches!(code, 1..=83 | 86..=88)
}

/// evdev code of a key event from a low-level keyboard hook.
pub fn to_evdev(scan: u32, extended: bool, vk: u32) -> Option<u32> {
    // Pause and Num Lock both report scan code 0x45.
    if vk == VK_PAUSE as u32 {
        return Some(119);
    }
    if vk == VK_NUMLOCK as u32 {
        return Some(69);
    }
    let scan = u16::try_from(scan).ok()?;
    if extended {
        if scan == 0x36 {
            return Some(54); // Right Shift, sometimes flagged extended
        }
        // Anything else (e.g. the fake Shift that Num Lock adds around
        // the arrow keys) has no key of its own.
        return EXTENDED.iter().find(|(_, s)| *s == scan).map(|(c, _)| *c);
    }
    if let Some((c, _)) = PLAIN.iter().find(|(_, s)| *s == scan) {
        return Some(*c);
    }
    is_identity(scan as u32).then_some(scan as u32)
}

/// How to inject an evdev key here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inject {
    Scan { scan: u16, extended: bool },
    Vk(VIRTUAL_KEY),
}

pub fn from_evdev(code: u32) -> Option<Inject> {
    if let Some((_, vk)) = BY_VK.iter().find(|(c, _)| *c == code) {
        return Some(Inject::Vk(*vk));
    }
    if let Some((_, s)) = EXTENDED.iter().find(|(c, _)| *c == code) {
        return Some(Inject::Scan { scan: *s, extended: true });
    }
    if let Some((_, s)) = PLAIN.iter().find(|(c, _)| *c == code) {
        return Some(Inject::Scan { scan: *s, extended: false });
    }
    // Power and wake keys (116, 143) are deliberately not injected.
    is_identity(code).then_some(Inject::Scan { scan: code as u16, extended: false })
}
