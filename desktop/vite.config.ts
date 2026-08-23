import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri serves this build from a custom protocol, so everything must be
// relative and self-contained: no CDN, no absolute origins.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    // The webview is always current, so there is nothing to transpile down for.
    target: "esnext",
    sourcemap: false,
  },
});
