#!/usr/bin/env node
/** Cheap structural feedback. Does not execute tests or certify correctness. */
import { existsSync } from "node:fs";
import { checkVersions } from "../release/version.mjs";
import { spawnSync } from "node:child_process";
import { lstat, realpath } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildAffectedPlan, isFrontendGlobal } from "./affected.mjs";
import { buildFrontendGraph } from "./frontend-graph.mjs";
import { loadContracts, readRepositoryFile } from "./contracts.mjs";
import { rustPackages } from "../pr-platform/rust-packages.mjs";

const rustRoots = rustPackages.map(({ directory }) => `${directory}/`);
const requiresOwnership = (file) =>
  isFrontendGlobal(file) ||
  file.startsWith("frontend/src/") ||
  (rustRoots.some((prefix) => file.startsWith(prefix)) &&
    /\.rs$|\/Cargo\.toml$|\/tests\/fixtures\//u.test(file));

// Deliberately limited Markdown support: ATX/setext headings outside fenced
// code, basic Markdown markup/link stripping, and duplicate-heading suffixes.
// HTML-bearing headings are unsupported; never partially strip HTML as text.
// This is not a complete GitHub Markdown renderer.
function headingAnchors(markdown) {
  const anchors = new Set();
  const counts = new Map();
  const lines = markdown.split(/\r?\n/u);
  let fence;
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/u.exec(line);
    if (marker) {
      const [, delimiter, info] = marker;
      if (!fence) {
        // Backtick fences cannot contain backticks in their opening info string.
        if (delimiter[0] !== "`" || !info.includes("`")) fence = delimiter;
      } else if (
        delimiter[0] === fence[0] &&
        delimiter.length >= fence.length &&
        /^[ \t]*$/u.test(info)
      ) {
        fence = undefined;
      }
      continue;
    }
    if (fence) continue;
    let heading = /^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$/u.exec(line)?.[1];
    if (
      !heading &&
      line.trim() &&
      /^ {0,3}(?:=+|-+)\s*$/u.test(lines[index + 1] ?? "")
    ) {
      heading = line.trim();
      index++;
    }
    if (!heading) continue;
    // Skipping unsupported headings would corrupt later duplicate-slug counts.
    if (/[<>]/u.test(heading))
      throw Error(
        "HTML-bearing headings are unsupported for anchored references",
      );
    const slug = heading
      .replace(/!?\[([^\]]+)\]\([^)]*\)/gu, "$1")
      .toLowerCase()
      .replace(/[^\p{L}\p{N}\p{M}_\-\s]/gu, "")
      .replace(/\s/gu, "-");
    let count = counts.get(slug) ?? 0;
    while (anchors.has(count ? `${slug}-${count}` : slug)) count++;
    counts.set(slug, count + 1);
    anchors.add(count ? `${slug}-${count}` : slug);
  }
  return anchors;
}

/** Inspect current registered contracts and tracked/untracked candidate inputs. */
export async function checkStandards(root) {
  const report = {
    schema: 1,
    scope:
      "structural standards only; no correctness or acceptance certification",
    ok: false,
    filesChecked: 0,
    contractsChecked: 0,
    issues: [],
    escalation: [],
  };
  const issue = (code, message, details = {}) =>
    report.issues.push({ code, ...details, message });
  let contracts;
  try {
    root = await realpath(root);
    contracts = await loadContracts(root);
    report.contractsChecked = contracts.length;
    // Select every owner to validate even dormant contracts and consumer chains.
    const registered = buildAffectedPlan([], {
      contracts,
      owners: contracts.map((contract) => contract.id),
      reason: "structural standards inventory",
    });
    for (const blocker of registered.blockers) issue("contract", blocker);
  } catch (error) {
    issue("contract", error.message);
    return report;
  }
  if (existsSync(path.join(root, "Cargo.toml"))) {
    try {
      checkVersions(root);
    } catch (error) {
      issue("product-version", error.message);
    }
  }
  for (const contract of contracts) {
    for (const [kind, prefix] of [
      ["node", ""],
      ["frontend", "frontend/"],
    ]) {
      for (const test of contract[kind] ?? []) {
        const file = `${prefix}${test}`;
        try {
          await readRepositoryFile(root, file);
        } catch (error) {
          issue(
            "test-reference",
            `Owner ${contract.id}: unavailable ${kind} test ${file}: ${error.message}`,
            { owner: contract.id, path: file },
          );
        }
      }
    }
    if (!contract.capability) continue;
    for (const reference of [
      contract.capability.guide,
      ...contract.capability.invariants.map((item) => item.reference),
    ]) {
      const [file, anchor] = reference.split("#");
      try {
        const markdown = await readRepositoryFile(root, file);
        if (anchor && !headingAnchors(markdown).has(anchor))
          throw Error(
            `missing heading anchor #${anchor} (ATX/setext headings supported)`,
          );
      } catch (error) {
        issue(
          "capability-reference",
          `Owner ${contract.id}: invalid capability reference ${reference}: ${error.message}`,
          { owner: contract.id, path: reference },
        );
      }
    }
  }
  const listed = spawnSync(
    "git",
    ["ls-files", "-z", "--cached", "--others", "--exclude-standard"],
    {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
    },
  );
  if (listed.status !== 0) {
    issue(
      "inventory",
      listed.error?.message ??
        listed.stderr?.trim() ??
        "Git source discovery failed",
    );
    return report;
  }
  const files = [];
  const current = new Map();
  for (const file of [
    ...new Set(listed.stdout.split("\0").filter(Boolean)),
  ].sort()) {
    if (!requiresOwnership(file)) continue;
    // Removed sources remain in the index; their references are checked above.
    try {
      await lstat(path.join(root, file));
    } catch (error) {
      if (error.code !== "ENOENT")
        issue("source", `Cannot inspect ${file}: ${error.message}`, {
          path: file,
        });
      continue;
    }
    try {
      const source = await readRepositoryFile(root, file);
      files.push(file);
      if (file.startsWith("frontend/src/")) current.set(file, source);
    } catch (error) {
      issue("source", `Cannot inspect ${file}: ${error.message}`, {
        path: file,
      });
    }
  }
  report.filesChecked = files.length;
  try {
    const frontend = buildFrontendGraph(current);
    const escalation = new Set();
    for (const file of files) {
      const plan = buildAffectedPlan([file], { contracts, frontend });
      for (const blocker of plan.blockers)
        issue("ownership", blocker, { path: file });
      for (const note of plan.escalation) escalation.add(note);
    }
    report.escalation = [...escalation].sort();
  } catch (error) {
    issue("inventory", error.message);
  }
  report.ok = report.issues.length === 0;
  return report;
}

async function main(args) {
  let root = fileURLToPath(new URL("../../", import.meta.url));
  let json = false;
  for (let index = 0; index < args.length; index++) {
    if (args[index] === "--json") json = true;
    else if (
      args[index] === "--root" &&
      args[index + 1] &&
      !args[index + 1].startsWith("-")
    )
      root = path.resolve(args[++index]);
    else throw Error(`Unknown or incomplete option: ${args[index]}`);
  }
  const report = await checkStandards(root);
  if (json) console.log(JSON.stringify(report, null, 2));
  else {
    console.log(
      `Structural standards: ${report.ok ? "pass" : "gaps"}; ${report.filesChecked} source inputs, ${report.contractsChecked} contracts`,
    );
    for (const issue of report.issues)
      console.log(`[${issue.code}] ${issue.message}`);
    console.log(report.scope);
  }
  process.exitCode = report.ok ? 0 : 1;
}
if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  main(process.argv.slice(2)).catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
