# KVM Bridge for Windows

The Windows version of KVM Bridge, for 64-bit Windows 10 and 11. It works
together with the Linux version. What KVM Bridge does and how to use it is described in
the [main README](../README.md).

## Installing

Download `KVM Bridge_<version>_x64-setup.exe` from the
[latest release](https://github.com/ShehryarUsman007/KVMBridge/releases/latest)
and run it. The installer is not code-signed, so Windows may warn about an
unknown publisher; choose **More info → Run anyway** to continue.

The installer:

* installs KVM Bridge for all users (it asks for administrator rights);
* installs Microsoft Edge WebView2 if it is missing;
* adds a Windows Firewall rule that lets KVM Bridge receive connections from
  the local network only (UDP port 47800 for finding computers, TCP port
  47801 for connections). Uninstalling removes the rule again.

To update, run the newer installer. To uninstall, use **Settings → Apps**.

## Getting your keyboard and mouse back

Press **`Ctrl+Alt+Shift+Esc`** on this computer's keyboard. It always frees
the keyboard and mouse here, even while the rest of KVM Bridge is busy, and
on a computer whose keyboard and mouse are paused it switches KVM Bridge off
there. Input is also given back when Windows locks or shows a sign-in or
security screen, when the monitors change, and when KVM Bridge quits.

While another computer controls the screens, this computer's own keyboard
and mouse are paused; no extra setup is needed for that on Windows.

## Things to know

* Dragging files across a screen edge works while another computer is in
  control. Dragging files while this Windows computer controls the others is
  not supported yet; copy and paste or **Send files** work instead.
* Settings, pairings and saved configs live in
  `%APPDATA%\kvmbridge\KVMBridge\config` (saved configs in its `configs`
  folder), logs in `%LOCALAPPDATA%\kvmbridge\KVMBridge\cache\logs`.

## Building from source

You need Rust (stable, MSVC), the Visual Studio Build Tools with the
"Desktop development with C++" workload, and Node.js 20 or newer.

The Windows app is built from this folder, but uses the engine and the
interface from the `Linux` folder, so install the packages of both:

```powershell
cd Linux;   npm ci
cd ..\Windows; npm ci
npx tauri dev     # run in development mode
npx tauri build   # installer in target\release\bundle\nsis
```

## Code

| Path                         | What                                                         |
|------------------------------|--------------------------------------------------------------|
| `crates/platform`            | Windows backends: keyboard and mouse hooks (sharing input and pausing it), `SendInput`, the clipboard, OLE drag and drop, monitors |
| `src-tauri`                  | The desktop app. It reuses the commands, tray, logging, overlay and backup files of `Linux/src-tauri` |
| `src-tauri/windows/hooks.nsh` | The installer's firewall rule                               |

The engine (`Linux/crates/core`) and the interface (`Linux/ui`) are shared
with Linux, so both versions always speak the same protocol and carry the
same version number.
