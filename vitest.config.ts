// Standalone Vitest config (takes precedence over vite.config.ts, which is the
// multi-entry Tauri build config and is not merged here). Runs the Svelte 5
// component + unit test suite in jsdom.
import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { svelteTesting } from "@testing-library/svelte/vite";

export default defineConfig({
  // svelteTesting() wires the "browser" resolve condition (so Svelte's client
  // runtime is used, not the SSR one) and auto-cleans mounted components between
  // tests. The svelte() plugin compiles .svelte and .svelte.ts (runes) files.
  plugins: [svelte(), svelteTesting()],
  test: {
    environment: "jsdom",
    // Left false so svelteTesting()'s auto-cleanup setup is registered (it skips
    // when globals is true). Test files import their APIs from "vitest" directly.
    globals: false,
    setupFiles: ["./vitest.setup.ts"],
    // Only pick up the test suite; never the app entry points.
    include: ["src/**/*.{test,spec}.{ts,js}"],
  },
});
