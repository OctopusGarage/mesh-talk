#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const ROOT = new URL("..", import.meta.url);

function runEval(args = [], env = {}) {
  return spawnSync(process.execPath, ["scripts/ai-eval.mjs", ...args], {
    cwd: ROOT,
    env: {
      ...process.env,
      OPENAI_API_KEY: "",
      ANTHROPIC_API_KEY: "",
      AI_EVAL_COMMAND: "",
      ...env,
    },
    encoding: "utf8",
  });
}

test("ai eval fails loudly when no model provider is configured", () => {
  const result = runEval();

  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /AI_EVAL_COMMAND|OPENAI_API_KEY|ANTHROPIC_API_KEY/);
});

test("ai eval command provider runs every stable case and writes a report", () => {
  const tmp = mkdtempSync(join(tmpdir(), "mesh-talk-ai-eval-"));
  const reportPath = join(tmp, "report.json");
  const evaluatorPath = join(tmp, "fake-evaluator.mjs");

  writeFileSync(
    evaluatorPath,
    `#!/usr/bin/env node
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => input += chunk);
process.stdin.on("end", () => {
  const caseId = (input.match(/Case ID: ([A-Z0-9-]+)/) || [])[1] || "UNKNOWN";
  console.log(JSON.stringify({
    case_id: caseId,
    pass: true,
    score: 1,
    findings: [],
    evidence: ["found required instructions"],
    required_improvements: []
  }));
});
`,
  );

  const result = runEval(["--report", reportPath], {
    AI_EVAL_COMMAND: `${process.execPath} ${evaluatorPath}`,
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const report = JSON.parse(readFileSync(reportPath, "utf8"));
  assert.ok(report.cases.length >= 5);
  assert.equal(report.summary.failed, 0);
  assert.equal(report.summary.provider, "command");
});

test("module coverage manifest includes every core module advertised in AGENTS.md", () => {
  const manifest = JSON.parse(
    readFileSync(new URL("../docs/evals/module-coverage.json", import.meta.url), "utf8"),
  );
  const modules = new Set(manifest.core_modules.map((module) => module.name));

  for (const expected of [
    "node",
    "identity",
    "transport",
    "discovery",
    "eventlog",
    "ratchet",
    "channel",
    "dm",
    "file",
    "postoffice",
    "storage",
  ]) {
    assert.ok(modules.has(expected), `missing module coverage: ${expected}`);
  }
});
