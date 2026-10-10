import { defineConfig } from "vite";
import { fileURLToPath, URL } from "node:url";

export default defineConfig({
  root: fileURLToPath(new URL("./studio/", import.meta.url)),
  base: "./",
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src/", import.meta.url)) },
  },
  build: {
    outDir: fileURLToPath(new URL("../site/market/studio/", import.meta.url)),
    emptyOutDir: true,
  },
});
