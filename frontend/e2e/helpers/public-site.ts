import { createServer } from "node:http";
import { readFile, mkdtemp, rm } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { resolve, sep, extname } from "node:path";
import { fileURLToPath } from "node:url";

/** Exercise the same /mesh-talk/ origin and relative assets as GitHub Pages.
 * WebKit's file:// download restrictions are unrelated to the hosted site. */
export async function servePublicSite() {
  const root = await mkdtemp(resolve(tmpdir(), "mesh-public-site-"));
  try {
    execFileSync(process.execPath, [
      fileURLToPath(
        new URL("../../../tools/help/build-site.mjs", import.meta.url),
      ),
      "--out",
      root,
    ]);
  } catch (error) {
    await rm(root, { recursive: true, force: true });
    throw error;
  }
  const server = createServer(async (request, response) => {
    try {
      const pathname = decodeURIComponent(
        new URL(request.url ?? "/", "http://localhost").pathname,
      );
      if (!pathname.startsWith("/mesh-talk/")) throw Error("Not found");
      const relative =
        pathname.slice("/mesh-talk/".length) +
        (pathname.endsWith("/") ? "index.html" : "");
      const file = resolve(root, relative);
      if (!file.startsWith(root + sep)) throw Error("Not found");
      const types: Record<string, string> = {
        ".html": "text/html",
        ".css": "text/css",
        ".js": "text/javascript",
        ".mjs": "text/javascript",
        ".json": "application/json",
        ".md": "text/markdown; charset=utf-8",
        ".txt": "text/plain; charset=utf-8",
        ".xml": "application/xml",
        ".webp": "image/webp",
        ".woff2": "font/woff2",
      };
      response.setHeader(
        "Content-Type",
        types[extname(file)] ?? "application/octet-stream",
      );
      response.end(await readFile(file));
    } catch {
      response.writeHead(404, { "Content-Type": "text/html" });
      response.end(await readFile(resolve(root, "404.html")));
    }
  });
  await new Promise<void>((done, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", done);
  });
  const address = server.address();
  if (!address || typeof address === "string")
    throw Error("Missing site address");
  return {
    url: `http://127.0.0.1:${address.port}/mesh-talk/index.html`,
    close: async () => {
      await new Promise<void>((done, reject) =>
        server.close((error) => (error ? reject(error) : done())),
      );
      await rm(root, { recursive: true, force: true });
    },
  };
}
