/** #133: runner-only probe, unchanged production binary, independently captured X11 pixels. */
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdir, readdir, readFile, readlink, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { analyzeRenderer, matchesRenderedInput } from "./renderer-report.mjs";
import { rendererLaunch, ownedExecutablePid } from "./renderer-launch.mjs";

assert.equal(process.platform, "linux", "Native Linux X11 required");
assert.ok(process.argv[2] && process.env.NATIVE_RENDERER_ARTIFACT_DIR);
const launch = rendererLaunch(process.argv[2], process.env);
const application = launch.executable;
const artifacts = resolve(process.env.NATIVE_RENDERER_ARTIFACT_DIR);
await mkdir(artifacts, { recursive: true });
const report = { application, launcher: launch.launcher, source: launch.source, paints: [], environment: {
  display: process.env.DISPLAY, backend: process.env.GDK_BACKEND,
  dmabufOverride: process.env.WEBKIT_DISABLE_DMABUF_RENDERER ?? null,
  compositingOverride: process.env.WEBKIT_DISABLE_COMPOSITING_MODE ?? null,
  desktop: "Xvfb/Openbox", gpuEquivalentToVMware: false,
} };
let driver;
let session;
let driverLog = "";
let launchError;
const pause = ms => new Promise(done => setTimeout(done, ms));
function native(command, args) {
  return execFileSync(command, args, { encoding: "utf8", timeout: 10000, maxBuffer: 1048576 });
}
async function request(method, path, body, timeout = 30000) {
  const response = await fetch(`http://127.0.0.1:4444${path}`, { method,
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(timeout) });
  const result = await response.json();
  assert.ok(response.ok && !result.value?.error, `Native WebDriver failed: ${method} ${path}`);
  return result.value;
}
const execute = (script, args = []) => request("POST", `/session/${session}/execute/sync`, { script, args });
async function until(description, check, timeout = 15000) {
  const deadline = Date.now() + timeout;
  do {
    if (launchError) throw launchError;
    assert.equal(driver.exitCode, null, "owned driver must remain alive");
    const result = await check();
    if (result) return result;
    await pause(200);
  } while (Date.now() < deadline);
  throw new Error(`Timed out: ${description}`);
}
async function verifyWindow() {
  assert.equal(await readlink(`/proc/${report.applicationPid}/exe`), application);
  assert.match(native("xprop", ["-id", report.windowId, "_NET_WM_PID"]),
    new RegExp(`= ${report.applicationPid}\\s*$`));
  assert.match(native("xwininfo", ["-id", report.windowId]), /Map State: IsViewable/);
  report.ownerVerified = true;
  report.mapped = true;
}
function capture(name) {
  native("import", ["-window", "root", resolve(artifacts, `${name}-desktop.png`)]);
  const file = resolve(artifacts, `${name}-window.png`);
  native("import", ["-window", report.windowId, file]);
  return file;
}
function pixel(file, point) {
  // X11 pixels, NOT WebDriver's WebView snapshot or the DOM's computed color.
  const rgb = native("convert", [file, "-crop", `1x1+${point.x}+${point.y}`,
    "+repage", "-depth", "8", "txt:-"]).match(/\(\s*(\d+),\s*(\d+),\s*(\d+)(?:,|\))/);
  assert.ok(rgb, "independent screenshot must yield an RGB pixel");
  return rgb.slice(1, 4).map(Number);
}
async function observePaint(check) {
  const deadline = Date.now() + 5000;
  do {
    await pause(250);
    await verifyWindow();
    if (check()) return true;
  } while (Date.now() < deadline);
  // Missing paint with a responsive DOM is an observation, not a harness exception.
  return false;
}
async function applicationPid() {
  const entries = (await readdir("/proc")).filter(entry => /^\d+$/.test(entry));
  const records = await Promise.all(entries.map(async entry => {
    const [executable, status] = await Promise.all([
      readlink(`/proc/${entry}/exe`).catch(() => null),
      readFile(`/proc/${entry}/status`, "utf8").catch(() => ""),
    ]);
    return { pid: Number(entry), executable, parentPid: Number(status.match(/^PPid:\s+(\d+)/m)?.[1]) };
  }));
  return ownedExecutablePid(records, application, driver.pid);
}
try {
  driver = spawn("tauri-driver", ["--port", "4444", "--native-port", "4445"],
    { stdio: ["ignore", "pipe", "pipe"] });
  driver.on("error", error => { launchError = error; });
  for (const stream of [driver.stdout, driver.stderr]) stream.on("data", data => {
    driverLog = (driverLog + data.toString()).slice(-524288);
  });
  await until("native driver ready", async () => {
    try { await request("GET", "/status", undefined, 1000); return true; } catch { return false; }
  }, 30000);
  const created = await request("POST", "/session", {
    capabilities: { alwaysMatch: { "tauri:options": { application: launch.launcher } } },
  }, 120000);
  session = created.sessionId;
  assert.ok(session);
  report.sessionReady = true;
  // Identify the original process/window before waiting for frontend readiness.
  report.applicationPid = await until("original executable under owned driver", applicationPid);
  const windows = await until("one mapped application window", () => {
    try {
      const found = native("xdotool", ["search", "--onlyvisible", "--pid",
        String(report.applicationPid), "--name", "."]).trim().split(/\s+/);
      return found.length === 1 ? found : false;
    } catch { return false; }
  });
  report.windowId = windows[0];
  await verifyWindow();
  capture("initial");
  await request("POST", `/session/${session}/timeouts`, { script: 10000, implicit: 5000 });
  await until("production login DOM ready", () => execute(
    'return !!window.__TAURI__ && !!document.querySelector("[data-testid=login-username]");'));
  report.domReady = true;
  const viewport = await execute("return {width:innerWidth,height:innerHeight,scale:devicePixelRatio};");
  report.viewport = viewport;
  assert.ok(viewport.width > 200 && viewport.height > 200 && viewport.scale > 0);
  capture("login");
  for (const [index, expected] of [[255, 0, 255], [0, 255, 0]].entries()) {
    const rect = await execute(`
      let marker = document.getElementById("__native_renderer_probe");
      if (!marker) { marker = document.createElement("div"); marker.id = "__native_renderer_probe"; document.body.append(marker); }
      marker.style.cssText = "position:fixed;left:64px;top:96px;width:64px;height:64px;z-index:2147483647;pointer-events:none;opacity:1;";
      marker.style.background = "rgb(" + arguments[0].join(",") + ")";
      const r = marker.getBoundingClientRect();
      return {x:r.x+r.width/2,y:r.y+r.height/2,width:r.width,height:r.height};
    `, [expected]);
    assert.equal(rect.width, 64);
    assert.equal(rect.height, 64);
    const point = { x: Math.round(rect.x * viewport.scale), y: Math.round(rect.y * viewport.scale) };
    const paint = { expected, point, actual: null, samples: 0 };
    report.paints.push(paint);
    const deadline = Date.now() + 5000;
    do {
      await pause(250);
      await verifyWindow();
      const file = capture(`marker-${index}`);
      const dimensions = native("identify", ["-format", "%w %h", file]).trim().split(" ").map(Number);
      assert.ok(Math.abs(dimensions[0] - viewport.width * viewport.scale) <= 1 &&
        Math.abs(dimensions[1] - viewport.height * viewport.scale) <= 1,
      "captured X11 client pixels must match DOM viewport coordinates");
      paint.actual = pixel(file, point);
      paint.samples++;
      if (paint.actual.every((value, channel) => Math.abs(value - expected[channel]) <= 4)) break;
    } while (Date.now() < deadline);
  }
  const inputRect = await execute(`
    document.getElementById("__native_renderer_probe").remove();
    const input = document.querySelector("[data-testid=login-username]");
    input.style.caretColor = "transparent";
    const r = input.getBoundingClientRect();
    return {x:r.x+12,y:r.y+6,width:r.width-24,height:r.height-12};
  `);
  // Wait for actual removal paint; a completed DOM command does not imply presentation.
  report.markerRemovedPainted = await observePaint(() => {
    const current = pixel(capture("before-input"), report.paints[1].point);
    return report.paints.every(paint =>
      current.some((value, channel) => Math.abs(value - paint.expected[channel]) > 4));
  });
  const region = `${Math.round(inputRect.width * viewport.scale)}x${Math.round(inputRect.height * viewport.scale)}+${Math.round(inputRect.x * viewport.scale)}+${Math.round(inputRect.y * viewport.scale)}`;
  const element = await request("POST", `/session/${session}/element`, {
    using: "css selector", value: "[data-testid=login-username]",
  });
  const elementId = element["element-6066-11e4-a52e-4f735466cecf"];
  await request("POST", `/session/${session}/element/${elementId}/value`, { text: "renderprobe" });
  report.interaction = await execute(
    'return document.querySelector("[data-testid=login-username]").value === "renderprobe";');
  await verifyWindow();
  report.interactionPainted = await observePaint(() => {
    const file = capture("interaction");
    const crop = resolve(artifacts, "input-text.png");
    native("convert", [file, "-crop", region, "+repage", "-colorspace", "Gray",
      "-negate", "-resize", "300%", crop]);
    // Recognize pixels without giving OCR the expected word; caret/placeholder changes cannot pass.
    report.renderedInput = native("tesseract", [crop, "stdout", "--psm", "7", "-l", "eng",
      "-c", "tessedit_char_whitelist=abcdefghijklmnopqrstuvwxyz", "quiet"]).trim();
    return matchesRenderedInput(report.renderedInput);
  });
} catch (error) {
  report.error = String(error);
  console.error(report.error);
} finally {
  report.analysis = analyzeRenderer(report);
  // Persist evidence before potentially slow session teardown.
  await writeFile(resolve(artifacts, "report.json"), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report.analysis));
  if (report.analysis.status === "inconclusive") process.exitCode = 1;
  if (session) await request("DELETE", `/session/${session}`, undefined, 5000).catch(() => {});
  if (report.applicationPid &&
    await readlink(`/proc/${report.applicationPid}/exe`).catch(() => "") === application) {
    try { process.kill(report.applicationPid); } catch { /* Already exited. */ }
  }
  if (driver) driver.kill();
  await writeFile(resolve(artifacts, "driver.log"), driverLog);
}
