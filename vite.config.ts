import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

/**
 * Vite marks its emitted `<script>`/`<link>` tags `crossorigin`, which turns
 * loading them into a CORS request. In a packaged Tauri app the page is served
 * from a custom protocol origin (`http://tauri.localhost` on Windows), where
 * that check fails and the asset is dropped without a visible error — the app
 * opens as an empty window. Nothing here is cross-origin, so the attribute has
 * no purpose beyond breaking the build.
 */
function stripCrossorigin(): Plugin {
  return {
    name: "mailer:strip-crossorigin",
    enforce: "post",
    transformIndexHtml(html) {
      return html.replace(/\s+crossorigin(=["'][^"']*["'])?/g, "");
    },
  };
}

export default defineConfig({
  plugins: [react(), stripCrossorigin()],
  // Relative asset URLs resolve under every protocol Tauri serves from.
  base: "./",
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**", "**/crates/**", "**/target/**"],
    },
  },
  build: {
    target: "es2022",
    sourcemap: false,
    rollupOptions: {
      output: {
        // KaTeX is bigger than the rest of the app put together. It is loaded
        // from local disk in a packaged build, so splitting it buys no latency —
        // it keeps one 600 kB blob from being rebuilt and re-read on every edit
        // to a component, and keeps the size warning meaningful.
        manualChunks: {
          katex: ["katex"],
          markdown: ["marked", "dompurify"],
        },
      },
    },
  },
});
