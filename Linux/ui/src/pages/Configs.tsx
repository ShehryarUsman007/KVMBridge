import { useEffect, useState } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import { api, ConfigView, inTauri } from "../api";
import { useEngine } from "../store";
import { IconFolder, IconKeyboard, IconPlus, IconTrash, IconUpload } from "../components/Icons";

const FILTERS = [{ name: "KVM Bridge config", extensions: ["kvmb"] }];

function ago(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  const d = Math.floor(s / 86400);
  return d === 1 ? "yesterday" : `${d} days ago`;
}

/** Ask for a config name (new config, rename). */
function NameDialog({
  title,
  initial,
  action,
  onSubmit,
  onClose,
}: {
  title: string;
  initial: string;
  action: string;
  onSubmit: (name: string) => Promise<boolean>;
  onClose: () => void;
}) {
  const [name, setName] = useState(initial);
  const [busy, setBusy] = useState(false);
  const submit = async () => {
    if (!name.trim()) return;
    setBusy(true);
    if (await onSubmit(name.trim())) onClose();
    setBusy(false);
  };
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal dialog" role="dialog" aria-modal="true" aria-labelledby="name-title">
        <h2 id="name-title">{title}</h2>
        <label className="field">
          <span className="field-label">Name</span>
          <input
            className="input"
            autoFocus
            value={name}
            maxLength={64}
            placeholder="e.g. Desk, Office, Travel"
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
          />
        </label>
        <div className="modal-actions end">
          <button className="btn ghost" onClick={onClose}>
            Cancel
          </button>
          <button className="btn primary" disabled={busy || !name.trim()} onClick={submit}>
            {action}
          </button>
        </div>
      </div>
    </div>
  );
}

type Dialog = { kind: "new" } | { kind: "rename"; config: ConfigView } | { kind: "delete"; config: ConfigView } | null;

export function ConfigsPage() {
  const { snap, run } = useEngine();
  const [dialog, setDialog] = useState<Dialog>(null);

  // Pick up files copied into the folder by hand.
  useEffect(() => {
    api.rescanConfigs().catch(() => {});
  }, []);

  if (!snap) return null;
  const current = snap.current_config;
  const nameOf = (c: ConfigView, id: string | null) => (id ? c.members.find((m) => m.id === id)?.name ?? null : null);

  const exportConfig = async (c: ConfigView) => {
    const path = await save({ filters: FILTERS, defaultPath: `${c.name}.kvmb` });
    if (path) run(api.exportConfigFile(c.id, path), `Saved a copy of "${c.name}"`);
  };
  const addFile = async () => {
    const path = await open({ multiple: false, directory: false, filters: FILTERS });
    if (typeof path === "string") run(api.addConfigFile(path), "Config added");
  };

  return (
    <div className="page configs-page">
      <div className="page-header">
        <div>
          <h1>Configs</h1>
          <p className="muted">
            A config saves the screen arrangement, the computer that controls it, encryption and the settings shared by all
            computers. Every paired computer keeps a copy, and loading one changes the setup on all of its computers.
          </p>
        </div>
        <div className="header-actions">
          <button className="btn primary" onClick={() => setDialog({ kind: "new" })}>
            <IconPlus /> Save current setup…
          </button>
        </div>
      </div>

      {current?.modified && (
        <div className="notice warn configs-modified">
          <span className="grow">
            The setup has changed since <b>{current.name}</b> was loaded.
          </span>
          <button className="btn small" onClick={() => run(api.saveConfig(current.name, current.id), `Saved "${current.name}"`)}>
            Save changes
          </button>
        </div>
      )}

      {snap.configs.length === 0 ? (
        <div className="empty-state">
          <IconFolder /> No saved configs yet. Arrange your screens, then choose "Save current setup…".
        </div>
      ) : (
        <div className="device-list">
          {snap.configs.map((c) => {
            const inUse = current?.id === c.id && !current.modified;
            const controller = nameOf(c, c.controller);
            const unpaired = c.members.filter((m) => !m.paired);
            return (
              <div className={`device-row card config-row ${inUse ? "active" : ""}`} key={c.id}>
                <div className="grow">
                  <div className="device-name">
                    {c.name}
                    {inUse && <span className="pill accent">In use</span>}
                  </div>
                  <div className="meta">
                    <span>{c.members.map((m) => m.name).join(", ")}</span>
                    {controller && (
                      <span className="config-controller" title="Controls the screens">
                        <IconKeyboard size={13} /> {controller}
                      </span>
                    )}
                    <span>
                      Saved {ago(c.saved_ms)}
                      {c.saved_by ? ` on ${c.saved_by}` : ""}
                    </span>
                  </div>
                  {!c.includes_me && <div className="meta warn-text">This config doesn't include this computer.</div>}
                  {c.includes_me && unpaired.length > 0 && (
                    <div className="meta warn-text">
                      Not paired any more: {unpaired.map((m) => m.name).join(", ")}. Loading it leaves {unpaired.length === 1 ? "it" : "them"} out.
                    </div>
                  )}
                </div>
                <div className="row-actions">
                  {!inUse && (
                    <button
                      className="btn"
                      disabled={!c.includes_me}
                      onClick={() => run(api.loadConfig(c.id), `Loaded "${c.name}"`)}
                      title={c.includes_me ? "Use this setup on all of its computers" : "This config doesn't include this computer"}
                    >
                      Load
                    </button>
                  )}
                  <button className="btn ghost" onClick={() => setDialog({ kind: "rename", config: c })}>
                    Rename
                  </button>
                  {inTauri && (
                    <button className="icon-btn" title="Save a copy elsewhere…" onClick={() => exportConfig(c)}>
                      <IconUpload size={16} />
                    </button>
                  )}
                  <button className="icon-btn" title="Show the file" onClick={() => run(api.revealPath(c.file))}>
                    <IconFolder size={16} />
                  </button>
                  <button className="icon-btn danger" title="Delete" onClick={() => setDialog({ kind: "delete", config: c })}>
                    <IconTrash size={16} />
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      <div className="configs-folder">
        <span className="muted">Saved in</span>
        <span className="mono path" title={snap.configs_dir}>
          {snap.configs_dir}
        </span>
        <button className="btn small ghost" onClick={() => run(api.openConfigsDir())}>
          Open folder
        </button>
        {inTauri && (
          <button className="btn small ghost" onClick={addFile}>
            Add a config file…
          </button>
        )}
      </div>

      {dialog?.kind === "new" && (
        <NameDialog
          title="Save current setup"
          initial=""
          action="Save"
          onSubmit={(name) => run(api.saveConfig(name, null), `Saved "${name}" on all paired computers`)}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "rename" && (
        <NameDialog
          title={`Rename "${dialog.config.name}"`}
          initial={dialog.config.name}
          action="Rename"
          onSubmit={(name) => run(api.renameConfig(dialog.config.id, name))}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "delete" && (
        <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && setDialog(null)}>
          <div className="modal dialog" role="dialog" aria-modal="true">
            <h2>Delete "{dialog.config.name}"?</h2>
            <p className="muted">It's deleted on all paired computers. The current setup stays as it is.</p>
            <div className="modal-actions end">
              <button className="btn ghost" onClick={() => setDialog(null)}>
                Cancel
              </button>
              <button
                className="btn danger"
                onClick={() => {
                  run(api.deleteConfig(dialog.config.id));
                  setDialog(null);
                }}
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
