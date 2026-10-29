import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import test from "node:test";

const root = () => readFileSync(new URL("../../.github/workflows/e2e-ui.yml", import.meta.url), "utf8");
const job = name => root().split(`  ${name}:\n`)[1]?.split(/^  [a-z][\w-]*:\n/m)[0];
function gate(name, env) {
  const block = root().split(/^      - /m).find(step => step.startsWith(`name: ${name}\n`));
  assert.ok(block, name);
  const script = block.split("        run: |\n")[1].replace(/^          /gm, "");
  return spawnSync("bash", ["-e", "-c", script], { encoding: "utf8", env: { ...process.env, ...env } }).status;
}

test("Linux coverage requires Linux and Health without waiting for desktop shards", () => {
  assert.match(job("coverage"), /needs: \[linux, health\]/);
  assert.doesNotMatch(job("coverage"), /needs:.*desktop/);
  for (const linux of ["success", "failure", "cancelled", "skipped", ""]) {
    assert.equal(gate("Require all browser shards", { BASELINE: linux, UNIT_COVERAGE: "success", DESKTOP: "failure" }) === 0, linux === "success");
  }
});

test("portable acceptance and final UI gate reject every unsuccessful Linux or desktop result", () => {
  const env = { LINUX: "success", DESKTOP: "success", COVERAGE: "success", PACKS: "success", CHROMIUM: "success", PORTABLE: "success", EXTENDED_CHROMIUM: "skipped", FIREFOX: "skipped", EXTENDED: "false" };
  for (const name of ["portable-evidence", "reports"]) assert.match(job(name), /needs: \[linux, desktop\]/);
  assert.match(job("ui-gate"), /needs: \[linux, desktop,/);
  for (const linux of ["success", "failure", "cancelled", "skipped", ""]) {
    for (const desktop of ["success", "failure", "cancelled", "skipped", ""]) {
      const expected = linux === "success" && desktop === "success";
      assert.equal(gate("Require all full Chromium shards", { LINUX: linux, DESKTOP: desktop }) === 0, expected);
      assert.equal(gate("Require complete selected UI matrices", { ...env, LINUX: linux, DESKTOP: desktop }) === 0, expected);
    }
  }
});

test("one reusable shard implementation retains twelve runners and all evidence identities", () => {
  const reusable = readFileSync(new URL("../../.github/workflows/ui-shards.yml", import.meta.url), "utf8");
  assert.match(reusable, /operating_systems:\n        type: string\n        required: true/);
  assert.match(reusable, /os: \$\{\{ fromJSON\(inputs.operating_systems\) \}\}/);
  assert.match(reusable, /shard: \[1, 2, 3, 4\]/);
  assert.match(job("linux"), /operating_systems: '\["ubuntu-24.04"\]'/);
  assert.match(job("desktop"), /operating_systems: '\["macos-latest","windows-latest"\]'/);
  assert.equal((root().match(/uses: \.\/\.github\/workflows\/ui-shards.yml/g) ?? []).length, 2);
  assert.ok(!root().includes("name: Run UI E2E"));
  assert.match(reusable, /npx playwright test --workers=1 --retries=0 --shard=\$\{\{ matrix.shard \}\}\/4 --reporter=list,blob,json/);
  assert.match(reusable, /MESH_TALK_E2E_COVERAGE: \$\{\{ runner.os == 'Linux' && '1' \|\| '0' \}\}/);
  for (const prefix of ["playwright-ui", "chromium-portable"]) assert.ok(reusable.includes(`name: ${prefix}-`));
  assert.match(reusable, /SOURCE_SHA: \$\{\{ github.sha \}\}/);
  assert.match(reusable, /if: always\(\)/);
  assert.match(reusable, /if-no-files-found: error/);
});

test("full Chromium, Rust and native use ARM while portable work shares Intel capacity", () => {
  const health = readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8");
  const rust = readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8");
  const native = readFileSync(new URL("../../.github/workflows/hidden-contacts-native.yml", import.meta.url), "utf8");
  assert.match(job("desktop"), /operating_systems: '\["macos-latest","windows-latest"\]'/);
  assert.match(job("portable"), /os: macos-26-intel, browser: webkit/);
  assert.match(job("chromium-extended"), /os: \[ubuntu-24.04, macos-26-intel, windows-latest\]/);
  assert.match(job("portable-evidence"), /macos-latest:darwin/);
  assert.match(job("reports"), /for os in ubuntu-24.04 macos-latest windows-latest/);
  assert.match(health, /os: \[ubuntu-latest, macos-26-intel, windows-latest\]/);
  assert.match(rust, /os: \[ubuntu-latest, macos-latest\]/);
  assert.match(native, /os: \[ubuntu-24.04, windows-2022, macos-latest\]/);
});

test("desktop browser burst waits for Health while Linux starts independently", () => {
  assert.match(job("desktop"), /needs: health/);
  assert.doesNotMatch(job("desktop"), /if: always\(\)/);
  assert.doesNotMatch(job("linux"), /needs:/);
});
