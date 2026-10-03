/** #137: real close-to-tray and exported tray Show menu, independently measured on X11. */
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdir, readFile, readlink, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { analyzeTrayGeometry, assertTrayGeometryExpectation } from "./tray-geometry-report.mjs";

assert.equal(process.platform, "linux", "Native Linux X11 required");
assert.ok(process.argv[2] && process.env.NATIVE_TRAY_ARTIFACT_DIR);
const application = resolve(process.argv[2]);
const artifacts = resolve(process.env.NATIVE_TRAY_ARTIFACT_DIR);
await mkdir(artifacts, { recursive: true });
const pause = ms => new Promise(done => setTimeout(done, ms));
const report = { application, rounds: [], environment: { display: process.env.DISPLAY,
  session: "X11", windowManager: "Openbox", renderer: "software" } };
const helpers = [];
const logs = new Map();
let session;
function helper(name, command, args) {
  const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
  helpers.push(child);
  logs.set(name, "");
  child.on("error", error => { child.launchError = error; });
  for (const stream of [child.stdout, child.stderr]) stream.on("data", data => {
    logs.set(name, (logs.get(name) + data.toString()).slice(-524288));
  });
  return child;
}
function native(command, args) {
  return execFileSync(command, args, { encoding: "utf8", timeout: 10000, maxBuffer: 1048576 });
}
async function until(description, check, timeout = 15000) {
  const deadline = Date.now() + timeout;
  do {
    for (const child of helpers) {
      if (child.launchError) throw child.launchError;
      assert.equal(child.exitCode, null, "owned diagnostic helper must remain alive");
    }
    const result = await check();
    if (result) return result;
    await pause(200);
  } while (Date.now() < deadline);
  throw new Error(`Timed out: ${description}`);
}
async function request(method, path, body) {
  const response = await fetch(`http://127.0.0.1:4444${path}`, { method,
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(120000) });
  const result = await response.json();
  assert.ok(response.ok && !result.value?.error, `Native WebDriver failed: ${method} ${path}`);
  return result.value;
}
const execute = script => request("POST", `/session/${session}/execute/sync`, { script, args: [] });
async function invoke(command, args = {}) {
  const result = await request("POST", `/session/${session}/execute/async`, {
    script: `const done = arguments[arguments.length - 1];
      window.__TAURI__.core.invoke(arguments[0], arguments[1]).then(
        value => done({ok:true,value}), error => done({ok:false,error:String(error)}));`,
    args: [command, args],
  });
  assert.ok(result.ok, `Native IPC failed: ${command}`);
  return result.value;
}
function geometry() {
  const fields = Object.fromEntries(native("xdotool", ["getwindowgeometry", "--shell", report.windowId])
    .trim().split("\n").map(line => line.split("=")));
  const info = native("xwininfo", ["-id", report.windowId]);
  return { x: Number(fields.X), y: Number(fields.Y), width: Number(fields.WIDTH),
    height: Number(fields.HEIGHT), mapped: /Map State: IsViewable/.test(info) };
}
async function verifyOwner() {
  assert.equal(await readlink(`/proc/${report.applicationPid}/exe`), application);
  assert.match(native("xprop", ["-id", report.windowId, "_NET_WM_PID"]),
    new RegExp(`= ${report.applicationPid}\\s*$`), "X11 window must belong to the original process");
}
async function screenshot(name) {
  // Independent desktop image, including window manager placement rather than just WebView pixels.
  native("import", ["-window", "root", resolve(artifacts, `${name}.png`)]);
}

try {
  const ready = resolve(artifacts, "tray-host-ready");
  helper("tray-host", "/usr/bin/python3", ["scripts/diagnostics/linux-tray-host.py", "host", ready]);
  await until("StatusNotifier host ready", async () => (await readFile(ready, "utf8").catch(() => "")) === "ready");
  helper("driver", "tauri-driver", ["--port", "4444", "--native-port", "4445"]);
  await until("native driver ready", async () => {
    try { await request("GET", "/status"); return true; } catch { return false; }
  }, 30000);
  const created = await request("POST", "/session", {
    capabilities: { alwaysMatch: { "tauri:options": { application } } },
  });
  session = created.sessionId;
  assert.ok(session);
  await request("POST", `/session/${session}/timeouts`, { script: 20000, implicit: 10000 });
  await until("real application UI ready", () => execute("return !!window.__TAURI__ && !!document.querySelector('button[aria-label=\"Close\"]');"));
  // Match exact PID and only its mapped main window; do not rely on a globally matching title.
  report.applicationPid = Number(native("pgrep", ["-f", `^${application.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`]).trim());
  assert.ok(Number.isInteger(report.applicationPid) && report.applicationPid > 0);
  const windows = native("xdotool", ["search", "--onlyvisible", "--pid", String(report.applicationPid), "--name", "."])
    .trim().split(/\s+/);
  assert.equal(windows.length, 1, "exactly one visible application window required");
  report.windowId = windows[0];
  await verifyOwner();
  const settings = await invoke("get_app_settings");
  await invoke("set_app_settings", { settings: { ...settings, minimize_to_tray: true } });
  for (let index = 0; index < 3; index++) {
    const target = { x: 180 + index * 60, y: 140 + index * 40, width: 900, height: 700 };
    native("xdotool", ["windowsize", "--sync", report.windowId, String(target.width), String(target.height)]);
    native("xdotool", ["windowmove", "--sync", report.windowId, String(target.x), String(target.y)]);
    await until("requested native geometry reached", () => {
      const current = geometry(); return current.mapped && Object.keys(target).every(key => current[key] === target[key]);
    });
    await pause(300);
    const round = { target, before: geometry(), applicationPid: report.applicationPid, windowId: report.windowId };
    report.rounds.push(round);
    await screenshot(`${index}-before`);
    const button = await request("POST", `/session/${session}/element`, {
      using: "css selector", value: 'button[aria-label="Close"]',
    });
    await request("POST", `/session/${session}/element/${button["element-6066-11e4-a52e-4f735466cecf"]}/click`, {});
    await until("production close-to-tray unmaps window", () => !geometry().mapped);
    round.hidden = geometry();
    await verifyOwner();
    await screenshot(`${index}-hidden`);
    round.tray = JSON.parse(native("/usr/bin/python3", ["scripts/diagnostics/linux-tray-host.py", "show", String(report.applicationPid)]));
    await until("real tray Show callback remaps window", () => geometry().mapped);
    await pause(500);
    round.after = geometry();
    await verifyOwner();
    await screenshot(`${index}-after`);
  }
  report.analysis = analyzeTrayGeometry(report);
  if (process.env.NATIVE_TRAY_EXPECTED) {
    assertTrayGeometryExpectation(report.analysis, process.env.NATIVE_TRAY_EXPECTED);
  }
  console.log(JSON.stringify(report.analysis));
} catch (error) {
  report.error = String(error);
  report.status = "environment-or-harness-failure";
  console.error(error);
  process.exitCode = 1;
} finally {
  if (session) await request("DELETE", `/session/${session}`).catch(() => {});
  if (report.applicationPid && await readlink(`/proc/${report.applicationPid}/exe`).catch(() => "") === application) {
    try { process.kill(report.applicationPid); } catch { /* Already exited. */ }
  }
  for (const child of helpers) child.kill();
  await writeFile(resolve(artifacts, "report.json"), JSON.stringify(report, null, 2));
  for (const [name, content] of logs) await writeFile(resolve(artifacts, `${name}.log`), content);
}
