import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";

// https://vitejs.dev/config/
export default defineConfig(({ command, mode }) => {
  const lite = mode === "lite" || process.env.MESH_TALK_VARIANT === "lite";
  const market = new URL("../site/market/", import.meta.url);
  const catalog = JSON.parse(
    readFileSync(new URL("catalog.json", market), "utf8"),
  ) as {
    id: string;
    kind: "avatar" | "theme" | "sticker";
    preinstall?: boolean;
    file: string;
    sha256: string;
  }[];
  const bundled = lite
    ? []
    : catalog.filter(
        ({ kind, preinstall }) => preinstall ?? kind !== "sticker",
      );
  return {
    define: {
      "import.meta.env.VITE_BUNDLED_PACK_IDS": JSON.stringify(
        bundled.map(({ id }) => id).join(","),
      ),
    },
    plugins: [
      react(),
      {
        name: "mesh-talk-bundled-packs",
        configureServer(server) {
          server.middlewares.use((request, response, next) => {
            const name = request.url?.split("?", 1)[0];
            const entry = bundled.find(
              ({ id }) => name === `/builtin-packs/${id}.zip`,
            );
            if (!entry) return next();
            response.setHeader("Content-Type", "application/zip");
            response.end(readFileSync(new URL(entry.file, market)));
          });
        },
        buildStart() {
          if (command !== "build") return;
          for (const entry of bundled) {
            const source = readFileSync(new URL(entry.file, market));
            const hash = createHash("sha256").update(source).digest("hex");
            if (hash !== entry.sha256)
              throw new Error(`Bundled pack checksum mismatch: ${entry.id}`);
            this.emitFile({
              type: "asset",
              fileName: `builtin-packs/${entry.id}.zip`,
              source,
            });
          }
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
  };
});
