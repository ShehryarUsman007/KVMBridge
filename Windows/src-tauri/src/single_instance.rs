//! Hands the command line to a KVM Bridge that is already running, before
//! anything else starts, so a second launch does not touch the log or the
//! network ports.
//!
//! This sends the same message tauri-plugin-single-instance sends from its
//! own setup (which only runs once the engine is up): a WM_COPYDATA to the
//! plugin's hidden window, holding `cwd|arg0|arg1|...`.

use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, SendMessageW, WM_COPYDATA,
};

/// Must match the identifier in tauri.conf.json.
const IDENTIFIER: &str = "io.kvmbridge.app";
/// The plugin's tag for its WM_COPYDATA messages.
const COPYDATA_TAG: usize = 1542;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns true if a running KVM Bridge took the command line.
pub fn forward_to_running_instance(args: &[String]) -> bool {
    let class = wide(&format!("{IDENTIFIER}-sic"));
    let title = wide(&format!("{IDENTIFIER}-siw"));
    let hwnd = unsafe { FindWindowW(class.as_ptr(), title.as_ptr()) };
    if hwnd.is_null() {
        return false;
    }
    // Only the running instance may bring its window to the front; hand it
    // that right.
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid != 0 {
        unsafe { AllowSetForegroundWindow(pid) };
    }
    let cwd = std::env::current_dir().ok().and_then(|d| d.to_str().map(String::from)).unwrap_or_default();
    let data = format!("{cwd}|{}\0", args.join("|"));
    let cds = COPYDATASTRUCT { dwData: COPYDATA_TAG, cbData: data.len() as u32, lpData: data.as_ptr() as _ };
    unsafe { SendMessageW(hwnd, WM_COPYDATA, 0, &cds as *const _ as isize) };
    true
}
