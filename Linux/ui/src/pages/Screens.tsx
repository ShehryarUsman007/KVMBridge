import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, Desktop, DeviceId, Layout, PeerView, Slot } from "../api";
import { cipherLabel, useEngine } from "../store";
import { IconEye, IconKeyboard, IconTrash, IconUpload } from "../components/Icons";
import { ControllerPicker } from "../components/Controller";
import { open } from "@tauri-apps/plugin-dialog";

const MAX_CELL_W = 184;
const MIN_CELL_W = 96;
const GAP = 16;

interface DeviceInfo {
  id: DeviceId;
  name: string;
  os: string;
  isMe: boolean;
  status: "me" | "connected" | "off" | "offline" | "unknown";
  desktop: Desktop | null;
  peer?: PeerView;
}

type Target = { col: number; row: number } | "tray" | null;

interface DragState {
  device: DeviceId;
  pointerId: number;
  offsetX: number;
  offsetY: number;
  x: number;
  y: number;
  target: Target;
  moved: boolean;
}

/** Apply a drag result to the layout. Returns null when nothing changes. */
function applyMove(layout: Layout, device: DeviceId, target: Target, name: string, me: DeviceId): Layout | null {
  if (target === null) return null;
  const slots: Slot[] = layout.slots.map((s) => ({ ...s }));
  const src = slots.find((s) => s.device === device);
  if (target === "tray") {
    if (!src || device === me) return null;
    return { ...layout, slots: slots.filter((s) => s.device !== device) };
  }
  const occupant = slots.find((s) => s.col === target.col && s.row === target.row);
  if (src) {
    if (src.col === target.col && src.row === target.row) return null;
    if (occupant) {
      occupant.col = src.col;
      occupant.row = src.row;
    }
    src.col = target.col;
    src.row = target.row;
  } else {
    if (occupant) {
      // Make room: push the occupant to the nearest free cell to the right.
      let c = target.col + 1;
      while (slots.some((s) => s.col === c && s.row === target.row)) c++;
      occupant.col = c;
    }
    slots.push({ device, col: target.col, row: target.row, number: 0, name });
  }
  return { ...layout, slots };
}

function monitorsText(d: Desktop | null): string | null {
  if (!d || d.monitors.length === 0) return null;
  return d.monitors.length === 1 ? "1 monitor" : `${d.monitors.length} monitors`;
}

function ScreenCard({
  info,
  number,
  controller,
  selected,
  flashing,
  dragging,
  compact,
  style,
  onPointerDown,
}: {
  info: DeviceInfo;
  number?: number;
  controller?: boolean;
  selected: boolean;
  flashing: boolean;
  dragging: boolean;
  compact?: boolean;
  style?: React.CSSProperties;
  onPointerDown: (e: React.PointerEvent) => void;
}) {
  const statusLabel =
    info.status === "off"
      ? info.isMe
        ? "This computer · off"
        : "Switched off"
      : info.isMe
        ? "This computer"
        : info.status === "connected"
          ? "Connected"
          : "Offline";
  const monitors = monitorsText(info.desktop);
  return (
    <div
      className={`screen-card status-${info.status} ${selected ? "selected" : ""} ${dragging ? "dragging" : ""} ${compact ? "compact" : ""}`}
      style={style}
      onPointerDown={onPointerDown}
      role="button"
      aria-label={`Screen ${number ?? ""} ${info.name}`}
      title={[info.name, statusLabel, monitors].filter(Boolean).join(" · ")}
    >
      {controller && (
        <span className="card-controller" title="Its keyboard and mouse control all screens">
          <IconKeyboard size={15} />
        </span>
      )}
      <div className={`screen-number ${number ? "" : "dim"}`}>{number || "–"}</div>
      {compact ? (
        <div className="card-name">
          <span className={`status-dot ${info.status}`} />
          <span className="name-text">{info.name || "Unknown"}</span>
        </div>
      ) : (
        <>
          <div className="card-name">{info.name || "Unknown"}</div>
          <div className="card-sub">
            <span className={`status-dot ${info.status}`} />
            {statusLabel}
          </div>
        </>
      )}
      {flashing && number ? <div className="flash-number">{number}</div> : null}
    </div>
  );
}

export function ScreensPage() {
  const { snap, run, identifyAt } = useEngine();
  const [pending, setPending] = useState<Layout | null>(null);
  const [drag, setDrag] = useState<DragState | null>(null);
  const dragRef = useRef<DragState | null>(null);
  const [selected, setSelected] = useState<DeviceId | null>(null);
  const [now, setNow] = useState(Date.now());
  const gridRef = useRef<HTMLDivElement>(null);
  const trayRef = useRef<HTMLDivElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const [avail, setAvail] = useState(900);

  useLayoutEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => setAvail(entries[0].contentRect.width));
    ro.observe(el);
    return () => ro.disconnect();
  }, [snap === null]);

  useEffect(() => {
    // The engine's layout replaced our optimistic copy.
    setPending(null);
  }, [snap?.layout.rev, snap?.layout.author]);

  useEffect(() => {
    if (!identifyAt) return;
    setNow(Date.now());
    const t = window.setTimeout(() => setNow(Date.now()), 3600);
    return () => window.clearTimeout(t);
  }, [identifyAt]);
  const flashing = identifyAt > 0 && now - identifyAt < 3500;

  const layout = pending ?? snap?.layout;

  const devices = useMemo(() => {
    const m = new Map<DeviceId, DeviceInfo>();
    if (!snap) return m;
    m.set(snap.me.id, { id: snap.me.id, name: snap.me.name, os: snap.me.os, isMe: true, status: snap.enabled ? "me" : "off", desktop: snap.desktop });
    for (const p of snap.peers) {
      if (!p.trusted) continue;
      m.set(p.id, {
        id: p.id,
        name: p.name,
        os: p.os,
        isMe: false,
        status: p.status !== "connected" ? "offline" : p.caps.enabled ? "connected" : "off",
        desktop: p.desktop,
        peer: p,
      });
    }
    for (const s of snap.layout.slots) {
      if (!m.has(s.device)) {
        m.set(s.device, { id: s.device, name: s.name, os: "", isMe: false, status: "unknown", desktop: null });
      }
    }
    return m;
  }, [snap]);

  if (!snap || !layout) return null;
  const me = snap.me.id;

  // Visible grid: everything placed plus a ring of empty cells around it.
  const cols = layout.slots.map((s) => s.col);
  const rows = layout.slots.map((s) => s.row);
  let minCol = Math.min(0, ...cols) - 1;
  let maxCol = Math.max(0, ...cols) + 1;
  let minRow = Math.min(0, ...rows) - 1;
  let maxRow = Math.max(0, ...rows) + 1;
  while (maxCol - minCol + 1 < 4) {
    maxCol++;
    if (maxCol - minCol + 1 < 4) minCol--;
  }
  while (maxRow - minRow + 1 < 3) {
    maxRow++;
    if (maxRow - minRow + 1 < 3) minRow--;
  }
  const nCols = maxCol - minCol + 1;
  const nRows = maxRow - minRow + 1;
  const CELL_W = Math.max(MIN_CELL_W, Math.min(MAX_CELL_W, Math.floor((avail - (nCols - 1) * GAP) / nCols)));
  const CELL_H = Math.round(CELL_W * 0.68);
  const compact = CELL_W < 150;
  const gridW = nCols * CELL_W + (nCols - 1) * GAP;
  const gridH = nRows * CELL_H + (nRows - 1) * GAP;
  const cellPos = (col: number, row: number) => ({
    left: (col - minCol) * (CELL_W + GAP),
    top: (row - minRow) * (CELL_H + GAP),
  });

  const unplaced = [...devices.values()].filter((d) => !d.isMe && !layout.slots.some((s) => s.device === d.id));
  const slotOf = (id: DeviceId) => layout.slots.find((s) => s.device === id);

  const targetAt = (clientX: number, clientY: number): Target => {
    const tray = trayRef.current?.getBoundingClientRect();
    if (tray && clientX >= tray.left && clientX <= tray.right && clientY >= tray.top && clientY <= tray.bottom) return "tray";
    const g = gridRef.current?.getBoundingClientRect();
    if (!g) return null;
    const x = clientX - g.left;
    const y = clientY - g.top;
    if (x < 0 || y < 0 || x > gridW || y > gridH) return null;
    const col = Math.floor(x / (CELL_W + GAP)) + minCol;
    const row = Math.floor(y / (CELL_H + GAP)) + minRow;
    return { col, row };
  };

  const updateDrag = (d: DragState | null) => {
    dragRef.current = d;
    setDrag(d);
  };

  const startDrag = (id: DeviceId) => (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    const r = el.getBoundingClientRect();
    try {
      el.setPointerCapture(e.pointerId);
    } catch {
      /* pointer already gone */
    }
    setSelected(id);
    updateDrag({ device: id, pointerId: e.pointerId, offsetX: e.clientX - r.left, offsetY: e.clientY - r.top, x: e.clientX, y: e.clientY, target: null, moved: false });
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d || e.pointerId !== d.pointerId) return;
    const moved = d.moved || Math.abs(e.clientX - d.x) + Math.abs(e.clientY - d.y) > 4;
    updateDrag({ ...d, x: e.clientX, y: e.clientY, moved, target: moved ? targetAt(e.clientX, e.clientY) : null });
  };

  const onPointerUp = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d || e.pointerId !== d.pointerId) return;
    updateDrag(null);
    const moved = d.moved || Math.abs(e.clientX - d.x) + Math.abs(e.clientY - d.y) > 4;
    if (!moved) return;
    const target = targetAt(e.clientX, e.clientY);
    const info = devices.get(d.device);
    const next = applyMove(layout, d.device, target, info?.name ?? "", me);
    if (next) {
      setPending(next);
      run(api.setLayout(next)).then((ok) => {
        if (!ok) setPending(null);
      });
    }
  };

  // Highlight where the dragged card would land.
  const dropCell = drag?.moved && drag.target && drag.target !== "tray" ? drag.target : null;
  const dropInvalid = drag?.target === "tray" && drag.device === me;

  // Links between adjacent screens.
  const links: { key: string; style: React.CSSProperties; horizontal: boolean }[] = [];
  for (const s of layout.slots) {
    const right = layout.slots.find((o) => o.col === s.col + 1 && o.row === s.row);
    const below = layout.slots.find((o) => o.col === s.col && o.row === s.row + 1);
    const p = cellPos(s.col, s.row);
    if (right) links.push({ key: `${s.device}-r`, horizontal: true, style: { left: p.left + CELL_W - 6, top: p.top + CELL_H / 2 - 9, width: GAP + 12 } });
    if (below) links.push({ key: `${s.device}-b`, horizontal: false, style: { left: p.left + CELL_W / 2 - 9, top: p.top + CELL_H - 6, height: GAP + 12 } });
  }
  const isolated = layout.slots.length > 1
    ? layout.slots.filter((s) => !layout.slots.some((o) => Math.abs(o.col - s.col) + Math.abs(o.row - s.row) === 1))
    : [];

  const sel = selected ? devices.get(selected) : undefined;
  const selSlot = selected ? slotOf(selected) : undefined;

  return (
    <div className="page screens-page">
      <div className="page-header">
        <div>
          <h1>Screen arrangement</h1>
          <p className="muted">Arrange the screens to match your desk. The cursor crosses wherever two screens touch.</p>
        </div>
        <div className="header-actions">
          <button className="btn" onClick={() => run(api.identify())} title="Show each screen's number on that screen">
            <IconEye /> Identify
          </button>
        </div>
      </div>

      <div className="screens-body">
        <div className="grid-wrap" ref={wrapRef} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={() => updateDrag(null)}>
          <div className="grid" ref={gridRef} style={{ width: gridW, height: gridH }}>
            {Array.from({ length: nRows }).map((_, r) =>
              Array.from({ length: nCols }).map((__, c) => {
                const col = c + minCol;
                const row = r + minRow;
                const p = cellPos(col, row);
                const isDrop = dropCell && dropCell.col === col && dropCell.row === row;
                return <div key={`${col},${row}`} className={`cell ${isDrop ? "drop" : ""}`} style={{ ...p, width: CELL_W, height: CELL_H }} />;
              }),
            )}
            {links.map((l) => (
              <div key={l.key} className={`edge-link ${l.horizontal ? "h" : "v"}`} style={l.style} />
            ))}
            {layout.slots.map((s) => {
              const info = devices.get(s.device) ?? { id: s.device, name: s.name, os: "", isMe: false, status: "unknown" as const, desktop: null };
              const p = cellPos(s.col, s.row);
              const isDragging = drag?.device === s.device && drag.moved;
              const style: React.CSSProperties = isDragging
                ? { position: "fixed", left: drag!.x - drag!.offsetX, top: drag!.y - drag!.offsetY, width: CELL_W, height: CELL_H, zIndex: 50 }
                : { position: "absolute", left: p.left, top: p.top, width: CELL_W, height: CELL_H };
              return (
                <ScreenCard
                  key={s.device}
                  info={info}
                  number={s.number}
                  controller={layout.controller === s.device}
                  selected={selected === s.device}
                  flashing={flashing}
                  dragging={isDragging}
                  compact={compact}
                  style={style}
                  onPointerDown={startDrag(s.device)}
                />
              );
            })}
          </div>
        </div>

        <aside className="screen-details card">
          {sel ? (
            <>
              <h3>
                {selSlot ? `Screen ${selSlot.number}` : "Not placed"} <span className="muted">· {sel.name}</span>
              </h3>
              <dl className="kv">
                <dt>Status</dt>
                <dd>
                  {sel.isMe ? "This computer" : sel.status === "connected" ? "Connected" : sel.status === "off" ? "Connected, switched off" : "Offline"}
                  {sel.isMe && sel.status === "off" ? " (switched off)" : ""}
                </dd>
                {sel.peer?.address && (
                  <>
                    <dt>Address</dt>
                    <dd className="mono">{sel.peer.address}</dd>
                  </>
                )}
                {sel.peer?.rtt_ms != null && (
                  <>
                    <dt>Latency</dt>
                    <dd>{sel.peer.rtt_ms.toFixed(1)} ms</dd>
                  </>
                )}
                {sel.peer?.cipher && (
                  <>
                    <dt>Encryption</dt>
                    <dd>{cipherLabel(sel.peer.cipher)}</dd>
                  </>
                )}
                <dt>Monitors</dt>
                <dd>
                  {sel.desktop?.monitors.length
                    ? sel.desktop.monitors.map((m, i) => (
                        <div key={i} className="mono small">
                          {Math.round(m.w)}×{Math.round(m.h)} at {Math.round(m.x)},{Math.round(m.y)}
                        </div>
                      ))
                    : "Unknown"}
                </dd>
              </dl>
              <div className="detail-actions">
                {!sel.isMe && sel.status === "connected" && (
                  <button
                    className="btn"
                    onClick={async () => {
                      const files = await open({ multiple: true, directory: false });
                      if (!files) return;
                      const list = Array.isArray(files) ? files : [files];
                      run(api.sendFiles(sel.id, list.map(String)), "Sending…");
                    }}
                  >
                    <IconUpload /> Send files…
                  </button>
                )}
                {!sel.isMe && selSlot && (
                  <button
                    className="btn ghost danger"
                    onClick={() => {
                      const next = applyMove(layout, sel.id, "tray", sel.name, me);
                      if (next) {
                        setPending(next);
                        run(api.setLayout(next));
                      }
                    }}
                  >
                    <IconTrash /> Remove from arrangement
                  </button>
                )}
              </div>
            </>
          ) : (
            <div className="muted">
              <h3>Arranging screens</h3>
              <ul className="tips small">
                <li>Each computer is one screen, however many monitors it has.</li>
                <li>Drop a screen onto another to swap them.</li>
                <li>Drag a screen to the area below to remove it.</li>
                <li>Identify shows each number on its computer.</li>
              </ul>
            </div>
          )}
          {isolated.length > 0 && (
            <div className="notice warn small">
              {isolated.map((s) => `Screen ${s.number}`).join(", ")} {isolated.length === 1 ? "isn't" : "aren't"} next to any other screen, so the cursor can't reach {isolated.length === 1 ? "it" : "them"}.
            </div>
          )}
        </aside>
      </div>

      <div className={`tray ${drag?.target === "tray" ? (dropInvalid ? "invalid" : "drop") : ""}`} ref={trayRef}>
        <div className="tray-label">Not arranged</div>
        {unplaced.length === 0 ? (
          <div className="muted small">
            {devices.size <= 1 ? "Pair another computer on the Devices page to add it here." : "All paired computers are placed."}
          </div>
        ) : (
          <div className="tray-items" onPointerMove={onPointerMove} onPointerUp={onPointerUp}>
            {unplaced.map((d) => {
              const isDragging = drag?.device === d.id && drag.moved;
              const style: React.CSSProperties = isDragging
                ? { position: "fixed", left: drag!.x - drag!.offsetX, top: drag!.y - drag!.offsetY, width: CELL_W, height: CELL_H, zIndex: 50 }
                : { width: 156, height: 106 };
              return (
                <ScreenCard
                  key={d.id}
                  info={d}
                  selected={selected === d.id}
                  flashing={false}
                  dragging={isDragging}
                  compact
                  style={style}
                  onPointerDown={startDrag(d.id)}
                />
              );
            })}
          </div>
        )}
      </div>

      <ControllerPicker />
    </div>
  );
}
