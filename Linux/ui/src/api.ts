// Typed bridge to the Rust backend. The types mirror the serde output of
// `kvmbridge_core::engine::view` and `kvmbridge_core::config`.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { mockInvoke } from "./mock";

/** True inside the desktop app; false when previewing in a browser. */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return inTauri ? tauriInvoke<T>(cmd, args) : (mockInvoke(cmd, args) as Promise<T>);
}

export type DeviceId = string;

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Desktop {
  monitors: Rect[];
}

export interface Slot {
  device: DeviceId;
  col: number;
  row: number;
  number: number;
  name: string;
}

export interface Layout {
  rev: number;
  author: DeviceId;
  slots: Slot[];
  /** Computer whose keyboard and mouse control all screens (null: any). */
  controller: DeviceId | null;
}

export type CipherPreference = "auto" | "aes-256-gcm" | "chacha20-poly1305";
export type Cipher = "aes-256-gcm" | "chacha20-poly1305";

export interface Timing {
  motion_interval_us: number;
  heartbeat_ms: number;
  heartbeat_misses: number;
  discovery_ms: number;
  clipboard_poll_ms: number;
}

export interface Settings {
  device_name: string;
  listen_port: number;
  discovery_port: number;
  discovery_enabled: boolean;
  manual_peers: string[];
  timing: Timing;
  clipboard: { enabled: boolean; files: boolean; max_bytes: number };
  transfers: { receive_dir: string | null; drag_and_drop: boolean };
  hotkeys: { release: number[]; lock: number[] };
  strict_cipher: boolean;
  start_minimized: boolean;
  close_to_background: boolean;
  appearance: "system" | "light" | "dark";
}

/** The parts of the settings that all paired computers share. */
export interface CommonSettings {
  strict_cipher: boolean;
  timing: Timing;
  clipboard: Settings["clipboard"];
  drag_and_drop: boolean;
  hotkeys: Settings["hotkeys"];
}

/** Settings that are the same on all paired computers (newest version wins). */
export interface SharedSettings {
  rev: number;
  author: DeviceId;
  cipher: CipherPreference;
  common: CommonSettings;
}

/** Pausing of this computer's own keyboard and mouse. */
export type GuardStatus =
  | { state: "off" }
  | { state: "paused"; detail: number }
  | { state: "needs_setup" }
  | { state: "unavailable"; detail: string }
  | { state: "escaped" };

export type BackendStatus =
  | { state: "idle" }
  | { state: "starting"; detail: string }
  | { state: "ready"; detail: string }
  | { state: "unavailable"; detail: string };

export interface Capabilities {
  capture: boolean;
  emulation: boolean;
  clipboard: boolean;
  drag_and_drop: boolean;
  /** KVM Bridge is switched on there. */
  enabled: boolean;
}

export type PeerStatus = "discovered" | "connecting" | "pairing" | "connected" | "offline";

export interface PeerView {
  id: DeviceId;
  name: string;
  os: string;
  address: string | null;
  fingerprint: string;
  status: PeerStatus;
  trusted: boolean;
  rtt_ms: number | null;
  cipher: Cipher | null;
  pairing: { sas: string; incoming: boolean; local_accepted: boolean; remote_accepted: boolean } | null;
  desktop: Desktop | null;
  caps: Capabilities;
  error: string | null;
  last_seen_ms: number | null;
}

export type InputView =
  | { mode: "local"; locked: boolean }
  | { mode: "controlling"; target: DeviceId; locked: boolean }
  | { mode: "controlled"; by: DeviceId };

export interface ItemMeta {
  rel_path: string;
  size: number;
  is_dir: boolean;
  mtime: number | null;
}

export interface TransferView {
  id: number;
  peer: DeviceId;
  outgoing: boolean;
  kind: "manual" | "drag" | "clipboard";
  state: "preparing" | "waiting" | "running" | "done" | "failed" | "cancelled";
  items: ItemMeta[];
  file_count: number;
  bytes_done: number;
  bytes_total: number;
  rate_bps: number;
  error: string | null;
  paths: string[];
}

export interface Snapshot {
  me: {
    id: DeviceId;
    name: string;
    os: string;
    fingerprint: string;
    port: number;
    addresses: string[];
    version: string;
  };
  peers: PeerView[];
  layout: Layout;
  settings: Settings;
  shared: SharedSettings;
  effective_cipher: Cipher;
  hw_aes: boolean;
  input: InputView;
  transfers: TransferView[];
  backends: { capture: BackendStatus; emulation: BackendStatus; clipboard: BackendStatus; dnd: BackendStatus };
  desktop: Desktop | null;
  enabled: boolean;
  guard: GuardStatus;
  max_devices: number;
  /** A search for devices on the network is running. */
  searching: boolean;
}

export type Notice =
  | { kind: "identify"; number: number; name: string }
  | { kind: "pairing_request"; device: DeviceId; name: string; sas: string }
  | { kind: "paired"; device: DeviceId; name: string }
  | { kind: "pairing_failed"; device: DeviceId; name: string; reason: string }
  | {
      kind: "transfer_finished";
      id: number;
      peer_name: string;
      outgoing: boolean;
      ok: boolean;
      error: string | null;
      paths: string[];
    }
  | { kind: "error"; message: string }
  | { kind: "info"; message: string };

export interface ExportOptions {
  settings: boolean;
  layout: boolean;
  devices: boolean;
  identity: boolean;
  password: string | null;
  note: string;
}

export interface ImportOptions {
  settings: boolean;
  layout: boolean;
  devices: boolean;
  identity: boolean;
}

export interface Inspection {
  encrypted: boolean;
  needs_password: boolean;
  has_identity: boolean;
  info: { created_ms: number; app_version: string; source_id: string; source_name: string; source_os: string; note: string } | null;
  has_settings: boolean;
  has_shared: boolean;
  layout_screens: number | null;
  devices: { id: string; name: string }[];
  from_this_device: boolean;
}

export interface AppInfo {
  version: string;
  config_dir: string;
  log_dir: string;
  os: string;
  session: string;
}

export const api = {
  snapshot: () => invoke<Snapshot>("get_snapshot"),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  setCipher: (cipher: CipherPreference) => invoke<void>("set_cipher", { cipher }),
  setController: (device: DeviceId | null) => invoke<void>("set_controller", { device }),
  setEnabled: (enabled: boolean) => invoke<void>("set_enabled", { enabled }),
  setupInputPause: () => invoke<void>("setup_input_pause"),
  retryInputPause: () => invoke<void>("retry_input_pause"),
  testDrag: () => invoke<void>("test_drag"),
  setLayout: (layout: Layout) => invoke<void>("set_layout", { layout }),
  pair: (device: DeviceId) => invoke<void>("pair", { device }),
  acceptPair: (device: DeviceId) => invoke<void>("accept_pair", { device }),
  rejectPair: (device: DeviceId) => invoke<void>("reject_pair", { device }),
  unpair: (device: DeviceId) => invoke<void>("unpair", { device }),
  connectAddress: (address: string) => invoke<void>("connect_address", { address }),
  identify: () => invoke<void>("identify"),
  search: () => invoke<void>("search"),
  sendFiles: (device: DeviceId, paths: string[]) => invoke<void>("send_files", { device, paths }),
  cancelTransfer: (id: number) => invoke<void>("cancel_transfer", { id }),
  releaseControl: () => invoke<void>("release_control"),
  setLocked: (locked: boolean) => invoke<void>("set_locked", { locked }),
  restartBackends: () => invoke<void>("restart_backends"),
  logs: () => invoke<string[]>("get_logs"),
  receiveDir: () => invoke<string>("receive_dir"),
  openReceiveDir: () => invoke<void>("open_receive_dir"),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
  appInfo: () => invoke<AppInfo>("app_info"),
  quit: () => invoke<void>("quit"),
  configExport: (path: string, options: ExportOptions) => invoke<string>("config_export", { path, options }),
  configInspect: (path: string, password: string | null) => invoke<Inspection>("config_inspect", { path, password }),
  configImport: (path: string, password: string | null, options: ImportOptions) =>
    invoke<{ restarting: boolean }>("config_import", { path, password, options }),
  pendingConfigFile: () => invoke<string | null>("pending_config_file"),
};

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
