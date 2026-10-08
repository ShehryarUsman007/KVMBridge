import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { api, errorText, inTauri, Inspection } from "../api";
import { useEngine } from "../store";

const FILTERS = [{ name: "KVM Bridge configuration", extensions: ["kvmb"] }];

function Check({ checked, onChange, label, hint, disabled }: { checked: boolean; onChange: (v: boolean) => void; label: string; hint?: string; disabled?: boolean }) {
  return (
    <label className={`check ${disabled ? "disabled" : ""}`}>
      <input type="checkbox" checked={checked} disabled={disabled} onChange={(e) => onChange(e.target.checked)} />
      <span>
        <span>{label}</span>
        {hint && <span className="muted small block">{hint}</span>}
      </span>
    </label>
  );
}

export function ExportDialog({ onClose }: { onClose: () => void }) {
  const { snap, run, pushToast } = useEngine();
  const [settings, setSettings] = useState(true);
  const [layout, setLayout] = useState(true);
  const [devices, setDevices] = useState(true);
  const [identity, setIdentity] = useState(false);
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  if (!snap) return null;
  const paired = snap.peers.filter((p) => p.trusted).length;
  const mismatch = password !== confirm;
  const needsPassword = identity && !password;
  const nothing = !settings && !layout && !devices && !identity;

  const doExport = async () => {
    const date = new Date().toISOString().slice(0, 10);
    const path = await save({ filters: FILTERS, defaultPath: `${snap.me.name}-${date}.kvmb` });
    if (!path) return;
    setBusy(true);
    const ok = await run(
      api.configExport(path, { settings, layout, devices, identity, password: password || null, note }).then((p) =>
        pushToast({ kind: "success", title: "Configuration saved", body: p }),
      ),
    );
    setBusy(false);
    if (ok) onClose();
  };

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal dialog" role="dialog" aria-modal="true" aria-labelledby="export-title">
        <h2 id="export-title">Save configuration</h2>
        <p className="muted small">Creates a .kvmb file you can keep as a backup or load on another computer.</p>
        <div className="check-list">
          <Check checked={settings} onChange={setSettings} label="Settings" hint="Timing, clipboard, shortcuts, network and encryption choice." />
          <Check checked={layout} onChange={setLayout} label="Screen arrangement" />
          <Check checked={devices} onChange={setDevices} label={`Paired devices (${paired})`} hint="Names, addresses and public keys." />
          <Check
            checked={identity}
            onChange={setIdentity}
            label="This computer's identity"
            hint="Lets you restore this computer later without pairing again. Requires a password."
          />
        </div>
        <div className="field">
          <label className="field-label" htmlFor="pw">
            Password {identity ? "" : <span className="muted">(optional, encrypts the file)</span>}
          </label>
          <input id="pw" className="input" type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" />
        </div>
        {password && (
          <div className="field">
            <label className="field-label" htmlFor="pw2">
              Repeat password
            </label>
            <input id="pw2" className="input" type="password" value={confirm} onChange={(e) => setConfirm(e.target.value)} autoComplete="new-password" />
            {confirm && mismatch && <div className="small error-text">The passwords don't match.</div>}
          </div>
        )}
        <div className="field">
          <label className="field-label" htmlFor="note">
            Note <span className="muted">(optional)</span>
          </label>
          <input id="note" className="input" value={note} onChange={(e) => setNote(e.target.value)} placeholder="e.g. Home office setup" />
        </div>
        <div className="modal-actions end">
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn primary" disabled={busy || nothing || needsPassword || (!!password && mismatch)} onClick={doExport}>
            {busy ? "Saving…" : "Save…"}
          </button>
        </div>
      </div>
    </div>
  );
}

export function ImportDialog({ path, onClose }: { path: string; onClose: () => void }) {
  const { run, pushToast } = useEngine();
  const [password, setPassword] = useState("");
  const [inspection, setInspection] = useState<Inspection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [sel, setSel] = useState({ settings: true, layout: true, devices: true, identity: false });

  const inspect = async (pw: string | null) => {
    setBusy(true);
    setError(null);
    try {
      const r = await api.configInspect(path, pw);
      setInspection(r);
      setSel({ settings: r.has_settings, layout: r.layout_screens !== null, devices: r.devices.length > 0, identity: false });
    } catch (e) {
      setError(errorText(e));
    }
    setBusy(false);
  };

  useEffect(() => {
    inspect(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path]);

  const doImport = async () => {
    setBusy(true);
    let restarting = false;
    const ok = await run(
      api.configImport(path, password || null, sel).then((r) => {
        restarting = r.restarting;
      }),
    );
    setBusy(false);
    if (ok) {
      pushToast({
        kind: "success",
        title: restarting ? "Identity restored — restarting KVM Bridge" : "Configuration loaded",
      });
      onClose();
    }
  };

  const fileName = path.split(/[\\/]/).pop();
  const info = inspection?.info;
  const locked = inspection?.needs_password;

  if (inspection?.config && inspection.config_id) {
    const id = inspection.config_id;
    const name = inspection.config;
    const add = async (load: boolean) => {
      setBusy(true);
      const ok = await run(api.importConfigFile(path), load ? undefined : `Added "${name}" to your configs`);
      if (ok && load) await run(api.loadConfig(id), `Loaded "${name}"`);
      setBusy(false);
      if (ok) onClose();
    };
    return (
      <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
        <div className="modal dialog" role="dialog" aria-modal="true" aria-labelledby="add-config-title">
          <h2 id="add-config-title">Add the config "{name}"?</h2>
          <p className="muted small mono">{fileName}</p>
          <p className="muted">
            It's saved from {info?.source_name ?? "another computer"} and covers {inspection.devices.length || "its"} computers.
            Adding it copies it into your configs folder.
          </p>
          <div className="modal-actions end">
            <button className="btn ghost" onClick={onClose}>
              Cancel
            </button>
            <button className="btn" disabled={busy} onClick={() => add(false)}>
              Add
            </button>
            <button className="btn primary" disabled={busy} onClick={() => add(true)}>
              Add and load
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal dialog" role="dialog" aria-modal="true" aria-labelledby="import-title">
        <h2 id="import-title">Load configuration</h2>
        <p className="muted small mono">{fileName}</p>
        {error && <div className="notice bad small">{error}</div>}
        {locked && (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              inspect(password);
            }}
          >
            <div className="field">
              <label className="field-label" htmlFor="ipw">
                This file is password protected
              </label>
              <div className="row">
                <input id="ipw" className="input" type="password" autoFocus value={password} onChange={(e) => setPassword(e.target.value)} />
                <button className="btn primary" disabled={!password || busy}>
                  Unlock
                </button>
              </div>
            </div>
          </form>
        )}
        {inspection && !locked && info && (
          <>
            <dl className="kv small">
              <dt>Saved from</dt>
              <dd>
                {info.source_name} {inspection.from_this_device && <span className="muted">(this computer)</span>}
              </dd>
              <dt>Date</dt>
              <dd>{new Date(info.created_ms).toLocaleString()}</dd>
              {info.note && (
                <>
                  <dt>Note</dt>
                  <dd>{info.note}</dd>
                </>
              )}
            </dl>
            <div className="check-list">
              <Check
                checked={sel.settings}
                disabled={!inspection.has_settings}
                onChange={(v) => setSel({ ...sel, settings: v })}
                label="Settings"
                hint={inspection.has_settings ? "This computer keeps its own name." : "Not in this file"}
              />
              <Check
                checked={sel.layout}
                disabled={inspection.layout_screens === null}
                onChange={(v) => setSel({ ...sel, layout: v })}
                label={inspection.layout_screens === null ? "Screen arrangement" : `Screen arrangement (${inspection.layout_screens} screens)`}
                hint={inspection.layout_screens === null ? "Not in this file" : undefined}
              />
              <Check
                checked={sel.devices}
                disabled={inspection.devices.length === 0}
                onChange={(v) => setSel({ ...sel, devices: v })}
                label={`Paired devices (${inspection.devices.length})`}
                hint={inspection.devices.map((d) => d.name).join(", ") || "Not in this file"}
              />
              <Check
                checked={sel.identity}
                disabled={!inspection.has_identity}
                onChange={(v) => setSel({ ...sel, identity: v })}
                label="Restore computer identity"
                hint={
                  inspection.has_identity
                    ? `This computer becomes "${info.source_name}" again so paired devices recognise it. KVM Bridge restarts.`
                    : "Not in this file"
                }
              />
            </div>
          </>
        )}
        <div className="modal-actions end">
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn primary"
            disabled={busy || !inspection || locked || !(sel.settings || sel.layout || sel.devices || sel.identity)}
            onClick={doImport}
          >
            Load
          </button>
        </div>
      </div>
    </div>
  );
}

/** Settings section with the save / load buttons. Also handles files opened from the file manager. */
export function BackupSection() {
  const [exporting, setExporting] = useState(false);
  const [importPath, setImportPath] = useState<string | null>(null);
  return (
    <>
      <div className="row wrap">
        <button className="btn" onClick={() => setExporting(true)}>
          Save configuration…
        </button>
        <button
          className="btn"
          onClick={async () => {
            const f = await open({ filters: FILTERS, multiple: false });
            if (typeof f === "string") setImportPath(f);
          }}
        >
          Load configuration…
        </button>
      </div>
      {exporting && <ExportDialog onClose={() => setExporting(false)} />}
      {importPath && <ImportDialog path={importPath} onClose={() => setImportPath(null)} />}
    </>
  );
}

/** Mounted once at the app root: opens the import dialog for double-clicked files. */
export function OpenedFileHandler() {
  const [path, setPath] = useState<string | null>(null);
  useEffect(() => {
    if (!inTauri) return;
    api.pendingConfigFile().then((p) => p && setPath(p)).catch(() => {});
    const un = listen<string>("open-config", (e) => setPath(e.payload));
    return () => {
      un.then((f) => f());
    };
  }, []);
  return path ? <ImportDialog path={path} onClose={() => setPath(null)} /> : null;
}
