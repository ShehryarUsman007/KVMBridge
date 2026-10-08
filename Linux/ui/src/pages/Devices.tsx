import { useState } from "react";
import { api, PeerView } from "../api";
import { cipherLabel, useEngine } from "../store";
import { IconCheck, IconLaptop, IconLinux, IconMonitor, IconPlus, IconTrash, IconUpload, IconWifi, IconWindows } from "../components/Icons";
import { open } from "@tauri-apps/plugin-dialog";

function OsIcon({ os }: { os: string }) {
  if (os === "windows") return <IconWindows size={16} />;
  if (os === "linux") return <IconLinux size={16} />;
  if (os === "macos") return <IconLaptop size={16} />;
  return <IconMonitor size={16} />;
}

function statusText(p: PeerView): string {
  switch (p.status) {
    case "connected":
      return p.caps.enabled ? (p.config ? `Connected · ${p.config}` : "Connected") : "Connected · switched off";
    case "connecting":
      return "Connecting…";
    case "pairing":
      return "Pairing…";
    case "offline":
      return "Offline";
    default:
      return "Available";
  }
}

export function DevicesPage() {
  const { snap, run } = useEngine();
  const [address, setAddress] = useState("");
  const [name, setName] = useState<string | null>(null);
  if (!snap) return null;

  const paired = snap.peers.filter((p) => p.trusted);
  const nearby = snap.peers.filter((p) => !p.trusted);
  const full = paired.length + 1 >= snap.max_devices;
  const slotNumber = (id: string) => snap.layout.slots.find((s) => s.device === id)?.number;

  const saveName = () => {
    if (name === null) return;
    const trimmed = name.trim();
    if (trimmed && trimmed !== snap.settings.device_name) {
      run(api.setSettings({ ...snap.settings, device_name: trimmed }), "Name updated");
    }
    setName(null);
  };

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>Devices</h1>
          <p className="muted">Computers running KVM Bridge on this network appear here automatically. Pair them once to share input.</p>
        </div>
      </div>

      <section className="card this-device">
        <div className="this-icon">
          <OsIcon os={snap.me.os} />
        </div>
        <div className="grow">
          <label className="field-label">This computer</label>
          <div className="row">
            <input
              className="input name-input"
              value={name ?? snap.me.name}
              onChange={(e) => setName(e.target.value)}
              onBlur={saveName}
              onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
              maxLength={64}
              aria-label="Device name"
            />
            {slotNumber(snap.me.id) !== undefined && <span className="pill">Screen {slotNumber(snap.me.id)}</span>}
          </div>
          <div className="meta">
            <span>
              Addresses: <span className="mono">{snap.me.addresses.join(", ") || "none"}</span>
            </span>
            <span>
              Port: <span className="mono">{snap.me.port}</span>
            </span>
            <span>
              Key fingerprint: <span className="mono">{snap.me.fingerprint}</span>
            </span>
          </div>
        </div>
      </section>

      <h2>
        Paired devices{" "}
        <span className="muted small">
          ({paired.length + 1} of {snap.max_devices} computers, including this one)
        </span>
      </h2>
      {paired.length === 0 ? (
        <div className="empty-state">No paired devices yet. Pick a device below to pair with it.</div>
      ) : (
        <div className="device-list">
          {paired.map((p) => (
            <div className={`device-row card status-${p.status}`} key={p.id}>
              <div className="device-icon">
                <OsIcon os={p.os} />
              </div>
              <div className="grow">
                <div className="device-name">
                  {p.name} {slotNumber(p.id) !== undefined && <span className="pill">Screen {slotNumber(p.id)}</span>}
                </div>
                <div className="meta">
                  <span className={`status-text ${p.status}`}>{statusText(p)}</span>
                  {p.address && <span className="mono">{p.address}</span>}
                  {p.rtt_ms != null && <span>{p.rtt_ms.toFixed(1)} ms</span>}
                  {p.cipher && <span>{cipherLabel(p.cipher)}</span>}
                  <span className="mono" title="Key fingerprint">
                    {p.fingerprint}
                  </span>
                </div>
                {p.status === "connected" && (!p.caps.emulation || !p.caps.capture) && (
                  <div className="meta warn-text">
                    {!p.caps.emulation && "This device can't be controlled yet (it has not granted input permission). "}
                    {!p.caps.capture && "This device can't send its input yet."}
                  </div>
                )}
                {p.error && <div className="meta error-text">{p.error}</div>}
              </div>
              <div className="row-actions">
                {p.status === "connected" && (
                  <button
                    className="btn"
                    onClick={async () => {
                      const files = await open({ multiple: true });
                      if (!files) return;
                      const list = Array.isArray(files) ? files : [files];
                      run(api.sendFiles(p.id, list.map(String)), "Sending…");
                    }}
                  >
                    <IconUpload /> Send files
                  </button>
                )}
                <button
                  className="btn ghost danger"
                  onClick={() => {
                    if (window.confirm(`Unpair ${p.name}? You will have to pair again to use it.`)) run(api.unpair(p.id));
                  }}
                >
                  <IconTrash /> Unpair
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      <h2>
        Nearby devices <span className="muted small">(discovered on your network)</span>
      </h2>
      {full && nearby.length > 0 && (
        <div className="notice warn small" style={{ marginBottom: 10 }}>
          You've reached the limit of {snap.max_devices} computers. Unpair one to pair another.
        </div>
      )}
      {nearby.length === 0 ? (
        <div className="empty-state">
          <IconWifi /> Looking for other computers running KVM Bridge… Make sure it's running on them and that they're on the same network.
        </div>
      ) : (
        <div className="device-list">
          {nearby.map((p) => (
            <div className="device-row card" key={p.id}>
              <div className="device-icon">
                <OsIcon os={p.os} />
              </div>
              <div className="grow">
                <div className="device-name">{p.name}</div>
                <div className="meta">
                  <span className={`status-text ${p.status}`}>{statusText(p)}</span>
                  {p.address && <span className="mono">{p.address}</span>}
                  <span className="mono">{p.fingerprint}</span>
                </div>
                {p.error && <div className="meta error-text">{p.error}</div>}
              </div>
              <div className="row-actions">
                {p.pairing ? (
                  <span className="pill accent">Code {p.pairing.sas}</span>
                ) : (
                  <button
                    className="btn primary"
                    onClick={() => run(api.pair(p.id))}
                    disabled={p.status === "connecting" || full}
                    title={full ? `Up to ${snap.max_devices} computers can be paired. Unpair one first.` : undefined}
                  >
                    <IconCheck /> Pair
                  </button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      <section className="card add-by-ip">
        <div className="grow">
          <label className="field-label" htmlFor="addr">
            Add a device by address
          </label>
          <p className="muted small">Use this if a device doesn't show up (for example when the network blocks broadcasts).</p>
          <div className="row">
            <input
              id="addr"
              className="input"
              placeholder="192.168.1.20 or hostname[:port]"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && address.trim()) {
                  run(api.connectAddress(address.trim()), "Connecting…");
                  setAddress("");
                }
              }}
            />
            <button
              className="btn"
              disabled={!address.trim()}
              onClick={() => {
                run(api.connectAddress(address.trim()), "Connecting…");
                setAddress("");
              }}
            >
              <IconPlus /> Connect
            </button>
          </div>
        </div>
      </section>
    </div>
  );
}
