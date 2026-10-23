import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import test from "node:test";

// Emulate gh's per-page --jq processing, including its failure propagation.
const fakeGh = `#!${process.execPath}
const fs = require('node:fs');
const { spawnSync } = require('node:child_process');
const args = process.argv.slice(2);
fs.writeFileSync(process.env.ARG_LOG, JSON.stringify(args));
let pages = JSON.parse(fs.readFileSync(process.env.API_PAGES));
if (process.env.STALE_RELEASE_CACHE === '1' && !args.includes('Cache-Control: no-cache')) pages = [[]];
const filter = args[args.indexOf('--jq') + 1];
if (!args.includes('--jq')) { process.stdout.write(JSON.stringify(pages)); process.exit(0); }
const result = spawnSync('jq', ['-cr', filter], { input: pages.map(JSON.stringify).join('\\n'), encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
process.stdout.write(result.stdout ?? '');
process.stderr.write(result.stderr ?? '');
process.exit(result.status ?? 1);
`;

test("release state reads revalidate cached empty release listings", { skip: process.platform === "win32" }, (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-release-cache-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "bin"));
  writeFileSync(join(root, "bin/gh"), fakeGh, { mode: 0o755 });
  const release = { id: 42, tag_name: "v0.4.4", draft: true, prerelease: false };
  const pages = join(root, "pages.json");
  writeFileSync(pages, JSON.stringify([[release]]));
  const module = new URL("../release/release-api.mjs", import.meta.url).href;
  const result = execFileSync(process.execPath, ["--input-type=module", "-e", `import {getReleaseByTag} from ${JSON.stringify(module)}; console.log(JSON.stringify(getReleaseByTag('owner/repo', 'v0.4.4')));`], {
    encoding: "utf8",
    env: { ...process.env, PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`, API_PAGES: pages, ARG_LOG: join(root, "args.json"), STALE_RELEASE_CACHE: "1" },
  });
  assert.deepEqual(JSON.parse(result), release, "a cached absence must not hide the actual draft");
});

test("release API filters large paginated histories before Node captures metadata", { skip: process.platform === "win32" }, (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-release-api-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "bin"));
  writeFileSync(join(root, "bin/gh"), fakeGh, { mode: 0o755 });
  const selected = { id: 42, tag_name: "v0.1.6-nightly.123.1", draft: false, prerelease: true, target_commitish: "a".repeat(40), assets: [{ name: "selected.zip" }] };
  const unrelated = Array.from({ length: 300 }, (_, id) => ({ id, tag_name: `v0.1.6-nightly.${id + 1000}.1`, draft: false, prerelease: true, target_commitish: "b".repeat(40), assets: [{ metadata: "x".repeat(10000) }] }));
  const api = join(root, "pages.json");
  const log = join(root, "args.json");
  const module = new URL("../release/release-api.mjs", import.meta.url).href;
  const run = (expression, pages) => {
    writeFileSync(api, JSON.stringify(pages));
    return JSON.parse(execFileSync(process.execPath, ["--input-type=module", "-e", `import * as api from ${JSON.stringify(module)}; console.log(JSON.stringify(${expression}));`], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`, API_PAGES: api, ARG_LOG: log } }));
  };
  const pages = [unrelated.slice(0, 100), unrelated.slice(100), [selected]];
  assert.deepEqual(run(`api.getReleaseByTag('owner/repo', '${selected.tag_name}')`, pages), selected);
  const args = JSON.parse(readFileSync(log, "utf8"));
  assert.ok(args.includes("--paginate"));
  assert.ok(args.includes("--jq"), "history must be filtered in gh before crossing the capture buffer");
  assert.ok(!args.includes("--slurp"), "gh should process pages incrementally");
  const summary = run(`api.getPublishedNightly('owner/repo', '${selected.target_commitish}')`, pages);
  assert.equal(summary.tag_name, selected.tag_name);
  assert.equal(Object.hasOwn(summary, "assets"), false, "schedule dedup must capture only a lean summary");
  assert.equal(run(`api.getReleaseByTag('owner/repo', 'v9.9.9')`, pages), null);
  assert.deepEqual(run(`api.getReleaseByTag('owner/repo', '${selected.tag_name}')`, [[...unrelated], [{ ...selected, draft: true }]]), { ...selected, draft: true }, "draft lookup must span every page");
  assert.equal(run("api.getLatestPublishedStable('owner/repo')", pages), null);
  assert.deepEqual(run("api.getLatestPublishedStable('owner/repo')", [[
    { tag_name: "v0.4.9", draft: false, prerelease: false, assets: ["excluded"] },
    { tag_name: "v0.4.10", draft: false, prerelease: false },
    { tag_name: "v0.5.0", draft: true, prerelease: false },
    { tag_name: "v0.6.0", draft: false, prerelease: true },
    { tag_name: "v1.0.0-nightly.3.1", draft: false, prerelease: false },
  ]]), { tag_name: "v0.4.10" }, "highest published numeric Stable wins, independent of creation order");
  const unusualTag = 'v0.1.6" or true #';
  assert.equal(run(`api.getReleaseByTag('owner/repo', ${JSON.stringify(unusualTag)})`, pages), null, "tag values must remain a jq string literal");
  assert.throws(() => run(`api.getReleaseByTag('owner/repo', '${selected.tag_name}')`, [[selected], [selected]]), /multiple releases/);
  assert.throws(() => run("api.getReleaseByTag('owner/repo', 'v9.9.9')", [{ releases: [] }]), /invalid release list page/);
  assert.throws(() => run("api.getReleaseByTag('owner/repo', 'v9.9.9')", [[null]]), /invalid release list entry/);
  assert.equal(run(`api.getPublishedNightly('owner/repo', '${selected.target_commitish}')`, [[{ ...selected, draft: true }]]), null);
});
