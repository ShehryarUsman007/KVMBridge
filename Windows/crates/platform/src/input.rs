//! The input thread: low-level keyboard and mouse hooks.
//!
//! One thread owns both hooks and two hidden windows, and runs their message
//! loop. It does three jobs:
//! * capture: when the cursor pushes against a screen edge that leads to
//!   another computer, this computer's mouse and keyboard are taken (the
//!   hooks swallow them and report them as events) until the engine gives
//!   them back. The cursor waits at the edge for a moment, in case the
//!   engine gives input straight back (nothing reachable there), and is then
//!   parked, hidden, in the middle of that monitor: motion is measured from
//!   there, so it never runs into a screen edge;
//! * pausing: while another computer controls the screens, this computer's
//!   own mouse and keyboard do nothing; injected input (from the
//!   controlling computer) passes;
//! * watching the monitors.
//!
//! Losing input is the worst thing that can happen, so:
//! * Ctrl+Alt+Shift+Esc on this keyboard always frees input here, handled on
//!   this thread without the engine;
//! * a watchdog gives input back if the cursor moves while it is held (the
//!   hook stopped working), when Windows locks or shows a secure screen, when
//!   the monitors change, and when the engine is gone;
//! * the hooks decide quickly and never wait on anything (Windows drops a
//!   hook that keeps it waiting, and input then flows normally); a hook that
//!   cannot get at its state lets the event through;
//! * hooks end with the process, so quitting or a crash frees input;
//! * input is never given back with the cursor on the edge that takes it
//!   (and taking it needs a push outwards, never a move along or away).

use crate::edges;
use crate::emulation::TAG;
use crate::keymap;
use crate::screens::{self, to_point, to_win};
use kvmbridge_core::layout::Desktop;
use kvmbridge_core::platform::{BackendStatus, CaptureEvent, EmulationEvent, GuardStatus};
use kvmbridge_core::types::{buttons, Edge, Point};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop, DESKTOP_READOBJECTS};
use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Input::{GetRawInputDeviceList, RAWINPUTDEVICELIST, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Commands for the input thread.
#[derive(Debug)]
pub enum Cmd {
    Edges(Vec<Edge>),
    Release(Option<Point>),
    Enable(bool),
    Block(bool),
    Restart,
    Stop,
}

const WM_APP_CMD: u32 = WM_APP + 1;
const TIMER_WATCH: usize = 1;
const TIMER_TAKE: usize = 2;
const WATCH_EVERY_MS: u32 = 250;
/// How long the cursor waits at the edge before it is parked: the engine
/// gives input straight back within milliseconds if nothing is reachable.
const TAKE_AFTER_MS: u32 = 120;
/// After input was given straight back at an edge, pushes there are ignored
/// for this long (the screen beyond is off or not connected).
const REFUSED_COOLDOWN: Duration = Duration::from_millis(500);
/// Side of the invisible window the parked cursor rests on.
const BLANK_SIZE: i32 = 64;

/// Input is taken for another screen (read by the drag-and-drop thread).
static TAKEN: AtomicBool = AtomicBool::new(false);

/// This computer's mouse and keyboard are taken for another screen.
pub fn taken() -> bool {
    TAKEN.load(Ordering::Relaxed)
}

/// Handle to the input thread.
pub struct InputThread {
    hwnd: isize,
    tx: mpsc::Sender<Cmd>,
}

impl InputThread {
    pub fn spawn(cap: UnboundedSender<CaptureEvent>, emu: UnboundedSender<EmulationEvent>) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("kvmb-input".into())
            .spawn(move || thread_main(rx, cap, emu, ready_tx))
            .map_err(|e| e.to_string())?;
        let hwnd = ready_rx.recv().map_err(|_| "the input thread did not start".to_string())??;
        Ok(InputThread { hwnd, tx })
    }

    pub fn send(&self, c: Cmd) {
        if self.tx.send(c).is_ok() {
            unsafe { PostMessageW(self.hwnd as HWND, WM_APP_CMD, 0, 0) };
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    /// Input is this computer's.
    Free,
    /// Input was taken at `at` on `edge`. The cursor still sits at `cursor`
    /// until the engine takes it, or gives it straight back.
    Taking { edge: Edge, at: POINT, cursor: POINT },
    /// Input is held for another screen; the cursor is parked at `park`.
    Held { edge: Edge, at: POINT, park: POINT },
}

struct State {
    cap: UnboundedSender<CaptureEvent>,
    emu: UnboundedSender<EmulationEvent>,
    rx: mpsc::Receiver<Cmd>,
    hwnd: HWND,
    /// Invisible window with an empty cursor, under the parked cursor.
    blank: HWND,
    hooks: Option<(HHOOK, HHOOK)>,
    desktop: Desktop,
    edges: Vec<Edge>,
    enabled: bool,
    mode: Mode,
    /// This computer's own keyboard and mouse are paused.
    blocking: bool,
    /// Keys (virtual key codes) and buttons of this computer's own keyboard
    /// and mouse that are down right now.
    down_keys: HashSet<u32>,
    down_buttons: HashSet<u32>,
    /// Down when input was taken or paused: letting go of them goes to this
    /// computer, which saw them go down.
    pass_keys: HashSet<u32>,
    pass_buttons: HashSet<u32>,
    /// Keys reported as pressed (drops auto-repeat).
    sent_keys: HashSet<u32>,
    /// Moves against an edge that left the cursor where it was.
    still_pushes: u32,
    cooldown: Option<(Edge, Instant)>,
    ticks: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` on the thread's state; `None` if it is not there or busy (a
/// hook called while the state is in use lets its event through).
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn cursor_pos() -> POINT {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    p
}

/// `p` moved `by` pixels away from `edge`, into the screen.
fn inset(p: POINT, edge: Edge, by: i32) -> POINT {
    match edge {
        Edge::Left => POINT { x: p.x + by, y: p.y },
        Edge::Right => POINT { x: p.x - by, y: p.y },
        Edge::Top => POINT { x: p.x, y: p.y + by },
        Edge::Bottom => POINT { x: p.x, y: p.y - by },
    }
}

/// Mouse button of a hook message, as an evdev code, and whether it went down.
fn mouse_button(msg: u32, data: u32) -> Option<(u32, bool)> {
    let x = || if (data >> 16) as u16 == XBUTTON2 { buttons::BTN_EXTRA } else { buttons::BTN_SIDE };
    match msg {
        WM_LBUTTONDOWN => Some((buttons::BTN_LEFT, true)),
        WM_LBUTTONUP => Some((buttons::BTN_LEFT, false)),
        WM_RBUTTONDOWN => Some((buttons::BTN_RIGHT, true)),
        WM_RBUTTONUP => Some((buttons::BTN_RIGHT, false)),
        WM_MBUTTONDOWN => Some((buttons::BTN_MIDDLE, true)),
        WM_MBUTTONUP => Some((buttons::BTN_MIDDLE, false)),
        WM_XBUTTONDOWN => Some((x(), true)),
        WM_XBUTTONUP => Some((x(), false)),
        _ => None,
    }
}

/// Number of keyboards and mice attached (for the pause status).
fn device_count() -> u32 {
    let size = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut n = 0u32;
    if unsafe { GetRawInputDeviceList(std::ptr::null_mut(), &mut n, size) } != 0 {
        return 0;
    }
    let mut list: Vec<RAWINPUTDEVICELIST> = vec![unsafe { std::mem::zeroed() }; n as usize];
    let got = unsafe { GetRawInputDeviceList(list.as_mut_ptr(), &mut n, size) };
    if got == u32::MAX {
        return 0;
    }
    list.iter().take(got as usize).filter(|d| d.dwType == RIM_TYPEMOUSE || d.dwType == RIM_TYPEKEYBOARD).count() as u32
}

/// The desktop the user sees can take injected input (not the lock screen
/// or a UAC prompt).
fn input_desktop_reachable() -> bool {
    let h = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if h.is_null() {
        return false;
    }
    unsafe { CloseDesktop(h) };
    true
}

fn install_hooks() -> Result<(HHOOK, HHOOK), String> {
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0) };
    if mouse.is_null() {
        return Err(format!("cannot install the mouse hook (error {})", unsafe { GetLastError() }));
    }
    let keyboard = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), module, 0) };
    if keyboard.is_null() {
        let e = unsafe { GetLastError() };
        unsafe { UnhookWindowsHookEx(mouse) };
        return Err(format!("cannot install the keyboard hook (error {e})"));
    }
    Ok((mouse, keyboard))
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if with_state(|s| s.on_mouse(wparam as u32, info)).unwrap_or(false) {
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn key_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if with_state(|s| s.on_key(info)).unwrap_or(false) {
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_CMD => {
            with_state(State::run_commands);
            0
        }
        WM_TIMER => {
            if wparam == TIMER_TAKE {
                with_state(State::park);
            } else {
                with_state(State::watch);
            }
            0
        }
        WM_DISPLAYCHANGE => {
            with_state(State::refresh_desktop);
            0
        }
        WM_WTSSESSION_CHANGE => {
            if matches!(wparam as u32, WTS_SESSION_LOCK | WTS_SESSION_LOGOFF | WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT) {
                with_state(|s| s.give_back("this computer was locked"));
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Creates the message window and the invisible window for the parked cursor.
fn create_windows() -> Result<(HWND, HWND), String> {
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let main_class = wide("KVMBridgeInput");
    let blank_class = wide("KVMBridgeHiddenCursor");
    // An all-transparent cursor: AND mask all ones, XOR mask all zeros.
    let and = [0xFFu8; 128];
    let xor = [0u8; 128];
    let empty = unsafe { CreateCursor(module, 0, 0, 32, 32, and.as_ptr().cast(), xor.as_ptr().cast()) };
    let mut wc: WNDCLASSEXW = unsafe { std::mem::zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.hInstance = module;
    wc.lpfnWndProc = Some(wndproc);
    wc.lpszClassName = main_class.as_ptr();
    unsafe { RegisterClassExW(&wc) };
    wc.lpfnWndProc = Some(DefWindowProcW);
    wc.lpszClassName = blank_class.as_ptr();
    wc.hCursor = empty;
    unsafe { RegisterClassExW(&wc) };

    let title = wide("KVM Bridge input");
    let null = std::ptr::null_mut();
    let hwnd = unsafe {
        CreateWindowExW(WS_EX_TOOLWINDOW, main_class.as_ptr(), title.as_ptr(), WS_POPUP, 0, 0, 0, 0, null, null, module, std::ptr::null())
    };
    if hwnd.is_null() {
        return Err(format!("cannot create the input window (error {})", unsafe { GetLastError() }));
    }
    let blank = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            blank_class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            BLANK_SIZE,
            BLANK_SIZE,
            null,
            null,
            module,
            std::ptr::null(),
        )
    };
    if blank.is_null() {
        return Err(format!("cannot create the cursor window (error {})", unsafe { GetLastError() }));
    }
    // Practically invisible, but still under the cursor (so its empty
    // cursor is the one shown).
    unsafe { SetLayeredWindowAttributes(blank, 0, 1, LWA_ALPHA) };
    Ok((hwnd, blank))
}

fn thread_main(
    rx: mpsc::Receiver<Cmd>,
    cap: UnboundedSender<CaptureEvent>,
    emu: UnboundedSender<EmulationEvent>,
    ready: mpsc::SyncSender<Result<isize, String>>,
) {
    // The hooks run on this thread: keep them quick to answer.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
    let (hwnd, blank) = match create_windows() {
        Ok(w) => w,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) };
    let hooks = install_hooks();
    let state = State {
        cap,
        emu,
        rx,
        hwnd,
        blank,
        hooks: hooks.as_ref().ok().copied(),
        desktop: screens::desktop(),
        edges: vec![],
        enabled: true,
        mode: Mode::Free,
        blocking: false,
        down_keys: HashSet::new(),
        down_buttons: HashSet::new(),
        pass_keys: HashSet::new(),
        pass_buttons: HashSet::new(),
        sent_keys: HashSet::new(),
        still_pushes: 0,
        cooldown: None,
        ticks: 0,
    };
    STATE.with(|s| *s.borrow_mut() = Some(state));
    with_state(|s| {
        s.send_geometry();
        match &hooks {
            Ok(_) => s.status(BackendStatus::Ready("Windows input hooks".into())),
            Err(e) => {
                tracing::warn!("input capture unavailable: {e}");
                s.status(BackendStatus::Unavailable(format!("Input capture is not available: {e}")));
            }
        }
    });
    unsafe { SetTimer(hwnd, TIMER_WATCH, WATCH_EVERY_MS, None) };
    let _ = ready.send(Ok(hwnd as isize));

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    if let Some(s) = STATE.with(|s| s.borrow_mut().take()) {
        if let Some((m, k)) = s.hooks {
            unsafe {
                UnhookWindowsHookEx(m);
                UnhookWindowsHookEx(k);
            }
        }
        unsafe {
            DestroyWindow(s.blank);
            DestroyWindow(s.hwnd);
        }
    }
    tracing::info!("input thread stopped");
}

impl State {
    fn emit(&self, e: CaptureEvent) {
        let _ = self.cap.send(e);
    }

    fn status(&self, s: BackendStatus) {
        self.emit(CaptureEvent::Status(s));
    }

    fn send_geometry(&self) {
        self.emit(CaptureEvent::Geometry(self.desktop.clone()));
        let _ = self.emu.send(EmulationEvent::Geometry(self.desktop.clone()));
    }

    fn taken(&self) -> bool {
        !matches!(self.mode, Mode::Free)
    }

    // ------------------------------------------------------------ hooks

    fn on_mouse(&mut self, msg: u32, info: &MSLLHOOKSTRUCT) -> bool {
        // Injected input (the controlling computer's, through this
        // computer's emulation, or another program's) passes untouched.
        if info.flags & LLMHF_INJECTED != 0 || info.dwExtraInfo == TAG {
            return false;
        }
        let button = mouse_button(msg, info.mouseData);
        if let Some((b, pressed)) = button {
            if pressed {
                self.down_buttons.insert(b);
            } else {
                self.down_buttons.remove(&b);
            }
        }
        let base = match self.mode {
            Mode::Free => {
                if self.blocking {
                    // Paused: nothing happens, except letting go of a button
                    // that was down when the pause began.
                    return !matches!(button, Some((b, false)) if self.pass_buttons.remove(&b));
                }
                if msg == WM_MOUSEMOVE && self.enabled {
                    return self.check_push(info.pt);
                }
                return false;
            }
            Mode::Taking { cursor, .. } => cursor,
            Mode::Held { park, .. } => park,
        };
        match msg {
            WM_MOUSEMOVE => {
                // The cursor does not move (this move is swallowed), so the
                // hook's position is the base plus this move.
                let (dx, dy) = (info.pt.x - base.x, info.pt.y - base.y);
                if dx != 0 || dy != 0 {
                    self.emit(CaptureEvent::Motion { dx: dx as f64, dy: dy as f64 });
                }
                true
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let delta = (info.mouseData >> 16) as u16 as i16 as i32;
                // Windows: positive is away from the user, or to the right.
                let (dx, dy) = if msg == WM_MOUSEWHEEL { (0, -delta) } else { (delta, 0) };
                self.emit(CaptureEvent::ScrollDiscrete { dx, dy });
                true
            }
            _ => match button {
                Some((b, pressed)) => {
                    self.emit(CaptureEvent::Button { button: b, pressed });
                    // A button that was down when input was taken is let go
                    // here too (the engine may also start a drag with it).
                    !(!pressed && self.pass_buttons.remove(&b))
                }
                None => true,
            },
        }
    }

    fn on_key(&mut self, info: &KBDLLHOOKSTRUCT) -> bool {
        if info.flags & LLKHF_INJECTED != 0 || info.dwExtraInfo == TAG {
            return false;
        }
        let up = info.flags & LLKHF_UP != 0;
        let vk = info.vkCode;
        if up {
            self.down_keys.remove(&vk);
        } else {
            self.down_keys.insert(vk);
        }
        if !up && vk == VK_ESCAPE as u32 && self.escape_combo() && (self.blocking || self.taken()) {
            self.escape();
            return true;
        }
        if !self.taken() {
            // Paused: nothing happens, except letting go of a key that was
            // down when the pause began.
            return self.blocking && !(up && self.pass_keys.remove(&vk));
        }
        if up && self.pass_keys.remove(&vk) {
            return false;
        }
        let extended = info.flags & LLKHF_EXTENDED != 0;
        if let Some(code) = keymap::to_evdev(info.scanCode, extended, vk) {
            // Windows repeats a held key; the other computer repeats it itself.
            let fresh = if up { self.sent_keys.remove(&code) } else { self.sent_keys.insert(code) };
            if fresh {
                self.emit(CaptureEvent::Key { code, pressed: !up });
            }
        }
        true
    }

    fn escape_combo(&self) -> bool {
        let down = |a: VIRTUAL_KEY, b: VIRTUAL_KEY| self.down_keys.contains(&(a as u32)) || self.down_keys.contains(&(b as u32));
        down(VK_LCONTROL, VK_RCONTROL) && down(VK_LMENU, VK_RMENU) && down(VK_LSHIFT, VK_RSHIFT)
    }

    /// Ctrl+Alt+Shift+Esc on this keyboard: input is this computer's again,
    /// whatever the engine is doing.
    fn escape(&mut self) {
        // Keys and buttons still down are let go here.
        self.pass_keys = self.down_keys.clone();
        self.pass_buttons = self.down_buttons.clone();
        if self.blocking {
            self.blocking = false;
            tracing::info!("emergency shortcut: this keyboard and mouse work again");
            self.emit(CaptureEvent::Guard(GuardStatus::Escaped));
        }
        self.give_back("the emergency shortcut was pressed");
    }

    /// The cursor pushes against an edge that leads to another screen: take
    /// input (see [`edges::check`]).
    fn check_push(&mut self, new: POINT) -> bool {
        if self.edges.is_empty() {
            return false;
        }
        // The hook runs before the cursor moves.
        let old = cursor_pos();
        let now = Instant::now();
        let cooldown = self.cooldown;
        let cooling = |e: Edge| cooldown.is_some_and(|(c, until)| c == e && now < until);
        let c = edges::check(&self.desktop, &self.edges, cooling, (old.x, old.y), (new.x, new.y), self.still_pushes);
        self.still_pushes = if c.still { self.still_pushes + 1 } else { 0 };
        match c.take {
            Some((edge, (x, y))) => {
                self.take(edge, POINT { x, y }, old);
                true
            }
            None => false,
        }
    }

    fn take(&mut self, edge: Edge, at: POINT, cursor: POINT) {
        self.mode = Mode::Taking { edge, at, cursor };
        TAKEN.store(true, Ordering::Relaxed);
        self.still_pushes = 0;
        self.pass_keys = self.down_keys.clone();
        self.pass_buttons = self.down_buttons.clone();
        self.sent_keys.clear();
        self.emit(CaptureEvent::Activated { edge, pos: to_point(at) });
        unsafe { SetTimer(self.hwnd, TIMER_TAKE, TAKE_AFTER_MS, None) };
    }

    /// The engine kept input: park the cursor, hidden, in the middle of the
    /// monitor it left from.
    fn park(&mut self) {
        unsafe { KillTimer(self.hwnd, TIMER_TAKE) };
        let Mode::Taking { edge, at, .. } = self.mode else { return };
        let m = screens::monitor_at(&self.desktop, to_point(at));
        let park = POINT { x: (m.x + m.w / 2.0) as i32, y: (m.y + m.h / 2.0) as i32 };
        let half = BLANK_SIZE / 2;
        unsafe {
            SetWindowPos(
                self.blank,
                HWND_TOPMOST,
                park.x - half,
                park.y - half,
                BLANK_SIZE,
                BLANK_SIZE,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            SetCursorPos(park.x, park.y);
        }
        self.mode = Mode::Held { edge, at, park };
    }

    /// Give input back to this computer, with the cursor at `pos` (from the
    /// engine, already inside the screen) or next to where it left.
    fn release(&mut self, pos: Option<Point>) {
        let (edge, at, held) = match self.mode {
            Mode::Free => return,
            Mode::Taking { edge, at, .. } => (edge, at, false),
            Mode::Held { edge, at, .. } => (edge, at, true),
        };
        self.mode = Mode::Free;
        TAKEN.store(false, Ordering::Relaxed);
        self.still_pushes = 0;
        self.sent_keys.clear();
        unsafe {
            KillTimer(self.hwnd, TIMER_TAKE);
            ShowWindow(self.blank, SW_HIDE);
        }
        let to = match pos {
            Some(p) => to_win(p),
            None => {
                if !held {
                    // Given straight back: nothing reachable beyond this edge.
                    self.cooldown = Some((edge, Instant::now() + REFUSED_COOLDOWN));
                }
                // Never back onto the edge itself.
                inset(at, edge, if held { 8 } else { 1 })
            }
        };
        unsafe { SetCursorPos(to.x, to.y) };
    }

    /// Input is given back on this computer's own initiative.
    fn give_back(&mut self, why: &str) {
        if !self.taken() {
            return;
        }
        tracing::info!("giving input back to this computer: {why}");
        self.emit(CaptureEvent::Deactivated);
        self.release(None);
    }

    fn set_blocking(&mut self, on: bool) {
        if on == self.blocking {
            return;
        }
        if !on {
            self.blocking = false;
            self.emit(CaptureEvent::Guard(GuardStatus::Off));
            return;
        }
        if self.hooks.is_none() {
            self.emit(CaptureEvent::Guard(GuardStatus::Unavailable("The input hooks could not be installed".into())));
            return;
        }
        self.give_back("another computer controls this one now");
        self.pass_keys = self.down_keys.clone();
        self.pass_buttons = self.down_buttons.clone();
        self.blocking = true;
        self.emit(CaptureEvent::Guard(GuardStatus::Paused(device_count())));
    }

    fn reinstall_hooks(&mut self) {
        if let Some((m, k)) = self.hooks.take() {
            unsafe {
                UnhookWindowsHookEx(m);
                UnhookWindowsHookEx(k);
            }
        }
        match install_hooks() {
            Ok(h) => {
                self.hooks = Some(h);
                self.status(BackendStatus::Ready("Windows input hooks".into()));
            }
            Err(e) => {
                tracing::warn!("input capture unavailable: {e}");
                self.blocking = false;
                self.status(BackendStatus::Unavailable(format!("Input capture is not available: {e}")));
            }
        }
    }

    // ------------------------------------------------------------ messages

    fn run_commands(&mut self) {
        while let Ok(c) = self.rx.try_recv() {
            match c {
                Cmd::Edges(e) => {
                    self.edges = e;
                    self.still_pushes = 0;
                }
                Cmd::Release(pos) => self.release(pos),
                Cmd::Enable(on) => {
                    self.enabled = on;
                    if !on {
                        // Switched off, or controlled from another computer:
                        // input must be free here, whatever happened before.
                        self.give_back("capture was switched off");
                    }
                }
                Cmd::Block(on) => self.set_blocking(on),
                Cmd::Restart => {
                    self.give_back("restarting");
                    self.reinstall_hooks();
                    self.send_geometry();
                }
                Cmd::Stop => {
                    self.blocking = false;
                    self.give_back("stopping");
                    unsafe { PostQuitMessage(0) };
                }
            }
        }
    }

    /// Runs every [`WATCH_EVERY_MS`].
    fn watch(&mut self) {
        if self.cap.is_closed() {
            // The engine is gone.
            self.blocking = false;
            self.give_back("KVM Bridge is shutting down");
            unsafe { PostQuitMessage(0) };
            return;
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(4) {
            self.refresh_desktop();
        }
        if let Mode::Held { park, .. } = self.mode {
            let p = cursor_pos();
            if (p.x - park.x).abs() > 2 || (p.y - park.y).abs() > 2 {
                tracing::warn!("the cursor moved while input was held here: the input hook stopped working");
                self.give_back("the input hook stopped working");
                self.reinstall_hooks();
                return;
            }
        }
        if self.taken() && !input_desktop_reachable() {
            self.give_back("Windows shows a secure screen (lock screen or UAC prompt)");
        }
    }

    fn refresh_desktop(&mut self) {
        let d = screens::desktop();
        if d == self.desktop {
            return;
        }
        tracing::info!("monitors changed: {:?}", d.monitors);
        self.desktop = d;
        self.give_back("the monitors changed");
        self.send_geometry();
    }
}
