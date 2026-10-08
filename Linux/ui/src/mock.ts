// Fake backend used when the UI runs in a plain browser (`npm run dev`), so
// the interface can be developed and previewed without the desktop app.

import type { Snapshot } from "./api";

const me = "11111111-1111-4111-8111-111111111111";
const laptop = "22222222-2222-4222-8222-222222222222";
const office = "33333333-3333-4333-8333-333333333333";
const nearby = "44444444-4444-4444-8444-444444444444";

let snap: Snapshot = {
  me: {
    id: me,
    name: "desk-pc",
    os: "linux",
    fingerprint: "A1B2-C3D4-E5F6-0718",
    port: 47801,
    addresses: ["192.168.2.46"],
    version: "0.1.0",
  },
  peers: [
    {
      id: laptop,
      name: "fedora-laptop",
      os: "linux",
      address: "192.168.2.61",
      fingerprint: "9F3E-11AA-42BC-7D20",
      status: "connected",
      trusted: true,
      rtt_ms: 1.8,
      cipher: "aes-256-gcm",
      pairing: null,
      desktop: { monitors: [{ x: 0, y: 0, w: 1920, h: 1200 }] },
      caps: { capture: true, emulation: true, clipboard: true, drag_and_drop: true, enabled: true },
      config: "Desk",
      error: null,
      last_seen_ms: 400,
    },
    {
      id: office,
      name: "office-win",
      os: "windows",
      address: "192.168.2.70",
      fingerprint: "77AC-0E91-B2D4-5531",
      status: "offline",
      trusted: true,
      rtt_ms: null,
      cipher: null,
      pairing: null,
      desktop: { monitors: [{ x: 0, y: 0, w: 2560, h: 1440 }] },
      caps: { capture: false, emulation: false, clipboard: false, drag_and_drop: false, enabled: false },
      config: null,
      error: null,
      last_seen_ms: null,
    },
    {
      id: nearby,
      name: "media-box",
      os: "linux",
      address: "192.168.2.80",
      fingerprint: "0C4F-9921-AB3E-66D0",
      status: "discovered",
      trusted: false,
      rtt_ms: null,
      cipher: null,
      pairing: null,
      desktop: null,
      caps: { capture: false, emulation: false, clipboard: false, drag_and_drop: false, enabled: false },
      config: null,
      error: null,
      last_seen_ms: 900,
    },
  ],
  layout: {
    rev: 3,
    author: me,
    slots: [
      { device: me, col: 0, row: 0, number: 1, name: "desk-pc" },
      { device: laptop, col: 1, row: 0, number: 2, name: "fedora-laptop" },
    ],
    controller: me,
  },
  settings: {
    device_name: "desk-pc",
    listen_port: 47801,
    discovery_port: 47800,
    discovery_enabled: true,
    manual_peers: [],
    timing: { motion_interval_us: 4000, heartbeat_ms: 1000, heartbeat_misses: 5, discovery_ms: 2000, clipboard_poll_ms: 500 },
    clipboard: { enabled: true, files: true, max_bytes: 64 * 1024 * 1024 },
    transfers: { receive_dir: null, drag_and_drop: true },
    hotkeys: { release: [29, 56, 42, 1], lock: [70] },
    strict_cipher: false,
    start_minimized: false,
    close_to_background: true,
    appearance: "system",
  },
  shared: { rev: 1, author: me, cipher: "auto" },
  effective_cipher: "aes-256-gcm",
  hw_aes: true,
  input: { mode: "local", locked: false },
  transfers: [
    {
      id: 7,
      peer: laptop,
      outgoing: true,
      kind: "drag",
      state: "running",
      items: [{ rel_path: "holiday-photos", size: 0, is_dir: true, mtime: null }],
      file_count: 214,
      bytes_done: 734003200,
      bytes_total: 1288490188,
      rate_bps: 94371840,
      error: null,
      paths: [],
    },
    {
      id: 6,
      peer: laptop,
      outgoing: false,
      kind: "manual",
      state: "done",
      items: [{ rel_path: "report.pdf", size: 2400000, is_dir: false, mtime: null }],
      file_count: 1,
      bytes_done: 2400000,
      bytes_total: 2400000,
      rate_bps: 0,
      error: null,
      paths: ["/home/max/Downloads/KVMBridge/report.pdf"],
    },
  ],
  backends: {
    capture: { state: "ready", detail: "InputCapture portal" },
    emulation: { state: "ready", detail: "RemoteDesktop portal (absolute)" },
    clipboard: { state: "ready", detail: "Clipboard portal" },
    dnd: { state: "unavailable", detail: "Drag and drop between screens is not implemented yet" },
  },
  desktop: { monitors: [{ x: 0, y: 0, w: 2560, h: 1440 }, { x: 2560, y: 180, w: 1920, h: 1080 }] },
  enabled: true,
  active_config: { id: "c1", name: "Desk", modified: false, saved: true },
  configs: [
    {
      id: "c1",
      name: "Desk",
      saved_ms: Date.now() - 3 * 3600_000,
      saved_by: "desk-pc",
      members: [
        { id: me, name: "desk-pc" },
        { id: laptop, name: "fedora-laptop" },
      ],
      controller: me,
      file: "/home/max/.config/kvmbridge/configs/Desk.kvmb",
      includes_me: true,
    },
    {
      id: "c2",
      name: "Office",
      saved_ms: Date.now() - 6 * 86400_000,
      saved_by: "office-win",
      members: [
        { id: me, name: "desk-pc" },
        { id: office, name: "office-win" },
      ],
      controller: office,
      file: "/home/max/.config/kvmbridge/configs/Office.kvmb",
      includes_me: true,
    },
  ],
  configs_dir: "/home/max/.config/kvmbridge/configs",
  guard: { state: "off" },
  max_devices: 5,
  searching: false,
};

type Listener = (s: Snapshot) => void;
const listeners = new Set<Listener>();
export function onMockSnapshot(l: Listener) {
  listeners.add(l);
  return () => listeners.delete(l);
}
function update(f: (s: Snapshot) => Snapshot) {
  snap = f(structuredClone(snap));
  listeners.forEach((l) => l(snap));
}

export async function mockInvoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 60));
  switch (cmd) {
    case "get_snapshot":
      return snap;
    case "search":
      update((s) => ({ ...s, searching: true }));
      setTimeout(() => update((s) => ({ ...s, searching: false })), 4000);
      return;
    case "set_layout":
      update((s) => {
        const l = args.layout as Snapshot["layout"];
        const minC = Math.min(...l.slots.map((x) => x.col));
        const minR = Math.min(...l.slots.map((x) => x.row));
        const used = new Set<number>();
        const slots = l.slots.map((x) => ({ ...x, col: x.col - minC, row: x.row - minR }));
        for (const x of slots) if (x.number) used.add(x.number);
        for (const x of slots)
          if (!x.number) {
            let n = 1;
            while (used.has(n)) n++;
            used.add(n);
            x.number = n;
          }
        return { ...s, layout: { rev: s.layout.rev + 1, author: me, slots, controller: l.controller } };
      });
      return null;
    case "set_settings":
      update((s) => ({ ...s, settings: args.settings as Snapshot["settings"], me: { ...s.me, name: (args.settings as Snapshot["settings"]).device_name } }));
      return null;
    case "set_cipher":
      update((s) => ({ ...s, shared: { ...s.shared, rev: s.shared.rev + 1, cipher: args.cipher as Snapshot["shared"]["cipher"] } }));
      return null;
    case "set_controller":
      update((s) => ({ ...s, layout: { ...s.layout, rev: s.layout.rev + 1, controller: (args.device as string | null) ?? null } }));
      return null;
    case "set_enabled":
      update((s) => ({ ...s, enabled: Boolean(args.enabled) }));
      return null;
    case "save_config":
      update((s) => {
        const id = (args.id as string | null) ?? `c${Date.now()}`;
        const name = String(args.name);
        const view = {
          id,
          name,
          saved_ms: Date.now(),
          saved_by: s.me.name,
          members: s.layout.slots.map((x) => ({ id: x.device, name: x.name })),
          controller: s.layout.controller,
          file: `${s.configs_dir}/${name}.kvmb`,
          includes_me: true,
        };
        const configs = [...s.configs.filter((c) => c.id !== id), view].sort((a, b) => a.name.localeCompare(b.name));
        return { ...s, configs, active_config: { id, name, modified: false, saved: true } };
      });
      return null;
    case "load_config":
      update((s) => {
        const c = s.configs.find((x) => x.id === args.id)!;
        return { ...s, active_config: { id: c.id, name: c.name, modified: false, saved: true } };
      });
      return null;
    case "rename_config":
      update((s) => ({ ...s, configs: s.configs.map((c) => (c.id === args.id ? { ...c, name: String(args.name) } : c)) }));
      return null;
    case "delete_config":
      update((s) => ({
        ...s,
        configs: s.configs.filter((c) => c.id !== args.id),
        active_config: s.active_config?.id === args.id ? null : s.active_config,
      }));
      return null;
    case "export_config_file":
      return args.path;
    case "pair":
      update((s) => ({
        ...s,
        peers: s.peers.map((p) => (p.id === args.device ? { ...p, status: "pairing", pairing: { sas: "482 913", incoming: false, local_accepted: false, remote_accepted: false } } : p)),
      }));
      return null;
    case "accept_pair":
      update((s) => ({
        ...s,
        peers: s.peers.map((p) => (p.id === args.device ? { ...p, status: "connected", trusted: true, pairing: null, rtt_ms: 2.4, cipher: "aes-256-gcm" } : p)),
      }));
      return null;
    case "reject_pair":
      update((s) => ({ ...s, peers: s.peers.map((p) => (p.id === args.device ? { ...p, status: "discovered", pairing: null } : p)) }));
      return null;
    case "set_locked":
      update((s) => ({ ...s, input: { mode: "local", locked: Boolean(args.locked) } }));
      return null;
    case "get_logs":
      return ["2026-10-07T23:40:01Z  INFO kvmbridge: KVM Bridge 0.1.0 starting", "2026-10-07T23:40:02Z  INFO kvmbridge_core::net: listening on TCP 47801"];
    case "receive_dir":
      return "/home/max/Downloads/KVMBridge";
    case "config_export":
      return args.path;
    case "config_inspect":
      return {
        encrypted: false,
        needs_password: false,
        has_identity: false,
        info: { created_ms: Date.now() - 86400000, app_version: "0.1.0", source_id: me, source_name: "desk-pc", source_os: "linux", note: "" },
        has_settings: true,
        has_shared: true,
        layout_screens: 2,
        devices: [{ id: laptop, name: "fedora-laptop" }],
        from_this_device: true,
        config: null,
        config_id: null,
      };
    case "config_import":
      return { restarting: false };
    case "app_info":
      return { version: "0.1.0", config_dir: "~/.config/kvmbridge", log_dir: "~/.cache/kvmbridge/logs", os: "linux", session: "Wayland" };
    default:
      return null;
  }
}
