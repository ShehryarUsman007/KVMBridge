import { createContext, ReactNode, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, errorText, inTauri, Notice, Snapshot } from "./api";
import { onMockSnapshot } from "./mock";

export interface Toast {
  id: number;
  kind: "info" | "success" | "error";
  title: string;
  body?: string;
  action?: { label: string; run: () => void };
}

interface Ctx {
  snap: Snapshot | null;
  toasts: Toast[];
  pushToast: (t: Omit<Toast, "id">) => void;
  dismissToast: (id: number) => void;
  /** Run a backend call, turn failures into an error toast; resolves to success. */
  run: (p: Promise<unknown>, okMessage?: string) => Promise<boolean>;
  /** Pairing requests that were not answered yet. */
  pairingRequests: { device: string; name: string; sas: string }[];
  dismissPairing: (device: string) => void;
  /** Screen numbers flashing after "Identify". */
  identifyAt: number;
}

const EngineCtx = createContext<Ctx | null>(null);

export function EngineProvider({ children }: { children: ReactNode }) {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [pairingRequests, setPairingRequests] = useState<{ device: string; name: string; sas: string }[]>([]);
  const [identifyAt, setIdentifyAt] = useState(0);
  const nextId = useRef(1);

  const dismissToast = useCallback((id: number) => setToasts((t) => t.filter((x) => x.id !== id)), []);
  const pushToast = useCallback(
    (t: Omit<Toast, "id">) => {
      const id = nextId.current++;
      setToasts((all) => [...all.slice(-4), { ...t, id }]);
      window.setTimeout(() => dismissToast(id), t.kind === "error" ? 9000 : 5500);
    },
    [dismissToast],
  );

  const run = useCallback(
    async (p: Promise<unknown>, okMessage?: string) => {
      try {
        await p;
        if (okMessage) pushToast({ kind: "success", title: okMessage });
        return true;
      } catch (e) {
        pushToast({ kind: "error", title: "Something went wrong", body: errorText(e) });
        return false;
      }
    },
    [pushToast],
  );

  useEffect(() => {
    let alive = true;
    api.snapshot().then((s) => alive && setSnap(s)).catch(() => {});
    const unsubs: Promise<() => void>[] = [];
    if (!inTauri) {
      const off = onMockSnapshot(setSnap);
      return () => {
        alive = false;
        off();
      };
    }
    unsubs.push(listen<Snapshot>("snapshot", (e) => setSnap(e.payload)));
    unsubs.push(
      listen<Notice>("notice", (e) => {
        const n = e.payload;
        switch (n.kind) {
          case "identify":
            setIdentifyAt(Date.now());
            break;
          case "pairing_request":
            setPairingRequests((r) => [...r.filter((x) => x.device !== n.device), { device: n.device, name: n.name, sas: n.sas }]);
            break;
          case "paired":
            setPairingRequests((r) => r.filter((x) => x.device !== n.device));
            pushToast({ kind: "success", title: `Paired with ${n.name}`, body: "Drag it next to this screen on the Screens page." });
            break;
          case "pairing_failed":
            setPairingRequests((r) => r.filter((x) => x.device !== n.device));
            pushToast({ kind: "error", title: `Pairing with ${n.name} failed`, body: n.reason });
            break;
          case "transfer_finished":
            if (n.ok) {
              const first = n.paths[0];
              pushToast({
                kind: "success",
                title: n.outgoing ? `Sent to ${n.peer_name}` : `Received from ${n.peer_name}`,
                body: n.outgoing ? undefined : n.paths.length === 1 ? first : `${n.paths.length} items`,
                action: !n.outgoing && first ? { label: "Show", run: () => api.revealPath(first) } : undefined,
              });
            } else {
              pushToast({ kind: "error", title: `Transfer with ${n.peer_name} failed`, body: n.error ?? undefined });
            }
            break;
          case "error":
            pushToast({ kind: "error", title: n.message });
            break;
          case "info":
            pushToast({ kind: "info", title: n.message });
            break;
        }
      }),
    );
    return () => {
      alive = false;
      unsubs.forEach((u) => u.then((f) => f()));
    };
  }, [pushToast]);

  // Drop pairing prompts the engine no longer knows about.
  useEffect(() => {
    if (!snap) return;
    setPairingRequests((reqs) =>
      reqs.filter((r) => snap.peers.some((p) => p.id === r.device && p.pairing && !p.pairing.local_accepted)),
    );
  }, [snap]);

  const dismissPairing = useCallback((device: string) => setPairingRequests((r) => r.filter((x) => x.device !== device)), []);

  const value = useMemo(
    () => ({ snap, toasts, pushToast, dismissToast, run, pairingRequests, dismissPairing, identifyAt }),
    [snap, toasts, pushToast, dismissToast, run, pairingRequests, dismissPairing, identifyAt],
  );
  return <EngineCtx.Provider value={value}>{children}</EngineCtx.Provider>;
}

export function useEngine(): Ctx {
  const c = useContext(EngineCtx);
  if (!c) throw new Error("useEngine outside provider");
  return c;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

export function cipherLabel(c: string | null | undefined): string {
  switch (c) {
    case "aes-256-gcm":
      return "AES-256-GCM";
    case "chacha20-poly1305":
      return "ChaCha20-Poly1305";
    case "auto":
      return "Automatic";
    default:
      return "—";
  }
}
