import assert from "node:assert/strict";
import { cp, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { build } from "../../frontend/node_modules/vite/dist/node/index.js";

test("production renderer and hooks share React despite duplicate dependency trees", async t => {
  const root = await mkdtemp(path.join(tmpdir(), "mesh-react-singleton-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const frontend = fileURLToPath(new URL("../../frontend/", import.meta.url));
  await mkdir(path.join(root, "node_modules", "node_modules"), { recursive: true });
  for (const name of ["react", "react-dom", "scheduler"]) {
    await cp(path.join(frontend, "node_modules", name), path.join(root, "node_modules", name), { recursive: true });
  }
  // A copied dependency cache can contain this second tree. The renderer must
  // still use the same hook dispatcher as the application's direct import.
  for (const name of ["react", "react-dom"]) {
    await cp(path.join(frontend, "node_modules", name), path.join(root, "node_modules", "node_modules", name), { recursive: true });
  }
  await writeFile(path.join(root, "package.json"), JSON.stringify({ type: "module" }));
  await writeFile(path.join(root, "index.html"), '<div id="root"></div><div id="portal"></div><script type="module" src="/main.jsx"></script>');
  await writeFile(path.join(root, "main.jsx"), `
    import React, {useRef} from 'react';
    import {createRoot} from 'react-dom/client';
    import {createPortal} from 'react-dom';
    function App() {
      const value = useRef('ready');
      return <>{value.current}{createPortal(<span>portal</span>, document.getElementById('portal'))}</>;
    }
    createRoot(document.getElementById('root')).render(<App/>);
  `);
  const result = await build({
    configFile: path.join(frontend, "vite.config.ts"),
    root,
    logLevel: "error",
    build: { write: false },
  });
  const chunks = result.output.filter(chunk => chunk.type === "chunk");
  const modules = chunks.flatMap(chunk => Object.keys(chunk.modules));
  for (const name of ["react", "react-dom"]) {
    const runtime = new RegExp(`[/\\\\]${name}[/\\\\]cjs[/\\\\]${name}\\.production\\.js$`);
    assert.equal(modules.filter(id => runtime.test(id)).length, 1, `${name} must have exactly one production runtime`);
  }
});
