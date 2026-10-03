/** Native-only regression probe for #136. No Tauri mocks or privileged test commands. */
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const application = resolve(process.argv[2] ?? "");
assert.ok(process.argv[2], "Usage: node window-controls.mjs <application>");
assert.ok(["linux", "win32"].includes(process.platform), "Native Linux/Windows required");
const artifacts = resolve("native-window-artifacts");
await mkdir(artifacts, { recursive: true });
const driver = spawn("tauri-driver", ["--port", "4444"], { stdio: ["ignore", "pipe", "pipe"] });
let driverLog = "";
let driverError;
driver.on("error", (error) => { driverError = error; });
for (const stream of [driver.stdout, driver.stderr]) {
  stream.on("data", (data) => { driverLog += data; process.stdout.write(data); });
}
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
let session;
const report = { platform: process.platform, application, checks: [] };

async function request(method, path, body) {
  const response = await fetch(`http://127.0.0.1:4444${path}`, {
    method,
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(30000),
  });
  const result = await response.json();
  if (!response.ok || result.value?.error) {
    throw new Error(`${method} ${path}: ${JSON.stringify(result)}`);
  }
  return result.value;
}

async function until(description, check, timeout = 15000) {
  const deadline = Date.now() + timeout;
  let last;
  do {
    last = await check();
    if (last) return last;
    await pause(200);
  } while (Date.now() < deadline);
  throw new Error(`Timed out: ${description}; last value ${JSON.stringify(last)}`);
}

const execute = (script, args = []) => request("POST", `/session/${session}/execute/sync`, { script, args });
const asyncExecute = (script, args = []) => request("POST", `/session/${session}/execute/async`, { script, args });
async function invoke(command, args = {}) {
  const result = await asyncExecute(`
    const done = arguments[arguments.length - 1];
    window.__TAURI__.core.invoke(arguments[0], arguments[1])
      .then(value => done({ ok: true, value }), error => done({ ok: false, error: String(error) }));
  `, [command, args]);
  assert.ok(result.ok, `${command}: ${result.error}`);
  return result.value;
}
const state = (name) => invoke(`plugin:window|${name}`, { label: "main" });
async function click(label) {
  // Use a WebDriver element click, not a synthetic call to a mocked window object.
  const element = await request("POST", `/session/${session}/element`, {
    using: "css selector", value: `button[aria-label="${label}"]`,
  });
  await request("POST", `/session/${session}/element/${element["element-6066-11e4-a52e-4f735466cecf"]}/click`, {});
}
async function restore() {
  const second = spawn(application, [], { stdio: "ignore" });
  await new Promise((done, fail) => {
    const timer = setTimeout(() => { second.kill(); fail(new Error("Second instance did not exit")); }, 15000);
    second.once("error", (error) => { clearTimeout(timer); fail(error); });
    second.once("exit", (code) => { clearTimeout(timer); code === 0 ? done() : fail(new Error(`Second instance exit ${code}`)); });
  });
  await until("single-instance restores main window", async () =>
    (await state("is_visible")) && !(await state("is_minimized")));
}

try {
  await until("tauri-driver ready", async () => {
    if (driverError) throw driverError;
    if (driver.exitCode !== null) throw new Error(`Driver exited ${driver.exitCode}`);
    try { await request("GET", "/status"); return true; } catch { return false; }
  }, 30000);
  const created = await request("POST", "/session", {
    capabilities: { alwaysMatch: { "tauri:options": { application } } },
  });
  session = created.sessionId;
  assert.ok(session, "WebDriver must return a session id");
  await request("POST", `/session/${session}/timeouts`, { script: 20000, implicit: 10000 });
  await until("real Tauri UI ready", () => execute(`
    return !!window.__TAURI__ && !!document.querySelector('button[aria-label="Close"]');
  `));
  await execute(`
    window.nativeWindowErrors = [];
    window.addEventListener('unhandledrejection', event => {
      window.nativeWindowErrors.push(String(event.reason));
    });
  `);
  const settings = await invoke("get_app_settings");
  await invoke("set_app_settings", { settings: { ...settings, minimize_to_tray: true } });
  const image = await request("GET", `/session/${session}/screenshot`);
  await writeFile(resolve(artifacts, "before.png"), Buffer.from(image, "base64"));

  for (const [label, query, expected] of [
    ["Minimize", "is_minimized", true],
    ["Maximize", "is_maximized", true],
    ["Close", "is_visible", false],
  ]) {
    const entry = { label, query, expected };
    report.checks.push(entry);
    try {
      await click(label);
      await until(`${label} changes native state`, async () => {
        const errors = await execute("return window.nativeWindowErrors;");
        assert.deepEqual(errors, [], `${label}: native IPC rejected`);
        return (await state(query)) === expected;
      });
      entry.passed = true;
      if (label === "Maximize") {
        await click(label);
        await until("Maximize toggles back", async () => !(await state("is_maximized")));
      } else {
        await restore();
      }
      entry.restored = true;
    } catch (error) {
      entry.passed = false;
      entry.error = String(error);
      entry.rejections = await execute("return window.nativeWindowErrors;").catch(() => []);
      await execute("window.nativeWindowErrors = [];").catch(() => {});
    }
    console.log(JSON.stringify(entry));
  }
  assert.ok(report.checks.every((entry) => entry.passed && entry.restored), "Window controls failed; see report.json");
  await invoke("set_app_settings", { settings: { ...settings, minimize_to_tray: false } });
  await click("Close").catch((error) => {
    // A driver may report loss of its target during the close click; closure is checked below.
    console.log(`Close-to-exit click response: ${error}`);
  });
  await until("close-to-exit destroys main window", async () => {
    try { return (await request("GET", `/session/${session}/window/handles`)).length === 0; }
    catch (error) {
      if (/invalid session id|no such window/i.test(String(error))) return true;
      throw error;
    }
  });
  report.closeToExit = true;
  console.log("PASS: native buttons, native state, restoration and close-to-exit");
} catch (error) {
  report.error = String(error);
  console.error(error);
  process.exitCode = 1;
} finally {
  if (session) await request("DELETE", `/session/${session}`).catch(() => {});
  if (process.platform === "win32" && driver.pid) {
    // Kill only this owned driver tree; Windows otherwise leaves the native driver alive.
    spawnSync("taskkill", ["/PID", String(driver.pid), "/T", "/F"], { stdio: "ignore" });
  } else {
    driver.kill();
  }
  await writeFile(resolve(artifacts, "report.json"), JSON.stringify(report, null, 2));
  await writeFile(resolve(artifacts, "driver.log"), driverLog);
}
