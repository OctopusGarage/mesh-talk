// Drive the real embedded W3C server and a real CLI node. No IPC replacement,
// seeded roster, browser substitute, or downloaded JavaScript driver is used.
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { randomBytes, createHash } from "node:crypto";
import { mkdir, mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { createServer } from "node:net";
import { createSocket } from "node:dgram";
import { setTimeout as delay } from "node:timers/promises";
import { validateEvidence } from "./hidden-contacts-report.mjs";
import { coreScenarios, restartedCoreScenarios, writeChildCommand, receiptScenarios, receiptRestartScenario, receiptColdRestartCheckpoint, signedPeerObservation, nativeWindowsKeyboardScript } from "./native-core-scenarios.mjs";

const executableSuffix = process.platform === "win32" ? ".exe" : "";
const gitSha = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
const sourceSha = process.env.EVAL_SOURCE_SHA ?? gitSha;
assert.match(sourceSha, /^[0-9a-f]{40}$/);
assert.equal(sourceSha, gitSha, "evaluation source SHA matches checked-out source");
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
const report = { schema: 1, platform: process.platform, native: true, mocked: false, scenarios: {}, startedAt: new Date().toISOString(), input: "embedded W3C DOM automation in actual native webview; contextmenu and focus-before-click dialog triggers use execute; keyboard uses in-process AppKit on macOS, owned-PID OS input on Windows/Linux; no physical keyboard user study", driver: { crate: "tauri-plugin-wdio-webdriver", version: "1.4.0", clickOrderingSource: "src/platform/executor.rs:688-701 (click_element)" }, kdf: { example: "fast-test-kdf (existing dev-dependency)", cli: "regular production KDF" } };
const owned = new Set();
report.schema = 2;
report.sourceSha = sourceSha;
if (process.env.GITHUB_RUN_ID) report.runId = process.env.GITHUB_RUN_ID;
let app, peer, session, endpoint, account, userId, signedInUser;
const registeredOwners = new Map();
const redact = value => String(value).replaceAll(password, "[REDACTED]");
function launch(binary, args) {
  const child = spawn(binary, args, { stdio: ["pipe", "pipe", "pipe"], windowsHide: false });
  owned.add(child);
  child.output = "";
  for (const stream of [child.stdout, child.stderr]) stream.on("data", data => { child.output = (child.output + redact(data)).slice(-64000); });
  child.on("error", error => { child.output += redact(error.message); });
  child.stdin.on("error", error => { child.output += redact(`stdin: ${error.message}`); });
  child.on("close", () => owned.delete(child));
  return child;
}
async function stop(child, graceful = false) {
  if (!child || !owned.has(child)) return;
  if (graceful) {
    try { await writeChildCommand(child, "/quit\n"); }
    catch (error) { child.output += redact(`graceful shutdown: ${error.message}`); child.kill("SIGTERM"); }
  } else child.kill("SIGTERM");
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
  if (!response.ok || result.value?.error) throw new Error(`WebDriver ${method} ${path}: ${redact(result.value?.message ?? result.value?.error ?? response.status)}`);
  return result.value;
}
const command = (method, path, body) => request(method, `/session/${session}${path}`, body);
const execute = (script, args = []) => command("POST", "/execute/sync", { script, args });
// Real IPC, never replaced. Receipt fixtures explicitly enqueue sticker/file
// through production IPC; other frontend actions use visible controls.
const invoke = (cmd, args = {}) => command("POST", "/execute/async", {
  script: "const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(v=>done({ok:true,value:v}),e=>done({ok:false,error:String(e)}));", args: [cmd, args],
}).then(result => { assert.equal(result.ok, true, `production ${cmd} observation failed`); return result.value; });
const observe = invoke;
const readFileBytes = fileConv => command("POST", "/execute/async", {
  script: "const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke('read_file',{fileConv:arguments[0]}).then(buffer=>done({ok:true,bytes:Array.from(new Uint8Array(buffer))}),error=>done({ok:false,error:String(error)}));",
  args: [fileConv],
}).then(result => { assert.equal(result.ok, true, "production read_file failed"); return result.bytes; });
const selector = id => `[data-testid="${id}"]`;
const exists = id => execute("return !!document.querySelector(arguments[0]);", [selector(id)]);
async function element(id) {
  await until(`visible ${id}`, () => execute("const e=document.querySelector(arguments[0]); return !!e && e.getBoundingClientRect().width>0;", [selector(id)]));
  const value = await command("POST", "/element", { using: "css selector", value: selector(id) });
  return value["element-6066-11e4-a52e-4f735466cecf"];
}
async function settledDialog(id) {
  await focusOwnedWindow();
  await until(`settled open ${id}`, () => execute("const e=document.querySelector(arguments[0]); return !!e && e.getAttribute('data-state')==='open' && getComputedStyle(e).opacity==='1' && !e.getAnimations({subtree:true}).some(a=>a.playState==='running');", [selector(id)]));
}
async function appKitKey(keyName) {
  assert.ok(owned.has(app), "AppKit input targets the owned native application");
  const nonce = randomBytes(8).toString("hex");
  const result = await command("POST", "/execute/async", { script: "const key=arguments[0],nonce=arguments[1],done=arguments[arguments.length-1]; window.__TAURI__.event.listen('native-contact-key-result',e=>{if(e.payload.nonce===nonce){unlisten();done(e.payload);}}).then(fn=>{unlisten=fn; return window.__TAURI__.event.emit('native-contact-key',{key,nonce});}).catch(e=>done({error:String(e)})); let unlisten=()=>{};", args: [keyName, nonce] });
  assert.equal(result.error, null, `native AppKit input failed: ${result.error}`);
  if (result.focus) report.nativeFocus = result.focus;
}
async function focusOwnedWindow() {
  if (process.platform !== "darwin") return;
  // Background WKWebViews can suspend modal animations. Activate only the
  // fixture process before waiting for animations or delivering native keys.
  await appKitKey("Focus");
  try {
    await until("owned native application focus", () => execute("return document.hasFocus();"), 10000);
  } catch (error) {
    await appKitKey("Focus");
    throw error;
  }
}
async function click(id) { await command("POST", `/element/${await element(id)}/click`, {}); }
async function openDialog(id) {
  await element(id);
  // Driver 1.4 click_element calls click() then focus(), moving focus outside
  // the newly opened modal. Match native mouse ordering for dialog triggers.
  await execute("const e=document.querySelector(arguments[0]); e.focus(); e.click();", [selector(id)]);
}
async function fill(id, text) {
  const el = await element(id);
  await command("POST", `/element/${el}/clear`, {});
  await command("POST", `/element/${el}/value`, { text });
}
async function key(value) {
  const keys = { "\uE004": [48, "{TAB}", "Tab"], "\uE007": [36, "{ENTER}", "Return"], "\uE00C": [53, "{ESC}", "Escape"], "\uE00E": [116, "{PGUP}", "Prior"] };
  const mapping = keys[value];
  assert.ok(mapping, "supported native evaluation key");
  const pid = app.pid;
  assert.ok(Number.isInteger(pid) && owned.has(app), "keyboard targets only the owned native application");
  async function input(binary, args) {
    const child = launch(binary, args);
    try { await until("native keyboard helper completion", () => !owned.has(child), process.platform === "win32" ? 60000 : 10000); }
    catch (error) { throw new Error(`${error.message}; helper pid=${child.pid}; output=${redact(child.output.slice(-2000))}`); }
    assert.equal(child.exitCode, 0, `native keyboard helper failed: ${child.output}`);
    return child.output.trim();
  }
  if (process.platform === "darwin") {
    await focusOwnedWindow();
    const keyName = { "\uE004": "Tab", "\uE007": "Enter", "\uE00C": "Escape", "\uE00E": "PageUp" }[value];
    await appKitKey(keyName);
  } else if (process.platform === "win32") {
    await input("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", nativeWindowsKeyboardScript(pid, value)]);
  } else {
    const windows = await input("xdotool", ["search", "--onlyvisible", "--pid", String(pid)]);
    const window = windows.split("\n")[0];
    assert.match(window, /^\d+$/);
    await input("xdotool", ["windowfocus", "--sync", window]);
    await input("xdotool", ["key", "--window", window, "--clearmodifiers", mapping[2]]);
  }
  // Delivery acknowledgement means the native event was queued. Wait for the
  // actual webview to paint before sampling focus or sending the next key.
  await command("POST", "/execute/async", { script: "const done=arguments[arguments.length-1]; requestAnimationFrame(()=>requestAnimationFrame(()=>done(true)));", args: [] });
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
  await writeFile(join(output, filename), JSON.stringify({ name, sourceSha, observations, at: new Date().toISOString() }, null, 2));
  const logFile = `${name}.log`;
  await writeFile(join(output, logFile), redact(`APP\n${app?.output ?? ""}\nPEER\n${peer?.output ?? ""}`));
  report.scenarios[name] = { passed: true, elapsedMs: Date.now() - previousScenario, evidence: [filename, await screenshot(name), logFile] };
  report.scenarios[name].evidenceDigests = Object.fromEntries(await Promise.all(report.scenarios[name].evidence.map(async file => [file, createHash("sha256").update(await readFile(join(output, file))).digest("hex")])));
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
async function ensureOverflowOpen() {
  const open = await execute("return document.querySelector('[data-testid=sidebar-overflow-menu]')?.getAttribute('data-state')==='open';");
  if (!open) await click("sidebar-overflow");
  await settledDialog("sidebar-overflow-menu");
}
async function manage() {
  await ensureOverflowOpen();
  await openDialog("sidebar-action-settings");
  await settledDialog("settings-dialog");
  await openDialog("manage-hidden-contacts");
  await element("hidden-contacts-dialog");
  await settledDialog("hidden-contacts-dialog");
}
async function closeDialogs() {
  await key("\uE00C");
  await until("hidden dialog dismissed", async () => !await exists("hidden-contacts-dialog"));
  await key("\uE00C");
  await until("settings dismissed", async () => !await exists("settings-dialog"));
}
const row = () => `conversation-row-${account}`;
const history = () => observe("account_history", { account, limit: 500 });
const privacyPolicy = () => observe("get_privacy", { owner: signedInUser });
async function privacySettings() {
  await ensureOverflowOpen();
  await openDialog("sidebar-action-settings");
  await settledDialog("settings-dialog");
  await until("privacy controls loaded", () => execute("return document.querySelector('[data-testid=invisible-switch]')?.disabled === false;"));
}
async function signOut() {
  await ensureOverflowOpen();
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
  const coreContext = { execute, observe, invoke, readFileBytes, until, passed, fill, click, get peer() { return peer; }, account, userId, fixture, history, row, privacySettings, key, signOut, login, userName, peerName, command, focusOwnedWindow, openDialog, settledDialog, element, exists, get owner() { return signedInUser; }, nonce: randomBytes(4).toString("hex"),
    stopPeer: async () => { const old = peer; await stop(old, true); assert.ok(!owned.has(old), "peer actually exited"); },
    peerExited: () => !owned.has(peer) && (peer.exitCode !== null || peer.signalCode !== null),
    restartPeer: async () => { const originalId = /node (\S+) listening/.exec(peer.output)?.[1]; assert.match(originalId ?? "", /^[0-9a-f]{32}$/); peer = launch(nodeBinary, ["--keystore", join(fixture, "peer", "identity.keystore"), "--password", password, "--name", peerName, "--discovery-port", String(discoveryPort)]); const restartedId = await until("same-keystore peer restarted", () => /node (\S+) listening/.exec(peer.output)?.[1], 180000); assert.equal(restartedId, originalId, "restarted peer has original signing identity"); },
  };
  const coreState = await coreScenarios(coreContext);
  const receiptState = await receiptScenarios(coreContext);
  const beforeText = `native-before-${randomBytes(4).toString("hex")}`;
  await fill("composer-input", beforeText);
  await click("composer-send");
  await until("CLI receives UI DM", () => peer.output.includes(beforeText));
  const originalHistory = await history();
  assert.ok(originalHistory.some(h => h.text === beforeText));

  // Real native controls + production IPC + the actual CLI. Browser-mocked
  // privacy.spec.ts is not accepted as evidence for these runtime scenarios.
  await privacySettings();
  assert.equal((await privacyPolicy()).invisible, false);
  await openDialog("invisible-switch");
  await until("invisible cancel default focus", () => execute("return document.activeElement?.getAttribute('data-testid')==='invisible-cancel';"));
  await key("\uE007");
  await until("invisible cancellation dismissed", async () => !await exists("invisible-confirm"));
  assert.equal((await privacyPolicy()).invisible, false);
  await openDialog("invisible-switch");
  await click("invisible-confirm");
  await until("durable invisible mode", async () => (await privacyPolicy()).invisible);
  assert.ok((await observe("list_accounts")).some(a => a.account_id === account), "passive roster remains available");
  await passed("privacy-mode", { cancelledBeforeCommit: true, durable: await privacyPolicy(), passivePeerRetained: true });
  await openDialog("manage-privacy");
  await settledDialog("privacy-dialog");
  await fill("privacy-search", peerName);
  await openDialog(`privacy-revoke-${account}`);
  await click("privacy-revoke-confirm");
  await until("reply permission revoked", async () => !(await privacyPolicy()).allowed_accounts.some(a => a.id === account));
  await until("revoke confirmation dismissed", async () => !await exists("privacy-revoke-confirm"));
  await click(`privacy-allow-${account}`);
  await until("manual reply permission saved", async () => (await privacyPolicy()).allowed_accounts.some(a => a.id === account && a.source === "Manual"));
  const privateReply = `native-private-${randomBytes(4).toString("hex")}`;
  const ownAccount = await observe("account_id");
  await writeChildCommand(peer, `/account-msg ${ownAccount} ${privateReply}\n`);
  await until("authorized private reply delivered", async () => (await history()).some(h => h.text === privateReply));
  await passed("privacy-reply", { manualGrantAfterRevocation: true, realCliReplyStored: true, policy: await privacyPolicy() });
  await coreContext.stopPeer();
  assert.equal(coreContext.peerExited(), true, "receiver exited before source restart");
  await stop(app);
  await startApp();
  await login(userName);
  const coldReceiptCheckpoint = await receiptColdRestartCheckpoint(coreContext, receiptState);
  await coreContext.restartPeer();
  await signedPeerObservation(coreContext);
  await until("same account rediscovered after receiver restart", async () => (await observe("list_accounts")).some(a => a.account_id === account && a.names.includes(peerName)));
  await receiptRestartScenario(coreContext, receiptState, coldReceiptCheckpoint);
  await restartedCoreScenarios(coreContext, coreState);
  const restoredPrivacy = await privacyPolicy();
  assert.equal(restoredPrivacy.invisible, true);
  assert.ok(restoredPrivacy.allowed_accounts.some(a => a.id === account && a.source === "Manual"));
  assert.ok((await history()).some(h => h.text === privateReply));
  const restartReply = `native-private-restart-${randomBytes(4).toString("hex")}`;
  await writeChildCommand(peer, `/account-msg ${ownAccount} ${restartReply}\n`);
  await until("private reply delivered after actual app restart", async () => (await history()).some(h => h.text === restartReply));
  await passed("privacy-restart", { restored: restoredPrivacy, realCliReplyAfterRestart: true, historyRetained: true });
  // Restore public mode before evaluating independent contact-list hiding.
  await privacySettings();
  await click("invisible-switch");
  await until("public mode restored", async () => !(await privacyPolicy()).invisible);
  await key("\uE00C");
  await until("privacy settings dismissed", async () => !await exists("settings-dialog"));
  await element(row());

  await contextMenu(row());
  await click(`hide-contact-menu-${account}`);
  await until("cancel receives default focus", () => execute("return document.activeElement?.getAttribute('data-testid')==='hide-contact-cancel';"));
  const confirmationCopy = await execute("return document.querySelector('[data-testid=hide-contact-dialog]').textContent;");
  // The real webview uses the host's locale; verify the same safety promises
  // in either shipped language instead of assuming an English OS.
  assert.match(confirmationCopy, /History is kept|聊天历史保留/);
  assert.match(confirmationCopy, /Messages and calls can still arrive|仍可收到消息和来电/);
  assert.match(confirmationCopy, /signed-in user on this device|本设备当前登录用户/);
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

  await writeChildCommand(peer, "/peers\n");
  await until("CLI discovered GUI identity", () => peer.output.includes(`peer ${userId}`));
  const hiddenText = `native-hidden-${randomBytes(4).toString("hex")}`;
  const guiAccount = await observe("account_id");
  assert.match(guiAccount, /^[0-9a-f]{32}$/);
  await writeChildCommand(peer, `/account-msg ${guiAccount} ${hiddenText}\n`);
  await until("CLI account command completed", () => {
    if (peer.output.includes("account send failed:")) throw new Error(peer.output);
    return peer.output.includes("account message sent");
  });
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
  const otherPrivacy = await privacyPolicy();
  assert.equal(otherPrivacy.owner, signedInUser);
  assert.equal(otherPrivacy.invisible, false);
  assert.deepEqual(otherPrivacy.allowed_accounts, []);
  await passed("local-user-isolation", { secondUserContactVisible: true, secondUserHasNoHiddenEntry: true, secondUserPrivacy: otherPrivacy });
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
  await until("nested hide confirmation unmounted", async () => !await exists("hide-contact-dialog"));
  await settledDialog("hidden-contacts-dialog");
  await passed("settings-search", { actualPeerNameFound: true, onlineRestoreWorked: true, searchAddTabHideWorked: true });
  await closeDialogs();

  await stop(peer, true);
  await until("real offline peer expires from roster", async () => !(await observe("list_peers")).some(p => p.name === peerName), 180000);
  await manage();
  await fill("hidden-contacts-search", peerName);
  const nativeRect = await command("POST", "/window/rect", coreState.minimumWindow);
  report.narrowWindow = { returned: nativeRect, client: await execute("return {width:innerWidth,height:innerHeight,scale:devicePixelRatio};") };
  // WebDriver sizes the outer window; include its measured native decorations
  // to exercise the configured 760x520 client minimum, not a smaller viewport.
  assert.ok(nativeRect.width >= 760 && nativeRect.width <= 800 && nativeRect.height >= 520 && nativeRect.height <= 580, `owned native window reached its minimum-size range: ${JSON.stringify(report.narrowWindow)}`);
  await until("native narrow viewport applied", () => execute("return Math.abs(innerWidth-760)<=1 && Math.abs(innerHeight-520)<=1;"));
  await settledDialog("hidden-contacts-dialog");
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
  await passed("narrow-keyboard-layout", { ...layout, nativeRect, keyboardFocusReachedRestore: true });
  assert.equal(await execute("return document.activeElement?.getAttribute('data-testid');"), `restore-contact-${account}`, "restore focus retained after evidence capture");
  await settledDialog("hidden-contacts-dialog");
  await key("\uE007");
  await until("offline contact saved through keyboard", async () => !(await hiddenContacts()).some(h => h.account_id === account));
  await until("offline restored row removed", async () => !await exists(`restore-contact-${account}`));
  assert.equal((await hiddenContacts()).some(h => h.account_id === account), false);
  assert.ok((await history()).some(h => h.text === beforeText));
  assert.ok((await history()).some(h => h.text === hiddenText));
  await passed("offline-restore", { realPeerStopped: true, rawRosterExpired: true, persistedIdentityRestored: true, durableHistoryRetained: true });
  const errors = await validateEvidence(report, output, { sourceSha: gitSha, platform: process.platform });
  assert.deepEqual(errors, []);
} catch (error) {
  report.failure = redact(error.stack ?? error);
  console.error(redact(error.message));
  process.exitCode = 1;
  try {
    if (session && owned.has(app)) report.dialogDiagnostics = await execute("return {documentHasFocus:document.hasFocus(),activeElement:{tag:document.activeElement?.tagName,testId:document.activeElement?.getAttribute('data-testid'),connected:document.activeElement?.isConnected},dialogs:Array.from(document.querySelectorAll('[role=dialog]')).map(e=>({testId:e.getAttribute('data-testid'),state:e.getAttribute('data-state'),hidden:e.getAttribute('aria-hidden'),pointerEvents:getComputedStyle(e).pointerEvents}))};");
  } catch { /* Preserve the original failure if the webview has already exited. */ }
  try { if (session && owned.has(app)) await screenshot("failure"); } catch { /* driver may already have exited */ }
} finally {
  for (const child of [...owned]) {
    try { await stop(child, child === peer); } catch (error) { report.cleanupFailure = redact(error.message); process.exitCode = 1; }
  }
  report.finishedAt = new Date().toISOString();
  report.elapsedMs = Date.now() - started;
  report.validationErrors = await validateEvidence(report, output, { sourceSha: gitSha, platform: process.platform });
  if (report.validationErrors.length) process.exitCode = 1;
  await writeFile(join(output, "report.json"), JSON.stringify(report, null, 2));
  await writeFile(join(output, "native-app.log"), redact(app?.output ?? "app not started"));
  await writeFile(join(output, "native-peer.log"), redact(peer?.output ?? "peer not started"));
}
