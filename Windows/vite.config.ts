import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

// The interface is the Linux one (Linux/ui, with its packages in
// Linux/node_modules), built here with Windows wording where the two differ.

/** [file, Linux text, Windows text] */
const WINDOWS_TEXT: [string, string, string][] = [
  [
    "/src/pages/Diagnostics.tsx",
    "If they ever stop responding here, press Super+Shift+Esc to get them back.",
    "If they ever stop responding here, press Ctrl+Alt+Shift+Esc to get them back.",
  ],
];

function windowsText(): Plugin {
  const done = new Set<number>();
  return {
    name: "kvmbridge-windows-text",
    enforce: "pre",
    transform(code, id) {
      let out = code;
      WINDOWS_TEXT.forEach(([file, from, to], i) => {
        if (id.replace(/\\/g, "/").endsWith(file) && out.includes(from)) {
          out = out.replace(from, to);
          done.add(i);
        }
      });
      return out === code ? null : out;
    },
    buildEnd() {
      // The Linux wording changed: update WINDOWS_TEXT rather than ship it.
      WINDOWS_TEXT.forEach(([file, from], i) => {
        if (!done.has(i)) this.error(`Windows text: "${from}" not found in ${file}`);
      });
    },
  };
}

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  root: fileURLToPath(new URL("../Linux/ui", import.meta.url)),
  plugins: [windowsText(), react()],
  clearScreen: false,
  build: {
    outDir: fileURLToPath(new URL("./dist", import.meta.url)),
    emptyOutDir: true,
    target: "es2021",
    sourcemap: false,
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
});
