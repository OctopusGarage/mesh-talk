import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
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
  const env = { LINUX: "success", DESKTOP: "success", COVERAGE: "success", PACKS: "success", CHROMIUM: "success", PORTABLE: "success", EXTENDED_CHROMIUM: "skipped", FIREFOX: "success", EXTENDED: "false" };
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

test("browsers, Rust and native use ARM while Health and backend share Intel capacity", () => {
  const health = readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8");
  const rust = readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8");
  const native = readFileSync(new URL("../../.github/workflows/hidden-contacts-native.yml", import.meta.url), "utf8");
  assert.match(job("desktop"), /operating_systems: '\["macos-latest","windows-latest"\]'/);
  assert.match(job("portable"), /os: macos-latest, browser: webkit/);
  assert.match(job("chromium-extended"), /os: \[ubuntu-24.04, macos-latest, windows-latest\]/);
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


// GitHub documents that reusable workflows inherit the caller's github context:
// https://docs.github.com/en/actions/reference/workflows-and-actions/reusing-workflow-configurations
function concurrency(file, github) {
  const source = readFileSync(new URL(`../../.github/workflows/${file}.yml`, import.meta.url), "utf8");
  const block = source.split("concurrency:\n")[1].split(/\n\S/)[0];
  const evaluate = text => text.replace(/\$\{\{ (.*?) \}\}/g, (_, expression) => String(Function("github", `return (${expression})`)(github)));
  return { group: evaluate(block.match(/^  group: (.+)$/m)[1]).toLowerCase(), cancel: evaluate(block.match(/^  cancel-in-progress: (.+)$/m)[1]) === "true" };
}

test("UI and reusable Health isolate caller/event streams while superseded candidates cancel", () => {
  const context = { workflow: "UI E2E", event_name: "schedule", ref: "refs/heads/main", event: { pull_request: {} } };
  const ui = concurrency("e2e-ui", context), called = concurrency("check-health", context);
  const standalone = concurrency("check-health", { ...context, workflow: "Code Quality and Health Check" });
  assert.notEqual(ui.group, called.group, "a called workflow must never cancel its caller");
  assert.notEqual(called.group, standalone.group, "standalone Health cannot replace same-run UI evidence");
  for (const owner of ["check-health", "e2e-ui"]) {
    assert.notEqual(concurrency(owner, context).group, concurrency(owner, { ...context, event_name: "push" }).group);
    assert.notEqual(concurrency(owner, context).group, concurrency(owner, { ...context, event_name: "workflow_dispatch" }).group);
    assert.equal(concurrency(owner, context).cancel, false);
    assert.equal(concurrency(owner, { ...context, event_name: "workflow_dispatch" }).cancel, false);
    for (const event_name of ["pull_request", "merge_group", "push"]) {
      const candidate = { ...context, event_name, ref: event_name === "push" ? "refs/heads/main" : "refs/merge-queue/main/candidate", event: event_name === "pull_request" ? { pull_request: { number: 42 } } : { pull_request: {} } };
      assert.equal(concurrency(owner, candidate).cancel, true);
      assert.equal(concurrency(owner, candidate).group, concurrency(owner, { ...candidate, sha: "replacement" }).group);
      assert.notEqual(concurrency(owner, candidate).group, concurrency(owner, { ...candidate, ref: event_name === "push" ? "refs/heads/dev" : "refs/merge-queue/main/other", event: event_name === "pull_request" ? { pull_request: { number: 43 } } : { pull_request: {} } }).group);
    }
  }
});

for (const owner of ["ci", "e2e-backend"]) test(`${owner} isolates evidence events while superseded candidates cancel`, () => {
  const context = { workflow: owner, ref: "refs/heads/main", event: { pull_request: {} } };
  const groups = [];
  for (const event_name of ["push", "pull_request", "merge_group", "schedule", "workflow_dispatch"]) {
    const event = { ...context, event_name };
    const observed = concurrency(owner, event);
    groups.push(observed.group);
    assert.equal(observed.cancel, ["push", "pull_request", "merge_group"].includes(event_name));
    assert.equal(observed.group, concurrency(owner, { ...event, sha: "replacement" }).group);
    assert.notEqual(observed.group, concurrency(owner, { ...event, ref: "refs/heads/other" }).group);
  }
  assert.equal(new Set(groups).size, groups.length, "independent event streams must not share a cancellation group");
  const pr = { ...context, event_name: "pull_request", event: { pull_request: { number: 42 } } };
  assert.notEqual(concurrency(owner, pr).group, concurrency(owner, { ...pr, event: { pull_request: { number: 43 } } }).group);
});

test("scheduled and manual fuzz executions remain separate without cancellation", () => {
  const context = { workflow: "Fuzz", event_name: "schedule", ref: "refs/heads/main", event: {} };
  const scheduled = concurrency("fuzz", context);
  const manual = concurrency("fuzz", { ...context, event_name: "workflow_dispatch" });
  assert.notEqual(scheduled.group, manual.group);
  assert.equal(scheduled.cancel, false);
  assert.equal(manual.cancel, false);
});

test("Firefox core runs on ordinary candidates and the final gate requires its real success", () => {
  const firefox = job("firefox-extended");
  assert.doesNotMatch(firefox, /^    if:/m, "core Firefox must not be scheduled-only");
  assert.match(firefox, /timeout-minutes: 20/);
  assert.match(firefox, /EVAL_TIER:.*'extended' \|\| 'core'/);
  assert.match(firefox, /browser-report\.mjs.*'extended' \|\| 'core'/);
  const env = { LINUX: "success", DESKTOP: "success", COVERAGE: "success", PACKS: "success", CHROMIUM: "success", PORTABLE: "success", EXTENDED_CHROMIUM: "skipped", FIREFOX: "success", EXTENDED: "false" };
  assert.equal(gate("Require complete selected UI matrices", env), 0);
  for (const EXTENDED of ["false", "true"]) for (const FIREFOX of ["failure", "cancelled", "skipped", ""]) {
    assert.notEqual(gate("Require complete selected UI matrices", { ...env, EXTENDED, FIREFOX, EXTENDED_CHROMIUM: EXTENDED === "true" ? "success" : "skipped" }), 0);
  }
});

function renderReports({ linux = "success", desktop = "skipped", platforms = ["ubuntu-24.04"], omit = "", attempt = "3", renderExit = "0" } = {}) {
  const directory = mkdtempSync(path.join(tmpdir(), "mesh-ui-report-inventory-"));
  try {
    mkdirSync(path.join(directory, "frontend"));
    for (const os of platforms) for (const shard of [1, 2, 3, 4]) {
      if (`${os}-${shard}` === omit) continue;
      const blob = path.join(directory, "target", "ui-reports", `playwright-ui-${os}-${shard}-${attempt}`, "blob-report");
      mkdirSync(blob, { recursive: true });
      writeFileSync(path.join(blob, `report-${shard}.zip`), "actual shard blob placeholder");
    }
    const block = root().split(/^      - /m).find(value => value.startsWith("name: Render each OS report from its raw blobs\n"));
    const script = block.split("        run: |\n")[1].replace(/^          /gm, "").replaceAll("${{ github.run_attempt }}", "3");
    const result = spawnSync("bash", ["-e", "-c", 'npx() { printf "%s\\n" "$PLAYWRIGHT_HTML_OUTPUT_DIR" >> "$RENDERED"; return "$RENDER_EXIT"; };\n' + script], {
      cwd: path.join(directory, "frontend"), encoding: "utf8",
      env: { ...process.env, LINUX_RESULT: linux, DESKTOP_RESULT: desktop, RENDERED: path.join(directory, "rendered"), RENDER_EXIT: renderExit },
    });
    let rendered = [];
    try { rendered = readFileSync(path.join(directory, "rendered"), "utf8").trim().split("\n"); } catch {}
    return { ...result, rendered };
  } finally { rmSync(directory, { recursive: true, force: true }); }
}

test("HTML reports render admitted producers and distinguish skipped desktop from missing evidence", () => {
  assert.match(job("reports"), /LINUX_RESULT: \$\{\{ needs\.linux\.result \}\}/);
  assert.match(job("reports"), /DESKTOP_RESULT: \$\{\{ needs\.desktop\.result \}\}/);
  const linuxOnly = renderReports();
  assert.equal(linuxOnly.status, 0, linuxOnly.stderr);
  assert.deepEqual(linuxOnly.rendered, ["playwright-report/ubuntu-24.04"]);
  const platforms = ["ubuntu-24.04", "macos-latest", "windows-latest"];
  const complete = renderReports({ desktop: "success", platforms });
  assert.equal(complete.status, 0, complete.stderr);
  assert.deepEqual(complete.rendered, platforms.map(os => `playwright-report/${os}`));
  for (const linux of ["success", "failure", "cancelled"]) {
    assert.equal(renderReports({ linux }).status, 0, `admitted ${linux} producer retains its report`);
    assert.notEqual(renderReports({ linux, omit: "ubuntu-24.04-2" }).status, 0, `${linux} must retain all four raw blobs`);
  }
  for (const desktop of ["success", "failure", "cancelled"]) {
    assert.equal(renderReports({ desktop, platforms }).status, 0, `${desktop} retains every admitted platform report`);
    assert.notEqual(renderReports({ desktop }).status, 0, `${desktop} cannot silently omit desktop artifacts`);
    for (const os of platforms) assert.notEqual(renderReports({ desktop, platforms, omit: `${os}-3` }).status, 0, `${os} missing shard`);
  }
  assert.notEqual(renderReports({ renderExit: "37" }).status, 0, "renderer failure remains a presentation failure");
  assert.notEqual(renderReports({ attempt: "2" }).status, 0, "previous-attempt blobs cannot replace this attempt");
  for (const result of ["", "unknown", "in_progress"]) {
    assert.notEqual(renderReports({ linux: result }).status, 0, `unknown Linux conclusion ${result}`);
    assert.notEqual(renderReports({ desktop: result, platforms }).status, 0, `unknown desktop conclusion ${result}`);
  }
  assert.equal(renderReports({ linux: "skipped", desktop: "success", platforms: ["macos-latest", "windows-latest"] }).status, 0);
  assert.notEqual(renderReports({ linux: "skipped", desktop: "skipped", platforms: [] }).status, 0, "empty presentation cannot claim a rendered report");
});
