import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath, URL } from "node:url";

const assetRoot = fileURLToPath(new URL("./src/assets/", import.meta.url));
const catalogId = "virtual:mesh-asset-catalog";
const catalogDirs = [
  "avatars/clubs",
  "avatars/players",
  "avatars/nba-teams",
  "avatars/cities",
  "avatars/nba-players",
  "avatars/sports",
  "avatars/famous",
  "avatars/cat-acrylic",
  "stickers/noto",
  "stickers/cats",
  "themes/nature",
  "themes/cat-acrylic",
];

function assetCatalog() {
  return {
    name: "mesh-asset-catalog",
    resolveId(id: string) {
      if (id === catalogId) return `\0${catalogId}`;
    },
    load(id: string) {
      if (id !== `\0${catalogId}`) return;
      const entries = Object.fromEntries(
        catalogDirs.map((dir) => [
          dir,
          readdirSync(join(assetRoot, dir)).filter((name) =>
            /\.(?:svg|webp|jpg)$/.test(name),
          ),
        ]),
      );
      return `export default ${JSON.stringify(entries)};`;
    },
  };
}

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), assetCatalog()],
  // Public URLs avoid one Vite module request per preset image during chat startup.
  publicDir: "src/assets",
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
