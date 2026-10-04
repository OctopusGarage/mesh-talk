import assert from "node:assert/strict";
import { resolve } from "node:path";

export function graphicsConfigurationFromLog(log, pid) {
  assert.ok(Number.isInteger(pid) && pid > 0, "verified application PID required");
  const keys = ["XDG_SESSION_TYPE", "GDK_BACKEND", "WEBKIT_DISABLE_DMABUF_RENDERER",
    "WEBKIT_DISABLE_COMPOSITING_MODE"];
  const values = ["unset", "custom", "x11", "wayland", "tty", "x11,wayland", "wayland,x11", "0", "1"];
  const configuration = {};
  for (const row of log.matchAll(/Linux graphics configuration: pid=(\d+) ([A-Z_]+)=([a-z0-9,]+)/g)) {
    if (Number(row[1]) === pid && keys.includes(row[2]) && values.includes(row[3])) {
      configuration[row[2]] = row[3];
    }
  }
  return configuration;
}

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
  const compatibility = environment.NATIVE_RENDERER_COMPAT ?? "0";
  assert.ok(["0", "1"].includes(compatibility), "explicit compatibility probe mode required");
  assert.ok(compatibility !== "1" || source === "dev", "new flag requires a current dev binary");
  return { launcher: resolve(path),
    executable: resolve(environment.NATIVE_RENDERER_EXECUTABLE ?? path), source,
    args: compatibility === "1" ? ["--linux-renderer-compat"] : [] };
}
