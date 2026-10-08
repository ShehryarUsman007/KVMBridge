import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import Identify from "./Identify";
import { EngineProvider } from "./store";
import "./styles.css";

const isIdentify = window.location.hash.startsWith("#/identify");
document.documentElement.classList.toggle("overlay", isIdentify);

// Use the last known appearance for the first paint (before settings load).
try {
  const a = localStorage.getItem("kvmb-appearance");
  if (a === "light" || a === "dark") document.documentElement.dataset.theme = a;
} catch {
  /* storage unavailable */
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {isIdentify ? (
      <Identify />
    ) : (
      <EngineProvider>
        <App />
      </EngineProvider>
    )}
  </React.StrictMode>,
);

// No browser context menu / reload shortcuts in the desktop app.
if (!isIdentify && !import.meta.env.DEV) {
  window.addEventListener("contextmenu", (e) => {
    const t = e.target as HTMLElement;
    if (!t.closest("input, textarea, pre")) e.preventDefault();
  });
}
