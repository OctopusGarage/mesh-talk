#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

const ROOT = resolve(new URL("..", import.meta.url).pathname);
const DEFAULT_SUITE = "docs/evals/ai-eval-suite.json";
const DEFAULT_REPORT = "target/ai-eval/report.json";

function parseArgs(argv) {
  const args = {
    suite: DEFAULT_SUITE,
    report: DEFAULT_REPORT,
    cases: new Set(),
    list: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--suite") args.suite = argv[++i];
    else if (arg === "--report") args.report = argv[++i];
    else if (arg === "--case") args.cases.add(argv[++i]);
    else if (arg === "--list") args.list = true;
    else if (arg === "--help" || arg === "-h") {
      printHelp();
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/ai-eval.mjs [--suite path] [--report path] [--case ID] [--list]

Runs the real AI eval suite. Configure a model provider with:
  AI_EVAL_COMMAND='claude -p --output-format text --permission-mode dontAsk'

The command receives the full eval prompt on stdin and must print a JSON object:
  {"case_id":"MT-AI-001","pass":true,"score":0.9,"findings":[],"evidence":[],"required_improvements":[]}
`);
}

function readJson(path) {
  return JSON.parse(readFileSync(resolve(ROOT, path), "utf8"));
}

function readText(path) {
  return readFileSync(resolve(ROOT, path), "utf8");
}

function buildPrompt(suite, testCase) {
  const evaluatorPrompt = readText(suite.evaluator_prompt);
  const subject = readText(testCase.subject_file);

  return `${evaluatorPrompt}

Case ID: ${testCase.id}
Case name: ${testCase.name}
Minimum passing score: ${testCase.min_score}

Scenario:
${testCase.scenario}

Rubric:
${testCase.rubric.map((item, index) => `${index + 1}. ${item}`).join("\n")}

Subject file: ${testCase.subject_file}

--- SUBJECT START ---
${subject}
--- SUBJECT END ---

Return only the JSON object for this case.`;
}

function commandProvider(prompt) {
  const command = process.env.AI_EVAL_COMMAND?.trim();
  if (!command) {
    throw new Error(
      "No AI eval model provider configured. Set AI_EVAL_COMMAND to a non-interactive model command. You may wrap OPENAI_API_KEY, ANTHROPIC_API_KEY, Codex, Claude Code, OpenAI CLI, or a local model behind that command.",
    );
  }

  const result = spawnSync(command, {
    cwd: ROOT,
    input: prompt,
    shell: true,
    encoding: "utf8",
    maxBuffer: 10 * 1024 * 1024,
    env: process.env,
  });

  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(
      `AI_EVAL_COMMAND exited ${result.status}.\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`,
    );
  }

  return result.stdout.trim();
}

function parseModelJson(output) {
  try {
    return JSON.parse(output);
  } catch (_error) {
    const match = output.match(/\{[\s\S]*\}/);
    if (!match) throw new Error(`model output did not contain JSON:\n${output}`);
    return JSON.parse(match[0]);
  }
}

function validateResult(testCase, result) {
  const errors = [];
  if (result.case_id !== testCase.id) {
    errors.push(`case_id mismatch: expected ${testCase.id}, got ${result.case_id}`);
  }
  if (typeof result.pass !== "boolean") errors.push("pass must be boolean");
  if (typeof result.score !== "number") errors.push("score must be number");
  if (!Array.isArray(result.findings)) errors.push("findings must be an array");
  if (!Array.isArray(result.evidence)) errors.push("evidence must be an array");
  if (!Array.isArray(result.required_improvements)) {
    errors.push("required_improvements must be an array");
  }
  if (typeof result.score === "number" && result.score < testCase.min_score) {
    errors.push(`score ${result.score} below minimum ${testCase.min_score}`);
  }
  if (result.pass !== true) errors.push("model evaluator marked case as failing");
  return errors;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const suite = readJson(args.suite);
  const selectedCases = suite.cases.filter(
    (testCase) => args.cases.size === 0 || args.cases.has(testCase.id),
  );

  if (args.list) {
    for (const testCase of selectedCases) {
      console.log(`${testCase.id}\t${testCase.name}`);
    }
    return;
  }

  if (selectedCases.length === 0) {
    throw new Error("no eval cases selected");
  }

  const startedAt = new Date().toISOString();
  const results = [];

  for (const testCase of selectedCases) {
    const output = commandProvider(buildPrompt(suite, testCase));
    const modelResult = parseModelJson(output);
    const errors = validateResult(testCase, modelResult);
    results.push({
      ...modelResult,
      id: testCase.id,
      name: testCase.name,
      subject_file: testCase.subject_file,
      errors,
    });
  }

  const failed = results.filter((result) => result.errors.length > 0);
  const report = {
    summary: {
      provider: "command",
      started_at: startedAt,
      finished_at: new Date().toISOString(),
      total: results.length,
      failed: failed.length,
    },
    cases: results,
  };

  const reportPath = resolve(ROOT, args.report);
  mkdirSync(dirname(reportPath), { recursive: true });
  writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);

  if (failed.length > 0) {
    for (const result of failed) {
      console.error(`${result.id} failed: ${result.errors.join("; ")}`);
    }
    console.error(`AI eval report written to ${args.report}`);
    process.exit(1);
  }

  console.log(`AI eval passed: ${results.length}/${results.length} cases`);
  console.log(`AI eval report written to ${args.report}`);
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
