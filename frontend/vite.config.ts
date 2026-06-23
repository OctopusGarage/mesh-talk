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
    // This is a Tauri desktop app: the bundle loads from the local filesystem inside the
    // app, never over a network. Vite's 500 kB warning targets web download/waterfall cost,
    // which doesn't apply here — a single ~750 kB chunk parses in milliseconds off disk, far
    // below the Argon2 login KDF that dominates cold start. So we don't code-split (its only
    // win is parallel network fetches); we raise the threshold to keep the build log honest.
    chunkSizeWarningLimit: 1000,
  },
  server: { port: 5173, strictPort: true },
  test: { environment: "node", globals: false, include: ["src/**/*.test.ts"] },
});
