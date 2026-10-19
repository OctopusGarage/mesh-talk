#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync, readFileSync } from "node:fs";
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

test("ai eval command provider runs every stable case and writes a report", (t) => {
  const tmp = mkdtempSync(join(tmpdir(), "mesh talk-ai-eval-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
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
    AI_EVAL_COMMAND: `"${process.execPath}" "${evaluatorPath}"`,
  });

  assert.equal(result.status, 0, result.stderr || result.stdout);
  const report = JSON.parse(readFileSync(reportPath, "utf8"));
  assert.ok(report.cases.length >= 5);
  assert.equal(report.summary.failed, 0);
  assert.equal(report.summary.provider, "command");
});

test("ai eval resolves its suite from checkout paths containing spaces", (t) => {
  const checkout = mkdtempSync(join(tmpdir(), "mesh talk-checkout-"));
  t.after(() => rmSync(checkout, { recursive: true, force: true }));
  mkdirSync(join(checkout, "scripts"));
  mkdirSync(join(checkout, "docs", "evals"), { recursive: true });
  copyFileSync(new URL("ai-eval.mjs", import.meta.url), join(checkout, "scripts", "ai-eval.mjs"));
  copyFileSync(new URL("../docs/evals/ai-eval-suite.json", import.meta.url), join(checkout, "docs", "evals", "ai-eval-suite.json"));
  const result = spawnSync(process.execPath, [join(checkout, "scripts", "ai-eval.mjs"), "--list"], { encoding: "utf8", cwd: tmpdir() });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /MT-AI-001/);
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

test("relay privacy eval is registered against the current design document", () => {
  const suite = JSON.parse(
    readFileSync(new URL("../docs/evals/ai-eval-suite.json", import.meta.url), "utf8"),
  );
  const privacy = suite.cases.find((entry) => entry.id === "MT-AI-006");
  assert.equal(privacy?.subject_file, "docs/post-office-metadata-privacy.md");
  assert.ok(privacy.rubric.some((item) => item.includes("authenticated connections")));
  assert.ok(privacy.rubric.some((item) => item.includes("forward secrecy")));
});

test("macOS post-office runbook eval covers credentials and delivery", () => {
  const suite = JSON.parse(
    readFileSync(new URL("../docs/evals/ai-eval-suite.json", import.meta.url), "utf8"),
  );
  const service = suite.cases.find((entry) => entry.id === "MT-AI-007");
  assert.equal(service?.subject_file, "docs/post-office-macos-service.md");
  assert.ok(service.rubric.some((item) => item.includes("password")));
  assert.ok(service.rubric.some((item) => item.includes("sender")));
  assert.ok(service.rubric.some((item) => item.includes("encrypted file bytes")));
});

test("Windows post-office runbook eval covers task recovery and delivery", () => {
  const suite = JSON.parse(
    readFileSync(new URL("../docs/evals/ai-eval-suite.json", import.meta.url), "utf8"),
  );
  const service = suite.cases.find((entry) => entry.id === "MT-AI-008");
  assert.equal(service?.subject_file, "docs/post-office-windows-service.md");
  assert.ok(service.rubric.some((item) => item.includes("password")));
  assert.ok(service.rubric.some((item) => item.includes("stale lock")));
  assert.ok(service.rubric.some((item) => item.includes("encrypted file bytes")));
});

test("routed discovery eval covers the opt-in bridge and its limits", () => {
  const suite = JSON.parse(
    readFileSync(new URL("../docs/evals/ai-eval-suite.json", import.meta.url), "utf8"),
  );
  const bridge = suite.cases.find((entry) => entry.id === "MT-AI-009");
  assert.equal(bridge?.subject_file, "docs/routed-network-discovery.md");
  assert.ok(bridge.rubric.some((item) => item.includes("A/B do not need Tailscale")));
  assert.ok(bridge.rubric.some((item) => item.includes("Noise identity")));
  assert.ok(bridge.rubric.some((item) => item.includes("WebRTC")));
});


test("ai eval includes declared supporting sources and records their provenance", (t) => {
  const tmp = mkdtempSync(join(tmpdir(), "mesh-context-eval-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  const primary = join(tmp, "primary.rs");
  const support = join(tmp, "validator.rs");
  const prompt = join(tmp, "prompt.md");
  const suite = join(tmp, "suite.json");
  const report = join(tmp, "report.json");
  const provider = join(tmp, "provider.mjs");
  writeFileSync(primary, "PRIMARY_SOURCE");
  writeFileSync(support, "DELEGATED_VALIDATION");
  writeFileSync(prompt, "Evaluate the supplied production sources.");
  writeFileSync(suite, JSON.stringify({ evaluator_prompt: prompt, cases: [{
    id: "MT-CONTEXT-001", name: "delegated validation", subject_file: primary,
    context_files: [support], min_score: 0.85, scenario: "Check the validator.", rubric: ["Validation exists."],
  }] }));
  writeFileSync(provider, `let input=""; process.stdin.setEncoding("utf8");
process.stdin.on("data", chunk => input += chunk);
process.stdin.on("end", () => {
  if (!input.includes("PRIMARY_SOURCE") || !input.includes("DELEGATED_VALIDATION")) throw Error("Missing production context");
  console.log(JSON.stringify({case_id:"MT-CONTEXT-001",pass:true,score:1,findings:[],evidence:[],required_improvements:[]}));
});`);
  const result = runEval(["--suite", suite, "--report", report], {
    AI_EVAL_COMMAND: `"${process.execPath}" "${provider}"`,
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.deepEqual(JSON.parse(readFileSync(report, "utf8")).cases[0].context_files, [support]);
});
