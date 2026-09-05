import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { resolve } from "path";

// Tauri expects a fixed dev port and no clearing of the terminal.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        settings: resolve(__dirname, "settings.html"),
        about: resolve(__dirname, "about.html"),
        baseline: resolve(__dirname, "baseline.html"),
        themes: resolve(__dirname, "themes.html"),
        fonts: resolve(__dirname, "fonts.html"),
      },
    },
  },
  server: {
    port: 1450,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1451 } : undefined,
    watch: {
      // Don't watch the Rust side; Cargo handles that.
      ignored: ["**/src-tauri/**"],
    },
  },
});
