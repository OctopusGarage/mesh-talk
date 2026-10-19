#!/usr/bin/env node
/** Focused development runner. Required PR, platform and review gates remain separate. */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { readFile, mkdir, mkdtemp, rm } from "node:fs/promises";
import { buildAffectedPlan } from "./affected.mjs";
import { buildFrontendGraph } from "./frontend-graph.mjs";
import { runStage } from "./stage.mjs";
import { runCommand, rustTestCount } from "../pr-platform/guest.mjs";

const contractFiles = [
  "scripts/validation/validation.json",
  "frontend/validation.json",
  "crates/mesh-talk-core/validation.json",
  "src-tauri/validation.json",
  "crates/mesh-talk-integrations/validation.json",
  "crates/mesh-talk-mcp/validation.json",
];
function git(root, args) {
  const result = spawnSync("git", args, {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.status !== 0)
    throw Error(result.stderr?.trim() || "Git source discovery failed");
  return result.stdout;
}
const nulList = (output) => output.split("\0").filter(Boolean);
export function discoverChanges(root, base = "HEAD") {
  if (!base || base.startsWith("-") || /[\x00-\x1f]/u.test(base))
    throw Error("Invalid comparison base");
  const resolved = git(root, ["merge-base", "HEAD", base]).trim();
  const files = [
    ...new Set([
      ...nulList(
        git(root, [
          "diff",
          "--name-only",
          "-z",
          "--no-renames",
          resolved,
          "--",
        ]),
      ),
      ...nulList(
        git(root, ["ls-files", "--others", "--exclude-standard", "-z"]),
      ),
    ]),
  ].sort();
  return { base: resolved, files };
}
export function parseOptions(args) {
  const options = {
    base: "HEAD",
    run: false,
    json: false,
    owners: [],
    reason: "",
  };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (["--run", "--json"].includes(arg)) {
      options[arg.slice(2)] = true;
      continue;
    }
    if (["--base", "--owner", "--reason"].includes(arg)) {
      const value = args[++i];
      if (!value || value.startsWith("-") || /[\x00-\x1f]/u.test(value))
        throw Error(`${arg} requires a literal value`);
      if (arg === "--owner") options.owners.push(value);
      else options[arg.slice(2)] = value;
    } else throw Error(`Unknown option: ${arg}`);
  }
  if (options.owners.length && !options.reason.trim())
    throw Error("--owner requires --reason");
  return options;
}

export async function createPlan(root, options) {
  const changes = discoverChanges(root, options.base);
  const contracts = [];
  for (const file of contractFiles) {
    const parsed = JSON.parse(await readFile(path.join(root, file), "utf8"));
    if (parsed.schema !== 1 || !Array.isArray(parsed.contracts))
      throw Error(`Invalid validation contract: ${file}`);
    contracts.push(...parsed.contracts);
  }
  let frontend;
  if (
    changes.files.some((file) => file.startsWith("frontend/src/")) ||
    options.owners.some((owner) => owner.startsWith("frontend-"))
  ) {
    const current = new Map();
    const previous = new Map();
    const tracked = nulList(
      git(root, [
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        "frontend/src",
      ]),
    );
    for (const file of new Set(tracked)) {
      try {
        current.set(file, await readFile(path.join(root, file), "utf8"));
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    }
    for (const file of nulList(
      git(root, [
        "ls-tree",
        "-r",
        "--name-only",
        "-z",
        changes.base,
        "--",
        "frontend/src",
      ]),
    )) {
      // Existing files provide resolution targets; only changed files need their former edges.
      previous.set(
        file,
        changes.files.includes(file)
          ? git(root, ["show", `${changes.base}:${file}`])
          : "",
      );
    }
    frontend = buildFrontendGraph(current, previous);
  }
  const plan = {
    ...buildAffectedPlan(changes.files, {
      contracts,
      frontend,
      owners: options.owners,
      reason: options.reason,
    }),
    base: changes.base,
  };
  for (const file of plan.node) await readFile(path.join(root, file));
  for (const file of plan.frontend)
    await readFile(path.join(root, "frontend", file));
  return plan;
}

async function checked(run, program, args, options) {
  const result = await run(program, args, {
    ...options,
    detached: false,
    timeoutMs: 2_147_483_647,
  });
  if (result.code !== 0 || result.timedOut || result.error)
    throw Error(
      `${program} failed: ${result.error || result.output.slice(-6000)}`,
    );
  return result.output;
}
export async function prepareRust(
  selections,
  { cwd = process.cwd(), run = runCommand, log = console.log } = {},
) {
  const prepared = [];
  for (const selection of selections) {
    const started = Date.now();
    const output = await checked(
      run,
      "cargo",
      [
        "test",
        "--locked",
        "-p",
        selection.package,
        "--lib",
        "--no-run",
        "--message-format=json",
      ],
      { cwd },
    );
    const executables = [];
    for (const line of output.split("\n")) {
      try {
        const item = JSON.parse(line);
        if (
          item.reason === "compiler-artifact" &&
          item.executable &&
          item.target?.kind?.includes("lib")
        )
          executables.push(item.executable);
      } catch {
        /* Cargo also emits text diagnostics. */
      }
    }
    if (executables.length !== 1)
      throw Error(
        `Missing or ambiguous library test executable for ${selection.package}`,
      );
    const executable = executables[0];
    const names = (text) =>
      [...text.matchAll(/^(.+): test\r?$/gmu)].map((match) => match[1]);
    const listed = names(await checked(run, executable, ["--list"], { cwd }));
    const ignored = new Set(
      names(await checked(run, executable, ["--list", "--ignored"], { cwd })),
    );
    const runnable = listed.filter((name) => !ignored.has(name));
    for (const filter of selection.filters)
      if (!runnable.some((name) => name.includes(filter)))
        throw Error(
          `Selector matches zero nonignored ${selection.package} tests: ${filter}`,
        );
    const count = runnable.filter((name) =>
      selection.filters.some((filter) => name.includes(filter)),
    ).length;
    prepared.push({ ...selection, executable, count });
    log(
      `${selection.package}: compile/inventory ${((Date.now() - started) / 1000).toFixed(1)}s; ${count} selected tests`,
    );
  }
  return prepared;
}
export function validateFrontendReport(report, files, directory) {
  if (!Array.isArray(report.testResults))
    throw Error("Invalid frontend test report");
  let count = 0;
  for (const file of files) {
    const result = report.testResults.find(
      (item) =>
        path.resolve(directory, item.name) === path.resolve(directory, file),
    );
    const passed =
      result?.assertionResults?.filter(
        (assertion) => assertion.status === "passed",
      ).length ?? 0;
    if (!passed)
      throw Error(
        `Selected frontend file ran zero passing assertions: ${file}`,
      );
    count += passed;
  }
  return count;
}
export async function executePlan(
  plan,
  { cwd = process.cwd(), run = runCommand, log = console.log } = {},
) {
  if (plan.blockers.length)
    throw Error(`Validation blocked: ${plan.blockers.join("\n")}`);
  const prepared = await prepareRust(plan.rust, { cwd, run, log });
  async function execute(program, args, directory) {
    const started = Date.now();
    const output = await checked(run, program, args, { cwd: directory });
    log(output);
    log(`Test execution: ${((Date.now() - started) / 1000).toFixed(1)}s`);
    return output;
  }
  for (const file of plan.node) {
    const output = await execute(
      process.execPath,
      ["--test", "--test-reporter=tap", file],
      cwd,
    );
    const declared = [...output.matchAll(/^\s*# Subtest: (.+)$/gmu)].some(
      (match) => path.resolve(cwd, match[1]) !== path.resolve(cwd, file),
    );
    if (!declared || !/^# pass [1-9][0-9]*$/mu.test(output))
      throw Error(`Selected diagnostic ran zero passing Node tests: ${file}`);
  }
  for (const selection of prepared) {
    const output = await execute(selection.executable, selection.filters, cwd);
    if (rustTestCount(output) < selection.count)
      throw Error(
        `Expected ${selection.count} nonignored ${selection.package} tests; fewer executed`,
      );
  }
  if (plan.frontend.length) {
    const directory = await mkdtemp(
      path.join(tmpdir(), "mesh-affected-vitest-"),
    );
    const report = path.join(directory, "results.json");
    try {
      await execute(
        process.execPath,
        [
          path.join(cwd, "frontend/node_modules/vitest/vitest.mjs"),
          "run",
          ...plan.frontend,
          "--reporter=json",
          "--outputFile",
          report,
        ],
        path.join(cwd, "frontend"),
      );
      const count = validateFrontendReport(
        JSON.parse(await readFile(report, "utf8")),
        plan.frontend,
        path.join(cwd, "frontend"),
      );
      log(
        `Frontend: ${count} passing assertions in ${plan.frontend.length} selected files`,
      );
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
}
function printPlan(plan) {
  console.log(
    `Development validation against ${plan.base.slice(0, 12)} (${plan.files.length} changed files)`,
  );
  for (const owner of plan.owners)
    console.log(`Owner ${owner.id}: ${owner.reasons.join("; ")}`);
  for (const selection of plan.rust)
    console.log(`Rust ${selection.package}: ${selection.filters.join(", ")}`);
  for (const file of plan.node) console.log(`Node: ${file}`);
  for (const file of plan.frontend)
    console.log(`Frontend ${file}: ${plan.frontendReasons[file].join("; ")}`);
  for (const file of plan.acknowledged)
    console.log(`Explicitly reviewed unmapped file: ${file}`);
  for (const note of plan.escalation) console.log(`Escalation: ${note}`);
  for (const blocker of plan.blockers) console.log(`BLOCKED: ${blocker}`);
  console.log(plan.scope);
}
async function main(args) {
  const options = parseOptions(args);
  const root = git(process.cwd(), ["rev-parse", "--show-toplevel"]).trim();
  const plan = await createPlan(root, options);
  if (options.json) console.log(JSON.stringify(plan, null, 2));
  else printPlan(plan);
  if (plan.blockers.length) {
    process.exitCode = 2;
    return;
  }
  const expectedResource = process.env.MESH_VALIDATION_AFFECTED_CHILD;
  const resource = plan.rust.length
    ? "heavy"
    : plan.frontend.length
      ? "focused-frontend"
      : "focused-node";
  const child = !!expectedResource;
  if (!options.run && !child) return;
  if (!plan.rust.length && !plan.node.length && !plan.frontend.length) {
    console.log("No tests selected for the current change set.");
    return;
  }
  if (child) {
    if (resource !== expectedResource)
      throw Error(
        "Validation resource class changed while queued; preview and run the updated plan.",
      );
    delete process.env.MESH_VALIDATION_AFFECTED_CHILD;
    return executePlan(plan, { cwd: root });
  }
  await mkdir(path.join(root, "target/validation"), { recursive: true });
  // The child recomputes the plan inside the existing cross-worktree resource lock.
  const forwarded = args.filter((arg) => arg !== "--run" && arg !== "--json");
  const controller = new AbortController();
  // The supervisor owns the deadline and kills the complete managed process group.
  // Nested commands do not time out independently and orphan Cargo/npm descendants.
  const deadline = setTimeout(() => controller.abort(), 1_800_000);
  const stop = () => controller.abort();
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
  try {
    const result = await runStage({
      root,
      cwd: root,
      name: "affected-tests",
      resource,
      profile: "shared",
      context: { base: plan.base, plan },
      env: { ...process.env, MESH_VALIDATION_AFFECTED_CHILD: resource },
      signal: controller.signal,
      command: [process.execPath, fileURLToPath(import.meta.url), ...forwarded],
    });
    process.exitCode = result.code === 0 ? 0 : 1;
  } finally {
    clearTimeout(deadline);
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
  }
}
if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  main(process.argv.slice(2)).catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
