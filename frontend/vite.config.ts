import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";
import { loadHelp } from "../tools/help/load.mjs";

// https://vitejs.dev/config/
export default defineConfig(() => {
  return {
    define: {
      "import.meta.env.VITE_BUNDLED_PACK_IDS": JSON.stringify(""),
    },
    publicDir: false,
    plugins: [
      react(),
      {
        name: "mesh-talk-help-content",
        async buildStart() {
          await loadHelp({
            root: fileURLToPath(new URL("../", import.meta.url)),
          });
        },
      },
    ],
    resolve: {
      alias: {
        "@": fileURLToPath(new URL("./src", import.meta.url)),
      },
    },
    build: {
      outDir: "dist",
      // macOS 12 WKWebView can reject Radix's class static blocks, which Vite's
      // default Safari 16.4 target leaves intact.
      target: "safari15",
      rolldownOptions: {
        output: {
          codeSplitting: {
            groups: [
              {
                // Keep the growing offline language catalog separate from UI code.
                name: "locales",
                test: /[\\/]src[\\/]locales[\\/]/,
              },
              {
                name: "react-vendor",
                // Keep React, its renderer and scheduler together so the shared
                // startup chunk stays small as application features grow.
                test: /[\\/]node_modules[\\/](?:react|react-dom|scheduler)[\\/]/,
              },
            ],
          },
        },
      },
      // The sign-in shell and authenticated chat are separate chunks. Assets load locally in
      // Tauri, so a modestly higher limit than Vite's web default is appropriate here.
      chunkSizeWarningLimit: 700,
    },
    server: {
      port: Number(process.env.MESH_TALK_E2E_PORT ?? 5173),
      strictPort: true,
      // A dev-server reconnect can reload the page mid-test and erase mock
      // authentication state. Playwright never edits source while it runs.
      hmr: process.env.MESH_TALK_E2E === "1" ? false : undefined,
    },
    test: {
      environment: "node",
      globals: false,
      include: ["src/**/*.test.ts"],
      coverage: {
        provider: "v8",
        include: ["src/**/*.{ts,tsx}"],
        exclude: ["src/**/*.test.ts", "src/**/*.d.ts"],
        reporter: ["text", "json-summary", "lcov"],
        thresholds: {
          "src/store/**/*.ts": { lines: 70 },
        },
      },
    },
  };
});
