# KVM Bridge

Share one keyboard and mouse between computers on the same network. Move the
cursor off the edge of one screen and it continues on the next computer;
the clipboard and files come along, and everything is encrypted.

* Finds other computers running KVM Bridge on the local network (up to 5
  computers, including this one).
* Screens are arranged on a grid, like the display settings of an operating
  system. **Identify** shows each screen's number on that screen.
* One computer's keyboard and mouse control all screens. The other
  computers' own keyboards and mice are paused meanwhile, so they cannot
  get in the way.
* Shared clipboard (text, HTML, images and files), file transfers, and
  dragging files from one screen to another.
* Saved configs: every computer in a config keeps a copy, and switching on
  one computer asks the others to load it and switch on too.
* End-to-end encryption with AES-256-GCM or ChaCha20-Poly1305 (Noise
  protocol, verified with a 6-digit code when pairing).
* Adjustable update intervals, to trade latency against CPU and network use.

This folder is the Linux version: a Wayland session on GNOME 45+ (Ubuntu
24.04, Fedora 39+) or KDE Plasma 6.1+. The Windows version is in progress and
will live in its own folder.

## Installing

Download the package for your system. Each one includes everything KVM Bridge
needs.

| System                | File                         | Install                                   |
|-----------------------|------------------------------|-------------------------------------------|
| Ubuntu, Debian        | `KVM Bridge_<ver>_amd64.deb` | `sudo apt install ./KVM\ Bridge_*.deb`    |
| Fedora, openSUSE      | `KVM Bridge-<ver>-1.x86_64.rpm` | `sudo dnf install ./KVM\ Bridge-*.rpm` |
| Any other Linux       | `KVM Bridge_<ver>_amd64.AppImage` | make it executable and run it        |

Install KVM Bridge on every computer that should take part.

The computers talk over UDP port 47800 (discovery) and TCP port 47801
(connections). Ubuntu and Fedora Workstation allow these by default. If you
run a firewall that blocks them, allow both ports on the local network.

## First start

GNOME asks for two permissions the first time:

1. **Input capture**: lets this computer's keyboard and mouse move to other
   screens. GNOME asks again on every start.
2. **Remote desktop**: lets other computers move the cursor and type here.
   KVM Bridge remembers this one.

If you dismiss one, open **Status** and click **Retry permissions**.

To take the keyboard and mouse back from KVM Bridge at any time, press
`Ctrl+Alt+Shift+Esc`. GNOME's own shortcut for ending input capture,
`Super+Shift+Esc`, works too.

## Using it

1. **Pair.** On **Devices**, nearby computers appear with their names. Click
   **Pair** on one, check that both computers show the same 6-digit code, and
   confirm on both. If a computer does not show up, add it by IP address.
2. **Arrange.** On **Screens**, drag the screens to match your desk. A
   computer with several monitors is one screen: its own system moves the
   cursor between its monitors.
3. **Choose the controlling computer.** Under **Keyboard and mouse**, pick
   the computer whose keyboard and mouse control everything. While it is
   connected, the other computers pause their own keyboards and mice.
4. **Save a config.** On **Configs**, choose **Save current setup…**. The
   config is copied to every computer in it.

### The on/off switch

The switch in the top bar (also in the tray menu) turns KVM Bridge off on that
computer only. A computer that is off keeps running in the background:

* If it was being controlled, the cursor moves to the nearest screen that is
  still on. Screens that are off are skipped when moving the cursor.
* Its own keyboard and mouse work normally again.
* Switching any computer of a config on asks the others to load that config
  and switch on as well.

### Paused keyboards and mice

When another computer controls the screens, a computer's own keyboards,
mice, touchpads and touchscreens are paused. To use them again, press
**`Ctrl+Alt+Shift+Esc` on that computer's keyboard**. This gives them back
and switches KVM Bridge off there.

Pausing uses a small helper, `kvmbridge-input-guard`, which runs with
administrator rights (through polkit) and holds the input devices while
another computer controls this one. It only discards their input and never
passes key presses on. The deb and rpm packages install it. The AppImage
asks once for your password to set it up (**Set up** in the banner).

### Configs

Configs are saved in `~/.config/kvmbridge/configs/`. The Configs page shows
the folder. A config holds:

* the screen arrangement and the controlling computer;
* the encryption choice;
* the computers in it, with their keys, so they trust each other;
* this computer's own settings.

**Load** applies a config here and on its other computers. **Delete**
removes it everywhere. Configs can also be copied as files (`.kvmb`), for
example to a computer that has not been paired yet. The format is described
in [docs/config-file-format.md](docs/config-file-format.md).

### Clipboard, files and drag and drop

* Copying on one computer and pasting on another works for text, rich text,
  images and files. Large items are transferred when they are pasted.
* **Transfers** sends files or folders to another computer, by drag and drop
  into the window or with the file picker.
* Dragging files across a screen edge drops them on the other computer, on
  the desktop, in a folder or into an application. This is new and
  experimental. **Status → Drag and drop → Test** tries it on one computer.

## Settings

* **Encryption**: AES-256-GCM (fastest with hardware AES) or
  ChaCha20-Poly1305 (fast everywhere). **Automatic** picks AES when the CPU
  supports it. All paired computers use the same choice.
* **Update interval**: presets from *Lowest latency* to *Power saver*, or
  custom values for motion, heartbeat, discovery and clipboard checks.
* **Shortcuts**: bring the cursor back (default `Ctrl+Alt+Shift+Esc`), and
  lock the cursor to the current screen (default `Scroll Lock`).
* **Backup**: a full backup of this computer, optionally including its
  identity (password protected).

## Building from source

Requirements: Rust (stable), Node.js 20+, and on Linux the WebKitGTK 4.1 and
GTK 3 development packages.

Run these from this `Linux` folder:

```bash
npm install
npm run tauri dev                           # run in development mode
npx tauri build --bundles deb,rpm,appimage  # packages in target/release/bundle
```

Layout of the code:

| Path                 | What                                                            |
|----------------------|-----------------------------------------------------------------|
| `crates/core`        | Engine: discovery, encryption, pairing, input sharing, clipboard, transfers, configs |
| `crates/platform`    | OS backends: Wayland portals (input capture, remote desktop, clipboard), keyboard/mouse pausing |
| `src-tauri`          | Desktop app: window, tray, drag and drop (GTK), commands          |
| `ui`                 | Interface (React + TypeScript)                                   |
| `packaging`          | Icons, polkit policy                                             |

## Security

* Connections use the Noise XX handshake (X25519, BLAKE2s) with AES-256-GCM or
  ChaCha20-Poly1305. Pairing shows a 6-digit code derived from the handshake;
  confirm it matches on both screens.
* Only paired computers can control this one, read its clipboard or send it
  files. Received files go to `~/Downloads/KVMBridge` (configurable).
* The keyboard/mouse pause helper can be started without a password by the
  user at the computer (polkit `allow_active`). It can only pause and resume
  local input, and `Ctrl+Alt+Shift+Esc` on the paused keyboard always
  releases it.
