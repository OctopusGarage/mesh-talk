// Drive the real embedded W3C server and a real CLI node. No IPC replacement,
// seeded roster, browser substitute, or downloaded JavaScript driver is used.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdir, mkdtemp, readdir, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { createServer } from "node:net";
import { createSocket } from "node:dgram";
import { setTimeout as delay } from "node:timers/promises";
import { validateEvidence } from "./hidden-contacts-report.mjs";

const executableSuffix = process.platform === "win32" ? ".exe" : "";
const appBinary = resolve(process.env.NATIVE_CONTACT_APP ?? `target/release/examples/native-contact-eval${executableSuffix}`);
const nodeBinary = resolve(process.env.NATIVE_CONTACT_NODE ?? `target/release/mesh-talk-node${executableSuffix}`);
const output = resolve(process.env.NATIVE_CONTACT_OUTPUT ?? "target/hidden-contacts-native");
await mkdir(output, { recursive: true });
const fixture = await mkdtemp(join(output, "fixture-"));
const discoverySocket = createSocket("udp4");
await new Promise((ok, fail) => { discoverySocket.once("error", fail); discoverySocket.bind(0, "127.0.0.1", ok); });
const discoveryPort = discoverySocket.address().port;
await new Promise(ok => discoverySocket.close(ok));
const password = randomBytes(24).toString("hex");
const peerName = `eval-peer-${randomBytes(4).toString("hex")}`;
const userName = `eval-user-${randomBytes(4).toString("hex")}`;
const otherName = `eval-other-${randomBytes(4).toString("hex")}`;
const started = Date.now();
let previousScenario = started;
const report = { schema: 1, platform: process.platform, native: true, mocked: false, scenarios: {}, startedAt: new Date().toISOString(), input: "embedded W3C DOM automation in actual native webview; contextmenu uses execute; keyboard uses in-process AppKit on macOS, owned-PID OS input on Windows/Linux; no physical keyboard user study", kdf: { example: "fast-test-kdf (existing dev-dependency)", cli: "regular production KDF" } };
const owned = new Set();
let app, peer, session, endpoint, account, userId, signedInUser;
const registeredOwners = new Map();
const redact = value => String(value).replaceAll(password, "[REDACTED]");
function launch(binary, args) {
  const child = spawn(binary, args, { stdio: ["pipe", "pipe", "pipe"], windowsHide: false });
  owned.add(child);
  child.output = "";
  for (const stream of [child.stdout, child.stderr]) stream.on("data", data => { child.output = (child.output + redact(data)).slice(-64000); });
  child.on("error", error => { child.output += redact(error.message); });
  child.on("close", () => owned.delete(child));
  return child;
}
async function stop(child, graceful = false) {
  if (!child || !owned.has(child)) return;
  if (graceful) child.stdin.write("/quit\n"); else child.kill("SIGTERM");
  const deadline = Date.now() + 10000;
  while (owned.has(child) && Date.now() < deadline) await delay(100);
  if (owned.has(child)) {
    child.kill("SIGKILL");
    const killDeadline = Date.now() + 5000;
    while (owned.has(child) && Date.now() < killDeadline) await delay(100);
    assert.ok(!owned.has(child), "owned child did not exit");
  }
}
async function until(description, check, timeout = 60000) {
  const deadline = Date.now() + timeout;
  let error;
  while (Date.now() < deadline) {
    try { const value = await check(); if (value) return value; } catch (e) { error = e; }
    await delay(250);
  }
  throw new Error(`Timed out: ${description}${error ? ` (${redact(error.message)})` : ""}`);
}
async function freePort() {
  const server = createServer();
  await new Promise((ok, fail) => { server.once("error", fail); server.listen(0, "127.0.0.1", ok); });
  const port = server.address().port;
  await new Promise(ok => server.close(ok));
  return port;
}
async function request(method, path, body) {
  const response = await fetch(`${endpoint}${path}`, { method, headers: { "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(15000) });
  const result = await response.json();
  if (!response.ok || result.value?.error) throw new Error(`WebDriver ${method} ${path}: ${redact(result.value?.message ?? response.status)}`);
  return result.value;
}
const command = (method, path, body) => request(method, `/session/${session}${path}`, body);
const execute = (script, args = []) => command("POST", "/execute/sync", { script, args });
// Actual production IPC, read-only observations only. Frontend behavior is
// changed exclusively through visible controls and native keyboard/mouse input.
const observe = (cmd, args = {}) => command("POST", "/execute/async", {
  script: "const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(v=>done({ok:true,value:v}),e=>done({ok:false,error:String(e)}));", args: [cmd, args],
}).then(result => { assert.equal(result.ok, true, `production ${cmd} observation failed`); return result.value; });
const selector = id => `[data-testid="${id}"]`;
const exists = id => execute("return !!document.querySelector(arguments[0]);", [selector(id)]);
async function element(id) {
  await until(`visible ${id}`, () => execute("const e=document.querySelector(arguments[0]); return !!e && e.getBoundingClientRect().width>0;", [selector(id)]));
  const value = await command("POST", "/element", { using: "css selector", value: selector(id) });
  return value["element-6066-11e4-a52e-4f735466cecf"];
}
async function click(id) { await command("POST", `/element/${await element(id)}/click`, {}); }
async function fill(id, text) {
  const el = await element(id);
  await command("POST", `/element/${el}/clear`, {});
  await command("POST", `/element/${el}/value`, { text });
}
async function key(value) {
  const keys = { "\uE004": [48, "{TAB}", "Tab"], "\uE007": [36, "{ENTER}", "Return"], "\uE00C": [53, "{ESC}", "Escape"] };
  const mapping = keys[value];
  assert.ok(mapping, "supported native evaluation key");
  const pid = app.pid;
  assert.ok(Number.isInteger(pid) && owned.has(app), "keyboard targets only the owned native application");
  async function input(binary, args) {
    const child = launch(binary, args);
    await until("native keyboard helper completion", () => !owned.has(child), 10000);
    assert.equal(child.exitCode, 0, `native keyboard helper failed: ${child.output}`);
    return child.output.trim();
  }
  if (process.platform === "darwin") {
    const nonce = randomBytes(8).toString("hex");
    const keyName = { "\uE004": "Tab", "\uE007": "Enter", "\uE00C": "Escape" }[value];
    const result = await command("POST", "/execute/async", { script: "const key=arguments[0],nonce=arguments[1],done=arguments[arguments.length-1]; window.__TAURI__.event.listen('native-contact-key-result',e=>{if(e.payload.nonce===nonce){unlisten();done(e.payload);}}).then(fn=>{unlisten=fn; return window.__TAURI__.event.emit('native-contact-key',{key,nonce});}).catch(e=>done({error:String(e)})); let unlisten=()=>{};", args: [keyName, nonce] });
    assert.equal(result.error, null, `native AppKit input failed: ${result.error}`);
  } else if (process.platform === "win32") {
    await input("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", `$shell = New-Object -ComObject WScript.Shell; if (-not $shell.AppActivate(${pid})) { throw 'Owned application could not be activated' }; Start-Sleep -Milliseconds 200; Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('${mapping[1]}')`]);
  } else {
    const windows = await input("xdotool", ["search", "--onlyvisible", "--pid", String(pid)]);
    const window = windows.split("\n")[0];
    assert.match(window, /^\d+$/);
    await input("xdotool", ["windowfocus", "--sync", window]);
    await input("xdotool", ["key", "--window", window, "--clearmodifiers", mapping[2]]);
  }
}
async function contextMenu(id) {
  await element(id);
  // Embedded driver 1.4 synthesizes mousedown/up but omits contextmenu.
  // Trigger the actual webview's UI event through standard W3C execute.
  await execute("const e=document.querySelector(arguments[0]), r=e.getBoundingClientRect(); e.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true,button:2,clientX:r.x+r.width/2,clientY:r.y+r.height/2}));", [selector(id)]);
}
async function screenshot(name) {
  const filename = `${name}.png`;
  const png = Buffer.from(await command("GET", "/screenshot"), "base64");
  assert.ok(png.length > 100 && png.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])), "native driver returned a PNG screenshot");
  await writeFile(join(output, filename), png);
  return filename;
}
async function passed(name, observations) {
  const filename = `${name}.json`;
  await writeFile(join(output, filename), JSON.stringify({ name, observations, at: new Date().toISOString() }, null, 2));
  report.scenarios[name] = { passed: true, elapsedMs: Date.now() - previousScenario, evidence: [filename, await screenshot(name)] };
  previousScenario = Date.now();
  console.log(`PASS ${name}`);
}
async function startApp() {
  const port = await freePort();
  endpoint = `http://127.0.0.1:${port}`;
  app = launch(appBinary, [join(fixture, "app"), String(port), String(discoveryPort)]);
  await until("embedded native WebDriver", async () => {
    if (!owned.has(app)) throw new Error(`app exited: ${app.output}`);
    return (await request("GET", "/status"))?.ready;
  });
  const value = await request("POST", "/session", { capabilities: { alwaysMatch: {} } });
  session = value.sessionId;
  assert.ok(session, "native session created");
  await command("POST", "/timeouts", { script: 10000, implicit: 0, pageLoad: 10000 });
}
async function login(username, register = false) {
  await element("login-form");
  if (await execute("return document.querySelector('[data-testid=login-stay-signed-in]')?.checked === true;")) await click("login-stay-signed-in");
  await until("stay signed in disabled in production settings", async () => (await observe("get_app_settings")).stay_signed_in === false);
  if (register) await click("login-tab-register");
  await fill("login-username", username);
  await fill("login-password", password);
  await click("login-submit");
  if (register) {
    await until("registration completed", () => execute("return document.querySelector('[data-testid=login-tab-signin]')?.getAttribute('data-state')==='active';"));
    await fill("login-password", password);
    await click("login-submit");
  }
  await element("chat-shell");
  if (!registeredOwners.has(username)) {
    // The production node creates accounts/<local user UUID> after the real UI
    // login. Observe that on-disk namespace rather than guessing the username is
    // the owner or intercepting the frontend's login response.
    const owner = await until("production local user storage namespace", async () => {
      const dirs = await readdir(join(fixture, "app", "accounts"), { withFileTypes: true });
      const unseen = dirs.filter(d => d.isDirectory() && ![...registeredOwners.values()].includes(d.name));
      assert.ok(unseen.length <= 1, "one new account namespace per real UI registration");
      return unseen[0]?.name;
    });
    assert.match(owner, /^[0-9a-f-]{36}$/);
    registeredOwners.set(username, owner);
  }
  signedInUser = registeredOwners.get(username);
}
const hiddenContacts = async () => (await observe("get_hidden_contacts", { owner: signedInUser })).contacts;
async function manage() {
  await click("sidebar-overflow");
  await click("sidebar-action-settings");
  await click("manage-hidden-contacts");
  await element("hidden-contacts-dialog");
}
async function closeDialogs() {
  await key("\uE00C");
  await until("hidden dialog dismissed", async () => !await exists("hidden-contacts-dialog"));
  await key("\uE00C");
  await until("settings dismissed", async () => !await exists("settings-dialog"));
}
const row = () => `conversation-row-${account}`;
const history = () => observe("account_history", { account, limit: 500 });
async function signOut() {
  await click("sidebar-overflow");
  await click("sidebar-sign-out");
  await element("login-form");
}
async function hide() {
  await contextMenu(row());
  await click(`hide-contact-menu-${account}`);
  await click("hide-contact-confirm");
  await until("contact removed", async () => !await exists(row()));
}
try {
  await mkdir(join(fixture, "peer"));
  peer = launch(nodeBinary, ["--keystore", join(fixture, "peer", "identity.keystore"), "--password", password, "--name", peerName, "--discovery-port", String(discoveryPort)]);
  await until("real peer node startup with production KDF", () => /node (\S+) listening/.test(peer.output), 180000);
  await startApp();
  await login(userName, true);
  userId = await until("production node identity ready", () => observe("my_id"));
  const accounts = await until("signed multicast discovery of real CLI peer", async () => (await observe("list_accounts")).find(a => a.names.includes(peerName)));
  account = accounts.account_id;
  await element(row());
  await click(row());
  const beforeText = `native-before-${randomBytes(4).toString("hex")}`;
  await fill("composer-input", beforeText);
  await click("composer-send");
  await until("CLI receives UI DM", () => peer.output.includes(beforeText));
  const originalHistory = await history();
  assert.ok(originalHistory.some(h => h.text === beforeText));

  await contextMenu(row());
  await click(`hide-contact-menu-${account}`);
  await until("cancel receives default focus", () => execute("return document.activeElement?.getAttribute('data-testid')==='hide-contact-cancel';"));
  const confirmationCopy = await execute("return document.querySelector('[data-testid=hide-contact-dialog]').textContent;");
  assert.match(confirmationCopy, /History is kept/);
  assert.match(confirmationCopy, /Messages and calls can still arrive/);
  assert.match(confirmationCopy, /signed-in user on this device/);
  await key("\uE007");
  await until("Enter cancels confirmation", async () => !await exists("hide-contact-dialog"));
  assert.equal(await exists(row()), true);
  await passed("hide-cancel", { retained: true, cancelDefaultFocus: true, enterCancelled: true, confirmationCopy });
  await hide();
  const hiddenSnapshot = await observe("get_hidden_contacts", { owner: signedInUser });
  assert.equal(hiddenSnapshot.owner, signedInUser);
  assert.ok(hiddenSnapshot.contacts.some(h => h.account_id === account));
  await passed("hide", { absent: !await exists(row()), hidden: hiddenSnapshot });
  const raw = await observe("list_peers");
  assert.ok(raw.some(p => p.name === peerName));
  assert.ok((await observe("list_accounts")).some(a => a.account_id === account));
  await passed("raw-peer-retained", { rawPeerPresent: true, accountPresent: true });

  peer.stdin.write("/peers\n");
  await until("CLI discovered GUI identity", () => peer.output.includes(`peer ${userId}`));
  const hiddenText = `native-hidden-${randomBytes(4).toString("hex")}`;
  peer.stdin.write(`/msg ${userId} ${hiddenText}\n`);
  await until("real inbound DM stored while hidden", async () => (await history()).some(h => h.text === hiddenText));
  assert.equal(await exists(row()), false);
  await passed("hidden-inbound", { inboundStored: true, rowAbsent: true });
  assert.deepEqual((await history()).filter(h => h.text === beforeText), originalHistory.filter(h => h.text === beforeText));
  await passed("history-retained", { originalEntriesUnchanged: true, inboundAdded: true });

  await command("DELETE", "");
  await stop(app);
  await startApp();
  await login(userName);
  await until("peer rediscovered after process restart", async () => (await observe("list_accounts")).some(a => a.account_id === account));
  assert.equal(await exists(row()), false);
  assert.ok((await history()).some(h => h.text === hiddenText));
  await passed("restart", { actualProcessRestart: true, hiddenPersisted: true, inboundDurable: true });

  await manage();
  await fill("hidden-contacts-search", peerName);
  await element(`restore-contact-${account}`);
  await closeDialogs();
  await signOut();
  await login(otherName, true);
  await until("second user's real discovered peer", () => exists(row()));
  const otherHidden = await hiddenContacts();
  assert.equal(otherHidden.some(h => h.account_id === account), false);
  await passed("local-user-isolation", { secondUserContactVisible: true, secondUserHasNoHiddenEntry: true });
  await signOut();
  await login(userName);
  await until("first user's hidden policy restored", async () => (await hiddenContacts()).some(h => h.account_id === account));

  await manage();
  await fill("hidden-contacts-search", peerName);
  await click(`restore-contact-${account}`);
  await until("online restore saved", async () => !(await hiddenContacts()).some(h => h.account_id === account));
  await click("hidden-contacts-add-tab");
  await fill("hidden-contacts-search", peerName);
  await click(`hide-contact-${account}`);
  await click("hide-contact-confirm");
  await until("settings hide saved", async () => (await hiddenContacts()).some(h => h.account_id === account));
  await passed("settings-search", { actualPeerNameFound: true, onlineRestoreWorked: true, searchAddTabHideWorked: true });
  await closeDialogs();

  await stop(peer, true);
  await until("real offline peer expires from roster", async () => !(await observe("list_peers")).some(p => p.name === peerName), 180000);
  await manage();
  await fill("hidden-contacts-search", peerName);
  await command("POST", "/window/rect", { width: 760, height: 520 });
  await element(`restore-contact-${account}`);
  const layout = await execute("const d=document.querySelector('[data-testid=hidden-contacts-dialog]'); const b=document.querySelector(arguments[0]); const r=d.getBoundingClientRect(), q=b.getBoundingClientRect(); return {role:d.getAttribute('role'), label:d.getAttribute('aria-labelledby'), buttonLabel:b.getAttribute('aria-label'), dialog:{x:r.x,y:r.y,right:r.right,bottom:r.bottom}, button:{x:q.x,y:q.y,right:q.right,bottom:q.bottom}, viewport:{width:innerWidth,height:innerHeight}};", [selector(`restore-contact-${account}`)]);
  assert.equal(layout.role, "dialog");
  assert.ok(layout.label && layout.buttonLabel);
  assert.ok(layout.dialog.x >= 0 && layout.dialog.y >= 0 && layout.dialog.right <= layout.viewport.width + 1 && layout.dialog.bottom <= layout.viewport.height + 1);
  assert.ok(layout.button.x >= layout.dialog.x && layout.button.right <= layout.dialog.right && layout.button.bottom <= layout.dialog.bottom);
  // Focus through native Tab traversal, then activate through native Enter.
  let focused = false;
  for (let i = 0; i < 30; i++) {
    await key("\uE004");
    focused = await execute("return document.activeElement?.getAttribute('data-testid')===arguments[0];", [`restore-contact-${account}`]);
    if (focused) break;
  }
  assert.equal(focused, true, "restore reachable through keyboard focus traversal");
  await passed("narrow-keyboard-layout", { ...layout, keyboardFocusReachedRestore: true });
  await key("\uE007");
  await until("offline contact restored through keyboard", async () => !await exists(`restore-contact-${account}`));
  assert.equal((await hiddenContacts()).some(h => h.account_id === account), false);
  assert.ok((await history()).some(h => h.text === beforeText));
  assert.ok((await history()).some(h => h.text === hiddenText));
  await passed("offline-restore", { realPeerStopped: true, rawRosterExpired: true, persistedIdentityRestored: true, durableHistoryRetained: true });
  const errors = await validateEvidence(report, output);
  assert.deepEqual(errors, []);
} catch (error) {
  report.failure = redact(error.stack ?? error);
  console.error(redact(error.message));
  process.exitCode = 1;
  try { if (session && owned.has(app)) await screenshot("failure"); } catch { /* driver may already have exited */ }
} finally {
  for (const child of [...owned]) {
    try { await stop(child, child === peer); } catch (error) { report.cleanupFailure = redact(error.message); process.exitCode = 1; }
  }
  report.finishedAt = new Date().toISOString();
  report.elapsedMs = Date.now() - started;
  report.validationErrors = await validateEvidence(report, output);
  if (report.validationErrors.length) process.exitCode = 1;
  await writeFile(join(output, "report.json"), JSON.stringify(report, null, 2));
  await writeFile(join(output, "native-app.log"), redact(app?.output ?? "app not started"));
  await writeFile(join(output, "native-peer.log"), redact(peer?.output ?? "peer not started"));
}
