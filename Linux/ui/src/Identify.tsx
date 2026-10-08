// Full screen overlay shown by "Identify screens".

export default function Identify() {
  const params = new URLSearchParams(window.location.hash.split("?")[1] ?? "");
  const n = params.get("n") ?? "?";
  const name = params.get("name") ?? "";
  return (
    <div className="identify">
      <div className="identify-card">
        <div className="identify-label">Screen</div>
        <div className="identify-number">{n === "0" ? "–" : n}</div>
        <div className="identify-name">{name}</div>
      </div>
    </div>
  );
}
