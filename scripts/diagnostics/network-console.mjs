/** #138: observe the actual release GUI, real sign-in, idle polling and native IPC. */
import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { analyzeNetworkConsole, assertNetworkConsoleExpectation } from "./network-console-report.mjs";
import { installNetworkTrace } from "./network-console-trace.mjs";

assert.equal(process.platform, "win32", "A native Windows desktop is required");
assert.ok(process.argv[2] && process.env.NATIVE_NETWORK_ARTIFACT_DIR && process.env.NATIVE_CONSOLE_CONTROL_DIR,
  "Usage: set artifact/control directories, then node network-console.mjs <application>");
const application = resolve(process.argv[2]);
const artifacts = resolve(process.env.NATIVE_NETWORK_ARTIFACT_DIR);
const controls = resolve(process.env.NATIVE_CONSOLE_CONTROL_DIR);
await mkdir(artifacts, { recursive: true });
const report = { platform: process.platform, controls: {}, calls: [] };
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
// tauri-driver proxies to a native driver on 4445; its own listener must be distinct.
const driver = spawn("tauri-driver", ["--port", "4444"], { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
let driverLog = "";
let driverError;
driver.on("error", (error) => { driverError = error; });
for (const stream of [driver.stdout, driver.stderr]) stream.on("data", (chunk) => {
  // Bound diagnostic memory even when a driver failure produces a flood of repeated errors.
  if (driverLog.length < 512 * 1024) driverLog += chunk;
});
let observer;
let observerLog = "";
let observerError;
let session;

async function until(description, check, timeout = 30000) {
  const deadline = Date.now() + timeout;
  do {
    if (await check()) return;
    await pause(250);
  } while (Date.now() < deadline);
  throw new Error(`Timed out: ${description}`);
}

async function request(method, path, body) {
  const response = await fetch(`http://127.0.0.1:4444${path}`, {
    method, headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(150000),
  });
  const result = await response.json();
  // Never include request bodies, credentials or SSID responses in failures.
  assert.ok(response.ok && !result.value?.error, `WebDriver ${method} ${path} failed`);
  return result.value;
}
const execute = (script) => request("POST", `/session/${session}/execute/sync`, { script, args: [] });
const asyncExecute = (script) => request("POST", `/session/${session}/execute/async`, { script, args: [] });
async function invoke(command, args = {}) {
  const result = await request("POST", `/session/${session}/execute/async`, {
    script: `const done = arguments[arguments.length - 1];
      window.__TAURI__.core.invoke(arguments[0], arguments[1])
        .then(value => done({ ok: true, value }), () => done({ ok: false }));`,
    args: [command, args],
  });
  assert.ok(result.ok, `${command} IPC failed`);
  return result.value;
}
async function applicationPids() {
  const output = execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
    "Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -eq $env:NATIVE_WINDOW_APPLICATION } | ForEach-Object { $_.ProcessId }"], {
    windowsHide: true, encoding: "utf8", timeout: 10000,
    env: { ...process.env, NATIVE_WINDOW_APPLICATION: application },
  });
  return output.trim().split(/\s+/).filter(Boolean).map(Number);
}
async function childExit(child, timeout = 15000) {
  await new Promise((done, fail) => {
    const timer = setTimeout(() => { child.kill(); fail(new Error("Owned diagnostic process timed out")); }, timeout);
    child.once("error", (error) => { clearTimeout(timer); fail(error); });
    child.once("exit", (code) => { clearTimeout(timer); code === 0 ? done() : fail(new Error(`Diagnostic process exit ${code}`)); });
  });
}
async function stopObserver() {
  if (!observer || observer.exitCode !== null) return;
  const exited = childExit(observer);
  await writeFile(resolve(artifacts, "observer-stop"), "stop");
  await exited;
}

try {
  await until("native driver ready", async () => {
    if (driverError) throw driverError;
    assert.equal(driver.exitCode, null, "Native driver exited before readiness");
    try { await request("GET", "/status"); return true; } catch { return false; }
  });
  const created = await request("POST", "/session", {
    capabilities: { alwaysMatch: { "tauri:options": { application } } },
  });
  session = created.sessionId;
  assert.ok(session, "Native session must be created");
  await request("POST", `/session/${session}/timeouts`, { script: 120000, implicit: 10000 });
  await until("real login UI", () => execute(`
    return !!window.__TAURI__ && !!document.querySelector('[data-testid="login-form"]');
  `));
  const pids = await applicationPids();
  assert.equal(pids.length, 1, "Exactly one owned application process must run");
  report.applicationPid = pids[0];

  observer = spawn("powershell.exe", ["-NoProfile", "-NonInteractive", "-STA", "-File",
    resolve("scripts/diagnostics/windows-console-observer.ps1"),
    "-Artifacts", artifacts, "-ApplicationPid", String(report.applicationPid)], {
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
  });
  observer.on("error", (error) => { observerError = error; });
  for (const stream of [observer.stdout, observer.stderr]) stream.on("data", (chunk) => { observerLog += chunk; });
  await until("WinEvent and process observer ready", async () => {
    if (observerError) throw observerError;
    assert.equal(observer.exitCode, null, "Observer failed; see observer.log");
    return (await readFile(resolve(artifacts, "observer-ready"), "utf8").catch(() => "")) === String(report.applicationPid);
  });
  for (const mode of ["positive", "negative"]) {
    const entry = { start: Date.now() };
    const resultFile = resolve(artifacts, `${mode}-control.txt`);
    // This parent is a GUI executable, just like the release app (not a console test runner).
    const child = spawn(resolve(controls, "console-launcher.exe"), [
      resolve(controls, "console-child.exe"), mode, resultFile,
    ], { stdio: "ignore" });
    await childExit(child);
    entry.end = Date.now();
    const presence = (await readFile(resultFile, "utf8")).trim();
    assert.ok(["true", "false"].includes(presence), "Console control must return an exact boolean");
    entry.consolePresent = presence === "true";
    report.controls[mode] = entry;
    await pause(300);
  }

  const settings = await invoke("get_app_settings");
  await invoke("set_app_settings", { settings: { ...settings, stay_signed_in: false } });
  // Tauri invoke is immutable. Observe its real fetch transport without changing IPC/results.
  assert.equal(await execute(`return (${installNetworkTrace.toString()})(window);`), true);
  // Generate ephemeral credentials inside the WebView: no plaintext secret in driver logs/artifacts.
  const registered = await asyncExecute(`
    const done = arguments[arguments.length - 1];
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    const suffix = Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
    window.nativeCredentials = { username: 'native138-' + suffix.slice(0, 12), password: suffix };
    window.__TAURI__.core.invoke('register', window.nativeCredentials)
      .then(result => done(result.success === true), () => done(false));
  `);
  assert.equal(registered, true, "Ephemeral native account registration failed");
  await execute(`
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    for (const field of ['username', 'password']) {
      const input = document.querySelector('[data-testid="login-' + field + '"]');
      setter.call(input, window.nativeCredentials[field]);
      input.dispatchEvent(new Event('input', { bubbles: true }));
    }
    window.nativeCredentials = null;
  `);
  const button = await request("POST", `/session/${session}/element`, {
    using: "css selector", value: '[data-testid="login-submit"]',
  });
  await request("POST", `/session/${session}/element/${button["element-6066-11e4-a52e-4f735466cecf"]}/click`, {});
  await until("native sign-in opens the real sidebar", () => execute(`
    return !!document.querySelector('[data-testid="sidebar"]');
  `), 120000);
  assert.equal(await invoke("plugin:window|is_visible", { label: "main" }), true);
  const screenshot = await request("GET", `/session/${session}/screenshot`);
  await writeFile(resolve(artifacts, "signed-in.png"), Buffer.from(screenshot, "base64"));
  console.log("Signed in to real release GUI; waiting for unchanged 60-second sidebar polling");
  await until("first real network IPC response observed", () => execute(`
    return window.nativeNetworkCalls.some(c => c.phase === 'natural' && c.end);
  `), 15000);
  await until("two completed natural network queries", async () => {
    assert.equal(observer.exitCode, null, "Native observer exited during idle observation");
    return execute(`return window.nativeNetworkCalls.filter(c => c.phase === 'natural' && c.end).length >= 2;`);
  }, 85000);
  // Repeated real IPC calls isolate the child-launch path from unrelated periodic rendering.
  await execute("window.nativeNetworkPhase = 'manual';");
  for (let index = 0; index < 6; index++) {
    await invoke("network_name");
    await pause(300);
  }
  report.calls = await execute("return window.nativeNetworkCalls;");
  await pause(1000); // Drain asynchronous WinEvent/WMI delivery, without accelerating application timers.
  await stopObserver();
  report.events = JSON.parse((await readFile(resolve(artifacts, "events.json"), "utf8")).replace(/^\uFEFF/, ""));
  report.analysis = analyzeNetworkConsole(report);
  if (process.env.NATIVE_NETWORK_EXPECTED) {
    assertNetworkConsoleExpectation(report.analysis, process.env.NATIVE_NETWORK_EXPECTED);
  }
  console.log(JSON.stringify(report.analysis));
  await invoke("logout");
} catch (error) {
  report.error = String(error);
  report.status = "environment-or-harness-failure";
  console.error(error);
  process.exitCode = 1;
} finally {
  await stopObserver().catch(() => {});
  if (session) report.calls = await execute("return window.nativeNetworkCalls ?? [];").catch(() => report.calls);
  report.events ??= await readFile(resolve(artifacts, "events.json"), "utf8")
    .then(text => JSON.parse(text.replace(/^\uFEFF/, ""))).catch(() => []);
  if (session) await request("DELETE", `/session/${session}`).catch(() => {});
  const remaining = await applicationPids().catch(() => []);
  if (remaining.includes(report.applicationPid)) {
    try { process.kill(report.applicationPid); } catch { /* Already exited. */ }
  }
  for (const child of [observer, driver]) {
    if (child?.pid && child.exitCode === null) {
      spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
    }
  }
  await writeFile(resolve(artifacts, "report.json"), JSON.stringify(report, null, 2));
  await writeFile(resolve(artifacts, "observer.log"), observerLog);
  await writeFile(resolve(artifacts, "driver.log"), driverLog);
}
