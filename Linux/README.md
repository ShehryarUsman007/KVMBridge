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
* One setup for all computers: the screen arrangement, the controlling
  computer and the shared settings are kept on every paired computer, and a
  change on one is sent to the others right away. Pairing with one computer
  of a setup pairs with all of them.
* Saved configs: name a setup, keep it on every paired computer and load it
  again in one click.
* End-to-end encryption with AES-256-GCM or ChaCha20-Poly1305 (Noise
  protocol, verified with a 6-digit code when pairing).
* Adjustable update intervals, to trade latency against CPU and network use.

This folder is the Linux version, for a GNOME 45+ session on Wayland. It has
been tested on Ubuntu 24.04 and Fedora with GNOME; other desktops have not
been tried. The Windows version is in progress and will live in its own
folder.

## Installing

Download the package for your system. Each one includes everything KVM Bridge
needs.

| System                | File                         | Install                                   |
|-----------------------|------------------------------|-------------------------------------------|
| Ubuntu, Debian        | `KVM Bridge_<ver>_amd64.deb` | `sudo apt install ./KVM\ Bridge_*.deb`    |
| Fedora, openSUSE      | `KVM Bridge-<ver>-1.x86_64.rpm` | `sudo dnf install ./KVM\ Bridge-*.rpm` |
| Any other Linux       | `KVM Bridge_<ver>_amd64.AppImage` | make it executable and run it        |

Install KVM Bridge on every computer that should take part.

The computers talk over UDP port 47800 (finding each other) and TCP port 47801
(connections). If a computer does not show up, a firewall is the usual cause.
The packages include firewall rules for KVM Bridge; allow them on the local
network (replace `192.168.1.0/24` with your network):

```bash
sudo ufw allow from 192.168.1.0/24 to any app "KVM Bridge"     # Ubuntu, Debian (ufw)
sudo firewall-cmd --permanent --add-service=kvmbridge && sudo firewall-cmd --reload   # Fedora (firewalld)
```

Fedora Workstation already allows these ports; Ubuntu's firewall is off unless
you switched it on.

## First start

GNOME asks for up to two permissions:

1. **Input capture**: lets this computer's keyboard and mouse move to other
   screens. Only the computer whose keyboard and mouse are used asks for it,
   once a screen is placed next to it. GNOME 46 cannot remember this
   permission, so it is asked again each time KVM Bridge starts, and when a
   screen is moved to a side of this one that had no screen before.
2. **Remote desktop**: lets other computers move the cursor and type here.
   KVM Bridge remembers this one.

If you dismiss one, open **Status** and click **Retry permissions**.

To take the keyboard and mouse back from KVM Bridge at any time, press
`Ctrl+Alt+Shift+Esc`. GNOME's own shortcut for ending input capture,
`Super+Shift+Esc`, works too, but it switches input capture off until you
click **Retry** on **Status**. If this computer's keyboard and mouse ever stop
responding altogether, `Super+Shift+Esc` is the one to press: GNOME handles it
itself, even when KVM Bridge cannot.

While another computer has the cursor, the screen you left keeps showing the
pointer where it crossed: GNOME holds it there and does not let applications
hide it. A computer that is being controlled tucks its own pointer into the
bottom-right corner when the cursor moves on.

## Using it

1. **Pair.** On **Devices**, click **Search** to find the computers on your
   network running KVM Bridge. Click **Pair** on one, check that both
   computers show the same 6-digit code, and confirm on both. The computer
   where you clicked **Pair** chooses whose settings both keep. If a computer
   does not show up, add it by IP address. KVM Bridge only searches when you
   click **Search**; paired computers find each other again on their own.
2. **Arrange.** On **Screens**, drag the screens to match your desk. A
   computer with several monitors is one screen: its own system moves the
   cursor between its monitors.
3. **Choose the controlling computer.** Under **Keyboard and mouse**, pick
   the computer whose keyboard and mouse control everything. While it is
   connected, the other computers pause their own keyboards and mice.

The arrangement, the controlling computer and the settings marked "all
computers" (encryption, update intervals, clipboard, drag and drop and
shortcuts) are stored on every paired computer. A change on one computer is
sent to the connected ones at once; a computer that was offline catches up
when it connects, and the newest change wins. A computer's name, ports,
folder for received files and window preferences stay its own.

### Setups

The computers in one arrangement form a setup. Pairing a computer with any
of them pairs it with all of them, without asking again. Its settings and
arrangement come from the computer chosen when pairing; when that computer
was already part of a setup, the others switch too. Unpairing a computer on
any of them unpairs it from all of them (computers that were offline follow
when they reconnect).

### Saved configs

**Configs** keeps named setups: the screen arrangement, the controlling
computer, encryption and the settings shared by all computers. **Save
current setup…** stores it on every paired computer, in the `configs`
folder next to the settings (`~/.config/kvmbridge/configs`). **Load** makes
it the current setup on all of its computers. Renaming and deleting reach the
other computers too. A `.kvmb` config file can also be added from elsewhere.

### The on/off switch

The switch in the top bar (also in the tray menu) turns KVM Bridge off on that
computer only. A computer that is off keeps running in the background:

* If it was being controlled, the cursor moves to the nearest screen that is
  still on. Screens that are off are skipped when moving the cursor.
* Its own keyboard and mouse work normally again.

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

### Backup

**Settings → Backup and restore** saves the settings, the arrangement and the
paired devices to a `.kvmb` file (optionally password protected) and restores
them, for example on a new computer. The format is described in
[docs/config-file-format.md](docs/config-file-format.md).

### Clipboard, files and drag and drop

* Copying on one computer and pasting on another works for text, rich text,
  images and files. Large items are transferred when they are pasted.
* **Transfers** sends files or folders to another computer, by drag and drop
  into the window or with the file picker. Files are always sent as copies:
  the originals are never changed or removed.
* Files can be dragged across a screen edge in both directions, from the
  computer whose keyboard and mouse are used to another one and back.
  Copies of them land wherever you let go: in a folder, in an application,
  or on the desktop if it shows icons. As with any drag there, letting go
  over something that takes no files (or over a panel or dock) does
  nothing. This is new and experimental. **Status → Drag and drop → Test**
  tries it on one computer.

## Settings

* **Encryption**: AES-256-GCM (fastest with hardware AES) or
  ChaCha20-Poly1305 (fast everywhere). **Automatic** picks AES when the CPU
  supports it. All paired computers use the same choice.
* **Update interval**: presets from *Lowest latency* to *Power saver*, or
  custom values for motion, heartbeat and clipboard checks.
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
| `crates/core`        | Engine: discovery, encryption, pairing, input sharing, clipboard, transfers, settings sync |
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
