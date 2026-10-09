import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { disable as autostartOff, enable as autostartOn, isEnabled as autostartEnabled } from "@tauri-apps/plugin-autostart";
import { api, CipherPreference, inTauri, Settings, Timing } from "../api";
import { cipherLabel, formatBytes, useEngine } from "../store";
import { evdevFromCode, keyName, sortKeys } from "../keycodes";
import { BackupSection } from "../components/Backup";
import { IconFolder } from "../components/Icons";

const PRESETS: Record<string, Timing> = {
  "low-latency": { motion_interval_us: 0, heartbeat_ms: 500, heartbeat_misses: 6, discovery_ms: 1000, clipboard_poll_ms: 250 },
  balanced: { motion_interval_us: 4000, heartbeat_ms: 1000, heartbeat_misses: 5, discovery_ms: 2000, clipboard_poll_ms: 500 },
  "power-saver": { motion_interval_us: 16000, heartbeat_ms: 3000, heartbeat_misses: 4, discovery_ms: 5000, clipboard_poll_ms: 1500 },
};
const PRESET_LABELS: Record<string, [string, string]> = {
  "low-latency": ["Low latency", "Sends every mouse movement."],
  balanced: ["Balanced", "Up to 250 mouse updates per second."],
  "power-saver": ["Power saver", "For laptops on battery or busy Wi-Fi."],
};

function same(a: Timing, b: Timing) {
  return (Object.keys(a) as (keyof Timing)[]).every((k) => a[k] === b[k]);
}

function Toggle({ checked, onChange, label, hint }: { checked: boolean; onChange: (v: boolean) => void; label: string; hint?: string }) {
  return (
    <label className="toggle-row">
      <div className="grow">
        <div>{label}</div>
        {hint && <div className="muted small">{hint}</div>}
      </div>
      <span className={`switch ${checked ? "on" : ""}`}>
        <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
        <span className="knob" />
      </span>
    </label>
  );
}

function Slider({
  label,
  value,
  min,
  max,
  step,
  onChange,
  format,
  hint,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
  format: (v: number) => string;
  hint?: string;
}) {
  return (
    <div className="slider-row">
      <div className="slider-head">
        <span>{label}</span>
        <span className="slider-value">{format(value)}</span>
      </div>
      <input type="range" min={min} max={max} step={step} value={value} onChange={(e) => onChange(Number(e.target.value))} />
      {hint && <div className="muted small">{hint}</div>}
    </div>
  );
}

function HotkeyInput({ value, onChange }: { value: number[]; onChange: (v: number[]) => void }) {
  const [recording, setRecording] = useState(false);
  useEffect(() => {
    if (!recording) return;
    const held = new Set<number>();
    const seen = new Set<number>();
    const down = (e: KeyboardEvent) => {
      e.preventDefault();
      const k = evdevFromCode(e.code);
      if (k !== undefined) {
        held.add(k);
        seen.add(k);
      }
    };
    const up = (e: KeyboardEvent) => {
      e.preventDefault();
      const k = evdevFromCode(e.code);
      if (k !== undefined) held.delete(k);
      if (held.size === 0 && seen.size > 0) {
        onChange(sortKeys([...seen]));
        setRecording(false);
      }
    };
    window.addEventListener("keydown", down, true);
    window.addEventListener("keyup", up, true);
    return () => {
      window.removeEventListener("keydown", down, true);
      window.removeEventListener("keyup", up, true);
    };
  }, [recording, onChange]);
  return (
    <div className="hotkey">
      <div className={`hotkey-keys ${recording ? "recording" : ""}`}>
        {recording ? (
          <span className="muted">Press the keys, then let go…</span>
        ) : value.length ? (
          value.map((k, i) => (
            <span key={i}>
              {i > 0 && <span className="plus">+</span>}
              <kbd>{keyName(k)}</kbd>
            </span>
          ))
        ) : (
          <span className="muted">Not set</span>
        )}
      </div>
      <button className="btn" onClick={() => setRecording(!recording)}>
        {recording ? "Cancel" : "Change"}
      </button>
    </div>
  );
}

/** Marks settings that every paired computer shares. */
function SharedNote() {
  return <span className="muted small"> · all computers</span>;
}

/** A number field that applies its value when it loses focus or on Enter. */
function PortInput({ value, onCommit }: { value: number; onCommit: (v: number) => void }) {
  const [text, setText] = useState<string | null>(null);
  const commit = () => {
    const v = Number(text);
    if (text !== null && Number.isInteger(v) && v >= 1 && v <= 65535 && v !== value) onCommit(v);
    setText(null);
  };
  return (
    <input
      className="input mono"
      type="number"
      min={1}
      max={65535}
      value={text ?? value}
      onChange={(e) => setText(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => e.key === "Enter" && commit()}
    />
  );
}

export function SettingsPage() {
  const { snap, run } = useEngine();
  // The last change made here, shown until the engine's snapshot has it.
  const [draft, setDraft] = useState<Settings | null>(null);
  const timer = useRef<number | null>(null);
  const awaiting = useRef(false);
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [peersText, setPeersText] = useState<string | null>(null);
  const [portsChanged, setPortsChanged] = useState(false);
  const [receiveDir, setReceiveDir] = useState("");

  useEffect(() => {
    if (!inTauri) return;
    autostartEnabled().then(setAutostart).catch(() => setAutostart(null));
  }, []);
  useEffect(() => {
    api.receiveDir().then(setReceiveDir).catch(() => {});
  }, [snap?.settings.transfers.receive_dir]);

  useEffect(() => {
    if (awaiting.current && timer.current === null) {
      awaiting.current = false;
      setDraft(null);
    }
  }, [snap?.settings]);
  useEffect(() => () => {
    if (timer.current !== null) window.clearTimeout(timer.current);
  }, []);

  const s = draft ?? snap?.settings ?? null;
  if (!snap || !s) return null;

  // Changes apply right away (sliders once they stop moving). Shared
  // settings go to every connected computer as soon as the engine has them.
  const set = (patch: (d: Settings) => Settings, delay = 0) => {
    const next = patch(structuredClone(s));
    setDraft(next);
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(async () => {
      timer.current = null;
      const ok = await run(api.setSettings(next));
      if (ok) awaiting.current = true;
      else setDraft(null);
    }, delay);
  };
  const slide = (patch: (d: Settings) => Settings) => set(patch, 300);
  const preset = Object.keys(PRESETS).find((k) => same(PRESETS[k], s.timing)) ?? "custom";

  const chooseCipher = (c: CipherPreference) => {
    if (c === snap.shared.cipher) return;
    run(api.setCipher(c), `Encryption set to ${cipherLabel(c)} on all paired devices`);
  };

  return (
    <div className="page settings-page">
      <div className="page-header">
        <div>
          <h1>Settings</h1>
          <p className="muted">
            Changes apply right away. Settings marked "all computers" are the same on every paired computer: changing
            them here changes them everywhere.
          </p>
        </div>
      </div>

      <section className="card settings-section">
        <h2>Appearance</h2>
        <div className="segmented" role="radiogroup" aria-label="Appearance">
          {(
            [
              ["system", "System"],
              ["light", "Light"],
              ["dark", "Dark"],
            ] as const
          ).map(([v, label]) => (
            <button
              key={v}
              role="radio"
              aria-checked={s.appearance === v}
              className={s.appearance === v ? "on" : ""}
              onClick={() => set((d) => ({ ...d, appearance: v }))}
            >
              {label}
            </button>
          ))}
        </div>
      </section>

      <section className="card settings-section">
        <h2>
          Encryption
          <SharedNote />
        </h2>
        <p className="muted small">
          Input, clipboard and files are always encrypted. Keys are exchanged with X25519 (Noise protocol). Changing the
          cipher applies to every paired computer; they reconnect automatically.
        </p>
        <div className="choice-list">
          {(
            [
              ["auto", "Automatic (recommended)", `AES-256-GCM when the processor has AES instructions, otherwise ChaCha20-Poly1305. This computer ${snap.hw_aes ? "has" : "does not have"} AES instructions.`],
              ["aes-256-gcm", "AES-256-GCM", "Fastest on modern desktop and laptop processors."],
              ["chacha20-poly1305", "ChaCha20-Poly1305", "Fast without special hardware; a good choice for older or low-power machines."],
            ] as [CipherPreference, string, string][]
          ).map(([value, label, desc]) => (
            <label key={value} className={`choice ${snap.shared.cipher === value ? "selected" : ""}`}>
              <input type="radio" name="cipher" checked={snap.shared.cipher === value} onChange={() => chooseCipher(value)} />
              <div>
                <div className="choice-title">{label}</div>
                <div className="muted small">{desc}</div>
              </div>
            </label>
          ))}
        </div>
        <div className="muted small">Currently in use: {cipherLabel(snap.effective_cipher)}.</div>
        <Toggle
          label="Only accept connections that use this cipher"
          hint="Refuse devices that offer a different cipher instead of accepting either one."
          checked={s.strict_cipher}
          onChange={(v) => set((d) => ({ ...d, strict_cipher: v }))}
        />
      </section>

      <section className="card settings-section">
        <h2>
          Update intervals
          <SharedNote />
        </h2>
        <p className="muted small">Shorter intervals feel more responsive; longer ones use less network and battery.</p>
        <div className="preset-row">
          {Object.keys(PRESETS).map((k) => (
            <button key={k} className={`preset ${preset === k ? "selected" : ""}`} onClick={() => set((d) => ({ ...d, timing: { ...PRESETS[k] } }))}>
              <div className="choice-title">{PRESET_LABELS[k][0]}</div>
              <div className="muted small">{PRESET_LABELS[k][1]}</div>
            </button>
          ))}
          <div className={`preset static ${preset === "custom" ? "selected" : ""}`}>
            <div className="choice-title">Custom</div>
            <div className="muted small">Set with the sliders.</div>
          </div>
        </div>
        <Slider
          label="Mouse update interval"
          value={s.timing.motion_interval_us / 1000}
          min={0}
          max={33}
          step={1}
          format={(v) => (v === 0 ? "Every event" : `${v} ms (≈${Math.round(1000 / v)}/s)`)}
          hint="Mouse movement is batched and sent at most this often."
          onChange={(v) => slide((d) => ({ ...d, timing: { ...d.timing, motion_interval_us: v * 1000 } }))}
        />
        <Slider
          label="Heartbeat interval"
          value={s.timing.heartbeat_ms}
          min={100}
          max={10000}
          step={100}
          format={(v) => (v < 1000 ? `${v} ms` : `${(v / 1000).toFixed(1)} s`)}
          hint="How often connected devices check that the other side is still there (also measures latency)."
          onChange={(v) => slide((d) => ({ ...d, timing: { ...d.timing, heartbeat_ms: v } }))}
        />
        <Slider
          label="Missed heartbeats before disconnecting"
          value={s.timing.heartbeat_misses}
          min={2}
          max={30}
          step={1}
          format={(v) => `${v} (≈${((v * s.timing.heartbeat_ms) / 1000).toFixed(1)} s)`}
          onChange={(v) => slide((d) => ({ ...d, timing: { ...d.timing, heartbeat_misses: v } }))}
        />
        <Slider
          label="Clipboard check interval"
          value={s.timing.clipboard_poll_ms}
          min={100}
          max={10000}
          step={100}
          format={(v) => (v < 1000 ? `${v} ms` : `${(v / 1000).toFixed(1)} s`)}
          hint="Only used on systems that can't notify about clipboard changes."
          onChange={(v) => slide((d) => ({ ...d, timing: { ...d.timing, clipboard_poll_ms: v } }))}
        />
      </section>

      <section className="card settings-section">
        <h2>
          Clipboard
          <SharedNote />
        </h2>
        <Toggle
          label="Share the clipboard"
          hint="Copy on one computer, paste on another."
          checked={s.clipboard.enabled}
          onChange={(v) => set((d) => ({ ...d, clipboard: { ...d.clipboard, enabled: v } }))}
        />
        <Toggle
          label="Include copied files"
          hint="Files are transferred when you paste them."
          checked={s.clipboard.files}
          onChange={(v) => set((d) => ({ ...d, clipboard: { ...d.clipboard, files: v } }))}
        />
        <Slider
          label="Largest text or image to share"
          value={Math.round(s.clipboard.max_bytes / (1024 * 1024))}
          min={1}
          max={256}
          step={1}
          format={(v) => formatBytes(v * 1024 * 1024)}
          onChange={(v) => slide((d) => ({ ...d, clipboard: { ...d.clipboard, max_bytes: v * 1024 * 1024 } }))}
        />
      </section>

      <section className="card settings-section">
        <h2>Files and drag and drop</h2>
        <Toggle
          label="Drag and drop between screens (all computers)"
          hint="Drag files from a file manager on one computer and drop copies of them in a folder or application on another computer's screen."
          checked={s.transfers.drag_and_drop}
          onChange={(v) => set((d) => ({ ...d, transfers: { ...d.transfers, drag_and_drop: v } }))}
        />
        <div className="field">
          <label className="field-label">Save received files in</label>
          <div className="row">
            <input className="input mono" readOnly value={s.transfers.receive_dir ?? receiveDir} />
            <button
              className="btn"
              onClick={async () => {
                const dir = await open({ directory: true, multiple: false });
                if (typeof dir === "string") set((d) => ({ ...d, transfers: { ...d.transfers, receive_dir: dir } }));
              }}
            >
              <IconFolder /> Choose…
            </button>
            {s.transfers.receive_dir && (
              <button className="btn ghost" onClick={() => set((d) => ({ ...d, transfers: { ...d.transfers, receive_dir: null } }))}>
                Default
              </button>
            )}
          </div>
        </div>
      </section>

      <section className="card settings-section">
        <h2>
          Shortcuts
          <SharedNote />
        </h2>
        <div className="field">
          <label className="field-label">Bring the cursor back to this computer</label>
          <HotkeyInput value={s.hotkeys.release} onChange={(v) => set((d) => ({ ...d, hotkeys: { ...d.hotkeys, release: v } }))} />
          <div className="muted small">Works while you're controlling another screen.</div>
        </div>
        <div className="field">
          <label className="field-label">Lock the cursor to the current screen (toggle)</label>
          <HotkeyInput value={s.hotkeys.lock} onChange={(v) => set((d) => ({ ...d, hotkeys: { ...d.hotkeys, lock: v } }))} />
        </div>
      </section>

      <section className="card settings-section">
        <h2>Network</h2>
        <Toggle
          label="Let other computers find this one"
          hint="Answers searches from other computers and announces this one once when KVM Bridge starts. Nothing is sent otherwise."
          checked={s.discovery_enabled}
          onChange={(v) => set((d) => ({ ...d, discovery_enabled: v }))}
        />
        <div className="two-col">
          <div className="field">
            <label className="field-label">Connection port (TCP)</label>
            <PortInput
              value={s.listen_port}
              onCommit={(v) => {
                setPortsChanged(true);
                set((d) => ({ ...d, listen_port: v }));
              }}
            />
          </div>
          <div className="field">
            <label className="field-label">Discovery port (UDP)</label>
            <PortInput
              value={s.discovery_port}
              onCommit={(v) => {
                setPortsChanged(true);
                set((d) => ({ ...d, discovery_port: v }));
              }}
            />
          </div>
        </div>
        {portsChanged && <div className="notice warn small">Port changes take effect after restarting KVM Bridge. All devices must use the same discovery port.</div>}
        <div className="field">
          <label className="field-label">Extra devices to contact directly</label>
          <textarea
            className="input mono"
            rows={2}
            placeholder="192.168.1.20, laptop.local:47801"
            value={peersText ?? s.manual_peers.join(", ")}
            onChange={(e) => setPeersText(e.target.value)}
            onBlur={() => {
              if (peersText === null) return;
              const list = peersText.split(/[\s,]+/).map((x) => x.trim()).filter(Boolean);
              setPeersText(null);
              if (list.join(",") !== s.manual_peers.join(",")) set((d) => ({ ...d, manual_peers: list }));
            }}
          />
        </div>
      </section>

      <section className="card settings-section">
        <h2>Startup</h2>
        {autostart !== null && (
          <Toggle
            label="Start KVM Bridge when I log in"
            checked={autostart}
            onChange={async (v) => {
              await run(v ? autostartOn() : autostartOff());
              setAutostart(await autostartEnabled().catch(() => v));
            }}
          />
        )}
        <Toggle
          label="Start minimized"
          checked={s.start_minimized}
          onChange={(v) => set((d) => ({ ...d, start_minimized: v }))}
        />
        <Toggle
          label="Keep running when the window is closed"
          hint="Input sharing keeps working in the background. Open the app again to show the window."
          checked={s.close_to_background}
          onChange={(v) => set((d) => ({ ...d, close_to_background: v }))}
        />
      </section>

      <section className="card settings-section">
        <h2>Backup and restore</h2>
        <p className="muted small">
          Save everything to a .kvmb file (optionally password protected) and load it again later or on another computer.
        </p>
        <BackupSection />
      </section>

    </div>
  );
}
