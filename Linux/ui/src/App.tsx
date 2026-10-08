import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, inTauri } from "./api";
import { useEngine } from "./store";
import { ScreensPage } from "./pages/Screens";
import { ConfigsPage } from "./pages/Configs";
import { DevicesPage } from "./pages/Devices";
import { TransfersPage } from "./pages/Transfers";
import { SettingsPage } from "./pages/Settings";
import { DiagnosticsPage, statusClass } from "./pages/Diagnostics";
import { PairingDialog, Toasts } from "./components/Overlays";
import { OpenedFileHandler } from "./components/Backup";
import {
  IconAlert,
  IconCursor,
  IconDevices,
  IconFolder,
  IconInfo,
  IconKeyboard,
  IconLock,
  IconPulse,
  IconRefresh,
  IconScreens,
  IconSettings,
  IconTransfer,
  IconUnlock,
} from "./components/Icons";

type Page = "screens" | "configs" | "devices" | "transfers" | "settings" | "status";

const NAV: { id: Page; label: string; icon: (p: { size?: number }) => JSX.Element }[] = [
  { id: "screens", label: "Screens", icon: IconScreens },
  { id: "configs", label: "Configs", icon: IconFolder },
  { id: "devices", label: "Devices", icon: IconDevices },
  { id: "transfers", label: "Transfers", icon: IconTransfer },
  { id: "settings", label: "Settings", icon: IconSettings },
  { id: "status", label: "Status", icon: IconPulse },
];

/** The on/off switch for KVM Bridge on this computer. */
function PowerSwitch() {
  const { snap, run } = useEngine();
  if (!snap) return null;
  const cfg = snap.active_config;
  return (
    <div className="power">
      <label className={`power-toggle ${snap.enabled ? "on" : ""}`} title={snap.enabled ? "Turn KVM Bridge off on this computer" : "Turn KVM Bridge on"}>
        <span className={`switch ${snap.enabled ? "on" : ""}`}>
          <input
            type="checkbox"
            checked={snap.enabled}
            onChange={(e) => run(api.setEnabled(e.target.checked))}
            aria-label="KVM Bridge on this computer"
          />
          <span className="knob" />
        </span>
        <span>{snap.enabled ? "On" : "Off"}</span>
      </label>
      {cfg && (
        <span className="power-config" title={cfg.modified ? "The setup changed since this config was saved" : "Active config"}>
          {cfg.name}
          {cfg.modified ? " (changed)" : ""}
        </span>
      )}
    </div>
  );
}

function InputStatus() {
  const { snap, run } = useEngine();
  if (!snap) return null;
  const name = (id: string) => snap.peers.find((p) => p.id === id)?.name ?? "another computer";
  const inp = snap.input;
  if (!snap.enabled) {
    return (
      <div className="input-status local">
        <IconCursor size={16} />
        <span>KVM Bridge is off on this computer</span>
      </div>
    );
  }
  if (inp.mode === "controlling") {
    return (
      <div className="input-status controlling">
        <IconCursor size={16} />
        <span>
          Controlling <b>{name(inp.target)}</b>
        </span>
        <button className="btn small" onClick={() => run(api.releaseControl())}>
          Bring cursor back
        </button>
      </div>
    );
  }
  if (inp.mode === "controlled") {
    return (
      <div className="input-status controlled">
        <IconCursor size={16} />
        <span>
          Being controlled from <b>{name(inp.by)}</b>
        </span>
      </div>
    );
  }
  return (
    <div className="input-status local">
      <IconCursor size={16} />
      <span>Input is on this computer</span>
      <button
        className={`icon-btn ${inp.locked ? "active" : ""}`}
        title={inp.locked ? "Cursor is locked to this screen (click to unlock)" : "Lock the cursor to this screen"}
        onClick={() => run(api.setLocked(!inp.locked))}
      >
        {inp.locked ? <IconLock size={16} /> : <IconUnlock size={16} />}
      </button>
    </div>
  );
}

/** Pausing of this computer's own keyboard and mouse. */
function GuardBanner() {
  const { snap, run } = useEngine();
  if (!snap) return null;
  const g = snap.guard;
  const controller = snap.peers.find((p) => p.id === snap.layout.controller)?.name ?? "another computer";
  if (g.state === "paused") {
    return (
      <div className="banner info">
        <IconKeyboard size={18} />
        <div className="grow">
          <b>This computer's keyboard and mouse are paused</b>
          <div className="small">
            {controller} controls the screens. To use them here again, press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+
            <kbd>Esc</kbd> on this keyboard.
          </div>
        </div>
      </div>
    );
  }
  if (g.state === "needs_setup") {
    return (
      <div className="banner pending">
        <IconInfo size={18} />
        <div className="grow">
          <b>One-time setup needed</b>
          <div className="small">
            To pause this computer's keyboard and mouse while {controller} controls it, KVM Bridge needs a small system helper.
            Installing it asks for your password once.
          </div>
        </div>
        <button className="btn small" onClick={() => run(api.setupInputPause(), "Set up — this keyboard and mouse can now be paused")}>
          Set up
        </button>
      </div>
    );
  }
  if (g.state === "unavailable") {
    return (
      <div className="banner pending">
        <IconAlert size={18} />
        <div className="grow">
          <b>This computer's keyboard and mouse can't be paused</b>
          <div className="small">{g.detail}. They still work here and may get in the way.</div>
        </div>
        <button className="btn small" onClick={() => run(api.retryInputPause())}>
          <IconRefresh size={14} /> Try again
        </button>
      </div>
    );
  }
  return null;
}

function BackendBanner({ goStatus }: { goStatus: () => void }) {
  const { snap, run } = useEngine();
  if (!snap) return null;
  const { capture, emulation } = snap.backends;
  const bad = [capture, emulation].filter((b) => b.state === "unavailable");
  const starting = [capture, emulation].filter((b) => b.state === "starting");
  if (bad.length === 0 && starting.length === 0) return null;
  const first = bad[0] ?? starting[0];
  const detail = "detail" in first ? first.detail : "";
  return (
    <div className={`banner ${bad.length ? "bad" : "pending"}`}>
      <IconAlert size={18} />
      <div className="grow">
        <b>{bad.length ? "Input sharing is not working" : "Waiting for permission"}</b>
        <div className="small">{detail}</div>
      </div>
      {bad.length > 0 && (
        <button className="btn small" onClick={() => run(api.restartBackends())}>
          <IconRefresh size={14} /> Retry
        </button>
      )}
      <button className="btn small ghost" onClick={goStatus}>
        Details
      </button>
    </div>
  );
}

/** Apply the appearance setting (light / dark / follow the system). */
export function applyAppearance(a: string) {
  const root = document.documentElement;
  if (a === "light" || a === "dark") root.dataset.theme = a;
  else delete root.dataset.theme;
  try {
    localStorage.setItem("kvmb-appearance", a);
  } catch {
    /* storage unavailable */
  }
  if (inTauri) {
    getCurrentWindow()
      .setTheme(a === "light" || a === "dark" ? a : null)
      .catch(() => {});
  }
}

export default function App() {
  const { snap } = useEngine();
  const [page, setPage] = useState<Page>("screens");
  const appearance = snap?.settings.appearance;
  useEffect(() => {
    if (appearance) applyAppearance(appearance);
  }, [appearance]);
  if (!snap) {
    return (
      <div className="loading">
        <div className="spinner" />
        Starting KVM Bridge…
      </div>
    );
  }
  const myNumber = snap.layout.slots.find((s) => s.device === snap.me.id)?.number;
  const pendingPairs = snap.peers.filter((p) => p.pairing).length;
  const connected = snap.peers.filter((p) => p.trusted && p.status === "connected").length;
  const activeTransfers = snap.transfers.filter((t) => t.state === "running" || t.state === "waiting").length;
  const healthy = [snap.backends.capture, snap.backends.emulation].every((b) => statusClass(b) === "ok");

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <img src="/icon.png" alt="" width={28} height={28} />
          <span>KVM Bridge</span>
        </div>
        <nav>
          {NAV.map((n) => (
            <button key={n.id} className={`nav-item ${page === n.id ? "active" : ""}`} onClick={() => setPage(n.id)}>
              <n.icon size={18} />
              <span>{n.label}</span>
              {n.id === "devices" && pendingPairs > 0 && <span className="badge">{pendingPairs}</span>}
              {n.id === "transfers" && activeTransfers > 0 && <span className="badge">{activeTransfers}</span>}
              {n.id === "status" && !healthy && <span className="badge warn">!</span>}
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          <div className="me-name">{snap.me.name}</div>
          <div className="muted small">
            {myNumber !== undefined ? `Screen ${myNumber}` : "Not arranged"} · {connected} connected
          </div>
        </div>
      </aside>
      <main className="main">
        <header className="topbar">
          <PowerSwitch />
          <InputStatus />
        </header>
        <BackendBanner goStatus={() => setPage("status")} />
        <GuardBanner />
        <div className="content">
          {page === "screens" && <ScreensPage />}
          {page === "configs" && <ConfigsPage />}
          {page === "devices" && <DevicesPage />}
          {page === "transfers" && <TransfersPage />}
          {page === "settings" && <SettingsPage />}
          {page === "status" && <DiagnosticsPage />}
        </div>
      </main>
      <PairingDialog />
      <OpenedFileHandler />
      <Toasts />
    </div>
  );
}
