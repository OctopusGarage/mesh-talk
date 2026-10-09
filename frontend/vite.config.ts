import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  build: {
    outDir: "dist",
    // The sign-in shell and authenticated chat are separate chunks. Assets load locally in
    // Tauri, so a modestly higher limit than Vite's web default is appropriate here.
    chunkSizeWarningLimit: 700,
  },
  server: {
    port: Number(process.env.MESH_TALK_E2E_PORT ?? 5173),
    strictPort: true,
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
});
