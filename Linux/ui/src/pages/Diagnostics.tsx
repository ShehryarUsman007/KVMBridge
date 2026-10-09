import { useEffect, useRef, useState } from "react";
import { api, AppInfo, BackendStatus } from "../api";
import { useEngine } from "../store";
import { IconRefresh } from "../components/Icons";

const BACKENDS: { key: "capture" | "emulation" | "clipboard" | "dnd"; label: string; what: string }[] = [
  {
    key: "capture",
    label: "Input capture",
    what: "Lets this computer's keyboard and mouse control other screens. If they ever stop responding here, press Super+Shift+Esc to get them back.",
  },
  { key: "emulation", label: "Input control", what: "Lets other computers move this cursor and type here." },
  { key: "clipboard", label: "Clipboard", what: "Shares copied text, images and files." },
  { key: "dnd", label: "Drag and drop", what: "Drags files across screens." },
];

export function statusClass(s: BackendStatus): string {
  return s.state === "ready" ? "ok" : s.state === "unavailable" ? "bad" : "pending";
}

export function DiagnosticsPage() {
  const { snap, run } = useEngine();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [logs, setLogs] = useState<string[]>([]);
  const logRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    api.appInfo().then(setInfo).catch(() => {});
    const load = () => api.logs().then(setLogs).catch(() => {});
    load();
    const t = window.setInterval(load, 2000);
    return () => window.clearInterval(t);
  }, []);
  useEffect(() => {
    const el = logRef.current;
    if (el && el.scrollHeight - el.scrollTop - el.clientHeight < 80) el.scrollTop = el.scrollHeight;
  }, [logs]);

  if (!snap) return null;
  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>Status</h1>
          <p className="muted">What KVM Bridge can do on this computer, and what it's been up to.</p>
        </div>
        <div className="header-actions">
          <button className="btn" onClick={() => run(api.restartBackends(), "Restarting…")}>
            <IconRefresh /> Retry permissions
          </button>
        </div>
      </div>
      <div className="backend-grid">
        {BACKENDS.map((b) => {
          const st = snap.backends[b.key];
          return (
            <div key={b.key} className={`card backend ${statusClass(st)}`}>
              <div className="backend-head">
                <span className={`status-dot ${statusClass(st)}`} />
                <b>{b.label}</b>
              </div>
              <div className="muted small">{b.what}</div>
              <div className="backend-detail">
                {st.state === "idle"
                  ? b.key === "capture"
                    ? "Not needed yet: starts when a screen edge here leads to another computer"
                    : "Not started"
                  : st.state === "ready"
                    ? `Working (${st.detail})`
                    : st.detail}
              </div>
              {b.key === "dnd" && st.state === "ready" && (
                <button
                  className="btn small"
                  title="A test file follows the pointer for 6 seconds and is dropped wherever the pointer is then"
                  onClick={() =>
                    run(api.testDrag(), "Move the pointer over a folder — the test file drops there in 6 seconds")
                  }
                >
                  Test
                </button>
              )}
            </div>
          );
        })}
      </div>
      {info && (
        <div className="card">
          <dl className="kv">
            <dt>Version</dt>
            <dd>{info.version}</dd>
            <dt>System</dt>
            <dd>
              {info.os} ({info.session})
            </dd>
            <dt>Settings folder</dt>
            <dd className="mono">{info.config_dir}</dd>
            <dt>Logs</dt>
            <dd className="mono">{info.log_dir}</dd>
          </dl>
        </div>
      )}
      <h2>Log</h2>
      <pre className="log card" ref={logRef}>
        {logs.join("\n")}
      </pre>
    </div>
  );
}
