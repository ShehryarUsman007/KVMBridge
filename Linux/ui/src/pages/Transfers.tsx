import { useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { api, inTauri, TransferView } from "../api";
import { formatBytes, useEngine } from "../store";
import { IconFolder, IconTransfer, IconUpload, IconX } from "../components/Icons";

function stateLabel(t: TransferView): string {
  switch (t.state) {
    case "preparing":
      return "Preparing…";
    case "waiting":
      return "Waiting…";
    case "running":
      return `${formatBytes(t.rate_bps)}/s`;
    case "done":
      return "Done";
    case "failed":
      return t.error ?? "Failed";
    case "cancelled":
      return "Cancelled";
  }
}

export function TransfersPage() {
  const { snap, run } = useEngine();
  const [dropping, setDropping] = useState(false);
  const [target, setTarget] = useState<string | null>(null);
  const [receiveDir, setReceiveDir] = useState("");

  const connected = snap?.peers.filter((p) => p.trusted && p.status === "connected") ?? [];
  const chosen = target && connected.some((p) => p.id === target) ? target : connected[0]?.id ?? null;

  useEffect(() => {
    api.receiveDir().then(setReceiveDir).catch(() => {});
  }, [snap?.settings.transfers.receive_dir]);

  // Files dropped onto the window are sent to the chosen device.
  useEffect(() => {
    if (!inTauri) return;
    const un = getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "enter" || p.type === "over") setDropping(true);
      else if (p.type === "leave") setDropping(false);
      else if (p.type === "drop") {
        setDropping(false);
        if (chosen && p.paths.length) run(api.sendFiles(chosen, p.paths), "Sending…");
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, [chosen, run]);

  if (!snap) return null;
  const name = (id: string) => snap.peers.find((p) => p.id === id)?.name ?? "Unknown";
  const list = snap.transfers.filter((t) => t.kind !== "clipboard" || t.state === "running" || t.state === "failed");

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>File transfers</h1>
          <p className="muted">
            Drag files across screens, copy and paste them, or drop them here. Everything is encrypted end to end.
          </p>
        </div>
        <div className="header-actions">
          <button className="btn" onClick={() => run(api.openReceiveDir())}>
            <IconFolder /> Open received files
          </button>
        </div>
      </div>

      <div className={`dropzone card ${dropping ? "active" : ""} ${connected.length === 0 ? "disabled" : ""}`}>
        <IconUpload size={28} />
        {connected.length === 0 ? (
          <div>Connect a paired device to send files.</div>
        ) : (
          <>
            <div>
              Drop files here to send them to{" "}
              <select className="input inline" value={chosen ?? ""} onChange={(e) => setTarget(e.target.value)}>
                {connected.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
            </div>
            <button
              className="btn primary"
              onClick={async () => {
                const files = await open({ multiple: true });
                if (!files || !chosen) return;
                const l = Array.isArray(files) ? files : [files];
                run(api.sendFiles(chosen, l.map(String)), "Sending…");
              }}
            >
              Choose files…
            </button>
          </>
        )}
        <div className="muted small">Received files are saved in <span className="mono">{receiveDir}</span></div>
      </div>

      {list.length === 0 ? (
        <div className="empty-state">
          <IconTransfer /> No transfers yet.
        </div>
      ) : (
        <div className="transfer-list">
          {list.map((t) => {
            const pct = t.bytes_total > 0 ? Math.min(100, (100 * t.bytes_done) / t.bytes_total) : t.state === "done" ? 100 : 0;
            const title =
              t.items.length === 0 ? "Files" : t.items.length === 1 ? t.items[0].rel_path : `${t.items[0].rel_path} and ${t.items.length - 1} more`;
            const active = t.state === "preparing" || t.state === "waiting" || t.state === "running";
            return (
              <div key={t.id} className={`transfer card state-${t.state}`}>
                <div className="transfer-head">
                  <div className="grow">
                    <div className="transfer-title">{title}</div>
                    <div className="meta">
                      <span>{t.outgoing ? `to ${name(t.peer)}` : `from ${name(t.peer)}`}</span>
                      <span>{t.kind === "drag" ? "Drag and drop" : t.kind === "clipboard" ? "Clipboard" : "Sent"}</span>
                      <span>
                        {formatBytes(t.bytes_done)} / {formatBytes(t.bytes_total)}
                      </span>
                      {t.file_count > 1 && <span>{t.file_count} files</span>}
                      <span className={t.state === "failed" ? "error-text" : ""}>{stateLabel(t)}</span>
                    </div>
                  </div>
                  {active ? (
                    <button className="btn ghost" onClick={() => run(api.cancelTransfer(t.id))} title="Cancel">
                      <IconX />
                    </button>
                  ) : (
                    !t.outgoing &&
                    t.state === "done" &&
                    t.paths[0] && (
                      <button className="btn ghost" onClick={() => run(api.revealPath(t.paths[0]))}>
                        <IconFolder /> Show
                      </button>
                    )
                  )}
                </div>
                <div className="progress">
                  <div className="bar" style={{ width: `${pct}%` }} />
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
