import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, readdir, realpath, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { execute } from "../validation/stage.mjs";
import { removeCliFixture } from "./test-fixtures/cli.mjs";

const owner = fileURLToPath(new URL("../ci-gitleaks.sh", import.meta.url));
const posixFixture = { skip: process.platform === "win32" ? "Linux scanner uses POSIX executable and archive fixtures; workflow policy is checked on every platform" : false };
const canonical = "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb";

async function fixture(t, { mode = "success", version = "8.30.1", scanStatus = 0, wrongDigest = false, linked = false } = {}) {
  const directory = await realpath(await mkdtemp(path.join(tmpdir(), "mesh gitleaks 'quoted'-")));
  t.after(() => removeCliFixture(directory));
  const bin = path.join(directory, "bin"), payload = path.join(directory, "payload"), scratch = path.join(directory, "scratch");
  for (const location of [bin, payload, scratch]) await mkdir(location);
  const events = path.join(directory, "events.jsonl");
  const executable = async (name, content) => { await writeFile(name, content); await chmod(name, 0o755); };
  const scanner = path.join(payload, "gitleaks");
  const scannerBody = `#!/usr/bin/env node
import fs from 'node:fs';
const args=process.argv.slice(2);
fs.appendFileSync(process.env.FIXTURE_EVENTS,JSON.stringify({kind:args[0]==='version'?'version':'scan',args,cwd:process.cwd()})+'\\n');
if(args[0]==='version'){console.log(process.env.FIXTURE_VERSION);}
else process.exitCode=Number(process.env.FIXTURE_SCAN_STATUS);
`;
  if (linked) {
    await executable(path.join(payload, "other"), scannerBody);
    await symlink(path.join(payload, "other"), scanner);
  } else await executable(scanner, scannerBody);
  const archive = path.join(directory, "fixture.tar.gz");
  const tar = spawnSync("tar", ["-czf", archive, "-C", payload, "gitleaks"], { encoding: "utf8", env: { ...process.env, COPYFILE_DISABLE: "1" } });
  assert.equal(tar.status, 0, tar.stderr);
  if (linked) {
    const inventory = spawnSync("tar", ["-tvzf", archive], { encoding: "utf8" });
    assert.match(inventory.stdout, /^l/m, "the actual fixture archive must retain its symbolic link");
  }
  const digest = createHash("sha256").update(await readFile(archive)).digest("hex");
  let script = await readFile(owner, "utf8");
  if (!wrongDigest) {
    assert(script.includes(canonical), "private fixture must replace only the reviewed digest");
    script = script.replace(canonical, digest);
  }
  const copiedOwner = path.join(directory, "owner.sh");
  await writeFile(copiedOwner, script);
  await executable(path.join(bin, "gh"), `#!/usr/bin/env node
import fs from 'node:fs';import path from 'node:path';
const args=process.argv.slice(2), stage=args[args.indexOf('--dir')+1], asset=args[args.indexOf('--pattern')+1];
const target=path.join(stage,asset);
const previous=fs.existsSync(process.env.FIXTURE_EVENTS)?fs.readFileSync(process.env.FIXTURE_EVENTS,'utf8').trim().split('\\n').filter(Boolean).map(JSON.parse):[];
const attempt=previous.filter(e=>e.kind==='download').length+1;
fs.appendFileSync(process.env.FIXTURE_EVENTS,JSON.stringify({kind:'download',args,stage,existed:fs.existsSync(target)})+'\\n');
if(process.env.FIXTURE_MODE==='failure'||process.env.FIXTURE_MODE==='partial'||process.env.FIXTURE_MODE==='recover'&&attempt===1){
 fs.writeFileSync(target,'partial archive');process.exit(process.env.FIXTURE_MODE==='partial'?0:1);
}
fs.copyFileSync(process.env.FIXTURE_ARCHIVE,target);
`);
  // macOS lacks GNU timeout. This private argv adapter bounds an actual child;
  // these mechanics do not qualify GNU timeout or native Linux execution.
  if (process.platform !== "linux") await executable(path.join(bin, "timeout"), `#!/usr/bin/env node
import {spawnSync} from 'node:child_process';
const args=process.argv.slice(2);while(args[0]?.startsWith('--'))args.shift();args.shift();
const result=spawnSync(args[0],args.slice(1),{stdio:'inherit',timeout:5000,killSignal:'SIGKILL'});
process.exitCode=result.error?.code==='ETIMEDOUT'?124:result.status??1;
`);
  // Exercise the real SHA-256 comparison on Darwin without requiring GNU tools.
  // Production uses sha256sum on its Ubuntu runner.
  if (process.platform === "darwin") await executable(path.join(bin, "sha256sum"), `#!/bin/sh
exec /usr/bin/shasum -a 256 "$@"
`);
  const realTar = spawnSync("which", ["tar"], { encoding: "utf8" }).stdout.trim();
  await executable(path.join(bin, "tar"), `#!/usr/bin/env node
import fs from 'node:fs';import{spawnSync}from'node:child_process';
const args=process.argv.slice(2),r=spawnSync(process.env.FIXTURE_TAR,args,{stdio:'inherit'});
const extracted=fs.lstatSync(args[args.indexOf('-C')+1]+'/gitleaks');
fs.appendFileSync(process.env.FIXTURE_EVENTS,JSON.stringify({kind:'extract',code:r.status??1,linked:extracted.isSymbolicLink()})+'\\n');process.exitCode=r.status??1;
`);
  const guard = path.join(scratch, "unrelated");
  await writeFile(guard, "preserve unrelated temporary output");
  const output = [];
  const result = await execute(["bash", copiedOwner], {
    cwd: directory,
    env: { ...process.env, PATH: `${bin}${path.delimiter}${path.dirname(process.execPath)}${path.delimiter}${process.env.PATH}`, TMPDIR: scratch,
      GH_TOKEN: "fixture-only", FIXTURE_MODE: mode, FIXTURE_VERSION: version, FIXTURE_SCAN_STATUS: String(scanStatus),
      FIXTURE_EVENTS: events, FIXTURE_ARCHIVE: archive, FIXTURE_TAR: realTar, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" },
    signal: AbortSignal.timeout(15000), onOutput(chunk) { output.push(chunk); },
  });
  assert.equal(result.cleanupUnverified, false);
  assert.equal(await readFile(guard, "utf8"), "preserve unrelated temporary output");
  assert.deepEqual(await readdir(scratch), ["unrelated"], "every exit must remove only its owned staging");
  const recorded = (await readFile(events, "utf8")).trim().split("\n").filter(Boolean).map(JSON.parse);
  return { result: { ...result, output: Buffer.concat(output).toString() }, events: recorded, directory };
}

test("verified archive runs the exact full-history scan and cleans private staging", posixFixture, async t => {
  const { result, events, directory } = await fixture(t);
  assert.equal(result.code, 0, result.error || result.output);
  const download = events.find(event => event.kind === "download");
  assert.deepEqual(download.args.slice(0, 7), ["release", "download", "v8.30.1", "--repo", "gitleaks/gitleaks", "--pattern", "gitleaks_8.30.1_linux_x64.tar.gz"]);
  assert.deepEqual(events.map(event => event.kind), ["download", "extract", "version", "scan"]);
  assert.deepEqual(events.at(-1), { kind: "scan", args: ["git", ".", "--config", ".gitleaks.toml", "--redact", "-v"], cwd: directory });
});

for (const options of [{ wrongDigest: true }, { mode: "partial" }]) test(`integrity rejection cannot extract or execute: ${JSON.stringify(options)}`, posixFixture, async t => {
  const { result, events } = await fixture(t, options);
  assert.notEqual(result.code, 0, JSON.stringify(events));
  assert.match(result.output, /FAILED|did NOT match/i, "the actual digest comparison must reject the archive");
  assert.deepEqual(events.map(event => event.kind), ["download"], "integrity failure must not retry or admit tar/version/scan");
});

test("failed partial transfers exhaust only two attempts without extraction", posixFixture, async t => {
  const { result, events } = await fixture(t, { mode: "failure" });
  assert.notEqual(result.code, 0);
  assert.deepEqual(events.map(event => event.kind), ["download", "download"]);
  assert(events.every(event => !event.existed), "partial bytes must be removed before the next attempt");
});

test("a failed transport can recover to one verified execution", posixFixture, async t => {
  const { result, events } = await fixture(t, { mode: "recover" });
  assert.equal(result.code, 0, result.error || result.output);
  assert.deepEqual(events.map(event => event.kind), ["download", "download", "extract", "version", "scan"]);
  assert(events.filter(event => event.kind === "download").every(event => !event.existed));
});

test("wrong tool version fails before scanning", posixFixture, async t => {
  const { result, events } = await fixture(t, { version: "0.0.0" });
  assert.notEqual(result.code, 0);
  assert.deepEqual(events.map(event => event.kind), ["download", "extract", "version"]);
});

test("a linked archive member cannot execute", posixFixture, async t => {
  const { result, events } = await fixture(t, { linked: true });
  assert.notEqual(result.code, 0, JSON.stringify(events));
  assert.deepEqual(events.map(event => event.kind), ["download", "extract"]);
  assert.equal(events.at(-1).code, 0, "actual extraction must succeed before the linked executable is rejected");
});

test("scanner findings preserve the scanner exit status", posixFixture, async t => {
  const { result, events } = await fixture(t, { scanStatus: 23 });
  assert.equal(result.code, 23);
  assert.equal(events.at(-1).kind, "scan");
});
