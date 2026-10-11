# KVM Bridge for Linux

The Linux version of KVM Bridge, for a GNOME 45 or newer session on Wayland,
such as Ubuntu 24.04 or Fedora. What KVM Bridge does and how to use it is described in the
[main README](../README.md).

This folder also holds the parts shared with the Windows version: the engine
(`crates/core`) and the interface (`ui`).

## Installing

Download the package for your system from the
[latest release](https://github.com/ShehryarUsman007/KVMBridge/releases/latest):

| System             | File                                  | Install                                |
|--------------------|---------------------------------------|----------------------------------------|
| Ubuntu, Debian     | `KVM Bridge_<version>_amd64.deb`      | `sudo apt install ./KVM\ Bridge_*.deb` |
| Fedora, openSUSE   | `KVM Bridge-<version>-1.x86_64.rpm`   | `sudo dnf install ./KVM\ Bridge-*.rpm` |
| Other Linux        | `KVM Bridge_<version>_amd64.AppImage` | make it executable and run it          |

On Fedora, opening the rpm in Software works too. After updating, quit KVM
Bridge from the tray menu and start it again.

Uninstalling (`sudo apt remove kvm-bridge` on Ubuntu) keeps the settings,
pairings and saved configs in `~/.config/kvmbridge`.

### Firewall

The computers find each other on UDP port 47800 and connect on TCP port
47801. The packages include firewall rules for KVM Bridge. Fedora
Workstation already allows these ports, and Ubuntu's firewall is off unless
you switched it on. If it is on, allow KVM Bridge on the local network
(replace `192.168.1.0/24` with your network):

```bash
sudo ufw allow from 192.168.1.0/24 to any app "KVM Bridge"
```

On Fedora with firewalld set to a stricter zone:

```bash
sudo firewall-cmd --permanent --add-service=kvmbridge && sudo firewall-cmd --reload
```

## First start

GNOME asks for up to two permissions:

1. **Input capture** lets this computer's keyboard and mouse move on to other
   screens. Only the controlling computer asks for it, once a screen is
   placed next to it. GNOME 46 can't remember this permission, so it asks
   again each time KVM Bridge starts, and when a screen is placed on a side
   that had none before.
2. **Remote desktop** lets other computers move the cursor and type here.
   GNOME remembers this one.

If you dismiss one, open **Status** and click **Retry permissions**.

## Getting your keyboard and mouse back

* `Ctrl+Alt+Shift+Esc` brings the cursor back to this computer. On a
  computer whose keyboard and mouse are paused, it gives them back and
  switches KVM Bridge off there.
* `Super+Shift+Esc` is GNOME's own shortcut for ending input capture. GNOME
  handles it even if KVM Bridge doesn't respond, so use it if the keyboard
  and mouse ever stop responding. Afterwards, click **Retry permissions** on
  **Status**.
* Quitting KVM Bridge always gives everything back.

### Pausing this computer's keyboard and mouse

While another computer controls the screens, this computer's keyboards,
mice, touchpads and touchscreens are paused. This uses a small helper,
`kvmbridge-input-guard`, which runs with administrator rights through
polkit and holds the input devices meanwhile. It only discards their input
and never passes key presses on. The deb and rpm packages install it; the
AppImage asks for your password once to set it up (**Set up** in the
banner).

## Things to know

* While another computer has the cursor, the screen you left keeps showing
  the pointer where it crossed: GNOME holds it there and doesn't let
  applications hide it. A computer that is being controlled tucks its own
  pointer into the bottom-right corner.
* Dragging files across a screen edge works in both directions. The files
  land wherever you let go: in a folder, in an application, or on the
  desktop if it shows icons. Letting go over something that takes no files,
  or over a panel or dock, does nothing. **Status → Drag and drop → Test**
  tries it on one computer.
* Settings live in `~/.config/kvmbridge`, saved configs in
  `~/.config/kvmbridge/configs`, logs in `~/.cache/kvmbridge/logs`.

## Building from source

You need Rust (stable), Node.js 20 or newer, and the WebKitGTK 4.1 and GTK 3
development packages. On Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

Then, in this folder:

```bash
npm install
npm run tauri dev                           # run in development mode
npx tauri build --bundles deb,rpm,appimage  # packages in target/release/bundle
```

`npm run dev` shows the interface in a browser with made-up data, without
the desktop app.

## Code

| Path               | What                                                                |
|--------------------|---------------------------------------------------------------------|
| `crates/core`      | Engine, shared with Windows: discovery, encryption, pairing, input sharing, clipboard, transfers, setups and saved configs |
| `crates/platform`  | Linux backends: input capture, remote desktop and clipboard through the GNOME portals, keyboard and mouse pausing |
| `src-tauri`        | The desktop app: window, tray, drag and drop (GTK), commands. Windows reuses some of these files |
| `ui`               | The interface (React and TypeScript), shared with Windows           |
| `packaging`        | Icons, desktop entry, polkit policy, firewall rules                 |
| `docs`             | The `.kvmb` file format                                             |

A change in `crates/core`, `ui` or the shared `src-tauri` files also changes
the Windows version, so build both.
