//! Drag and drop between screens on Linux (GTK 3 on Wayland).
//!
//! Wayland only tells a program about a drag when the drag is over one of
//! its windows, and only lets it start one from a button press it received.
//! Both are arranged with invisible maximized windows. (Not full-screen ones:
//! GNOME puts a black backing behind a full-screen window that is not
//! opaque, which blacks out the screen and catches the pointer even where
//! the window lets input through.)
//!
//! * **Receiving** (`StartDrag`): a window is shown under the
//!   remote-controlled cursor. Once it is fully on screen and the pointer is
//!   over it (a small injected movement makes the compositor notice), a
//!   button press is injected into it. That press starts a native drag of the
//!   incoming files; the window then lets input through, and once the
//!   application under the cursor takes the drag (the pointer is nudged until
//!   it does, for a moment), an injected release drops the files there. The
//!   drag only offers copying, so the application copies the files. When it
//!   asks for the data, we wait until the files have arrived and hand it
//!   their local paths. If the pointer never gets over the window (it is
//!   over a panel or dock, say), nothing is pressed (the press would click
//!   whatever is under the cursor instead). An injected press is always
//!   released again, however the drag ends: while it is held, GNOME ignores
//!   every other click of that button, the touchpad's included.
//!   A drag *carried* back to this screen (`after_release`) arrives with the
//!   real button still held, which GNOME never saw pressed (input was
//!   captured), so the drag cannot start yet and we are not told when the
//!   button is let go. GNOME drops a press of a button that is already held,
//!   though: while the pointer is over the window, a press is injected now
//!   and then, and released again if it did not arrive. The first one that
//!   arrives means the button was let go right there, and it starts the
//!   drag, which then drops as above. GNOME may also keep the pointer on
//!   whatever it was over while the button is held, so the window only sees
//!   the pointer once it is let go: then the press arrives at once, which
//!   counts as "let go here" if the pointer came in well inside the window,
//!   and as "let go elsewhere" (over a panel or dock beside it, so nothing is
//!   dropped) if it came in over an edge. The drag is dropped only once the
//!   other computer has said which files it carries (`DragFiles`); until
//!   then it hangs on the pointer, so no application is left waiting for
//!   files that may never come.
//! * **Sending** (`Probe`): when a button held while the cursor left this
//!   screen is let go elsewhere, a drop target is shown. Mutter only moves a
//!   drag onto a new window when the pointer moves, so once it is mapped
//!   (`ProbeReady`) the engine lets go of input capture with the pointer
//!   moved off the edge, into the window (docks and panels along the edge
//!   are not covered by it). A file drag in progress then enters the window,
//!   which tells us the files; the engine ends the local drag with Escape and
//!   captures input again.
//!   If the drag is still around when the cursor comes back (`ProbeDone`),
//!   full-screen windows are put on every monitor (they must cover the edge
//!   where the cursor comes in); if it shows up over them it is cancelled
//!   with Escape, so nothing lands here by accident. (Mutter ignores a
//!   button release from the emulated pointer when that pointer never
//!   pressed the button, so releasing cannot end the drag.)
//!
//! The windows are fully transparent; their CSS background is cleared too,
//! or GTK tells the compositor they are opaque and they show up black.

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use kvmbridge_core::platform::{BackendSide, BackendStatus, DndCommand, DndEvent, ProbeOutcome};
use kvmbridge_core::transfer::{parse_uri_list, path_to_uri};
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
/// How long to wait for the injected press that starts a drag (from showing
/// the window).
const PRESS_WAIT: Duration = Duration::from_millis(2500);
/// Carried drags: how long an injected press may take to arrive before it
/// counts as dropped (the button is still held), the pause before the next
/// one, how recently the button must have been seen held for an arriving
/// press to mean "let go here", and how long to wait for that at most.
const CARRY_CHECK: Duration = Duration::from_millis(150);
const CARRY_GAP: Duration = Duration::from_millis(50);
const CARRY_HELD_RECENTLY: Duration = Duration::from_millis(600);
const CARRY_WAIT: Duration = Duration::from_secs(30);
/// Carried drags: a pointer that comes in this close to the window's edge
/// came from a panel or dock beside it.
const CARRY_EDGE: f64 = 24.0;
/// While dropping, how often to check whether an application takes the drag
/// (nudging the pointer while none does), and for how long before letting go
/// anyway.
const DROP_TICK: Duration = Duration::from_millis(100);
const ACCEPT_WAIT: Duration = Duration::from_millis(2000);
/// Carried drags: how long a drag may wait to be dropped until its files are
/// known (the engine normally gives up on it first).
const FILES_WAIT: Duration = Duration::from_secs(8);
/// How long GNOME gets to cancel a drag on Escape before the injected press
/// is let go (a release before that would drop it).
const ESCAPE_GAP: Duration = Duration::from_millis(150);

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
        DndCommand::Probe { probe, .. } => start_probe(probe, events),
        DndCommand::ProbeDone { probe, outcome } => finish_probe(probe, outcome, events),
        DndCommand::StartDrag { drag, names, drop_after_ms, after_release, .. } => {
            start_drag(drag, names, Duration::from_millis(drop_after_ms), after_release, events)
        }
        DndCommand::DragFiles { drag, names } => drag_files(drag, names),
        DndCommand::CancelDrag { drag } => cancel_drag(drag),
    }
}

/// How an overlay covers the screen.
enum Cover {
    /// The work area of the monitor the pointer is on, without docks and
    /// panels; it stays see-through.
    WorkArea,
    /// All of monitor `index`. GNOME shows it black (see above), so only for
    /// short-lived windows that must cover docks and panels too.
    Monitor(i32),
}

/// An invisible window.
fn overlay(cover: Cover) -> gtk::Window {
    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("KVM Bridge drag and drop");
    win.set_decorated(false);
    win.set_skip_taskbar_hint(true);
    win.set_skip_pager_hint(true);
    win.set_app_paintable(true);
    win.add_events(
        gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK | gdk::EventMask::ENTER_NOTIFY_MASK,
    );
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
    match (cover, GtkWindowExt::screen(&win)) {
        (Cover::WorkArea, _) => win.maximize(),
        (Cover::Monitor(i), Some(screen)) => win.fullscreen_on_monitor(&screen, i),
        (Cover::Monitor(_), None) => win.fullscreen(),
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

fn start_probe(id: u64, events: Events) {
    if let Some(old) = PROBE.with(|p| p.borrow_mut().take()) {
        if let Some(w) = old.win {
            w.close();
        }
    }
    let st = Rc::new(RefCell::new(ProbeState { id, events, seen: false, requested: false, reported: false, ready: false }));
    let win = overlay(Cover::WorkArea);
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
            let w = overlay(Cover::Monitor(i));
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
    /// The window is settled on screen; the press goes out once the pointer is over it.
    settled: bool,
    /// The pointer is over the window.
    inside: bool,
    /// The press that starts the drag went out (it is injected once, unless
    /// the drag is carried).
    tried: bool,
    /// An injected press is held (a release must follow).
    pressed: bool,
    /// A carried drag: it starts once the real button is let go.
    carried: bool,
    /// Carried: an injected press is on its way.
    trying: bool,
    /// Carried: when an injected press last did not arrive (the button was
    /// still held then).
    held_at: Option<std::time::Instant>,
    /// Carried: the pointer last came in well inside the window (not over an
    /// edge).
    entered_deep: bool,
    /// The application under the pointer takes the drag.
    taken: bool,
    released: bool,
    failed: bool,
    ended: bool,
    ctx: Option<gdk::DragContext>,
    /// Names of the files dragged, and whether they are known (a carried
    /// drag learns them only after it started).
    names: Vec<String>,
    known: bool,
    /// The files were asked for (only once).
    asked: bool,
    /// Local paths once known.
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

fn start_drag(id: u64, names: Vec<String>, drop_after: Duration, carried: bool, events: Events) {
    if let Some(old) = DRAG.with(|d| d.borrow_mut().take()) {
        stop_drag(old);
    }
    let st = Rc::new(RefCell::new(DragState {
        id,
        events: events.clone(),
        started: false,
        settled: false,
        inside: false,
        tried: false,
        pressed: false,
        carried,
        trying: false,
        held_at: None,
        entered_deep: false,
        taken: false,
        released: false,
        failed: false,
        ended: false,
        ctx: None,
        known: !carried,
        names,
        asked: false,
        paths: None,
    }));
    let win = overlay(Cover::WorkArea);
    {
        let st = st.clone();
        win.connect_button_press_event(move |w, ev| {
            if ev.button() != 1 || st.borrow().started {
                return glib::Propagation::Proceed;
            }
            let (carried, pressed, let_go_here) = {
                let s = st.borrow();
                let held_recently = s.held_at.is_some_and(|t| t.elapsed() <= CARRY_HELD_RECENTLY);
                (s.carried, s.pressed, held_recently || s.entered_deep)
            };
            if carried && !pressed {
                // Not our press (or one we already gave up on).
                return glib::Propagation::Proceed;
            }
            if carried && !let_go_here {
                // The button was let go somewhere else (over a panel or
                // dock) and the pointer came here later: drop nothing.
                tracing::info!("the button was let go elsewhere; nothing is dropped");
                let_go(&st);
                end_drag(&st, false);
                return glib::Propagation::Stop;
            }
            st.borrow_mut().started = true;
            // A URI list, plus the file-transfer portal's types when it is
            // there, so sandboxed (Flatpak) applications can read the files.
            let targets = gtk::TargetList::new(&[]);
            targets.add_uri_targets(0);
            let (x, y) = ev.position();
            // Copies only: the files must stay where they are for the
            // application to copy.
            let actions = gdk::DragAction::COPY;
            match w.drag_begin_with_coordinates(&targets, actions, 1, Some(&**ev), x as i32, y as i32) {
                Some(ctx) => {
                    let (n, icon) = {
                        let s = st.borrow();
                        (s.names.len(), drag_icon(&s.names))
                    };
                    tracing::info!("started a native drag of {n} item(s)");
                    ctx.drag_set_icon_name(icon, 0, 0);
                    {
                        // The compositor reports the action the application
                        // under the pointer chose (none if it takes nothing).
                        let st = st.clone();
                        ctx.connect_action_changed(move |_, action| {
                            st.borrow_mut().taken = !action.is_empty();
                        });
                    }
                    st.borrow_mut().ctx = Some(ctx);
                    // Let the drag reach the applications under this window.
                    // GTK only hands the new input region to the compositor
                    // when the window is drawn again, so redraw it.
                    w.input_shape_combine_region(Some(&cairo::Region::create()));
                    w.queue_draw();
                    drop_later(&st, drop_after);
                }
                None => {
                    tracing::warn!("could not start a native drag");
                    let_go(&st);
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
            // Ended early (Escape, say): the injected press is still held.
            let_go(&st);
        });
    }
    {
        // Once it is fully on screen, a small movement makes the compositor
        // put the pointer over it (enter-notify), and then the press goes out.
        let st = st.clone();
        win.connect_map_event(move |_, _| {
            let st = st.clone();
            glib::timeout_add_local_once(SETTLE, move || {
                let (inside, events) = {
                    let mut s = st.borrow_mut();
                    s.settled = true;
                    (s.inside, s.events.clone())
                };
                if inside {
                    press_into(&st);
                } else if !st.borrow().carried {
                    let _ = events.send(DndEvent::InjectMove { dx: 1.0, dy: 0.0 });
                    let _ = events.send(DndEvent::InjectMove { dx: -1.0, dy: 0.0 });
                }
            });
            glib::Propagation::Proceed
        });
    }
    {
        let st = st.clone();
        win.connect_enter_notify_event(move |w, ev| {
            let (x, y) = ev.position();
            let a = w.allocation();
            let (wd, ht) = (f64::from(a.width()), f64::from(a.height()));
            {
                let mut s = st.borrow_mut();
                s.inside = true;
                s.entered_deep = (CARRY_EDGE..=wd - CARRY_EDGE).contains(&x) && (CARRY_EDGE..=ht - CARRY_EDGE).contains(&y);
            }
            press_into(&st);
            glib::Propagation::Proceed
        });
    }
    {
        let st = st.clone();
        win.connect_leave_notify_event(move |_, _| {
            st.borrow_mut().inside = false;
            glib::Propagation::Proceed
        });
    }
    {
        let st = st.clone();
        let wait = if st.borrow().carried { CARRY_WAIT } else { PRESS_WAIT };
        glib::timeout_add_local_once(wait, move || {
            let (started, pressed, ended) = {
                let s = st.borrow();
                (s.started, s.pressed, s.ended)
            };
            if !started && !ended {
                if pressed {
                    tracing::warn!("the injected press did not reach the drag window");
                    let_go(&st);
                } else {
                    tracing::warn!("the pointer did not get over the drag window; nothing was dropped");
                }
                end_drag(&st, false);
            }
        });
    }
    if carried {
        // Show that something is being dragged while the button is held.
        win.connect_realize(|w| {
            if let (Some(gw), Some(d)) = (w.window(), gdk::Display::default()) {
                gw.set_cursor(gdk::Cursor::from_name(&d, "copy").as_ref());
            }
        });
    }
    win.show_all();
    DRAG.with(|d| *d.borrow_mut() = Some(NativeDrag { id, win, st }));
}

/// Inject the press that starts the drag, once the window is settled and the
/// pointer is over it (only once, or for a carried drag, again and again
/// until one arrives).
fn press_into(st: &Rc<RefCell<DragState>>) {
    if st.borrow().carried {
        carry_try(st);
        return;
    }
    let events = {
        let mut s = st.borrow_mut();
        if !s.settled || !s.inside || s.ended || std::mem::replace(&mut s.tried, true) {
            return;
        }
        s.pressed = true;
        s.events.clone()
    };
    let _ = events.send(DndEvent::InjectButton { pressed: true });
}

/// Carried drag: inject a press. If it has not arrived a moment later, the
/// real button is still held (GNOME dropped it): release it and try again
/// while the pointer is over the window.
fn carry_try(st: &Rc<RefCell<DragState>>) {
    let events = {
        let mut s = st.borrow_mut();
        if !s.settled || !s.inside || s.ended || s.started || s.trying {
            return;
        }
        s.trying = true;
        s.pressed = true;
        s.events.clone()
    };
    let _ = events.send(DndEvent::InjectButton { pressed: true });
    let st = st.clone();
    glib::timeout_add_local_once(CARRY_CHECK, move || {
        let unanswered = {
            let mut s = st.borrow_mut();
            s.trying = false;
            !s.started && !s.ended
        };
        if !unanswered {
            return;
        }
        st.borrow_mut().held_at = Some(std::time::Instant::now());
        unpress(&st);
        glib::timeout_add_local_once(CARRY_GAP, move || carry_try(&st));
    });
}

/// Release an injected press that did not start anything.
fn unpress(st: &Rc<RefCell<DragState>>) {
    let events = {
        let mut s = st.borrow_mut();
        if !std::mem::replace(&mut s.pressed, false) {
            return;
        }
        s.events.clone()
    };
    let _ = events.send(DndEvent::InjectButton { pressed: false });
}

/// Drop the drag wherever the cursor is after `delay`: as soon as the
/// application under it takes the drag, the button is let go. Small nudges
/// make the compositor offer the drag to that application (it only moves a
/// drag to another window when the pointer moves). If nothing takes it for
/// a while, the button is let go anyway and the drop is refused, as it would
/// be for any drag there.
fn drop_later(st: &Rc<RefCell<DragState>>, delay: Duration) {
    let st = st.clone();
    glib::timeout_add_local_once(delay, move || wait_for_files(st, std::time::Instant::now() + FILES_WAIT));
}

/// A carried drag is dropped only once its files are known. If they never
/// are, the drag is cancelled (the engine normally does that first), so the
/// injected press is not left held.
fn wait_for_files(st: Rc<RefCell<DragState>>, deadline: std::time::Instant) {
    let (over, known) = {
        let s = st.borrow();
        (s.ended || s.released, s.known)
    };
    if over {
        return;
    }
    if known {
        drop_step(st, std::time::Instant::now() + ACCEPT_WAIT);
        return;
    }
    if std::time::Instant::now() >= deadline {
        tracing::warn!("the dragged files were never named; nothing is dropped");
        end_drag(&st, false);
        abort_drag(&st, None);
        return;
    }
    glib::timeout_add_local_once(DROP_TICK, move || wait_for_files(st, deadline));
}

fn drop_step(st: Rc<RefCell<DragState>>, deadline: std::time::Instant) {
    let (over, taken, events) = {
        let s = st.borrow();
        (s.ended || s.released, s.taken, s.events.clone())
    };
    if over {
        return;
    }
    if taken {
        tracing::info!("an application takes the drag; dropping it there");
        let_go(&st);
        return;
    }
    if std::time::Instant::now() >= deadline {
        tracing::info!("nothing under the cursor takes the drag; letting go");
        let_go(&st);
        return;
    }
    let _ = events.send(DndEvent::InjectMove { dx: 1.0, dy: 0.0 });
    let _ = events.send(DndEvent::InjectMove { dx: -1.0, dy: 0.0 });
    glib::timeout_add_local_once(DROP_TICK, move || drop_step(st, deadline));
}

/// Release the injected press (once): this drops the drag, or, if the drag
/// is already over, just lets go of the button.
fn let_go(st: &Rc<RefCell<DragState>>) {
    st.borrow_mut().released = true;
    unpress(st);
}

/// The drop target wants the data: wait (keeping the UI responsive) until
/// the files have arrived. The files are asked for once; a second request
/// while waiting (some applications read the data twice) waits for the same
/// answer.
fn wait_for_paths(st: &Rc<RefCell<DragState>>) -> Option<Vec<PathBuf>> {
    let ask = {
        let mut s = st.borrow_mut();
        s.paths.is_none() && !std::mem::replace(&mut s.asked, true)
    };
    if ask {
        let (id, events) = {
            let s = st.borrow();
            (s.id, s.events.clone())
        };
        let (tx, rx) = oneshot::channel();
        if events.send(DndEvent::DropRequested { drag: id, reply: tx }).is_err() {
            st.borrow_mut().paths = Some(None);
        } else {
            let st = st.clone();
            glib::MainContext::default().spawn_local(async move {
                let result = rx.await.ok().flatten();
                tracing::info!("handing over {} dropped item(s)", result.as_ref().map_or(0, Vec::len));
                st.borrow_mut().paths = Some(result);
            });
        }
    }
    let ctx = glib::MainContext::default();
    while st.borrow().paths.is_none() {
        ctx.iteration(true);
    }
    st.borrow().paths.clone().flatten()
}

fn drag_icon(names: &[String]) -> &'static str {
    if names.len() == 1 && !names[0].contains('.') {
        "folder"
    } else {
        "text-x-generic"
    }
}

/// The files of a carried drag are known: it may be dropped.
fn drag_files(id: u64, names: Vec<String>) {
    let Some(st) = DRAG.with(|d| d.borrow().as_ref().filter(|d| d.id == id).map(|d| d.st.clone())) else {
        return;
    };
    let ctx = {
        let mut s = st.borrow_mut();
        s.names = names;
        s.known = true;
        s.ctx.clone()
    };
    if let Some(ctx) = ctx {
        ctx.drag_set_icon_name(drag_icon(&st.borrow().names), 0, 0);
    }
}

fn cancel_drag(id: u64) {
    if let Some(d) = DRAG.with(|d| d.borrow_mut().take_if(|d| d.id == id)) {
        stop_drag(d);
    }
}

/// End a drag here without reporting it: cancel the native drag (without
/// dropping it), let go of the injected press and stop injecting more (a
/// carried drag keeps trying until it is over).
fn stop_drag(d: NativeDrag) {
    d.st.borrow_mut().ended = true;
    let st = d.st.clone();
    abort_drag(&st, Some(d.win));
}

/// Cancel a native drag without dropping it, then let go of the injected
/// press (and close `win`). GNOME, not GTK, ends a drag on Wayland: letting
/// go of the button drops it on whatever is under the pointer, even once
/// GTK has cancelled it. Escape makes GNOME cancel it first.
fn abort_drag(st: &Rc<RefCell<DragState>>, win: Option<gtk::Window>) {
    let (live, events) = {
        let mut s = st.borrow_mut();
        let live = s.started && s.pressed && !s.released;
        // No drop from here on.
        s.released = true;
        (live, s.events.clone())
    };
    let finish = move |st: &Rc<RefCell<DragState>>| {
        let ctx = st.borrow().ctx.clone();
        if let Some(ctx) = ctx {
            ctx.drag_cancel();
        }
        unpress(st);
        if let Some(w) = win {
            w.close();
        }
    };
    if !live {
        // Not started, or already dropped: nothing to cancel in GNOME.
        finish(st);
        return;
    }
    let _ = events.send(DndEvent::InjectEscape);
    let st = st.clone();
    glib::timeout_add_local_once(ESCAPE_GAP, move || finish(&st));
}
