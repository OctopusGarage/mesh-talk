// Real source-pinned desktop processes, disposable profiles and production pack/IPC paths.
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { createServer } from "node:net";
import { createSocket } from "node:dgram";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { verifyNativeBuild } from "../validation/native-build.mjs";
import { captureNativeEvidence } from "./native-capture.mjs";
import { enterNativeAccount, settledSignedInSession } from "./native-core-scenarios.mjs";

const require = createRequire(new URL("../../frontend/package.json", import.meta.url));
const { unzipSync, zipSync, strToU8 } = require("fflate");
const PACK = "native-sticker-acceptance";
const FALLBACK = "👋";
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const selector = id => `[data-testid="${id}"]`;

/** Derive a one-image pack from the maintained offline marketplace fixture. */
export function stickerFixtures(zip) {
  const files = unzipSync(zip);
  const source = JSON.parse(new TextDecoder().decode(files["manifest.json"]));
  assert.equal(source.kind, "sticker");
  const images = source.stickers.map(sticker => ({ bytes: files[sticker.file], ext: sticker.file.split(".").at(-1) }));
  const original = images[0], replaced = images.find(image => hash(image.bytes) !== hash(original.bytes));
  assert.ok(replaced, "reviewed fixture provides distinct artwork");
  function pack(image, version) {
    const filename = `images/wave.${image.ext}`;
    const manifest = { format: 1, id: PACK, version, name: "Native Sticker Acceptance", kind: "sticker", stickers: ["missing", "installed", "replaced"].map(id => ({ id, label: id, fallback: FALLBACK, file: filename })) };
    const name = `mt-sticker-v1-${Buffer.from(`pack:${PACK}:installed\n${FALLBACK}`).toString("base64url")}.${image.ext}`;
    return { ...image, name, sha256: hash(image.bytes), zip: zipSync({ "manifest.json": strToU8(JSON.stringify(manifest)), [filename]: image.bytes }) };
  }
  return { original: pack(original, "1.0.0"), replaced: pack(replaced, "2.0.0") };
}

/** Evidence is specific to received media, not legacy ID-only sticker rendering. */
export function validateStickerEvidence(report) {
  assert.equal(report.native, true);
  assert.equal(report.mocked, false);
  assert.equal(report.buildVerified, true);
  assert.match(report.sourceSha, /^[a-f0-9]{40,64}$/);
  assert.equal(report.receiverRestarted, true);
  assert.equal(report.originalMediaPreserved, true);
  assert.equal(report.productionRegistrationAccepted, true);
  assert.match(report.originalSha256, /^[a-f0-9]{64}$/);
  const ids = new Set();
  for (const kind of ["missing", "installed", "replaced", "repeat"]) {
    const item = report.cases[kind];
    assert.match(item.id, /^[a-f0-9]{64}$/);
    assert.ok(!ids.has(item.id), "intentional sends retain distinct identities");
    ids.add(item.id);
    assert.match(item.fileConv, /^[a-f0-9]{64}$/);
    assert.equal(item.historyRows, 1);
    assert.equal(item.sha256, report.originalSha256);
    assert.equal(item.decoded, true);
    assert.ok(item.width > 0 && item.height > 0);
    assert.equal(item.delivered, true);
    assert.match(item.captureSha256, /^[a-f0-9]{64}$/);
  }
  assert.equal(new Set(["missing", "installed", "replaced"].map(kind => report.cases[kind].fileConv)).size, 3, "each source scenario stages an independent transfer");
  assert.equal(report.cases.repeat.fileConv, report.cases.replaced.fileConv, "repeat exercises same-conversation transfer reuse");
  assert.ok(report.limitations.includes("Native older-peer and paired-device alias acceptance are not covered by this scenario."));
}

async function until(description, check, timeout = 60_000) {
  const deadline = Date.now() + timeout;
  let last;
  while (Date.now() < deadline) {
    try { const value = await check(); if (value) return value; } catch (error) { last = error; }
    await delay(200);
  }
  throw Error(`Timed out: ${description}${last ? ` (${last.message})` : ""}`);
}
async function freePort(udp = false) {
  const socket = udp ? createSocket("udp4") : createServer();
  await new Promise((ok, fail) => { socket.once("error", fail); udp ? socket.bind(0, "127.0.0.1", ok) : socket.listen(0, "127.0.0.1", ok); });
  const port = socket.address().port;
  await new Promise(ok => socket.close(ok));
  return port;
}

class Desktop {
  constructor(binary, root, discoveryPort, password) { Object.assign(this, { binary, root, discoveryPort, password }); }
  async start() {
    const port = await freePort();
    this.endpoint = `http://127.0.0.1:${port}`;
    this.log = "";
    this.child = spawn(this.binary, [this.root, String(port), String(this.discoveryPort)], { stdio: ["ignore", "pipe", "pipe"] });
    const append = data => { this.log = (this.log + String(data).replaceAll(this.password, "[REDACTED]")).slice(-64_000); };
    this.child.stdout.on("data", append); this.child.stderr.on("data", append); this.child.on("error", append);
    await until("isolated embedded native driver", async () => {
      assert.equal(this.child.exitCode, null, this.log);
      return (await this.request("GET", "/status"))?.ready;
    });
    this.session = (await this.request("POST", "/session", { capabilities: { alwaysMatch: {} } })).sessionId;
    assert.ok(this.session);
    await this.command("POST", "/timeouts", { script: 15_000, implicit: 0, pageLoad: 10_000 });
  }
  async stop() {
    if (!this.child || this.child.exitCode !== null || this.child.signalCode) return;
    const exited = new Promise(resolve => this.child.once("close", resolve));
    this.child.kill("SIGTERM");
    await Promise.race([exited, delay(10_000)]);
    if (this.child.exitCode === null && !this.child.signalCode) { this.child.kill("SIGKILL"); await exited; }
  }
  async request(method, path, body) {
    const response = await fetch(this.endpoint + path, { method, headers: { "Content-Type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(20_000) });
    const result = await response.json();
    assert.ok(response.ok && !result.value?.error, `native driver: ${result.value?.message ?? response.status}`);
    return result.value;
  }
  command = (method, path, body) => this.request(method, `/session/${this.session}${path}`, body);
  execute = (script, args = []) => this.command("POST", "/execute/sync", { script, args });
  invoke = async (command, params = {}) => {
    const result = await this.command("POST", "/execute/async", { script: "const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(value=>done({ok:true,value}),error=>done({ok:false,error:String(error)}));", args: [command, params] });
    assert.equal(result.ok, true, `${command}: ${result.error}`);
    return result.value;
  };
  async focus() {
    if (process.platform !== "darwin") return;
    const result = await this.command("POST", "/execute/async", { script: `const done=arguments[arguments.length-1],ipc=window.__TAURI_INTERNALS__,nonce=arguments[0],event='native-contact-key-result';let eventId;const handler=ipc.transformCallback(e=>{if(e.payload.nonce!==nonce)return;ipc.invoke('plugin:event|unlisten',{event,eventId}).then(()=>done(e.payload));});ipc.invoke('plugin:event|listen',{event,target:{kind:'Any'},handler}).then(id=>{eventId=id;return ipc.invoke('plugin:event|emit',{event:'native-contact-key',payload:{key:'Focus',nonce}});}).catch(error=>done({error:String(error)}));`, args: [randomBytes(8).toString("hex")] });
    assert.equal(result.error, null);
  }
  element = async id => {
    await until(`native element ${id}`, () => this.execute("const e=document.querySelector(arguments[0]);return !!e&&e.getBoundingClientRect().width>0;", [selector(id)]));
    return (await this.command("POST", "/element", { using: "css selector", value: selector(id) }))["element-6066-11e4-a52e-4f735466cecf"];
  };
  click = async id => { await this.focus(); await this.command("POST", `/element/${await this.element(id)}/click`, {}); };
  fill = async (id, text) => { const element = await this.element(id); await this.command("POST", `/element/${element}/clear`, {}); await this.command("POST", `/element/${element}/value`, { text }); };
  async login(username, register) {
    await this.element("login-form");
    await until("fixture login preferences loaded", () => this.execute("return document.querySelector('[data-testid=login-stay-signed-in]')?.disabled===false;"));
    if (await this.execute("return document.querySelector('[data-testid=login-stay-signed-in]').checked;")) await this.click("login-stay-signed-in");
    await enterNativeAccount(this, username, this.password, register);
    const owners = await until("disposable account namespace", async () => {
      const entries = (await readdir(join(this.root, "accounts"), { withFileTypes: true })).filter(entry => entry.isDirectory());
      return entries.length && entries;
    });
    assert.equal(owners.length, 1, "only the disposable fixture account exists");
    this.owner = owners[0].name;
    await settledSignedInSession({ observe: this.invoke, execute: this.execute, until }, this.owner);
    this.account = await this.invoke("account_id");
  }
  async openStickers() {
    if (await this.execute("return !!document.querySelector('[data-testid=sticker-panel]');")) return;
    if (!await this.execute("const e=document.querySelector('[data-testid=composer-emoji]');return !!e&&e.getBoundingClientRect().width>0;")) await this.click("composer-more-tools");
    if (!await this.execute("return !!document.querySelector('[data-testid=composer-stickers]');")) await this.click("composer-emoji");
    await this.click("composer-stickers");
  }
  async install(artwork, version) {
    await this.openStickers();
    // Standard webview File/Change events exercise the real ZIP parser/store;
    // no application IPC or store method is replaced. OS file picker is excluded.
    await this.execute("const details=document.querySelector('[data-testid=sticker-panel] details');details.open=true;const input=details.querySelector('input[type=file]'),data=new DataTransfer();data.items.add(new File([Uint8Array.from(atob(arguments[0]),c=>c.charCodeAt(0))],'native-sticker.zip',{type:'application/zip'}));input.files=data.files;input.dispatchEvent(new Event('change',{bubbles:true}));", [Buffer.from(artwork.zip).toString("base64")]);
    await until("production pack import committed", () => this.execute("return !!document.querySelector(arguments[0])&&document.querySelector('[data-testid=pack-manager-sticker]').textContent.includes(arguments[1]);", [selector(`sticker-pack-${PACK}`), `v${version}`]));
    await this.invoke("register_sticker_source", { owner: this.owner, account: this.account, name: artwork.name, bytes: Array.from(artwork.bytes) });
    await this.click("composer-emoji");
  }
  history = account => this.invoke("owner_account_history", { owner: this.owner, account, limit: 500 });
  async media(fileConv) {
    const result = await this.command("POST", "/execute/async", { script: "const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke('read_media',{fileConv:arguments[0]}).then(value=>done({ok:true,bytes:Array.from(Array.isArray(value)?value:new Uint8Array(value))}),error=>done({ok:false,error:String(error)}));", args: [fileConv] });
    assert.equal(result.ok, true, result.error);
    return Buffer.from(result.bytes);
  }
}

export async function runNativeStickerAcceptance() {
  const sourceSha = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  const appBinary = resolve(process.env.NATIVE_CONTACT_APP ?? `target/release/examples/native-contact-eval${process.platform === "win32" ? ".exe" : ""}`);
  const build = await verifyNativeBuild({ sourceSha, appBinary });
  assert.equal(build.buildVerified, true, "native acceptance requires clean source-pinned candidate build");
  const output = resolve(process.env.NATIVE_STICKER_OUTPUT ?? "target/native-sticker-acceptance");
  await mkdir(output, { recursive: true });
  const fixture = await mkdtemp(join(output, "fixture-")), nonce = randomBytes(6).toString("hex");
  const discoveryPort = await freePort(true), password = randomBytes(24).toString("hex");
  const apps = ["sender", "receiver"].map(role => new Desktop(appBinary, join(fixture, `${role}-${nonce}`, "app"), discoveryPort, password));
  const [sender, receiver] = apps;
  const names = [`sticker-sender-${nonce}`, `sticker-receiver-${nonce}`];
  const artwork = stickerFixtures(await readFile("site/market/packs/google-noto.zip"));
  const report = { schema: 1, native: true, mocked: false, sourceSha, buildVerified: build.buildVerified, buildProof: build, platform: process.platform, originalSha256: artwork.original.sha256, cases: {}, limitations: ["Native older-peer and paired-device alias acceptance are not covered by this scenario.", "Standard W3C File/Change events select the fixture ZIP; the OS file picker is excluded.", "Exact ciphertext omission and legacy fallback are asserted by core protocol loopback regressions, not inferred from native rendering."] };
  let first;
  async function sendCase(kind) {
    const prior = new Set((await sender.history(receiver.account)).map(row => row.id));
    await sender.openStickers();
    await sender.click(`sticker-pack-${PACK}`);
    await sender.click(`sticker-option-pack:${PACK}:${kind === "repeat" ? "replaced" : kind}`);
    const sent = await until("new intentional sticker accepted", async () => (await sender.history(receiver.account)).find(row => row.file?.media && !prior.has(row.id)));
    const rows = await until("exact sticker received once", async () => {
      const matches = (await receiver.history(sender.account)).filter(row => row.id === sent.id);
      assert.ok(matches.length <= 1, "one logical native history row per intended send");
      return matches.length === 1 && matches;
    });
    const received = rows[0];
    assert.equal(received.file.file_conv, sent.file.file_conv);
    const bytes = await until("native durable image bytes", () => receiver.media(received.file.file_conv));
    assert.equal(hash(bytes), artwork.original.sha256);
    const image = await until("real native webview decoded received sticker", () => receiver.execute("const img=document.querySelector('[data-message-id=\"'+arguments[0]+'\"] [data-testid=message-sticker]'),r=img?.getBoundingClientRect();return img?.complete&&img.naturalWidth>0&&r.width>0&&r.top>=0&&r.bottom<=innerHeight&&{decoded:true,width:img.naturalWidth,height:img.naturalHeight};", [received.id]));
    await until("exact sender completion", async () => (await sender.invoke("owner_delivery_statuses", { owner: sender.owner, account: receiver.account, ids: [sent.id] })).some(row => row.id === sent.id && row.status === "delivered"));
    await receiver.focus();
    const { png } = await captureNativeEvidence({ command: receiver.command, execute: receiver.execute, until, focus: () => receiver.focus() });
    await writeFile(join(output, `${kind}.png`), png);
    report.cases[kind] = { id: sent.id, fileConv: received.file.file_conv, historyRows: rows.length, sha256: hash(bytes), ...image, delivered: true, captureSha256: hash(png) };
    first ??= received;
  }
  try {
    for (let index = 0; index < apps.length; index++) { await apps[index].start(); await apps[index].login(names[index], true); }
    for (const [app, peer] of [[sender, receiver], [receiver, sender]]) {
      await until("signed disposable peer discovery", async () => (await app.invoke("list_peers")).some(row => row.account_id === peer.account));
      await app.click(`conversation-row-${peer.account}`);
    }
    await sender.install(artwork.original, "1.0.0");
    await sendCase("missing");
    await receiver.install(artwork.original, "1.0.0");
    report.productionRegistrationAccepted = true;
    await sendCase("installed");
    await receiver.install(artwork.replaced, "2.0.0");
    await receiver.stop(); await receiver.start(); await receiver.login(names[1], false);
    await receiver.click(`conversation-row-${sender.account}`);
    report.receiverRestarted = true;
    await sendCase("replaced");
    await sendCase("repeat");
    assert.equal(hash(await receiver.media(first.file.file_conv)), artwork.original.sha256);
    report.originalMediaPreserved = true;
    const finalBuild = await verifyNativeBuild({ sourceSha, appBinary, expectedManifestDigest: build.manifestDigest });
    assert.equal(finalBuild.buildVerified, true);
    validateStickerEvidence(report);
    report.passed = true;
  } catch (error) {
    report.passed = false; report.error = String(error.message).replaceAll(password, "[REDACTED]");
    throw error;
  } finally {
    await Promise.all(apps.map(app => app.stop()));
    await writeFile(join(output, "report.json"), JSON.stringify(report, null, 2));
    for (let index = 0; index < apps.length; index++) await writeFile(join(output, `${index === 0 ? "sender" : "receiver"}.log`), apps[index].log ?? "");
  }
  console.log(`PASS native sticker acceptance: ${join(output, "report.json")}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await runNativeStickerAcceptance();
