import assert from "node:assert/strict";
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { deflateSync } from "node:zlib";
import { crc32 } from "../evals/png-evidence.mjs";
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

function receiptPng(kind = "painted") {
  const chunk = (type, data) => { const value = Buffer.alloc(data.length + 12); value.writeUInt32BE(data.length); value.write(type,4); data.copy(value,8); value.writeUInt32BE(crc32(value.subarray(4,-4)),value.length-4); return value; };
  const header = Buffer.alloc(13); header.writeUInt32BE(500); header.writeUInt32BE(500,4); header[8]=8; header[9]=2;
  const rows = Buffer.alloc(500 * 1501);
  for (let y=0;y<500;y++) for(let x=0;x<500;x++) {
    const badge = x>=20&&x<80&&y>=40&&y<56;
    const stroke = kind!=="blank" && x%8<(kind==="changed"?4:3) && y>42&&y<54;
    const value = badge ? (stroke?85:247) : x%256;
    rows.set([value,value,badge?value:y%256],y*1501+1+x*3);
  }
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]),chunk("IHDR",header),chunk("IDAT",deflateSync(rows)),chunk("IEND",Buffer.alloc(0))]).toString("base64");
}
const paintedReceiptPng = receiptPng();

function webview({ hidden = false, status = "awaiting", moved = false, screenshots } = {}) {
  let shots = 0, frames = 0, samples = 0;
  const bounds = { x: 0, y: 10, width: 100, height: 50, top: 10, bottom: 60, left: 0, right: 100 };
  const badge = { getAttribute: () => status, getBoundingClientRect: () => ({ x:20,y:hidden?900:40,width:60,height:16,top:hidden?900:40,bottom:hidden?916:56,left:20,right:80 }) };
  const bubble = { textContent: "receipt text", getBoundingClientRect: () => ({ ...bounds, x: moved && ++samples > 1 ? 30 : 0 }), querySelectorAll: () => [badge] };
  const document = { getAnimations: () => [], querySelectorAll: selector => selector.includes("message-bubble") ? [bubble] : [], querySelector: selector => selector.includes("role=log") ? ({ getBoundingClientRect: () => ({ top: 0, bottom: 500, left: 0, right: 500 }) }) : null };
  return {
    command: async (_method, path, body) => {
      if (path === "/execute/async") return new Promise(resolve => {
        const args = [resolve];
        runInNewContext(body.script, { arguments: args, requestAnimationFrame: callback => setImmediate(() => { frames++; callback(); }) });
      });
      assert.equal(path, "/screenshot"); shots++;
      assert.ok(frames >= 2, "native screenshot follows two actual frame callbacks");
      return screenshots?.[Math.min(shots-1,screenshots.length-1)] ?? paintedReceiptPng;
    },
    execute: async (script, args) => runInNewContext(`(function(){${script}}).apply(null, args)`, { args, document, innerWidth: 500, innerHeight: 500, getComputedStyle: () => ({visibility:"visible",opacity:"1",display:"block"}) }),
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
  assert.equal(c.shots(), 2);
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
  assert.equal(c.shots(), 3);
});

test("auth error capture requires a fully visible localized alert on the login form", async () => {
  const { captureNativeEvidence } = await load();
  for (const config of [ { text: "用户名或密码错误", top: 40, accepted: true }, { text: "decrypt failed", top: 40 }, { text: "用户名或密码错误", top: 700 }, { text: "用户名或密码错误", top: 40, chat: true }, { text: "用户名或密码错误", top: 40, opacity: "0" } ]) {
    const c = webview();
    const rect = { x: 10, y: config.top, top: config.top, bottom: config.top + 20, left: 10, right: 150, width: 140, height: 20 };
    const alert = { textContent: config.text, getBoundingClientRect: () => rect };
    const form = { getBoundingClientRect: () => ({ x: 0, y: 0, top: 0, left: 0, right: 300, bottom: 500, width: 300, height: 500 }) };
    const document = { getAnimations: () => [], querySelectorAll: () => [], querySelector: selector => selector.includes("[role=alert]") ? alert : selector.includes("chat-shell") ? config.chat : selector.includes("login-form") ? form : null };
    c.execute = async (script,args) => runInNewContext(`(function(){${script}}).apply(null,args)`, { document, args, innerWidth: 500, innerHeight: 500, getComputedStyle: () => ({ visibility: "visible", opacity: config.opacity ?? "1", display: "block" }) });
    const capture = captureNativeEvidence(c,{ kind: "auth-error", texts: ["用户名或密码错误"] });
    if (config.accepted) { assert.equal((await capture).observation.text, config.text); assert.equal(c.shots(), 2); }
    else { await assert.rejects(capture,/never settled/); assert.equal(c.shots(),0); }
  }
});

test("general native capture rejects foreground loading and running finite animations", async () => {
  const { captureNativeEvidence } = await load();
  for (const kind of ["loader", "finite-animation"]) {
    const c = webview();
    const original = c.execute;
    c.execute = async (script,args) => script.includes("getAnimations") ? { settled: false, foregroundLoader: kind === "loader", finiteAnimations: kind === "finite-animation" ? 1 : 0 } : original(script,args);
    await assert.rejects(captureNativeEvidence(c), /never settled/);
    assert.equal(c.shots(),0);
  }
});

test("settled capture permits an infinite sidebar search spinner but rejects a real main loader", async () => {
  const { captureNativeEvidence } = await load();
  for (const foreground of [false,true]) {
    const c=webview();
    const spinner={getBoundingClientRect:()=>({width:20,height:20,top:1,bottom:21,left:1,right:21})};
    const document={querySelector:()=>null,querySelectorAll:selector=>selector==='main .animate-spin'&&foreground?[spinner]:[],getAnimations:()=>[{playState:'running',effect:{getComputedTiming:()=>({endTime:Infinity})}}]};
    c.execute=async script=>runInNewContext(`(function(){${script}})()`,{document,innerWidth:500,innerHeight:500,getComputedStyle:()=>({display:'block',visibility:'visible',opacity:'1'})});
    if(foreground) {await assert.rejects(captureNativeEvidence(c),/never settled/);assert.equal(c.shots(),0);}
    else {const capture=await captureNativeEvidence(c);assert.equal(capture.surface.settled,true);assert.equal(c.shots(),1);}
  }
});

test("capture timeout preserves the last blocking native surface", async () => {
  const { captureNativeEvidence } = await load();
  const c = webview();
  const surface = { settled: false, foregroundLoader: false, finiteAnimations: 1, animationBlockers: [{ tag: "DIV", testId: "message-bubble", duration: 220, endTime: 220 }] };
  c.execute = async () => surface;
  await assert.rejects(captureNativeEvidence(c), error => {
    assert.match(error.message, /capture never settled/);
    assert.deepEqual(error.captureDiagnostics, { phase: "surface-before", surfaceBefore: surface });
    return true;
  });
  assert.equal(c.shots(), 0, "diagnostics do not bypass capture readiness");
});

test("raw failure capture records an unsettled screen only as diagnostic evidence", async () => {
  const { captureNativeFailure } = await load();
  const surface = { settled: false, foregroundLoader: true, finiteAnimations: 0 };
  let shots = 0;
  const result = await captureNativeFailure({
    command: async (method, path) => { assert.equal(method, "GET"); assert.equal(path, "/screenshot"); shots++; return Buffer.from("raw failure png").toString("base64"); },
    execute: async () => surface,
    until: () => { throw new Error("failure diagnostics must not await passing readiness"); },
  });
  assert.equal(result.png.toString(), "raw failure png");
  assert.deepEqual(result.surface, surface);
  assert.equal(shots, 1);
});

test("long-message offscreen CSS transitions cannot block a painted viewport", async () => {
  const { captureNativeEvidence } = await load();
  for (const config of [
    { top: 1200, opacity: "0", accepted: true },
    { top: 30, opacity: "0", accepted: false },
    { top: 30, opacity: "1", accepted: false },
    { top: 480, opacity: "1", accepted: false },
  ]) {
    const c = webview();
    const target = { tagName: "DIV", getAttribute: () => "opacity-0 transition-opacity group-hover:opacity-100", getBoundingClientRect: () => ({ x: 20, y: config.top, left: 20, right: 86, top: config.top, bottom: config.top + 32, width: 66, height: 32 }) };
    const animation = { playState: "running", transitionProperty: "opacity", effect: { target, getComputedTiming: () => ({ duration: 150, endTime: 150, iterations: 1 }) } };
    const document = { querySelector: () => null, querySelectorAll: () => [], getAnimations: () => [animation] };
    c.execute = async script => runInNewContext(`(function(){${script}})()`, { document, innerWidth: 500, innerHeight: 500, getComputedStyle: () => ({ display: "block", visibility: "visible", opacity: config.opacity }) });
    if (config.accepted) {
      const result = await captureNativeEvidence(c);
      assert.equal(result.surface.settled, true);
      assert.equal(c.shots(), 1);
    } else {
      await assert.rejects(captureNativeEvidence(c), /never settled/);
      assert.equal(c.shots(), 0, "visible finite transitions block even at opacity zero");
      animation.playState = "finished";
      await captureNativeEvidence(c);
      assert.equal(c.shots(), 1, "the viewport is ready after its transition ends");
    }
  }
});

test("receipt capture rejects stale compositor frame then requires two identical painted crops", async () => {
  const { captureNativeEvidence } = await load();
  const c = webview({ screenshots: [receiptPng("blank"), paintedReceiptPng, receiptPng("changed"), receiptPng("changed")] });
  c.until = async (_description,poll) => { for (let i=0;i<5;i++) { const value=await poll(); if(value)return value; } throw new Error("capture never settled"); };
  const result = await captureNativeEvidence(c,{label:"receipt text",status:"awaiting"});
  assert.equal(c.shots(),4,"DOM stability and frame callbacks cannot certify blank/stale PNGs");
  assert.equal(result.paint.painted,true);
  const blank = webview({ screenshots:[receiptPng("blank")] });
  await assert.rejects(captureNativeEvidence(blank,{label:"receipt text",status:"awaiting"}), error => {
    assert.equal(error.captureDiagnostics.paint.painted,false);
    return /never settled/.test(error.message);
  });
});
