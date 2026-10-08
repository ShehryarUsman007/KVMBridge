//! Drag and drop between screens on Linux (GTK 3 on Wayland).
//!
//! Wayland only tells a program about a drag when the drag is over one of
//! its windows, and only lets it start one from a button press it received.
//! Both are arranged with invisible full-screen windows:
//!
//! * **Receiving** (`StartDrag`): a window is shown on the monitor under the
//!   remote-controlled cursor and a button press is injected into it. That
//!   press starts a native drag of the incoming files; the window then lets
//!   input through, and a release injected a moment later drops the files on
//!   the application under the cursor. When that application asks for the
//!   data, we wait until the files have arrived and hand it their local paths.
//! * **Sending** (`Probe`): when a button held while the cursor left this
//!   screen is let go elsewhere, a drop target is shown under the (frozen)
//!   local pointer. Mutter only moves a drag onto a new window when the
//!   pointer moves, so once it is mapped (`ProbeReady`) the engine lets go of
//!   input capture with the pointer moved by a pixel. A file drag in progress
//!   then enters the window, which tells us the files; the engine ends the
//!   local drag with Escape and captures input again.
//!   If the drag is still around when the cursor comes back (`ProbeDone`),
//!   windows are put on every monitor; if it shows up over them it is
//!   cancelled with Escape, so nothing lands here by accident. (Mutter
//!   ignores a button release from the emulated pointer when that pointer
//!   never pressed the button, so releasing cannot end the drag.)
//!
//! The windows are fully transparent; their CSS background is cleared too,
//! or GTK tells the compositor they are opaque and they show up black.

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use kvmbridge_core::platform::{BackendSide, BackendStatus, DndCommand, DndEvent, ProbeOutcome};
use kvmbridge_core::transfer::{parse_uri_list, path_to_uri};
use kvmbridge_core::types::Point;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

const URI_LIST: &str = "text/uri-list";
/// How long a new window takes to be fully on screen (GNOME animates it in
/// slightly shrunk, so its edges are not under the pointer before that).
const SETTLE: Duration = Duration::from_millis(350);
/// How long to wait for a drag to show up once the pointer has moved.
const PROBE_WAIT: Duration = Duration::from_millis(500);
/// How long a probe window may take to appear.
const MAP_WAIT: Duration = Duration::from_millis(2000);
/// How long to look for a leftover local drag once the cursor is home.
const SINK_WAIT: Duration = Duration::from_millis(800);
/// How long to wait for the injected press that starts a drag.
const PRESS_WAIT: Duration = Duration::from_millis(2000);

type Events = UnboundedSender<DndEvent>;

pub fn start(app: &AppHandle, side: BackendSide<DndCommand, DndEvent>) {
    let BackendSide { mut commands, events } = side;
    let app = app.clone();
    let ev = events.clone();
    let _ = app.run_on_main_thread(move || {
        let wayland = gdk::Display::default().is_some_and(|d| d.type_().name().contains("Wayland"));
        let status = if wayland {
            BackendStatus::Ready("GTK (Wayland)".into())
        } else {
            BackendStatus::Unavailable("Drag and drop between screens needs a Wayland session".into())
        };
        let _ = ev.send(DndEvent::Status(status));
    });
    tauri::async_runtime::spawn(async move {
        while let Some(cmd) = commands.recv().await {
            let ev = events.clone();
            let _ = app.run_on_main_thread(move || on_command(cmd, ev));
        }
    });
}

thread_local! {
    static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    static SINKS: RefCell<Vec<gtk::Window>> = const { RefCell::new(Vec::new()) };
    static DRAG: RefCell<Option<NativeDrag>> = const { RefCell::new(None) };
}

fn on_command(cmd: DndCommand, events: Events) {
    match cmd {
        DndCommand::Probe { probe, pos } => start_probe(probe, pos, events),
        DndCommand::ProbeDone { probe, outcome } => finish_probe(probe, outcome, events),
        DndCommand::StartDrag { drag, names, pos, drop_after_ms } => {
            start_drag(drag, names, pos, Duration::from_millis(drop_after_ms), events)
        }
        DndCommand::CancelDrag { drag } => cancel_drag(drag),
    }
}

/// An invisible window covering the monitor at `pos` (or monitor `index`).
fn overlay(pos: Option<Point>, index: Option<i32>) -> gtk::Window {
    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("KVM Bridge drag and drop");
    win.set_decorated(false);
    win.set_skip_taskbar_hint(true);
    win.set_skip_pager_hint(true);
    win.set_app_paintable(true);
    win.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK);
    if let Some(screen) = GtkWindowExt::screen(&win) {
        if let Some(v) = screen.rgba_visual() {
            win.set_visual(Some(&v));
        }
    }
    let css = gtk::CssProvider::new();
    if css.load_from_data(b"window { background-color: transparent; }").is_ok() {
        win.style_context().add_provider(&css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    win.connect_draw(|_, cr| {
        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = cr.paint();
        glib::Propagation::Stop
    });
    let monitor = index.or_else(|| {
        let p = pos?;
        let d = gdk::Display::default()?;
        let m = d.monitor_at_point(p.x as i32, p.y as i32)?;
        (0..d.n_monitors()).find(|i| d.monitor(*i).as_ref() == Some(&m))
    });
    match (GtkWindowExt::screen(&win), monitor) {
        (Some(screen), Some(i)) => win.fullscreen_on_monitor(&screen, i),
        _ => win.fullscreen(),
    }
    win
}

fn uri_target() -> gtk::TargetEntry {
    gtk::TargetEntry::new(URI_LIST, gtk::TargetFlags::OTHER_APP, 0)
}

// ------------------------------------------------------------------ sending

struct Probe {
    id: u64,
    win: Option<gtk::Window>,
}

struct ProbeState {
    id: u64,
    events: Events,
    seen: bool,
    requested: bool,
    reported: bool,
    /// The window is on screen and `ProbeReady` went out.
    ready: bool,
}

fn report(st: &Rc<RefCell<ProbeState>>, paths: Vec<PathBuf>) {
    let mut s = st.borrow_mut();
    if s.reported {
        return;
    }
    s.reported = true;
    let _ = s.events.send(DndEvent::Probed { probe: s.id, paths, dragging: s.seen });
}

fn close_probe_window(id: u64) {
    PROBE.with(|p| {
        if let Some(p) = p.borrow_mut().as_mut().filter(|p| p.id == id) {
            if let Some(w) = p.win.take() {
                w.close();
            }
        }
    });
}

fn start_probe(id: u64, pos: Point, events: Events) {
    if let Some(old) = PROBE.with(|p| p.borrow_mut().take()) {
        if let Some(w) = old.win {
            w.close();
        }
    }
    let st = Rc::new(RefCell::new(ProbeState { id, events, seen: false, requested: false, reported: false, ready: false }));
    let win = overlay(Some(pos), None);
    win.drag_dest_set(
        gtk::DestDefaults::empty(),
        &[uri_target()],
        gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK,
    );
    {
        let st = st.clone();
        win.connect_drag_motion(move |w, ctx, _, _, time| {
            ctx.drag_status(gdk::DragAction::COPY, time);
            let ask = {
                let mut s = st.borrow_mut();
                s.seen = true;
                !std::mem::replace(&mut s.requested, true)
            };
            if ask {
                tracing::info!("drag probe: a drag came over the probe window; asking for its files");
                w.drag_get_data(ctx, &gdk::Atom::intern(URI_LIST), time);
            }
            true
        });
    }
    {
        let st = st.clone();
        win.connect_drag_data_received(move |_, _, _, _, data, _, _| {
            let uris: Vec<String> = data.uris().iter().map(|u| u.to_string()).collect();
            let paths = parse_uri_list(&uris.join("\r\n"));
            tracing::info!("a drag leaving this screen carries {} item(s)", paths.len());
            report(&st, paths);
            close_probe_window(id);
        });
    }
    win.connect_drag_drop(|_, ctx, _, _, time| {
        ctx.drag_finish(false, false, time);
        true
    });
    {
        // Once it is on screen, ask for the pointer to move, then give the
        // drag a moment to come over.
        let st = st.clone();
        win.connect_map_event(move |_, _| {
            let st = st.clone();
            glib::timeout_add_local_once(SETTLE, move || {
                {
                    let mut s = st.borrow_mut();
                    if s.reported || std::mem::replace(&mut s.ready, true) {
                        return;
                    }
                    let _ = s.events.send(DndEvent::ProbeReady { probe: id });
                }
                glib::timeout_add_local_once(PROBE_WAIT, move || probe_waited(st, id));
            });
            glib::Propagation::Proceed
        });
    }
    win.show_all();
    {
        let st = st.clone();
        glib::timeout_add_local_once(MAP_WAIT, move || {
            let pending = { let s = st.borrow(); !s.reported && !s.ready };
            if pending {
                tracing::info!("drag probe: the probe window did not appear");
                report(&st, vec![]);
                close_probe_window(id);
            }
        });
    }
    PROBE.with(|p| *p.borrow_mut() = Some(Probe { id, win: Some(win) }));
}

/// `PROBE_WAIT` after the pointer moved over the probe window.
fn probe_waited(st: Rc<RefCell<ProbeState>>, id: u64) {
    if !st.borrow().seen {
        tracing::info!("drag probe: no drag came over the probe window");
        report(&st, vec![]);
        close_probe_window(id);
        return;
    }
    // A drag is there but its data is slow: give it a bit longer.
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        if !st.borrow().reported {
            tracing::info!("drag probe: a drag is here but it did not hand over any files");
        }
        report(&st, vec![]);
        close_probe_window(id);
    });
}

fn finish_probe(id: u64, outcome: ProbeOutcome, events: Events) {
    let Some(p) = PROBE.with(|p| p.borrow_mut().take_if(|p| p.id == id)) else { return };
    if let Some(w) = p.win {
        w.close();
    }
    tracing::debug!("drag probe finished ({outcome:?})");
    // Otherwise the button was let go on another screen, and the drag that
    // went with it may still be going on here.
    if outcome != ProbeOutcome::Cancelled {
        end_local_drag(events);
    }
}

/// The button of a local drag was let go on another screen, which the
/// compositor did not see: if the drag is still going on here, cancel it.
fn end_local_drag(events: Events) {
    close_sinks();
    let done = Rc::new(std::cell::Cell::new(false));
    let n = gdk::Display::default().map(|d| d.n_monitors()).unwrap_or(1).max(1);
    let sinks: Vec<gtk::Window> = (0..n)
        .map(|i| {
            let w = overlay(None, Some(i));
            w.drag_dest_set(gtk::DestDefaults::empty(), &[uri_target()], gdk::DragAction::all());
            let (done, events) = (done.clone(), events.clone());
            w.connect_drag_motion(move |_, ctx, _, _, time| {
                ctx.drag_status(gdk::DragAction::COPY, time);
                if !done.replace(true) {
                    tracing::info!("cancelling the drag left on this screen");
                    let _ = events.send(DndEvent::InjectEscape);
                    glib::timeout_add_local_once(Duration::from_millis(150), close_sinks);
                }
                true
            });
            w.connect_drag_drop(|_, ctx, _, _, time| {
                ctx.drag_finish(false, false, time);
                true
            });
            w.show_all();
            w
        })
        .collect();
    SINKS.with(|s| *s.borrow_mut() = sinks);
    // Once they are on screen, make sure the compositor notices them.
    glib::timeout_add_local_once(Duration::from_millis(150), move || {
        let _ = events.send(DndEvent::InjectMove { dx: 1.0, dy: 0.0 });
        let _ = events.send(DndEvent::InjectMove { dx: -1.0, dy: 0.0 });
    });
    glib::timeout_add_local_once(SINK_WAIT, close_sinks);
}

fn close_sinks() {
    for w in SINKS.with(|s| std::mem::take(&mut *s.borrow_mut())) {
        w.close();
    }
}

// ------------------------------------------------------------------ receiving

struct NativeDrag {
    id: u64,
    win: gtk::Window,
    st: Rc<RefCell<DragState>>,
}

struct DragState {
    id: u64,
    events: Events,
    started: bool,
    failed: bool,
    ended: bool,
    ctx: Option<gdk::DragContext>,
    /// Local paths once known (asked for at most once).
    paths: Option<Option<Vec<PathBuf>>>,
}

fn close_drag(id: u64) {
    if let Some(d) = DRAG.with(|d| d.borrow_mut().take_if(|d| d.id == id)) {
        d.win.close();
    }
}

fn end_drag(st: &Rc<RefCell<DragState>>, accepted: bool) {
    let (id, events) = {
        let mut s = st.borrow_mut();
        if std::mem::replace(&mut s.ended, true) {
            return;
        }
        (s.id, s.events.clone())
    };
    let _ = events.send(DndEvent::DragEnded { drag: id, accepted });
    glib::idle_add_local_once(move || close_drag(id));
}

fn start_drag(id: u64, names: Vec<String>, pos: Point, drop_after: Duration, events: Events) {
    if let Some(old) = DRAG.with(|d| d.borrow_mut().take()) {
        if let Some(ctx) = old.st.borrow().ctx.clone() {
            ctx.drag_cancel();
        }
        old.win.close();
    }
    let st = Rc::new(RefCell::new(DragState {
        id,
        events: events.clone(),
        started: false,
        failed: false,
        ended: false,
        ctx: None,
        paths: None,
    }));
    let win = overlay(Some(pos), None);
    let icon = if names.len() == 1 && !names[0].contains('.') { "folder" } else { "text-x-generic" };
    {
        let st = st.clone();
        win.connect_button_press_event(move |w, ev| {
            if ev.button() != 1 || std::mem::replace(&mut st.borrow_mut().started, true) {
                return glib::Propagation::Proceed;
            }
            let targets = gtk::TargetList::new(&[uri_target()]);
            let (x, y) = ev.position();
            let actions = gdk::DragAction::COPY | gdk::DragAction::MOVE;
            match w.drag_begin_with_coordinates(&targets, actions, 1, Some(&**ev), x as i32, y as i32) {
                Some(ctx) => {
                    tracing::info!("started a native drag of {} item(s)", names.len());
                    ctx.drag_set_icon_name(icon, 0, 0);
                    st.borrow_mut().ctx = Some(ctx);
                    // Let the drag reach the applications under this window.
                    w.input_shape_combine_region(Some(&cairo::Region::create()));
                    drop_later(&st, drop_after);
                }
                None => {
                    tracing::warn!("could not start a native drag");
                    end_drag(&st, false);
                }
            }
            glib::Propagation::Stop
        });
    }
    {
        let st = st.clone();
        win.connect_drag_data_get(move |_, _, data, _, _| {
            if let Some(paths) = wait_for_paths(&st) {
                let uris: Vec<String> = paths.iter().map(|p| path_to_uri(p)).collect();
                let refs: Vec<&str> = uris.iter().map(String::as_str).collect();
                data.set_uris(&refs);
            }
        });
    }
    {
        let st = st.clone();
        win.connect_drag_failed(move |_, _, result| {
            tracing::info!("the drag was not accepted ({result:?})");
            st.borrow_mut().failed = true;
            glib::Propagation::Stop
        });
    }
    {
        let st = st.clone();
        win.connect_drag_end(move |_, _| {
            let failed = st.borrow().failed;
            end_drag(&st, !failed);
        });
    }
    {
        let events = events.clone();
        win.connect_map_event(move |_, _| {
            let events = events.clone();
            // Give the compositor a moment to put the window under the cursor.
            glib::timeout_add_local_once(Duration::from_millis(80), move || {
                let _ = events.send(DndEvent::InjectButton { pressed: true });
            });
            glib::Propagation::Proceed
        });
    }
    {
        let st = st.clone();
        glib::timeout_add_local_once(PRESS_WAIT, move || {
            if !st.borrow().started {
                tracing::warn!("no button press arrived to start the drag");
                end_drag(&st, false);
            }
        });
    }
    win.show_all();
    DRAG.with(|d| *d.borrow_mut() = Some(NativeDrag { id, win, st }));
}

/// Drop the drag wherever the cursor is after `delay`: a small nudge makes the
/// compositor send the application under it a drag motion, then the button
/// is let go.
fn drop_later(st: &Rc<RefCell<DragState>>, delay: Duration) {
    let st = st.clone();
    glib::timeout_add_local_once(delay, move || {
        let (ended, events) = {
            let s = st.borrow();
            (s.ended, s.events.clone())
        };
        if ended {
            return;
        }
        let _ = events.send(DndEvent::InjectMove { dx: 1.0, dy: 0.0 });
        let _ = events.send(DndEvent::InjectMove { dx: -1.0, dy: 0.0 });
        glib::timeout_add_local_once(Duration::from_millis(100), move || {
            let _ = events.send(DndEvent::InjectButton { pressed: false });
        });
    });
}

/// The drop target wants the data: wait (keeping the UI responsive) until
/// the files have arrived.
fn wait_for_paths(st: &Rc<RefCell<DragState>>) -> Option<Vec<PathBuf>> {
    if let Some(p) = st.borrow().paths.clone() {
        return p;
    }
    let (id, events) = {
        let s = st.borrow();
        (s.id, s.events.clone())
    };
    let (tx, mut rx) = oneshot::channel();
    if events.send(DndEvent::DropRequested { drag: id, reply: tx }).is_err() {
        return None;
    }
    let ctx = glib::MainContext::default();
    // Wakes the loop below regularly to check for the reply.
    let tick = glib::timeout_add_local(Duration::from_millis(30), || glib::ControlFlow::Continue);
    let result = loop {
        match rx.try_recv() {
            Ok(v) => break v,
            Err(oneshot::error::TryRecvError::Empty) => {
                ctx.iteration(true);
            }
            Err(oneshot::error::TryRecvError::Closed) => break None,
        }
    };
    tick.remove();
    tracing::info!("handing over {} dropped item(s)", result.as_ref().map_or(0, Vec::len));
    st.borrow_mut().paths = Some(result.clone());
    result
}

fn cancel_drag(id: u64) {
    let Some(d) = DRAG.with(|d| d.borrow_mut().take_if(|d| d.id == id)) else { return };
    let ctx = d.st.borrow().ctx.clone();
    if let Some(ctx) = ctx {
        ctx.drag_cancel();
    }
    d.win.close();
}
