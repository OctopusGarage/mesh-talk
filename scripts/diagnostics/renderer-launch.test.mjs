import assert from "node:assert/strict";
import test from "node:test";
import { resolve } from "node:path";
import { rendererLaunch, ownedExecutablePid } from "./renderer-launch.mjs";

test("ordinary release launches and verifies the same executable", () => {
  const config = rendererLaunch("target/release/mesh-talk");
  assert.equal(config.launcher, resolve("target/release/mesh-talk"));
  assert.equal(config.executable, config.launcher);
  assert.equal(config.source, "dev");
});
test("AppRun launch still verifies the actual packaged ELF instead of the shell wrapper", () => {
  const config = rendererLaunch("/tmp/package/AppRun", {
    NATIVE_RENDERER_EXECUTABLE: "/tmp/package/usr/bin/mesh-talk",
    NATIVE_RENDERER_SOURCE: "published-appimage",
  });
  assert.equal(config.launcher, "/tmp/package/AppRun");
  assert.equal(config.executable, "/tmp/package/usr/bin/mesh-talk");
  assert.equal(config.source, "published-appimage");
});
test("rejects missing launcher and unrecognized package provenance", () => {
  assert.throws(() => rendererLaunch(""));
  assert.throws(() => rendererLaunch("/tmp/app", { NATIVE_RENDERER_SOURCE: "unknown" }));
});
test("selects the actual executable under the owned driver regardless of AppRun argv0", () => {
  const records = [
    { pid: 10, parentPid: 1, executable: "/driver" },
    { pid: 20, parentPid: 10, executable: "/native-driver" },
    { pid: 30, parentPid: 20, executable: "/AppRun" },
    { pid: 40, parentPid: 30, executable: "/app", argv0: "mesh-talk" },
    { pid: 50, parentPid: 1, executable: "/app" },
  ];
  assert.equal(ownedExecutablePid(records, "/app", 10), 40);
  assert.equal(ownedExecutablePid(records, "/other/app", 10), null);
});
test("does not accept unrelated matching binaries, unknown ancestry or ancestry cycles", () => {
  for (const records of [
    [{ pid: 20, parentPid: 1, executable: "/app" }],
    [{ pid: 20, parentPid: 30, executable: "/app" }],
    [{ pid: 20, parentPid: 30, executable: "/app" }, { pid: 30, parentPid: 20 }],
  ]) assert.equal(ownedExecutablePid(records, "/app", 10), null);
});
test("rejects ambiguous owned application processes instead of guessing a cleanup target", () => {
  assert.throws(() => ownedExecutablePid([
    { pid: 20, parentPid: 10, executable: "/app" },
    { pid: 30, parentPid: 10, executable: "/app" },
  ], "/app", 10));
});
