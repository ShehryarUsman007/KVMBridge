//! Drag and drop between screens through OLE drag and drop.
//!
//! A thread of its own (an OLE apartment, with a hidden window for the
//! engine's commands) does all of it:
//!
//! * **Receiving** (`StartDrag`): files dragged on another computer and let
//!   go over this screen are dropped here by a drag KVM Bridge runs itself,
//!   under the cursor. A drag only gets the mouse (DoDragDrop sees no
//!   pointer movement otherwise) when it starts from a press in one of the
//!   program's own windows, so an invisible window is put under the cursor
//!   and a left press is injected into it, as the Linux version does; the
//!   drag starts there, and the window then lets the pointer through to the
//!   applications below. The drag itself decides when it ends, whatever the
//!   button does: once the application under the cursor takes it (after
//!   `drop_after_ms`), it is dropped there. If nothing takes it for a moment,
//!   it is cancelled, as any refused drop would be; small nudges of the
//!   pointer meanwhile make Windows offer it again. The injected press is
//!   always let go afterwards, onto the invisible window, so nothing gets
//!   clicked. If something else is under the pointer (a menu, say), nothing
//!   is pressed and nothing is dropped.
//!   Only copying is offered, so the application copies the files; their
//!   paths (CF_HDROP) are handed over only when it asks for them, once the
//!   files have arrived. Explorer (and browsers) ask as soon as the drag
//!   comes over them, so they wait for the files before the drop.
//! * **Sending** (`Probe`, on a computer controlled from another one): when
//!   the cursor leaves this screen with the button held, an invisible window
//!   that takes drags is put over all screens, and the engine moves the
//!   pointer onto it. A file drag in progress (Explorer's, say) comes over it
//!   and names its files; the engine then cancels that drag with Escape. The
//!   window never takes a drop, and it stays until a drag over it has ended,
//!   so the button let go after Escape cannot drop the files anywhere else.
//!
//! * **Sending while this computer controls the others**: the input thread
//!   says when input is taken with this computer's own left button held
//!   ([`left_held_across`]). The same window then goes over all screens as a
//!   *guard*, under the parked cursor, and a file drag in progress comes over
//!   it and names its files. When the button is let go on the other screen,
//!   the input thread lets it go here only once the drag is over the guard
//!   (or a moment later), so the drag ends on the guard, which takes nothing:
//!   the originals stay where they are. The engine's probe that follows gets
//!   the files right away (there is no need to let go of input to read the
//!   drag, as on Linux).
//! * **A drag carried back here** (`StartDrag` with `after_release`): the
//!   cursor came back with the left button pressed on another screen still
//!   held. Windows never saw that press; when it is let go here
//!   ([`carry_let_go`]) and the files are named, they are dropped under the
//!   cursor like any received drag.
//!
//! While a drag runs, this thread only handles Windows' drag loop, so the
//! engine's commands also leave a note in [`Signals`] that the drag reads.

use crate::clipboard::{global_bytes, hdrop_paths};
use crate::{clipfmt, input};
use kvmbridge_core::platform::{BackendSide, BackendStatus, DndCommand, DndEvent, ProbeOutcome};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot::{self, error::TryRecvError};
use windows::core::{implement, Error, Ref, Result as OleResult, BOOL, HRESULT};
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, DV_E_TYMED, E_FAIL, E_INVALIDARG,
    E_NOTIMPL, E_OUTOFMEMORY, HGLOBAL, HWND as OleHwnd, OLE_E_ADVISENOTSUPPORTED, POINTL, S_OK,
};
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, DATADIR_GET, DVASPECT_CONTENT,
    FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, OleInitialize, OleUninitialize,
    RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

type Events = UnboundedSender<DndEvent>;

pub enum Cmd {
    Engine(DndCommand),
    Stop,
}

const WM_APP_CMD: u32 = WM_APP + 1;
/// The input thread has news about the left button ([`LEFT`], [`CARRY`]).
const WM_APP_WAKE: u32 = WM_APP + 2;
const TIMER_TICK: usize = 1;
/// How often a drag or a probe in progress is looked after.
const TICK_MS: u32 = 100;
/// How long the injected press may take to reach the drag window.
const PRESS_WAIT: Duration = Duration::from_millis(2500);
/// How long the application under the cursor gets to take a drop before
/// the drag is cancelled.
const ACCEPT_WAIT: Duration = Duration::from_millis(2000);
/// How long the drag window stays after the injected press was let go (the
/// release lands on it, not on an application).
const GRAB_LINGER: Duration = Duration::from_millis(300);
/// How long a probe waits for a drag to come over its window.
const PROBE_WAIT: Duration = Duration::from_millis(600);
/// How long a drag still over the probe window gets to end once the probe
/// is done (it is being cancelled with Escape).
const PROBE_LINGER: Duration = Duration::from_millis(1500);
/// The probe window never stays up longer than this.
const PROBE_MAX: Duration = Duration::from_secs(6);
/// Longest wait for dropped files to arrive (the engine answers first when
/// the transfer fails or is cancelled).
const FILES_WAIT: Duration = Duration::from_secs(3600);
/// Longest wait for a drag carried back here to be let go and named.
const CARRY_MAX: Duration = Duration::from_secs(120);

// ------------------------------------------------- news from the input thread

/// This computer's own left button while it controls the others ([`LEFT`]).
const LEFT_FREE: u8 = 0;
/// Down when input was taken, and still held.
const LEFT_ACROSS: u8 = 1;
/// Let go on the other screen (and here).
const LEFT_LET_GO: u8 = 2;
static LEFT: AtomicU8 = AtomicU8::new(LEFT_FREE);

/// A left button pressed on another screen and held on the way back ([`CARRY`]).
const CARRY_NONE: u8 = 0;
const CARRY_HELD: u8 = 1;
const CARRY_LET_GO: u8 = 2;
static CARRY: AtomicU8 = AtomicU8::new(CARRY_NONE);

/// A drag is over the guard window right now (read by the input thread).
static GUARD_OVER: AtomicBool = AtomicBool::new(false);
/// This thread's command window (0 until it runs).
static WINDOW: AtomicIsize = AtomicIsize::new(0);

fn wake() {
    let w = WINDOW.load(Ordering::Relaxed);
    if w != 0 {
        unsafe { PostMessageW(w as HWND, WM_APP_WAKE, 0, 0) };
    }
}

/// Input was taken with this computer's own left button held: a drag of it
/// may be leaving this screen.
pub fn left_held_across() {
    LEFT.store(LEFT_ACROSS, Ordering::Relaxed);
    GUARD_OVER.store(false, Ordering::Relaxed);
    wake();
}

/// That button was let go (on the other screen, and now here).
pub fn left_let_go() {
    LEFT.store(LEFT_LET_GO, Ordering::Relaxed);
    wake();
}

/// Input came back with that button still held: a drag of it carries on here.
pub fn left_back_home() {
    let _ = LEFT.compare_exchange(LEFT_ACROSS, LEFT_FREE, Ordering::Relaxed, Ordering::Relaxed);
    wake();
}

/// A drag is over the guard window, so letting go of the button ends it there.
pub fn guard_over() -> bool {
    GUARD_OVER.load(Ordering::Relaxed)
}

/// Input came back with a left button pressed on another screen still held.
pub fn carry_held() {
    CARRY.store(CARRY_HELD, Ordering::Relaxed);
}

/// That button was let go here.
pub fn carry_let_go() {
    CARRY.store(CARRY_LET_GO, Ordering::Relaxed);
    wake();
}

/// Starts the backend: its thread, fed with the engine's commands. Needs a
/// tokio runtime.
pub fn start(side: BackendSide<DndCommand, DndEvent>) {
    match DndThread::spawn(side.events.clone()) {
        Ok(thread) => {
            tokio::spawn(forward(side, thread));
        }
        Err(e) => {
            tracing::warn!("drag and drop unavailable: {e}");
            side.unavailable(format!("Drag and drop is not available: {e}"), DndEvent::Status);
        }
    }
}

async fn forward(mut side: BackendSide<DndCommand, DndEvent>, thread: DndThread) {
    while let Some(c) = side.commands.recv().await {
        thread.send(Cmd::Engine(c));
    }
    thread.send(Cmd::Stop);
}

/// Notes the engine's commands leave for a drag in progress.
#[derive(Default)]
struct Signals {
    /// The drag the engine called off.
    cancelled: AtomicU64,
    /// The latest drag the engine started (an older one still going on gives
    /// way to it).
    latest: AtomicU64,
}

struct DndThread {
    hwnd: isize,
    tx: mpsc::Sender<Cmd>,
    signals: Arc<Signals>,
}

impl DndThread {
    fn spawn(events: Events) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let signals = Arc::new(Signals::default());
        let s = signals.clone();
        std::thread::Builder::new()
            .name("kvmb-dnd".into())
            .spawn(move || thread_main(rx, events, s, ready_tx))
            .map_err(|e| e.to_string())?;
        let hwnd = ready_rx.recv().map_err(|_| "the drag-and-drop thread did not start".to_string())??;
        Ok(DndThread { hwnd, tx, signals })
    }

    fn send(&self, c: Cmd) {
        match &c {
            Cmd::Engine(DndCommand::CancelDrag { drag }) => self.signals.cancelled.store(*drag, Ordering::Relaxed),
            Cmd::Engine(DndCommand::StartDrag { drag, after_release: false, .. }) => {
                self.signals.latest.store(*drag, Ordering::Relaxed)
            }
            _ => {}
        }
        if self.tx.send(c).is_ok() {
            unsafe { PostMessageW(self.hwnd as HWND, WM_APP_CMD, 0, 0) };
        }
    }
}

/// Clipboard formats a drag carries besides CF_HDROP.
#[derive(Clone, Copy)]
struct Formats {
    /// "Preferred DropEffect": copy.
    drop_effect: u16,
    /// Marks KVM Bridge's own drags (a probe ignores them).
    own: u16,
}

fn register(name: &str) -> u16 {
    let w = wide(name);
    unsafe { RegisterClipboardFormatW(w.as_ptr()) as u16 }
}

fn formatetc(cf: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// The application would copy a drop with this effect.
fn copies(e: DROPEFFECT) -> bool {
    e.0 & DROPEFFECT_COPY.0 != 0
}

fn cursor_pos() -> POINT {
    let mut p = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut p) };
    p
}

// ------------------------------------------------------------------ thread

struct State {
    events: Events,
    signals: Arc<Signals>,
    rx: mpsc::Receiver<Cmd>,
    /// Hidden window for the engine's commands and the timer.
    hwnd: HWND,
    /// The invisible window a drag starts from (hidden when unused).
    grab: HWND,
    /// When to hide it again.
    grab_hide_at: Option<Instant>,
    /// The window that had the focus before the press activated the drag
    /// window: it gets it back.
    focus_before: HWND,
    /// The probe window (registered as a drop target, hidden when unused).
    window: HWND,
    formats: Formats,
    probe: Rc<RefCell<Option<ProbeRun>>>,
    /// Kept alive while it is registered.
    _target: IDropTarget,
    /// The latest native drag (it may still be handing over its files).
    drag: Option<Rc<Drag>>,
    /// Inside DoDragDrop.
    in_drag: bool,
    /// Waiting for dropped files to arrive (an application asked for them).
    waiting: u32,
    /// Drags that start once the one going on is over.
    deferred: VecDeque<DndCommand>,
    /// A drag carried back here, waiting to be let go and named.
    carry: Option<Carry>,
    ticking: bool,
}

impl State {
    /// A new drag may start: none is starting or running (DoDragDrop must not
    /// run inside another one), and no application waits for files.
    fn idle(&self) -> bool {
        !self.in_drag && self.waiting == 0 && !self.drag.as_ref().is_some_and(|d| d.awaiting_press())
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` on the thread's state. Never call into OLE or pump messages
/// inside `f`: code that runs meanwhile needs the state too.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_CMD => {
            run_commands();
            0
        }
        WM_APP_WAKE => {
            look_after_guard();
            look_after_carry();
            0
        }
        WM_TIMER if wparam == TIMER_TICK => {
            tick();
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn probe_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_SETCURSOR && input::taken() {
        // The parked cursor stays hidden while the guard is over it.
        unsafe { SetCursor(std::ptr::null_mut()) };
        return 1;
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn grab_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            tracing::debug!("the drag window was pressed");
            run_drag();
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A topmost, practically invisible window (still there for the pointer).
fn overlay(class: &[u16], module: *mut core::ffi::c_void, activate: bool) -> HWND {
    let title = wide("KVM Bridge drag and drop");
    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | if activate { 0 } else { WS_EX_NOACTIVATE };
    let null = std::ptr::null_mut();
    let w = unsafe { CreateWindowExW(ex, class.as_ptr(), title.as_ptr(), WS_POPUP, 0, 0, 1, 1, null, null, module, std::ptr::null()) };
    if !w.is_null() {
        unsafe { SetLayeredWindowAttributes(w, 0, 1, LWA_ALPHA) };
    }
    w
}

/// The hidden command window, the drag window and the probe window.
fn create_windows() -> Result<(HWND, HWND, HWND), String> {
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let cmd_class = wide("KVMBridgeDragAndDrop");
    let grab_class = wide("KVMBridgeDragStart");
    let probe_class = wide("KVMBridgeDropProbe");
    let mut wc: WNDCLASSEXW = unsafe { std::mem::zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.hInstance = module;
    wc.lpfnWndProc = Some(wndproc);
    wc.lpszClassName = cmd_class.as_ptr();
    unsafe { RegisterClassExW(&wc) };
    wc.hCursor = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) };
    wc.lpfnWndProc = Some(grab_proc);
    wc.lpszClassName = grab_class.as_ptr();
    unsafe { RegisterClassExW(&wc) };
    wc.lpfnWndProc = Some(probe_proc);
    wc.lpszClassName = probe_class.as_ptr();
    unsafe { RegisterClassExW(&wc) };

    let null = std::ptr::null_mut();
    let title = wide("KVM Bridge drag and drop");
    let hwnd = unsafe {
        CreateWindowExW(0, cmd_class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, null, module, std::ptr::null())
    };
    if hwnd.is_null() {
        return Err(format!("cannot create the drag-and-drop window (error {})", unsafe { GetLastError() }));
    }
    // A click activates the drag window (the drag needs that); the probe
    // window never takes the focus.
    let grab = overlay(&grab_class, module, true);
    let window = overlay(&probe_class, module, false);
    if grab.is_null() || window.is_null() {
        let e = unsafe { GetLastError() };
        unsafe {
            DestroyWindow(grab);
            DestroyWindow(window);
            DestroyWindow(hwnd);
        }
        return Err(format!("cannot create the drag windows (error {e})"));
    }
    Ok((hwnd, grab, window))
}

fn thread_main(rx: mpsc::Receiver<Cmd>, events: Events, signals: Arc<Signals>, ready: mpsc::SyncSender<Result<isize, String>>) {
    if let Err(e) = unsafe { OleInitialize(None) } {
        let _ = ready.send(Err(format!("OLE could not start ({e})")));
        return;
    }
    let (hwnd, grab, window) = match create_windows() {
        Ok(w) => w,
        Err(e) => {
            unsafe { OleUninitialize() };
            let _ = ready.send(Err(e));
            return;
        }
    };
    let formats = Formats { drop_effect: register("Preferred DropEffect"), own: register("KVM Bridge drag") };
    let probe = Rc::new(RefCell::new(None));
    let target: IDropTarget = ProbeTarget { run: probe.clone(), events: events.clone(), own: formats.own }.into();
    if let Err(e) = unsafe { RegisterDragDrop(OleHwnd(window), &target) } {
        unsafe {
            DestroyWindow(grab);
            DestroyWindow(window);
            DestroyWindow(hwnd);
            OleUninitialize();
        }
        let _ = ready.send(Err(format!("cannot take drags ({e})")));
        return;
    }
    let _ = events.send(DndEvent::Status(BackendStatus::Ready("OLE drag and drop".into())));
    let state = State {
        events,
        signals,
        rx,
        hwnd,
        grab,
        grab_hide_at: None,
        focus_before: std::ptr::null_mut(),
        window,
        formats,
        probe,
        _target: target,
        drag: None,
        in_drag: false,
        waiting: 0,
        deferred: VecDeque::new(),
        carry: None,
        ticking: false,
    };
    STATE.with(|s| *s.borrow_mut() = Some(state));
    WINDOW.store(hwnd as isize, Ordering::Relaxed);
    let _ = ready.send(Ok(hwnd as isize));
    // Input may have been taken with the button held before this ran.
    look_after_guard();

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    let _ = WINDOW.compare_exchange(hwnd as isize, 0, Ordering::Relaxed, Ordering::Relaxed);
    if let Some(s) = STATE.with(|s| s.borrow_mut().take()) {
        if let Some(d) = &s.drag {
            d.stop();
            d.let_go();
        }
        unsafe {
            let _ = RevokeDragDrop(OleHwnd(s.window));
            DestroyWindow(s.window);
            DestroyWindow(s.grab);
            DestroyWindow(s.hwnd);
        }
        drop(s);
    }
    unsafe { OleUninitialize() };
    tracing::info!("drag-and-drop thread stopped");
}

fn run_commands() {
    loop {
        let next = with_state(|s| {
            if s.idle() {
                if let Some(c) = s.deferred.pop_front() {
                    return Some(Cmd::Engine(c));
                }
            }
            s.rx.try_recv().ok()
        })
        .flatten();
        match next {
            None => return,
            Some(Cmd::Engine(c)) => on_command(c),
            Some(Cmd::Stop) => {
                if let Some(d) = with_state(|s| s.drag.clone()).flatten() {
                    d.stop();
                    d.let_go();
                }
                hide_probe();
                unsafe { PostQuitMessage(0) };
                return;
            }
        }
    }
}

fn on_command(cmd: DndCommand) {
    match cmd {
        DndCommand::Probe { probe, .. } => start_probe(probe),
        DndCommand::ProbeDone { probe, outcome } => probe_done(probe, outcome),
        DndCommand::StartDrag { drag, drop_after_ms, after_release: true, .. } => {
            // The engine gave up on any earlier one already.
            let released = CARRY.compare_exchange(CARRY_LET_GO, CARRY_NONE, Ordering::Relaxed, Ordering::Relaxed).is_ok();
            tracing::info!("the cursor came back with the button held; files dragged over there are dropped where it is let go");
            with_state(|s| {
                s.carry = Some(Carry { drag, names: None, drop_after_ms, released, since: Instant::now() });
            });
            ensure_tick();
            look_after_carry();
        }
        DndCommand::StartDrag { .. } => start_drag(cmd),
        DndCommand::DragFiles { drag, names } => {
            let known = with_state(|s| match s.carry.as_mut().filter(|c| c.drag == drag) {
                Some(c) => {
                    c.names = Some(names);
                    true
                }
                None => false,
            });
            if known == Some(true) {
                look_after_carry();
            }
        }
        DndCommand::CancelDrag { drag } => {
            let carried = with_state(|s| s.carry.take_if(|c| c.drag == drag).is_some());
            if carried == Some(true) {
                tracing::info!("nothing is dropped here: the drag over there carried no files, or it was called off");
            }
            let Some(d) = with_state(|s| {
                s.deferred.retain(|c| !matches!(c, DndCommand::StartDrag { drag: d, .. } if *d == drag));
                s.drag.clone().filter(|d| d.id == drag)
            })
            .flatten() else {
                return;
            };
            tracing::info!("the drag here was called off");
            let not_begun = d.awaiting_press();
            d.stop();
            if not_begun {
                d.let_go();
                hide_grab_soon();
            }
        }
    }
}

/// Looks after a drag or probe in progress, every [`TICK_MS`] (not while
/// Windows runs a drag here).
fn tick() {
    let Some((drag, grab_hide_at, probe, events)) =
        with_state(|s| (s.drag.clone(), s.grab_hide_at, s.probe.clone(), s.events.clone()))
    else {
        return;
    };
    if let Some(d) = drag.as_ref().filter(|d| d.awaiting_press() && d.created.elapsed() >= PRESS_WAIT) {
        tracing::warn!("the injected press did not reach the drag window; nothing is dropped");
        d.let_go();
        d.report(false);
        hide_grab_soon();
    }
    if grab_hide_at.is_some_and(|t| Instant::now() >= t) {
        with_state(|s| {
            s.grab_hide_at = None;
            unsafe {
                if GetForegroundWindow() == s.grab && IsWindow(s.focus_before) != 0 {
                    SetForegroundWindow(s.focus_before);
                }
                ShowWindow(s.grab, SW_HIDE);
            }
            // Drags that waited for this one.
            unsafe { PostMessageW(s.hwnd, WM_APP_CMD, 0, 0) };
        });
    }
    let mut empty = None;
    let mut hide = false;
    if let Some(r) = probe.borrow_mut().as_mut().filter(|r| !r.guard) {
        if !r.reported && r.asked.elapsed() >= PROBE_WAIT {
            r.reported = true;
            empty = r.id;
        }
        hide = r.done_at.is_some_and(|t| !r.over || t.elapsed() >= PROBE_LINGER) || r.asked.elapsed() >= PROBE_MAX;
    }
    if let Some(id) = empty {
        tracing::info!("drag probe: no drag came over the probe window");
        let _ = events.send(DndEvent::Probed { probe: id, paths: vec![], dragging: false });
    }
    if hide {
        hide_probe();
    }
    look_after_guard();
    look_after_carry();
    let busy = drag.is_some_and(|d| d.awaiting_press());
    with_state(|s| {
        if !busy && s.grab_hide_at.is_none() && s.probe.borrow().is_none() && s.carry.is_none() {
            s.ticking = false;
            unsafe { KillTimer(s.hwnd, TIMER_TICK) };
        }
    });
}

fn ensure_tick() {
    with_state(|s| {
        if !std::mem::replace(&mut s.ticking, true) {
            unsafe { SetTimer(s.hwnd, TIMER_TICK, TICK_MS, None) };
        }
    });
}

/// Hides the drag window once a release let go just now has landed on it.
fn hide_grab_soon() {
    with_state(|s| s.grab_hide_at = Some(Instant::now() + GRAB_LINGER));
    ensure_tick();
}

/// Handles this thread's own messages (the engine's commands, the timer)
/// for a moment, while an application waits for something. Messages for
/// other windows (OLE's, COM's) wait, so nothing else runs in between.
fn pump_commands() {
    std::thread::sleep(Duration::from_millis(20));
    let Some(hwnd) = with_state(|s| s.hwnd) else { return };
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE) } != 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ------------------------------------------------------------------ receiving

/// A native drag of files from another computer, run here.
struct Drag {
    id: u64,
    events: Events,
    signals: Arc<Signals>,
    created: Instant,
    drop_after: Duration,
    /// An injected left press is held (it is always let go again).
    pressed: Cell<bool>,
    /// When DoDragDrop began.
    began: Cell<Option<Instant>>,
    /// What the application under the cursor would do with a drop.
    effect: Cell<DROPEFFECT>,
    /// The drag was told to drop or to cancel (this also stops the nudges).
    decided: Arc<AtomicBool>,
    /// No drop: Escape, nothing took it, or it was called off.
    cancel: Cell<bool>,
    /// Called off by the engine (or replaced by a newer drag): how it ends is
    /// not reported.
    stopped: Cell<bool>,
    /// The files were asked for (only once); the answer on its way, and the
    /// answer.
    asked: Cell<bool>,
    reply: RefCell<Option<oneshot::Receiver<Option<Vec<PathBuf>>>>>,
    paths: RefCell<Option<Option<Vec<PathBuf>>>>,
    reported: Cell<bool>,
}

impl Drag {
    /// The press that starts it is on its way.
    fn awaiting_press(&self) -> bool {
        self.pressed.get() && self.began.get().is_none() && !self.stopped.get()
    }

    /// The engine called it off, or started a newer one.
    fn called_off(&self) -> bool {
        self.stopped.get()
            || self.signals.cancelled.load(Ordering::Relaxed) == self.id
            || self.signals.latest.load(Ordering::Relaxed) != self.id
    }

    /// End it without reporting: no drop, and an application waiting for the
    /// files gets none.
    fn stop(&self) {
        self.stopped.set(true);
        self.cancel.set(true);
        let mut p = self.paths.borrow_mut();
        if p.is_none() {
            *p = Some(None);
        }
    }

    /// Let go of the injected press (once).
    fn let_go(&self) {
        if self.pressed.replace(false) {
            let _ = self.events.send(DndEvent::InjectButton { pressed: false });
        }
    }

    fn report(&self, accepted: bool) {
        if self.stopped.get() || self.reported.replace(true) {
            return;
        }
        let _ = self.events.send(DndEvent::DragEnded { drag: self.id, accepted });
    }
}

/// The work area (without the taskbar) of the monitor at `p`.
fn work_area(p: POINT) -> (i32, i32, i32, i32) {
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let m = unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST) };
    unsafe { GetMonitorInfoW(m, &mut info) };
    let r = info.rcWork;
    (r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// The drag window lets the pointer through (while dragging) or not.
fn see_through(grab: HWND, on: bool) {
    let ex = unsafe { GetWindowLongPtrW(grab, GWL_EXSTYLE) };
    let t = WS_EX_TRANSPARENT as isize;
    unsafe { SetWindowLongPtrW(grab, GWL_EXSTYLE, if on { ex | t } else { ex & !t }) };
}

/// A new drag: the drag window goes under the cursor and a press is injected
/// into it ([`run_drag`] goes on from there). One drag at a time: one going
/// on gives way to this one (it sees it in [`Signals`]), and this one waits
/// for it to be over.
fn start_drag(cmd: DndCommand) {
    let DndCommand::StartDrag { drag, ref names, drop_after_ms, .. } = cmd else { return };
    let Some((idle, latest, events, signals, grab)) =
        with_state(|s| (s.idle(), s.signals.latest.load(Ordering::Relaxed), s.events.clone(), s.signals.clone(), s.grab))
    else {
        return;
    };
    if latest != drag {
        // A newer drag came while this one waited.
        return;
    }
    if !idle {
        with_state(|s| s.deferred.push_back(cmd.clone()));
        return;
    }
    let d = Rc::new(Drag {
        id: drag,
        events: events.clone(),
        signals,
        created: Instant::now(),
        drop_after: Duration::from_millis(drop_after_ms),
        pressed: Cell::new(false),
        began: Cell::new(None),
        effect: Cell::new(DROPEFFECT_NONE),
        decided: Arc::new(AtomicBool::new(false)),
        cancel: Cell::new(false),
        stopped: Cell::new(false),
        asked: Cell::new(false),
        reply: RefCell::new(None),
        paths: RefCell::new(None),
        reported: Cell::new(false),
    });
    with_state(|s| {
        s.drag = Some(d.clone());
        s.grab_hide_at = None;
        s.focus_before = unsafe { GetForegroundWindow() };
    });
    let p = cursor_pos();
    let (x, y, w, h) = work_area(p);
    see_through(grab, false);
    unsafe { SetWindowPos(grab, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW) };
    if unsafe { WindowFromPoint(p) } != grab {
        // The press would click whatever is there.
        tracing::info!("something else is under the pointer (the taskbar or a menu, say); nothing is dropped");
        unsafe { ShowWindow(grab, SW_HIDE) };
        d.report(false);
        return;
    }
    tracing::info!("dragging {} item(s) here; pressing into the drag window", names.len());
    d.pressed.set(true);
    let _ = events.send(DndEvent::InjectButton { pressed: true });
    ensure_tick();
}

/// The injected press reached the drag window: run the drag from here until
/// it is dropped or cancelled, then let go of the press.
fn run_drag() {
    let Some((d, formats, grab)) = with_state(|s| (s.drag.clone(), s.formats, s.grab)) else { return };
    let Some(d) = d.filter(|d| d.pressed.get() && d.began.get().is_none()) else {
        // Not our press (or one already let go).
        return;
    };
    d.began.set(Some(Instant::now()));
    tracing::info!("started a native drag");
    with_state(|s| s.in_drag = true);
    // The drag sees the applications below the window from now on.
    see_through(grab, true);
    nudge_while_waiting(&d);
    let data: IDataObject = DataObject { drag: d.clone(), formats }.into();
    let source: IDropSource = DropSource { drag: d.clone() }.into();
    let mut effect = DROPEFFECT_NONE;
    let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    tracing::debug!("the native drag is over ({hr:?}, effect {})", effect.0);
    d.decided.store(true, Ordering::Relaxed);
    with_state(|s| s.in_drag = false);
    // Let go onto the drag window: nothing below gets clicked.
    see_through(grab, false);
    d.let_go();
    hide_grab_soon();
    if hr != DRAGDROP_S_DROP && hr != DRAGDROP_S_CANCEL {
        tracing::warn!("the drag could not run ({})", Error::from(hr));
    }
    let dropped = hr == DRAGDROP_S_DROP && copies(effect);
    if hr == DRAGDROP_S_DROP && !dropped {
        tracing::info!("the application did not take the files");
    }
    d.report(dropped);
}

/// While the drag waits to be dropped (or called off), move the pointer a
/// little every [`TICK_MS`] from another thread: Windows asks the drag
/// whether to go on, and offers it to the application under the pointer,
/// only when the pointer moves.
fn nudge_while_waiting(d: &Drag) {
    let (events, signals, decided, id) = (d.events.clone(), d.signals.clone(), d.decided.clone(), d.id);
    let from = Instant::now() + d.drop_after;
    let _ = std::thread::Builder::new().name("kvmb-dnd-nudge".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_millis(TICK_MS as u64));
        if decided.load(Ordering::Relaxed) {
            return;
        }
        let called_off =
            signals.cancelled.load(Ordering::Relaxed) == id || signals.latest.load(Ordering::Relaxed) != id;
        if (Instant::now() >= from || called_off)
            && (events.send(DndEvent::InjectMove { dx: 1.0, dy: 0.0 }).is_err()
                || events.send(DndEvent::InjectMove { dx: -1.0, dy: 0.0 }).is_err())
        {
            return;
        }
    });
}

/// An application wants the dropped files: wait until they have arrived.
/// They are asked for once; another request meanwhile waits for the same
/// answer.
fn wait_for_paths(d: &Drag) -> Option<Vec<PathBuf>> {
    if !d.asked.replace(true) && d.paths.borrow().is_none() {
        let (tx, rx) = oneshot::channel();
        if d.events.send(DndEvent::DropRequested { drag: d.id, reply: tx }).is_ok() {
            tracing::info!("the application asks for the dropped files; they are handed over once they have arrived");
            *d.reply.borrow_mut() = Some(rx);
        } else {
            *d.paths.borrow_mut() = Some(None);
        }
    }
    with_state(|s| s.waiting += 1);
    let deadline = Instant::now() + FILES_WAIT;
    let paths = loop {
        if let Some(p) = d.paths.borrow().clone() {
            break p;
        }
        let got = d.reply.borrow_mut().as_mut().map(|rx| rx.try_recv());
        match got {
            Some(Err(TryRecvError::Empty)) if Instant::now() < deadline => pump_commands(),
            Some(Ok(p)) => {
                tracing::info!("handing over {} dropped item(s)", p.as_ref().map_or(0, Vec::len));
                *d.paths.borrow_mut() = Some(p);
            }
            _ => {
                tracing::info!("the dropped files did not arrive");
                *d.paths.borrow_mut() = Some(None);
            }
        }
    };
    *d.reply.borrow_mut() = None;
    with_state(|s| {
        s.waiting -= 1;
        // Drags that waited for this.
        unsafe { PostMessageW(s.hwnd, WM_APP_CMD, 0, 0) };
    });
    paths
}

#[implement(IDropSource)]
struct DropSource {
    drag: Rc<Drag>,
}

impl IDropSource_Impl for DropSource_Impl {
    /// Called by Windows as the drag goes on: drop as soon as the
    /// application under the cursor takes it (not before `drop_after`),
    /// cancel if nothing does. The mouse buttons play no part.
    fn QueryContinueDrag(&self, escape: BOOL, _keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        let d = &self.drag;
        let since = d.began.get().map_or(Duration::ZERO, |t| t.elapsed());
        let answer = if d.cancel.get() {
            DRAGDROP_S_CANCEL
        } else if d.called_off() {
            tracing::info!("the drag here was called off");
            d.stop();
            DRAGDROP_S_CANCEL
        } else if escape.as_bool() {
            tracing::info!("Escape cancels the drag");
            DRAGDROP_S_CANCEL
        } else if since < d.drop_after {
            S_OK
        } else if copies(d.effect.get()) {
            tracing::info!("an application takes the drag; dropping it there");
            DRAGDROP_S_DROP
        } else if since >= d.drop_after + ACCEPT_WAIT {
            tracing::info!("nothing under the cursor takes the drag; nothing is dropped");
            DRAGDROP_S_CANCEL
        } else {
            S_OK
        };
        if answer != S_OK {
            d.decided.store(true, Ordering::Relaxed);
        }
        answer
    }

    fn GiveFeedback(&self, effect: DROPEFFECT) -> HRESULT {
        self.drag.effect.set(effect);
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// What a drag from another computer carries: the files (CF_HDROP, once
/// they have arrived), "copy" as the preferred effect, and KVM Bridge's mark.
#[implement(IDataObject)]
struct DataObject {
    drag: Rc<Drag>,
    formats: Formats,
}

impl DataObject {
    fn offered(&self) -> [u16; 3] {
        [CF_HDROP.0, self.formats.drop_effect, self.formats.own]
    }

    /// The format asked for, if it is offered as global memory.
    fn check(&self, f: *const FORMATETC) -> OleResult<u16> {
        let f = unsafe { f.as_ref() }.ok_or_else(|| Error::from(E_INVALIDARG))?;
        if !self.offered().contains(&f.cfFormat) {
            return Err(DV_E_FORMATETC.into());
        }
        if f.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            return Err(DV_E_TYMED.into());
        }
        Ok(f.cfFormat)
    }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, f: *const FORMATETC) -> OleResult<STGMEDIUM> {
        let cf = self.check(f)?;
        let bytes = if cf == CF_HDROP.0 {
            let paths = wait_for_paths(&self.drag).filter(|p| !p.is_empty()).ok_or_else(|| Error::from(E_FAIL))?;
            clipfmt::hdrop(&paths)
        } else if cf == self.formats.drop_effect {
            DROPEFFECT_COPY.0.to_le_bytes().to_vec()
        } else {
            self.drag.id.to_le_bytes().to_vec()
        };
        let h = global_bytes(&bytes);
        if h.is_null() {
            return Err(E_OUTOFMEMORY.into());
        }
        Ok(STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: HGLOBAL(h) }, pUnkForRelease: ManuallyDrop::new(None) })
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> OleResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, f: *const FORMATETC) -> HRESULT {
        match self.check(f) {
            Ok(_) => S_OK,
            Err(e) => e.code(),
        }
    }

    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
        if let Some(out) = unsafe { out.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        E_NOTIMPL
    }

    fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: BOOL) -> OleResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, direction: u32) -> OleResult<IEnumFORMATETC> {
        if direction != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        let list: Vec<FORMATETC> = self.offered().iter().map(|&cf| formatetc(cf)).collect();
        unsafe { SHCreateStdEnumFmtEtc(&list) }
    }

    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> OleResult<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> OleResult<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> OleResult<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

// ------------------------------------------------------------------ sending

/// The probe window at work: a probe on a computer controlled from another
/// one, or a guard (see the module documentation).
struct ProbeRun {
    /// The engine's probe (a guard gets one once the button is let go).
    id: Option<u64>,
    /// When the engine asked.
    asked: Instant,
    guard: bool,
    /// The files of the drag that came over the window (guard).
    paths: Option<Vec<PathBuf>>,
    /// The engine has its answer.
    reported: bool,
    /// A drag is over the window right now.
    over: bool,
    /// The engine is done with the probe: the window goes once no drag is
    /// over it (or after [`PROBE_LINGER`]).
    done_at: Option<Instant>,
    /// Guard: when the button was seen let go.
    let_go_at: Option<Instant>,
    /// The window is on screen.
    shown: bool,
}

/// The engine asks what is being dragged off this screen. While this
/// computer controls the others, the guard has the answer (or gets it in a
/// moment); otherwise the probe window goes over all screens and the engine
/// moves the pointer onto it.
fn start_probe(id: u64) {
    let Some((events, window, probe)) = with_state(|s| (s.events.clone(), s.window, s.probe.clone())) else { return };
    let guarded = probe.borrow_mut().as_mut().filter(|r| r.guard && r.id.is_none()).map(|r| {
        r.id = Some(id);
        r.asked = Instant::now();
        r.paths.clone()
    });
    match guarded {
        Some(Some(paths)) => {
            if let Some(r) = probe.borrow_mut().as_mut() {
                r.reported = true;
            }
            tracing::info!("the drag that left this screen carries {} item(s)", paths.len());
            let _ = events.send(DndEvent::Probed { probe: id, paths, dragging: false });
            look_after_guard();
        }
        Some(None) => {
            // It may still come over the guard (see the input thread).
            ensure_tick();
        }
        None if input::taken() => {
            tracing::info!("no drag from this screen is under the cursor");
            let _ = events.send(DndEvent::Probed { probe: id, paths: vec![], dragging: false });
        }
        None => {
            *probe.borrow_mut() = Some(ProbeRun {
                id: Some(id),
                asked: Instant::now(),
                guard: false,
                paths: None,
                reported: false,
                over: false,
                done_at: None,
                let_go_at: None,
                shown: true,
            });
            show_probe_window(window);
            let _ = events.send(DndEvent::ProbeReady { probe: id });
            ensure_tick();
        }
    }
}

fn probe_done(id: u64, outcome: ProbeOutcome) {
    let Some(probe) = with_state(|s| s.probe.clone()) else { return };
    let hide = match probe.borrow_mut().as_mut() {
        Some(r) if r.id == Some(id) => {
            tracing::debug!("drag probe finished ({outcome:?})");
            r.done_at = Some(Instant::now());
            r.reported = true;
            !r.guard && !r.over
        }
        _ => false,
    };
    if hide {
        hide_probe();
    }
}

fn hide_probe() {
    with_state(|s| {
        unsafe { ShowWindow(s.window, SW_HIDE) };
        GUARD_OVER.store(false, Ordering::Relaxed);
        s.probe.borrow_mut().take();
    });
}

/// Puts the probe window over all screens.
fn show_probe_window(window: HWND) {
    unsafe {
        SetWindowPos(
            window,
            HWND_TOPMOST,
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    };
}

/// The guard (see the module documentation): up while this computer's own
/// left button is held across the edge; after it is let go, until the drag
/// over it has ended; and its run is kept until the engine's probe has its
/// answer.
fn look_after_guard() {
    let Some((events, window, probe)) = with_state(|s| (s.events.clone(), s.window, s.probe.clone())) else { return };
    let left = LEFT.load(Ordering::Relaxed);
    // Nothing can be dragging without the button down as far as Windows
    // knows (the guard takes every click while it is up).
    let down = left_button_down();
    let mut run = probe.borrow_mut();
    let guarding = run.as_ref().is_some_and(|r| r.guard);
    if left == LEFT_ACROSS && down && !run.as_ref().is_some_and(|r| r.guard && r.shown && r.id.is_none()) {
        // A new crossing with the button held. Anything still waiting on the
        // window is over.
        if let Some(old) = run.take().filter(|r| !r.reported) {
            if let Some(id) = old.id {
                let _ = events.send(DndEvent::Probed { probe: id, paths: vec![], dragging: false });
            }
        }
        *run = Some(ProbeRun {
            id: None,
            asked: Instant::now(),
            guard: true,
            paths: None,
            reported: false,
            over: false,
            done_at: None,
            let_go_at: None,
            shown: true,
        });
        drop(run);
        GUARD_OVER.store(false, Ordering::Relaxed);
        show_probe_window(window);
        tracing::info!("the button is held while the cursor is on another screen; a drag from here is caught by the guard window");
        // A drag in progress only looks again when the pointer moves.
        input::nudge();
        ensure_tick();
        return;
    }
    if !guarding {
        return;
    }
    let Some(r) = run.as_mut() else { return };
    let mut empty = None;
    let mut hide = false;
    if r.shown {
        hide = match left {
            LEFT_ACROSS => !down,
            LEFT_LET_GO => {
                let t = *r.let_go_at.get_or_insert_with(Instant::now);
                !r.over || t.elapsed() >= PROBE_LINGER
            }
            // Back with the button held: the drag carries on here.
            _ => true,
        };
        if hide {
            r.shown = false;
            r.over = false;
            GUARD_OVER.store(false, Ordering::Relaxed);
        }
    }
    if let Some(id) = r.id.filter(|_| !r.reported && r.asked.elapsed() >= PROBE_WAIT) {
        r.reported = true;
        empty = Some(id);
    }
    // Kept for the engine's probe, which follows the button let go.
    let awaited = match r.id {
        Some(_) => !r.reported,
        None => left == LEFT_LET_GO && r.let_go_at.is_none_or(|t| t.elapsed() < PROBE_MAX),
    };
    let forget = !r.shown && !awaited;
    if forget {
        *run = None;
    }
    drop(run);
    if hide {
        unsafe { ShowWindow(window, SW_HIDE) };
    }
    if let Some(id) = empty {
        tracing::info!("drag probe: no file drag came over the guard window");
        let _ = events.send(DndEvent::Probed { probe: id, paths: vec![], dragging: false });
    }
}

/// The left (primary) button is down as far as Windows knows.
fn left_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    let vk = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { VK_RBUTTON } else { VK_LBUTTON };
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

/// A drag carried back here starts once its button is let go and its files
/// are named.
fn look_after_carry() {
    let Some(Some((drag, released, named, since))) =
        with_state(|s| s.carry.as_ref().map(|c| (c.drag, c.released, c.names.is_some(), c.since)))
    else {
        return;
    };
    let released = released
        || CARRY.compare_exchange(CARRY_LET_GO, CARRY_NONE, Ordering::Relaxed, Ordering::Relaxed).is_ok_and(|_| {
            tracing::info!("the button was let go here");
            true
        });
    if since.elapsed() >= CARRY_MAX {
        tracing::info!("the drag carried back here was not let go and named in time; nothing is dropped");
        with_state(|s| {
            s.carry = None;
            let _ = s.events.send(DndEvent::DragEnded { drag, accepted: false });
        });
        return;
    }
    if !(released && named) {
        with_state(|s| {
            if let Some(c) = s.carry.as_mut() {
                c.released = released;
            }
        });
        return;
    }
    let Some(Some(c)) = with_state(|s| s.carry.take()) else { return };
    with_state(|s| s.signals.latest.store(drag, Ordering::Relaxed));
    let names = c.names.unwrap_or_default();
    start_drag(DndCommand::StartDrag {
        drag,
        names,
        pos: kvmbridge_core::types::Point::default(),
        drop_after_ms: c.drop_after_ms,
        after_release: false,
    });
}

/// A drag carried back here: see [`look_after_carry`].
struct Carry {
    drag: u64,
    /// Named by the computer it comes from (`None` until then).
    names: Option<Vec<String>>,
    drop_after_ms: u64,
    /// Its button was let go here.
    released: bool,
    since: Instant,
}

/// The files a drag carries (none if it carries no real files).
fn files_of(data: &IDataObject) -> Vec<PathBuf> {
    let Ok(mut medium) = (unsafe { data.GetData(&formatetc(CF_HDROP.0)) }) else { return vec![] };
    let paths = if medium.tymed == TYMED_HGLOBAL.0 as u32 { hdrop_paths(unsafe { medium.u.hGlobal.0 }) } else { vec![] };
    unsafe { ReleaseStgMedium(&mut medium) };
    paths
}

/// Answer a drag over the probe with "copy" (if it allows that), like a
/// folder would; the probe never takes the drop itself.
fn offer_copy(effect: *mut DROPEFFECT) {
    if let Some(e) = unsafe { effect.as_mut() } {
        *e = if copies(*e) { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    }
}

#[implement(IDropTarget)]
struct ProbeTarget {
    run: Rc<RefCell<Option<ProbeRun>>>,
    events: Events,
    own: u16,
}

impl IDropTarget_Impl for ProbeTarget_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> OleResult<()> {
        let paths = match data.ok() {
            Ok(d) if unsafe { d.QueryGetData(&formatetc(self.own)) } == S_OK => {
                tracing::info!("drag probe: this is KVM Bridge's own drag");
                vec![]
            }
            Ok(d) => files_of(d),
            Err(_) => vec![],
        };
        if let Some(r) = self.run.borrow_mut().as_mut() {
            r.over = true;
            if r.guard {
                GUARD_OVER.store(true, Ordering::Relaxed);
                if r.paths.is_none() {
                    tracing::info!("the guard window: the drag here carries {} item(s)", paths.len());
                    r.paths = Some(paths.clone());
                }
            }
            if let Some(id) = r.id.filter(|_| !r.reported) {
                r.reported = true;
                if !r.guard {
                    tracing::info!("a drag leaving this screen carries {} item(s)", paths.len());
                }
                let _ = self.events.send(DndEvent::Probed { probe: id, paths, dragging: !r.guard });
            }
        }
        offer_copy(effect);
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> OleResult<()> {
        if let Some(r) = self.run.borrow_mut().as_mut() {
            r.over = true;
            if r.guard {
                GUARD_OVER.store(true, Ordering::Relaxed);
            }
        }
        offer_copy(effect);
        Ok(())
    }

    fn DragLeave(&self) -> OleResult<()> {
        if let Some(r) = self.run.borrow_mut().as_mut() {
            r.over = false;
        }
        GUARD_OVER.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn Drop(&self, _: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> OleResult<()> {
        if let Some(r) = self.run.borrow_mut().as_mut() {
            r.over = false;
        }
        GUARD_OVER.store(false, Ordering::Relaxed);
        tracing::info!("a drag was let go over the probe window; nothing was dropped");
        if let Some(e) = unsafe { effect.as_mut() } {
            *e = DROPEFFECT_NONE;
        }
        Ok(())
    }
}
