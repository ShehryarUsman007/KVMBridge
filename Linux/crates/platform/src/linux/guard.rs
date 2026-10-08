//! Pausing this computer's own keyboards and mice while another computer
//! controls it, so they cannot interfere.
//!
//! Wayland offers no API for that. A small privileged helper
//! (`kvmbridge-input-guard`, started through pkexec; its polkit policy lets
//! the signed-in user run it without a password) takes exclusive hold of the
//! keyboard and mouse event devices (`EVIOCGRAB`) and throws away what they
//! produce. Key presses never reach this process: the helper only reports
//! how many devices it holds. It lets go when told to, when this app exits
//! (its stdin closes), and when Ctrl+Alt+Shift+Esc is pressed on one of the
//! held keyboards.
//!
//! Input injected by the controlling computer goes through the compositor's
//! own virtual devices and is not affected.

use kvmbridge_core::platform::{CaptureEvent, GuardStatus};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

pub const HELPER_NAME: &str = "kvmbridge-input-guard";
/// Installed by the deb/rpm packages.
const PKG_HELPER: &str = "/usr/bin/kvmbridge-input-guard";
const PKG_POLICY: &str = "/usr/share/polkit-1/actions/io.kvmbridge.input-guard.policy";
/// Installed by the one-time setup (AppImage, development builds).
const LOCAL_HELPER: &str = "/usr/local/lib/kvmbridge/kvmbridge-input-guard";
const LOCAL_POLICY: &str = "/usr/share/polkit-1/actions/io.kvmbridge.input-guard-local.policy";

/// polkit policy that lets the active local user run `helper` as root.
pub fn policy_xml(action: &str, helper: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/PolicyKit/1/policyconfig.dtd">
<policyconfig>
  <vendor>KVM Bridge</vendor>
  <action id="{action}">
    <description>Pause this computer's keyboard and mouse while another computer controls it</description>
    <message>KVM Bridge wants to pause this computer's keyboard and mouse</message>
    <defaults>
      <allow_any>no</allow_any>
      <allow_inactive>no</allow_inactive>
      <allow_active>yes</allow_active>
    </defaults>
    <annotate key="org.freedesktop.policykit.exec.path">{helper}</annotate>
  </action>
</policyconfig>
"#
    )
}

/// The helper to run, if it is installed together with its policy.
fn installed_helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KVMBRIDGE_GUARD_HELPER") {
        return Some(p.into());
    }
    [(PKG_HELPER, PKG_POLICY), (LOCAL_HELPER, LOCAL_POLICY)]
        .into_iter()
        .find(|(h, p)| Path::new(h).is_file() && Path::new(p).is_file())
        .map(|(h, _)| h.into())
}

/// One-time setup for builds that are not installed from a package:
/// install the helper and its polkit policy (asks for an administrator
/// password).
pub fn install_helper() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let src = exe.with_file_name(HELPER_NAME);
    let bytes = std::fs::read(&src).map_err(|e| format!("The helper is missing ({}): {e}", src.display()))?;
    let policy = policy_xml("io.kvmbridge.input-guard-local", LOCAL_HELPER);
    let script = format!(
        "set -e; install -d -m 755 /usr/local/lib/kvmbridge; cat > {LOCAL_HELPER}.tmp; \
         chmod 755 {LOCAL_HELPER}.tmp; mv {LOCAL_HELPER}.tmp {LOCAL_HELPER}; \
         cat > {LOCAL_POLICY} <<'KVMB_POLICY'\n{policy}KVMB_POLICY\nchmod 644 {LOCAL_POLICY}"
    );
    let mut child = Command::new("pkexec")
        .args(["/bin/sh", "-c", &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot ask for administrator rights (pkexec: {e})"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => {
            tracing::info!("installed {LOCAL_HELPER}");
            Ok(())
        }
        Some(126) => Err("Cancelled".into()),
        Some(127) => Err("Not authorized".into()),
        _ => Err(format!("Setup failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
    }
}

// ------------------------------------------------------------------ app side

enum Event {
    Want(bool),
    Line(String),
    Exited(Option<i32>),
}

/// Handle to the thread that drives the helper.
#[derive(Clone)]
pub(crate) struct Guard {
    tx: mpsc::Sender<Event>,
}

impl Guard {
    pub(crate) fn spawn(events: UnboundedSender<CaptureEvent>) -> Guard {
        let (tx, rx) = mpsc::channel();
        let me = tx.clone();
        let r = std::thread::Builder::new().name("kvmb-guard".into()).spawn(move || drive(rx, me, events));
        if let Err(e) = r {
            tracing::warn!("cannot start the input guard thread: {e}");
        }
        Guard { tx }
    }

    pub(crate) fn set(&self, paused: bool) {
        let _ = self.tx.send(Event::Want(paused));
    }
}

/// A running helper. Dropping it closes its stdin, which makes it let go of
/// everything and exit.
struct Helper {
    _child: Child,
    stdin: ChildStdin,
}

impl Helper {
    fn tell(&mut self, cmd: &str) -> bool {
        self.stdin.write_all(format!("{cmd}\n").as_bytes()).and_then(|_| self.stdin.flush()).is_ok()
    }
}

fn drive(rx: mpsc::Receiver<Event>, me: mpsc::Sender<Event>, events: UnboundedSender<CaptureEvent>) {
    let status = |s: GuardStatus| {
        let _ = events.send(CaptureEvent::Guard(s));
    };
    let mut helper: Option<Helper> = None;
    let mut want = false;
    while let Ok(ev) = rx.recv() {
        match ev {
            Event::Want(w) => {
                want = w;
                if !w {
                    if let Some(h) = &mut helper {
                        h.tell("resume");
                    }
                    status(GuardStatus::Off);
                    continue;
                }
                if helper.is_none() {
                    let Some(path) = installed_helper() else {
                        status(GuardStatus::NeedsSetup);
                        continue;
                    };
                    match start_helper(&path, me.clone()) {
                        Ok(h) => helper = Some(h),
                        Err(e) => {
                            status(GuardStatus::Unavailable(e));
                            continue;
                        }
                    }
                }
                if let Some(h) = &mut helper {
                    if !h.tell("pause") {
                        helper = None;
                        status(GuardStatus::Unavailable("The helper stopped".into()));
                    }
                }
            }
            Event::Line(l) => {
                let (word, rest) = l.split_once(' ').unwrap_or((l.as_str(), ""));
                match word {
                    "paused" if want => status(GuardStatus::Paused(rest.trim().parse().unwrap_or(0))),
                    "resumed" if !want => status(GuardStatus::Off),
                    "escape" => {
                        want = false;
                        status(GuardStatus::Escaped);
                    }
                    "error" => status(GuardStatus::Unavailable(rest.to_string())),
                    _ => {}
                }
            }
            Event::Exited(code) => {
                helper = None;
                if want {
                    let why = match code {
                        Some(126) => "Permission to pause the keyboard and mouse was refused".to_string(),
                        Some(127) => "Not allowed to pause the keyboard and mouse".to_string(),
                        _ => "The keyboard/mouse helper stopped".to_string(),
                    };
                    status(GuardStatus::Unavailable(why));
                }
            }
        }
    }
}

fn start_helper(path: &Path, events: mpsc::Sender<Event>) -> Result<Helper, String> {
    tracing::info!("starting {}", path.display());
    let mut child = Command::new("pkexec")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Cannot start the keyboard/mouse helper (pkexec: {e})"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let pid = child.id();
    std::thread::Builder::new()
        .name("kvmb-guard-out".into())
        .spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if events.send(Event::Line(line)).is_err() {
                    return;
                }
            }
            // Collect the exit code (pkexec reports refusals that way).
            let code = wait_pid(pid);
            let _ = events.send(Event::Exited(code));
        })
        .map_err(|e| e.to_string())?;
    Ok(Helper { _child: child, stdin })
}

fn wait_pid(pid: u32) -> Option<i32> {
    let mut status = 0;
    // SAFETY: waiting for our own child process.
    let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
    (r > 0 && libc::WIFEXITED(status)).then(|| libc::WEXITSTATUS(status))
}

// ------------------------------------------------------------------ devices

/// An event node of a keyboard or pointing device.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub dev: PathBuf,
    pub name: String,
    pub keyboard: bool,
}

/// Keyboards, mice, touchpads, tablets and touchscreens (not power buttons,
/// media key interfaces, game controllers or software devices).
/// `sys`, `udev` and `devdir` are normally `/sys`, `/run/udev`, `/dev/input`.
pub fn scan(sys: &Path, udev: &Path, devdir: &Path) -> Vec<Node> {
    let Ok(dir) = std::fs::read_dir(sys.join("class/input")) else { return vec![] };
    let mut out: Vec<(u32, Node)> = dir
        .flatten()
        .filter_map(|e| {
            let ev = e.file_name().to_string_lossy().into_owned();
            let n: u32 = ev.strip_prefix("event")?.parse().ok()?;
            let node = std::fs::canonicalize(e.path()).ok()?;
            let input = node.parent()?;
            if input.to_string_lossy().contains("/devices/virtual/input/") {
                return None;
            }
            let props = udev_props(udev, &read(node.join("dev")).unwrap_or_default());
            let (keyboard, pointer) =
                if props.contains_key("ID_INPUT") { classify_udev(&props) } else { classify_caps(input) };
            (keyboard || pointer).then(|| {
                (n, Node { dev: devdir.join(&ev), name: read(input.join("name")).unwrap_or_default(), keyboard })
            })
        })
        .collect();
    out.sort_by_key(|(n, _)| *n);
    out.into_iter().map(|(_, n)| n).collect()
}

fn read(p: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn udev_props(udev: &Path, dev: &str) -> HashMap<String, String> {
    let Some(text) = read(udev.join("data").join(format!("c{dev}"))) else { return HashMap::new() };
    text.lines()
        .filter_map(|l| l.strip_prefix("E:"))
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// (keyboard, pointing device) as classified by udev's input_id builtin.
fn classify_udev(p: &HashMap<String, String>) -> (bool, bool) {
    let on = |k: &str| p.get(k).is_some_and(|v| v == "1");
    let pointer = ["ID_INPUT_MOUSE", "ID_INPUT_TOUCHPAD", "ID_INPUT_POINTINGSTICK", "ID_INPUT_TRACKBALL", "ID_INPUT_TOUCHSCREEN"]
        .iter()
        .any(|k| on(k))
        || (on("ID_INPUT_TABLET") && !on("ID_INPUT_TABLET_PAD"));
    (on("ID_INPUT_KEYBOARD"), pointer && !on("ID_INPUT_JOYSTICK"))
}

/// Fallback without a udev database: look at the capability bitmaps.
fn classify_caps(input: &Path) -> (bool, bool) {
    let bits = |f: &str| -> Vec<u64> {
        read(input.join(f))
            .unwrap_or_default()
            .split_whitespace()
            .rev()
            .map(|w| u64::from_str_radix(w, 16).unwrap_or(0))
            .collect()
    };
    let word = usize::BITS as usize;
    let has = |b: &[u64], n: usize| b.get(n / word).is_some_and(|w| (w >> (n % word)) & 1 == 1);
    let (key, rel, abs) = (bits("capabilities/key"), bits("capabilities/rel"), bits("capabilities/abs"));
    // KEY_Q, KEY_A, KEY_Z, KEY_SPACE
    let keyboard = [16, 30, 44, 57].iter().all(|&k| has(&key, k));
    // REL_X + REL_Y + BTN_LEFT, or ABS_X + ABS_Y + BTN_TOUCH / BTN_TOOL_FINGER
    let pointer = (has(&rel, 0) && has(&rel, 1) && has(&key, 0x110))
        || (has(&abs, 0) && has(&abs, 1) && (has(&key, 0x14a) || has(&key, 0x145)));
    (keyboard, pointer)
}

// ------------------------------------------------------------------ helper side

const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
/// EVIOCGKEY(96): current key/button state (KEY_MAX + 1 bits).
const EVIOCGKEY_96: libc::c_ulong = 0x8060_4518;
const EV_KEY: u16 = 0x01;
const KEY_ESC: u16 = 1;
const CTRL: [u16; 2] = [29, 97];
const ALT: [u16; 2] = [56, 100];
const SHIFT: [u16; 2] = [42, 54];

struct Held {
    file: File,
    node: Node,
}

/// Grab a device unless keys or buttons are down on it right now (grabbing
/// then would leave them stuck for the compositor).
fn grab(node: &Node) -> Result<File, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&node.dev)
        .map_err(|e| e.to_string())?;
    let fd = file.as_raw_fd();
    let mut state = [0u8; 96];
    // SAFETY: valid fd and a buffer of the size encoded in the request.
    if unsafe { libc::ioctl(fd, EVIOCGKEY_96 as _, state.as_mut_ptr()) } >= 0 && state.iter().any(|b| *b != 0) {
        return Err("busy".into());
    }
    // SAFETY: valid fd; the argument is an int flag.
    if unsafe { libc::ioctl(fd, EVIOCGRAB as _, 1 as libc::c_int) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(file)
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Entry point of the `kvmbridge-input-guard` helper (runs as root).
///
/// Commands on stdin: `pause`, `resume`. Replies on stdout: `paused <n>`,
/// `resumed`, `escape`, `error <text>`. End of input releases everything.
pub fn helper_main() -> i32 {
    // SAFETY: getuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        say("error The keyboard/mouse helper must run as root");
        return 1;
    }
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(Some(line.trim().to_string())).is_err() {
                return;
            }
        }
        let _ = tx.send(None);
    });
    let (sys, udev, devdir) = (Path::new("/sys"), Path::new("/run/udev"), Path::new("/dev/input"));
    let mut paused = false;
    let mut held: Vec<Held> = Vec::new();
    let mut reported = usize::MAX;
    let mut next_scan = Instant::now();
    let mut down: std::collections::HashSet<u16> = Default::default();
    loop {
        loop {
            match rx.try_recv() {
                Ok(Some(cmd)) if cmd == "pause" => {
                    paused = true;
                    next_scan = Instant::now();
                    reported = usize::MAX;
                }
                Ok(Some(cmd)) if cmd == "resume" => {
                    paused = false;
                    held.clear();
                    down.clear();
                    say("resumed");
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => return 0,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if paused && Instant::now() >= next_scan {
            for node in scan(sys, udev, devdir) {
                if held.iter().any(|h| h.node.dev == node.dev) {
                    continue;
                }
                if let Ok(file) = grab(&node) {
                    held.push(Held { file, node });
                }
            }
            if held.len() != reported {
                reported = held.len();
                say(&format!("paused {reported}"));
            }
            next_scan = Instant::now() + Duration::from_millis(500);
        }
        if held.is_empty() {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let mut fds: Vec<libc::pollfd> =
            held.iter().map(|h| libc::pollfd { fd: h.file.as_raw_fd(), events: libc::POLLIN, revents: 0 }).collect();
        // SAFETY: `fds` is a valid array for the duration of the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };
        if n <= 0 {
            continue;
        }
        const EV: usize = std::mem::size_of::<libc::input_event>();
        let mut buf = [0u8; EV * 64];
        let mut gone = Vec::new();
        let mut escape = false;
        for (i, (pfd, h)) in fds.iter().zip(held.iter_mut()).enumerate() {
            if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                gone.push(i);
                continue;
            }
            if pfd.revents & libc::POLLIN == 0 {
                continue;
            }
            loop {
                match h.file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(len) => {
                        if !h.node.keyboard {
                            continue;
                        }
                        for chunk in buf[..len].chunks_exact(EV) {
                            // SAFETY: the kernel writes whole input_event structs.
                            let ev: libc::input_event = unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
                            if ev.type_ != EV_KEY {
                                continue;
                            }
                            match ev.value {
                                0 => {
                                    down.remove(&ev.code);
                                }
                                1 => {
                                    down.insert(ev.code);
                                    let held_any = |ks: [u16; 2]| ks.iter().any(|k| down.contains(k));
                                    if ev.code == KEY_ESC && held_any(CTRL) && held_any(ALT) && held_any(SHIFT) {
                                        escape = true;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        gone.push(i);
                        break;
                    }
                }
            }
        }
        if escape {
            paused = false;
            held.clear();
            down.clear();
            say("escape");
            continue;
        }
        for i in gone.into_iter().rev() {
            held.remove(i);
        }
    }
}
