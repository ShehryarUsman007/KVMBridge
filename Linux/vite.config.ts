import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://v2.tauri.app/start/frontend/vite/
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  root: "ui",
  plugins: [react()],
  clearScreen: false,
  build: {
    outDir: "../dist",
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
