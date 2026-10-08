import { api } from "../api";
import { useEngine } from "../store";
import { IconAlert, IconCheck, IconInfo, IconShield, IconX } from "./Icons";

export function Toasts() {
  const { toasts, dismissToast } = useEngine();
  return (
    <div className="toasts" aria-live="polite">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`}>
          <span className="toast-icon">{t.kind === "error" ? <IconAlert /> : t.kind === "success" ? <IconCheck /> : <IconInfo />}</span>
          <div className="grow">
            <div className="toast-title">{t.title}</div>
            {t.body && <div className="toast-body">{t.body}</div>}
          </div>
          {t.action && (
            <button
              className="btn small"
              onClick={() => {
                t.action!.run();
                dismissToast(t.id);
              }}
            >
              {t.action.label}
            </button>
          )}
          <button className="icon-btn" onClick={() => dismissToast(t.id)} aria-label="Dismiss">
            <IconX size={14} />
          </button>
        </div>
      ))}
    </div>
  );
}

/** Pairing confirmation: shown for incoming requests and for pairings we started. */
export function PairingDialog() {
  const { snap, run, pairingRequests, dismissPairing } = useEngine();
  if (!snap) return null;
  // Prefer live state from the snapshot; fall back to the notice.
  const active = snap.peers.find((p) => p.pairing && !p.pairing.local_accepted);
  const waiting = snap.peers.find((p) => p.pairing && p.pairing.local_accepted && !p.pairing.remote_accepted);
  const req = active ?? null;
  const fallback = !req && pairingRequests[0];
  if (!req && !fallback && !waiting) return null;

  if (!req && waiting) {
    return (
      <div className="modal-backdrop">
        <div className="modal card">
          <div className="modal-icon">
            <IconShield size={28} />
          </div>
          <h2>Waiting for {waiting.name}</h2>
          <p className="muted">Confirm this code on {waiting.name}.</p>
          <div className="sas">{waiting.pairing!.sas}</div>
          <div className="modal-actions">
            <button className="btn ghost" onClick={() => run(api.rejectPair(waiting.id))}>
              Cancel
            </button>
          </div>
        </div>
      </div>
    );
  }

  const id = req ? req.id : fallback ? fallback.device : "";
  const name = req ? req.name : fallback ? fallback.name : "";
  const sas = req ? req.pairing!.sas : fallback ? fallback.sas : "";
  const incoming = req ? req.pairing!.incoming : true;
  const remoteOk = req?.pairing?.remote_accepted ?? false;
  return (
    <div className="modal-backdrop">
      <div className="modal card" role="dialog" aria-modal="true" aria-labelledby="pair-title">
        <div className="modal-icon">
          <IconShield size={28} />
        </div>
        <h2 id="pair-title">{incoming ? `Pairing request from ${name}` : `Pair with ${name}`}</h2>
        <p className="muted">Make sure {name} shows the same code before you continue.</p>
        <div className="sas">{sas}</div>
        {remoteOk && <p className="small ok-text">{name} already confirmed the code.</p>}
        <div className="modal-actions">
          <button
            className="btn ghost"
            onClick={() => {
              dismissPairing(id);
              run(api.rejectPair(id));
            }}
          >
            Cancel
          </button>
          <button
            className="btn primary"
            onClick={() => {
              dismissPairing(id);
              run(api.acceptPair(id));
            }}
          >
            Pair
          </button>
        </div>
      </div>
    </div>
  );
}
