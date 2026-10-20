import assert from "node:assert/strict";
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { runInNewContext } from "node:vm";

const load = () => import("./native-capture.mjs");
test("driver provenance follows the exact Cargo.lock package rather than another version", async () => {
  const { nativeDriverEvidence } = await load();
  const lock = 'version = 4\n[[package]]\nname = "other"\nversion = "9.0.0"\n[[package]]\nname = "tauri-plugin-wdio-webdriver"\nversion = "1.5.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "abc"\n';
  assert.deepEqual(nativeDriverEvidence(lock), { crate: "tauri-plugin-wdio-webdriver", version: "1.5.0", source: "registry+https://github.com/rust-lang/crates.io-index", checksum: "abc", evidence: "Cargo.lock" });
  assert.throws(() => nativeDriverEvidence('version = "1.5.0"'), /exactly one/);
  assert.throws(() => nativeDriverEvidence(lock + lock), /exactly one/);
  const actual = nativeDriverEvidence(await readFile(new URL("../../Cargo.lock", import.meta.url), "utf8"));
  assert.match(actual.version, /^\d+\.\d+\.\d+/);
});

function webview({ hidden = false, status = "awaiting", moved = false } = {}) {
  let shots = 0, frames = 0, samples = 0;
  const bounds = { x: 0, y: 10, width: 100, height: 50, top: 10, bottom: 60, left: 0, right: 100 };
  const badge = { getAttribute: () => status, getBoundingClientRect: () => ({ ...bounds, top: hidden ? 900 : 40, bottom: hidden ? 920 : 60 }) };
  const bubble = { textContent: "receipt text", getBoundingClientRect: () => ({ ...bounds, x: moved && ++samples > 1 ? 30 : 0 }), querySelectorAll: () => [badge] };
  const document = { querySelectorAll: () => [bubble], querySelector: () => ({ getBoundingClientRect: () => ({ top: 0, bottom: 500, left: 0, right: 500 }) }) };
  return {
    command: async (_method, path, body) => {
      if (path === "/execute/async") return new Promise(resolve => {
        const args = [resolve];
        runInNewContext(body.script, { arguments: args, requestAnimationFrame: callback => setImmediate(() => { frames++; callback(); }) });
      });
      assert.equal(path, "/screenshot"); shots++;
      assert.ok(frames >= 2, "native screenshot follows two actual frame callbacks");
      return Buffer.from("png bytes").toString("base64");
    },
    execute: async (script, args) => runInNewContext(`(function(){${script}}).apply(null, args)`, { args, document, innerWidth: 500, innerHeight: 500 }),
    until: async (_description, poll) => { for (let i = 0; i < 3; i++) { const result = await poll(); if (result) return result; } throw new Error("capture never settled"); },
    shots: () => shots,
  };
}

test("native capture waits for painted visible receipt state and retains its observation", async () => {
  const { captureNativeEvidence } = await load();
  const c = webview();
  const result = await captureNativeEvidence(c, { label: "receipt text", status: "awaiting" });
  assert.equal(result.observation.label, "receipt text");
  assert.equal(result.observation.status, "awaiting");
  assert.equal(result.observation.visible, true);
  assert.equal(c.shots(), 1);
});
test("native capture rejects a matching DOM receipt whose status is outside the viewport", async () => {
  const { captureNativeEvidence } = await load();
  for (const config of [{ hidden: true }, { status: "delivered" }]) {
    const c = webview(config);
    await assert.rejects(captureNativeEvidence(c, { label: "receipt text", status: "awaiting" }), /never settled/);
    assert.equal(c.shots(), 0, "unpainted or wrong state must never become PNG evidence");
  }
});
test("native capture rejects layout changes during the screenshot", async () => {
  const { captureNativeEvidence } = await load();
  const c = webview({ moved: true });
  // A later stable retry is allowed; the first moving sample is discarded.
  await captureNativeEvidence(c, { label: "receipt text", status: "awaiting" });
  assert.equal(c.shots(), 2);
});

test("auth error capture requires a fully visible localized alert on the login form", async () => {
  const { captureNativeEvidence } = await load();
  for (const config of [ { text: "用户名或密码错误", top: 40, accepted: true }, { text: "decrypt failed", top: 40 }, { text: "用户名或密码错误", top: 700 }, { text: "用户名或密码错误", top: 40, chat: true }, { text: "用户名或密码错误", top: 40, opacity: "0" } ]) {
    const c = webview();
    const rect = { x: 10, y: config.top, top: config.top, bottom: config.top + 20, left: 10, right: 150, width: 140, height: 20 };
    const alert = { textContent: config.text, getBoundingClientRect: () => rect };
    const form = { getBoundingClientRect: () => ({ x: 0, y: 0, top: 0, left: 0, right: 300, bottom: 500, width: 300, height: 500 }) };
    const document = { querySelector: selector => selector.includes("[role=alert]") ? alert : selector.includes("chat-shell") ? config.chat : form };
    c.execute = async (script,args) => runInNewContext(`(function(){${script}}).apply(null,args)`, { document, args, innerWidth: 500, innerHeight: 500, getComputedStyle: () => ({ visibility: "visible", opacity: config.opacity ?? "1", display: "block" }) });
    const capture = captureNativeEvidence(c,{ kind: "auth-error", texts: ["用户名或密码错误"] });
    if (config.accepted) { assert.equal((await capture).observation.text, config.text); assert.equal(c.shots(), 1); }
    else { await assert.rejects(capture,/never settled/); assert.equal(c.shots(),0); }
  }
});
