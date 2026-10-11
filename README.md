# KVM Bridge

Use one keyboard and mouse across several computers on the same network.
Move the cursor off the edge of one screen and it carries on onto the next
computer. The clipboard and files come along, and every connection is
encrypted.

KVM Bridge runs on Linux (GNOME on Wayland) and Windows, and the two work
together.

## What it does

* **One keyboard and mouse for all screens.** Arrange the screens on a grid
  to match your desk, and choose the computer whose keyboard and mouse
  control them. The other computers' own keyboards and mice are paused
  meanwhile, so they cannot get in the way.
* **Shared clipboard.** Copy on one computer and paste on another: text, rich
  text, images and files.
* **Files.** Drag files across a screen edge and drop them on the other
  computer, or send them from the **Transfers** page. Files are always
  copied; the originals are never touched.
* **Setups.** The arrangement, the controlling computer and the shared
  settings are kept on every computer, and a change on one reaches the
  others right away. Pairing with one computer of a setup pairs with all of
  them; unpairing a computer anywhere unpairs it everywhere.
* **Saved configs.** Save a setup under a name and load it again later, on
  all its computers at once.
* **Encryption.** AES-256-GCM or ChaCha20-Poly1305 over the Noise protocol,
  checked with a 6-digit code when pairing.
* Up to 5 computers, found on the local network or added by address.

## Download

Get the files for your system from the
[latest release](https://github.com/ShehryarUsman007/KVMBridge/releases/latest).
Each one includes everything KVM Bridge needs.

| System                 | File                                | Install                                |
|------------------------|-------------------------------------|----------------------------------------|
| Windows 10 and 11      | `KVM Bridge_<version>_x64-setup.exe` | run it                                |
| Ubuntu, Debian         | `KVM Bridge_<version>_amd64.deb`    | `sudo apt install ./KVM\ Bridge_*.deb` |
| Fedora, openSUSE       | `KVM Bridge-<version>-1.x86_64.rpm` | `sudo dnf install ./KVM\ Bridge-*.rpm` |
| Other Linux            | `KVM Bridge_<version>_amd64.AppImage` | make it executable and run it        |

Install it on every computer that should take part. The Linux version needs
GNOME on Wayland; see [Linux/README.md](Linux/README.md) and
[Windows/README.md](Windows/README.md) for what each system needs and asks
for on first start.

## Getting started

1. **Pair.** Open **Devices** and click **Search**. Click **Pair** next to
   another computer, check that both show the same 6-digit code, and confirm
   on both. The computer where you clicked **Pair** chooses whose settings
   both keep. If a computer doesn't show up, add it by its IP address.
2. **Arrange.** On **Screens**, drag the screens to match where they stand.
   A computer with several monitors counts as one screen.
3. **Choose the controlling computer.** Under **Keyboard and mouse**, pick
   the computer whose keyboard and mouse you use.

Then move the cursor off the edge that leads to the other screen.

To get a paused keyboard and mouse back, or to take input back from KVM
Bridge, press **`Ctrl+Alt+Shift+Esc`** on that computer's keyboard.

## Using it

### Shared settings

Encryption, update intervals, clipboard, drag and drop and shortcuts are
the same on every paired computer: change them on one and the others follow.
A computer that was offline catches up when it reconnects. A computer's
name, ports, folder for received files and window preferences stay its own.

### Setups

The computers in one arrangement form a setup. Pairing a new computer with
any of them pairs it with all of them, without asking again. It takes the
settings and arrangement of the computer chosen when pairing; if that is the
new computer, the rest of the setup switches to its settings. Unpairing a
computer on any of them unpairs it from all of them.

### Saved configs

**Configs** keeps named setups: the arrangement, the controlling computer,
encryption and the shared settings. **Save current setup…** stores it on
every paired computer, and **Load** makes it the current setup on all of its
computers. The top bar shows which config is in use. Configs are `.kvmb`
files in the `configs` folder next to the settings; the page shows where.

### The on/off switch

The switch in the top bar (also in the tray menu) turns KVM Bridge off on
that computer only. Its keyboard and mouse work normally again, and the
cursor skips its screen. **Send files** still works while it is off.

### Backup

**Settings → Backup and restore** saves the settings, the arrangement and
the paired computers to a `.kvmb` file, optionally password protected, and
restores them, for example on a new computer. The format is described in
[Linux/docs/config-file-format.md](Linux/docs/config-file-format.md).

## Network

The computers find each other on UDP port 47800 and connect on TCP port
47801. KVM Bridge only searches when you click **Search**; paired computers
find each other again on their own. If a computer doesn't show up, a
firewall is the usual cause: see the README for its system.

## Security

* Connections use the Noise XX handshake (X25519, BLAKE2s) with AES-256-GCM
  or ChaCha20-Poly1305. The 6-digit pairing code is derived from the
  handshake, so matching codes mean nobody is in between.
* Only paired computers can control a computer, read its clipboard or send
  it files. Received files go to `Downloads/KVMBridge` (configurable).

## Repository

| Folder               | What                                                         |
|----------------------|--------------------------------------------------------------|
| [`Linux`](Linux)     | The shared engine and interface, and the Linux app           |
| [`Windows`](Windows) | The Windows app, built on the engine and interface in `Linux` |

Both versions always have the same version number. Pushing a tag
`v<version>` builds every package and attaches them to one draft release
(`.github/workflows/build.yml`).

## License

Apache License 2.0, see [LICENSE](LICENSE).
