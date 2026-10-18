import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
export const topic = {
  id: "connect",
  title: "Connect devices",
  summary: "Find a peer",
  category: "connection",
  keywords: ["连接", "network"],
  platforms: ["macos", "windows", "linux"],
  document: "topics/connect.md",
};
export async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "mesh-help-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, "docs/help/topics"), { recursive: true });
  await writeFile(
    join(root, "Cargo.toml"),
    '[workspace.package]\nversion = "0.4.0"\n',
  );
  await writeFile(
    join(root, "docs/help/catalog.json"),
    JSON.stringify({ schemaVersion: 1, language: "en", topics: [topic] }),
  );
  await writeFile(
    join(root, "docs/help/topics/connect.md"),
    "# Connect devices\n\n## Start\n\n1. Open **Settings**.\n2. Use `Connection`.\n\n[Again](connect.md)\n",
  );
  return root;
}
