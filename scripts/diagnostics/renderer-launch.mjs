import assert from "node:assert/strict";
import { resolve } from "node:path";

export function ownedExecutablePid(records, executable, ownerPid) {
  assert.ok(Number.isInteger(ownerPid) && ownerPid > 0);
  const byPid = new Map(records.map(record => [record.pid, record]));
  function owned(record) {
    const visited = new Set([record.pid]);
    let parent = record.parentPid;
    while (Number.isInteger(parent) && parent > 0 && !visited.has(parent)) {
      if (parent === ownerPid) return true;
      visited.add(parent);
      parent = byPid.get(parent)?.parentPid;
    }
    return false;
  }
  const candidates = records.filter(record => record.executable === executable && owned(record));
  assert.ok(candidates.length <= 1, "exactly one owned application process required");
  return candidates[0]?.pid ?? null;
}

export function rendererLaunch(path, environment = {}) {
  assert.ok(path, "explicit launcher path required");
  const source = environment.NATIVE_RENDERER_SOURCE ?? "dev";
  assert.ok(["dev", "published-deb", "published-appimage"].includes(source),
    "known package provenance required");
  return { launcher: resolve(path),
    executable: resolve(environment.NATIVE_RENDERER_EXECUTABLE ?? path), source };
}
