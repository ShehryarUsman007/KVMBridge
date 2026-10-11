//! The shared clipboard through the Windows clipboard.
//!
//! A thread of its own owns a hidden window that listens for clipboard
//! changes. Content from another computer is put on the clipboard with
//! delayed rendering: Windows asks for the data (WM_RENDERFORMAT) only when
//! an application pastes it, and this thread then waits for it to arrive.
//! It never touches the input hooks, whose thread must not wait.

use crate::clipfmt::{self, Rgba};
use kvmbridge_core::platform::{BackendStatus, ClipboardCommand, ClipboardEvent};
use kvmbridge_core::transfer::{parse_uri_list, uri_list};
use kvmbridge_core::types::ClipFormat;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, GlobalFree, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub enum Cmd {
    Engine(ClipboardCommand),
    Stop,
}

const WM_APP_CMD: u32 = WM_APP + 1;
const TIMER_CHANGED: usize = 1;
/// Programs often change the clipboard several times for one copy.
const SETTLE_MS: u32 = 150;
/// The same offer again this soon (one copy reported twice) does not take
/// the clipboard again.
const DUPLICATE_WINDOW: Duration = Duration::from_millis(500);
/// How long a paste here waits for content from another computer. Files are
/// copied over first, so they may take longer.
const PASTE_WAIT: Duration = Duration::from_secs(25);
const PASTE_FILES_WAIT: Duration = Duration::from_secs(120);
/// Files asked for this soon after another computer's copy arrived were not
/// asked for by a person pasting (Explorer reads every clipboard change).
const EAGER_WINDOW: Duration = Duration::from_secs(1);
/// DROPEFFECT_COPY: files pasted from another computer are copies.
const DROPEFFECT_COPY: u32 = 1;

pub struct ClipThread {
    hwnd: isize,
    tx: mpsc::Sender<Cmd>,
}

impl ClipThread {
    pub fn spawn(events: UnboundedSender<ClipboardEvent>, rt: tokio::runtime::Handle) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("kvmb-clipboard".into())
            .spawn(move || thread_main(rx, events, rt, ready_tx))
            .map_err(|e| e.to_string())?;
        let hwnd = ready_rx.recv().map_err(|_| "the clipboard thread did not start".to_string())??;
        Ok(ClipThread { hwnd, tx })
    }

    pub fn send(&self, c: Cmd) {
        if self.tx.send(c).is_ok() {
            unsafe { PostMessageW(self.hwnd as HWND, WM_APP_CMD, 0, 0) };
        }
    }
}

/// Clipboard formats Windows knows by name.
struct Names {
    html: u32,
    png: u32,
    drop_effect: u32,
    /// Programs put these next to content that must not be shared or kept
    /// (password managers, for example).
    private: [u32; 2],
}

fn register(name: &str) -> u32 {
    let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { RegisterClipboardFormatW(w.as_ptr()) }
}

impl Names {
    fn new() -> Names {
        Names {
            html: register("HTML Format"),
            png: register("PNG"),
            drop_effect: register("Preferred DropEffect"),
            private: [register("ExcludeClipboardContentFromMonitorProcessing"), register("Clipboard Viewer Ignore")],
        }
    }

    /// Windows formats a remote format is offered as.
    fn windows_formats(&self, f: ClipFormat) -> Vec<u32> {
        match f {
            ClipFormat::Text => vec![CF_UNICODETEXT as u32],
            ClipFormat::Html => vec![self.html],
            ClipFormat::Png => vec![self.png, CF_DIBV5 as u32],
            ClipFormat::Files => vec![CF_HDROP as u32],
        }
    }

    fn format_of(&self, wf: u32) -> Option<ClipFormat> {
        match wf {
            f if f == CF_UNICODETEXT as u32 => Some(ClipFormat::Text),
            f if f == self.html => Some(ClipFormat::Html),
            f if f == self.png || f == CF_DIBV5 as u32 || f == CF_DIB as u32 => Some(ClipFormat::Png),
            f if f == CF_HDROP as u32 => Some(ClipFormat::Files),
            _ => None,
        }
    }
}

struct State {
    events: UnboundedSender<ClipboardEvent>,
    rx: mpsc::Receiver<Cmd>,
    rt: tokio::runtime::Handle,
    hwnd: HWND,
    names: Names,
    /// The remote offer this computer's clipboard holds right now.
    offer: Option<u64>,
    last_set: Option<(Vec<ClipFormat>, Instant)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn available(f: u32) -> bool {
    unsafe { IsClipboardFormatAvailable(f) != 0 }
}

/// Opens the clipboard, which another program may have open for a moment.
fn open(hwnd: HWND) -> bool {
    for _ in 0..20 {
        if unsafe { OpenClipboard(hwnd) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    tracing::warn!("cannot open the clipboard (error {})", unsafe { GetLastError() });
    false
}

/// Copies a clipboard entry's bytes out (the clipboard must be open).
fn read_bytes(f: u32) -> Option<Vec<u8>> {
    let h = unsafe { GetClipboardData(f) };
    if h.is_null() {
        return None;
    }
    let size = unsafe { GlobalSize(h) };
    let p = unsafe { GlobalLock(h) } as *const u8;
    if p.is_null() {
        return None;
    }
    let data = unsafe { std::slice::from_raw_parts(p, size) }.to_vec();
    unsafe { GlobalUnlock(h) };
    Some(data)
}

/// The files on the clipboard (the clipboard must be open).
fn read_files() -> Option<Vec<PathBuf>> {
    let h = unsafe { GetClipboardData(CF_HDROP as u32) } as HDROP;
    if h.is_null() {
        return None;
    }
    Some(hdrop_paths(h))
}

/// The files in a CF_HDROP entry (also used for drags).
pub(crate) fn hdrop_paths(h: HDROP) -> Vec<PathBuf> {
    let n = unsafe { DragQueryFileW(h, u32::MAX, std::ptr::null_mut(), 0) };
    let mut paths = Vec::new();
    for i in 0..n {
        let len = unsafe { DragQueryFileW(h, i, std::ptr::null_mut(), 0) } as usize;
        let mut buf = vec![0u16; len + 1];
        let got = unsafe { DragQueryFileW(h, i, buf.as_mut_ptr(), buf.len() as u32) } as usize;
        paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..got])));
    }
    paths
}

/// `data` in a new global memory block (null if out of memory). Whoever
/// receives it frees it.
pub(crate) fn global_bytes(data: &[u8]) -> HGLOBAL {
    let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)) };
    if h.is_null() {
        return h;
    }
    let p = unsafe { GlobalLock(h) } as *mut u8;
    if p.is_null() {
        unsafe { GlobalFree(h) };
        return std::ptr::null_mut();
    }
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        GlobalUnlock(h);
    }
    h
}

/// Puts `data` on the clipboard as format `f` (the clipboard must be open,
/// or this answers a WM_RENDERFORMAT).
fn put(f: u32, data: &[u8]) -> bool {
    let h = global_bytes(data);
    if h.is_null() {
        return false;
    }
    if unsafe { SetClipboardData(f, h) }.is_null() {
        unsafe { GlobalFree(h) };
        return false;
    }
    true
}

fn u16_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// Reads this computer's clipboard as `format`, in the shared form.
fn read_local(hwnd: HWND, names: &Names, format: ClipFormat) -> Option<Vec<u8>> {
    enum Raw {
        Text(Vec<u8>),
        Html(Vec<u8>),
        Png(Vec<u8>),
        Dib(Vec<u8>),
        Files(Vec<PathBuf>),
    }
    if !open(hwnd) {
        return None;
    }
    let raw = match format {
        ClipFormat::Text => read_bytes(CF_UNICODETEXT as u32).map(Raw::Text),
        ClipFormat::Html => read_bytes(names.html).map(Raw::Html),
        ClipFormat::Png if available(names.png) => read_bytes(names.png).map(Raw::Png),
        ClipFormat::Png if available(CF_DIBV5 as u32) => read_bytes(CF_DIBV5 as u32).map(Raw::Dib),
        ClipFormat::Png => read_bytes(CF_DIB as u32).map(Raw::Dib),
        ClipFormat::Files => read_files().map(Raw::Files),
    };
    unsafe { CloseClipboard() };
    // Converting may take a moment: the clipboard is closed by now.
    match raw? {
        Raw::Text(b) => {
            let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            Some(clipfmt::text_from_windows(&units).into_bytes())
        }
        Raw::Html(b) => clipfmt::html_fragment(&b).map(String::into_bytes),
        Raw::Png(b) => Some(b),
        Raw::Dib(b) => clipfmt::png_encode(&clipfmt::dib_to_rgba(&b)?),
        Raw::Files(paths) => (!paths.is_empty()).then(|| uri_list(&paths).into_bytes()),
    }
}

/// Content from another computer in the Windows form of `wf`.
fn to_windows(names: &Names, wf: u32, data: &[u8]) -> Option<Vec<u8>> {
    match wf {
        f if f == CF_UNICODETEXT as u32 => Some(u16_bytes(&clipfmt::text_to_windows(&String::from_utf8_lossy(data)))),
        f if f == names.html => Some(clipfmt::html_to_windows(&String::from_utf8_lossy(data))),
        f if f == names.png => Some(data.to_vec()),
        f if f == CF_DIBV5 as u32 => clipfmt::png_decode(data).map(|img: Rgba| clipfmt::rgba_to_dibv5(&img)),
        f if f == CF_HDROP as u32 => {
            let paths = parse_uri_list(&String::from_utf8_lossy(data));
            (!paths.is_empty()).then(|| clipfmt::hdrop(&paths))
        }
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_CMD => {
            with_state(State::run_commands);
            0
        }
        WM_CLIPBOARDUPDATE => {
            // Wait for the program to finish changing it.
            if unsafe { GetClipboardOwner() } != hwnd {
                unsafe { SetTimer(hwnd, TIMER_CHANGED, SETTLE_MS, None) };
            }
            0
        }
        WM_TIMER if wparam == TIMER_CHANGED => {
            unsafe { KillTimer(hwnd, TIMER_CHANGED) };
            with_state(State::local_changed);
            0
        }
        WM_RENDERFORMAT => {
            render(wparam as u32);
            0
        }
        WM_DESTROYCLIPBOARD => {
            // Another program took the clipboard.
            with_state(|s| s.offer = None);
            0
        }
        // Quitting while other computers' content is on the clipboard: it
        // goes (fetching it all now could take long).
        WM_RENDERALLFORMATS => 0,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Process id and program name of the window `w`.
fn program_of(w: HWND) -> (u32, String) {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(w, &mut pid) };
    let mut name = String::from("?");
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if !h.is_null() {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        if unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } != 0 {
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            name = path.rsplit('\\').next().unwrap_or(&path).to_string();
        }
        unsafe { CloseHandle(h) };
    }
    (pid, name)
}

/// A program pastes content from another computer: fetch it and hand it
/// over. The program waits meanwhile, as it would for a slow local one.
fn render(wf: u32) {
    let Some((events, rt, offer, format, since_copy)) = with_state(|s| {
        let format = s.names.format_of(wf)?;
        let since_copy = s.last_set.as_ref().map(|(_, t)| t.elapsed());
        Some((s.events.clone(), s.rt.clone(), s.offer?, format, since_copy))
    })
    .flatten() else {
        return;
    };
    if format == ClipFormat::Files {
        // Files travel only when a person pastes them: some programs read
        // every clipboard change (Explorer itself does, within milliseconds),
        // which would copy the files over at once. So the request has to
        // come from the window being used, and not right after the copy.
        let opener = unsafe { GetOpenClipboardWindow() };
        let (pid, name) = if opener.is_null() { (0, "A program".to_string()) } else { program_of(opener) };
        if since_copy.is_some_and(|t| t < EAGER_WINDOW) {
            tracing::info!("{name} asked for the files right after the copy; they come over only when pasted");
            return;
        }
        if !opener.is_null() {
            let (front, _) = program_of(unsafe { GetForegroundWindow() });
            if pid != front {
                tracing::info!("{name} (in the background) asked for the files on the clipboard; they come over only when pasted");
                return;
            }
        }
        tracing::info!("{name} pastes the files from the clipboard");
    }
    let (tx, rx) = oneshot::channel();
    if events.send(ClipboardEvent::Paste { offer, format, reply: tx }).is_err() {
        return;
    }
    let wait = if format == ClipFormat::Files { PASTE_FILES_WAIT } else { PASTE_WAIT };
    let data = rt.block_on(async { tokio::time::timeout(wait, rx).await.ok().and_then(|r| r.ok()).flatten() });
    let Some(data) = data else {
        tracing::info!("nothing to paste for {format:?}");
        return;
    };
    let converted = with_state(|s| to_windows(&s.names, wf, &data)).flatten();
    match converted {
        Some(d) if put(wf, &d) => {}
        _ => tracing::warn!("could not paste {format:?} here"),
    }
}

fn create_window() -> Result<HWND, String> {
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class: Vec<u16> = "KVMBridgeClipboard".encode_utf16().chain(std::iter::once(0)).collect();
    let mut wc: WNDCLASSEXW = unsafe { std::mem::zeroed() };
    wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
    wc.hInstance = module;
    wc.lpfnWndProc = Some(wndproc);
    wc.lpszClassName = class.as_ptr();
    unsafe { RegisterClassExW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(format!("cannot create the clipboard window (error {})", unsafe { GetLastError() }));
    }
    if unsafe { AddClipboardFormatListener(hwnd) } == 0 {
        let e = unsafe { GetLastError() };
        unsafe { DestroyWindow(hwnd) };
        return Err(format!("cannot watch the clipboard (error {e})"));
    }
    Ok(hwnd)
}

fn thread_main(
    rx: mpsc::Receiver<Cmd>,
    events: UnboundedSender<ClipboardEvent>,
    rt: tokio::runtime::Handle,
    ready: mpsc::SyncSender<Result<isize, String>>,
) {
    let hwnd = match create_window() {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = events.send(ClipboardEvent::Status(BackendStatus::Ready("Windows clipboard".into())));
    STATE.with(|s| *s.borrow_mut() = Some(State { events, rx, rt, hwnd, names: Names::new(), offer: None, last_set: None }));
    let _ = ready.send(Ok(hwnd as isize));

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    STATE.with(|s| s.borrow_mut().take());
    unsafe {
        RemoveClipboardFormatListener(hwnd);
        DestroyWindow(hwnd);
    }
}

impl State {
    fn run_commands(&mut self) {
        while let Ok(c) = self.rx.try_recv() {
            match c {
                Cmd::Engine(ClipboardCommand::AdvertiseRemote { offer, formats }) => self.advertise(offer, formats),
                Cmd::Engine(ClipboardCommand::Read { format, reply }) => {
                    let _ = reply.send(read_local(self.hwnd, &self.names, format));
                }
                Cmd::Stop => unsafe { PostQuitMessage(0) },
            }
        }
    }

    /// Puts another computer's clipboard here; the data comes when pasted.
    fn advertise(&mut self, offer: u64, formats: Vec<ClipFormat>) {
        self.offer = Some(offer);
        if self.last_set.as_ref().is_some_and(|(f, t)| *f == formats && t.elapsed() < DUPLICATE_WINDOW) {
            return;
        }
        if !open(self.hwnd) {
            return;
        }
        unsafe { EmptyClipboard() };
        for f in &formats {
            for wf in self.names.windows_formats(*f) {
                // No data yet: Windows asks for it when something pastes.
                unsafe { SetClipboardData(wf, std::ptr::null_mut()) };
            }
        }
        if formats.contains(&ClipFormat::Files) {
            // Pasted files are copies, never moved.
            put(self.names.drop_effect, &DROPEFFECT_COPY.to_le_bytes());
        }
        unsafe { CloseClipboard() };
        // EmptyClipboard told the previous owner (maybe this window) to let go.
        self.offer = Some(offer);
        self.last_set = Some((formats, Instant::now()));
    }

    /// Another program changed the clipboard.
    fn local_changed(&mut self) {
        if unsafe { GetClipboardOwner() } == self.hwnd {
            return;
        }
        self.offer = None;
        self.last_set = None;
        if self.names.private.iter().any(|&f| f != 0 && available(f)) {
            tracing::info!("the clipboard changed here; the program marked it private, so it is not shared");
            return;
        }
        let mut formats = Vec::new();
        if available(CF_HDROP as u32) {
            formats.push(ClipFormat::Files);
        }
        if available(self.names.png) || available(CF_DIBV5 as u32) || available(CF_DIB as u32) {
            formats.push(ClipFormat::Png);
        }
        if available(self.names.html) {
            formats.push(ClipFormat::Html);
        }
        if available(CF_UNICODETEXT as u32) {
            formats.push(ClipFormat::Text);
        }
        if !formats.is_empty() {
            let _ = self.events.send(ClipboardEvent::LocalChanged { formats });
        }
    }
}
