import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri sets TAURI_DEV_HOST when the dev server must be reachable from a device.
const host = process.env["TAURI_DEV_HOST"];

export default defineConfig({
  plugins: [react()],
  // Tauri's CLI owns the terminal; don't wipe its output.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host ?? false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    // Matches the webview floor Tauri supports on each platform.
    target: process.env["TAURI_ENV_PLATFORM"] === "windows" ? "chrome105" : "safari15",
    sourcemap: false,
  },
});
