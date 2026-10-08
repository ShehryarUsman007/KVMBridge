import { api } from "../api";
import { useEngine } from "../store";

/** Choose the computer whose keyboard and mouse control all screens. */
export function ControllerPicker() {
  const { snap, run } = useEngine();
  if (!snap) return null;
  const nameOf = (id: string) =>
    id === snap.me.id ? snap.me.name : snap.peers.find((p) => p.id === id)?.name ?? snap.layout.slots.find((s) => s.device === id)?.name ?? "";
  const computers = snap.layout.slots.map((s) => s.device);
  const controller = snap.layout.controller;

  return (
    <section className="card controller-picker">
      <h2>Keyboard and mouse</h2>
      <p className="muted">
        Choose the computer whose keyboard and mouse control all screens. The other computers' own keyboards and mice are
        paused while it's connected, so they can't get in the way.
      </p>
      <label className="field">
        <span className="field-label">Controlled from</span>
        <select className="input" value={controller ?? ""} onChange={(e) => run(api.setController(e.target.value || null))}>
          <option value="">Any computer</option>
          {computers.map((id) => (
            <option key={id} value={id}>
              {nameOf(id)}
              {id === snap.me.id ? " (this computer)" : ""}
            </option>
          ))}
        </select>
      </label>
      {controller ? (
        <p className="small muted hint">
          If a paused keyboard is ever needed, press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>Esc</kbd> on it.
          That gives that computer its keyboard and mouse back and turns KVM Bridge off there.
        </p>
      ) : (
        <p className="small muted hint">Every computer's keyboard and mouse can move to the other screens.</p>
      )}
    </section>
  );
}
