/** Conservative CI scope selection bound to the actual tested PR merge. */
import { execFile } from "node:child_process";
import { isDeepStrictEqual, promisify } from "node:util";
import { readFile, appendFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { loadContracts } from "../validation/contracts.mjs";
import { buildAffectedPlan } from "../validation/affected.mjs";
const execute = promisify(execFile);
const sha = (value) =>
  typeof value === "string" && /^[a-f0-9]{40}$/u.test(value);
async function git(root, args, deadline) {
  const remaining = deadline - Date.now();
  if (remaining <= 0) throw Error("CI scope deadline exceeded");
  const { stdout } = await execute("git", args, {
    cwd: root,
    encoding: "utf8",
    timeout: remaining,
    maxBuffer: 4 * 1024 * 1024,
    env: {
      ...process.env,
      GIT_NO_REPLACE_OBJECTS: "1",
      GIT_OPTIONAL_LOCKS: "0",
    },
  });
  return stdout;
}
export function parseRawDiff(raw) {
  if (!raw) return [];
  if (!raw.endsWith("\0")) throw Error("Truncated raw diff");
  const chunks = raw.split("\0");
  chunks.pop();
  const records = [];
  for (let i = 0; i < chunks.length; i += 2) {
    const match =
      /^:([0-7]{6}) ([0-7]{6}) ([a-f0-9]{40}) ([a-f0-9]{40}) ([AMDTUXB])$/u.exec(
        chunks[i],
      );
    if (!match || typeof chunks[i + 1] !== "string" || !chunks[i + 1])
      throw Error("Invalid raw diff record");
    records.push({
      oldMode: match[1],
      newMode: match[2],
      status: match[5],
      path: chunks[i + 1],
    });
  }
  return records;
}
export function isProseChange(record) {
  const file = record.path;
  return (
    ["A", "M"].includes(record.status) &&
    record.newMode === "100644" &&
    (record.status === "A"
      ? record.oldMode === "000000"
      : record.oldMode === "100644") &&
    typeof file === "string" &&
    !/[\\\x00-\x1f\x7f\uFFFD]/u.test(file) &&
    !file.split("/").some((part) => !part || part.startsWith(".")) &&
    (file === "README.md" || /^docs\/.+\.md$/u.test(file)) &&
    !/^docs\/(?:help|evals)(?:\/|$)/u.test(file) &&
    !/^docs\/(?:ci-maintenance|quality-gates|testing-feedback-loop|ai-coding-reliability|DOCUMENTATION_GUIDELINES)\.md$/u.test(
      file,
    )
  );
}
// Dependency scope is an acceptance profile, never an actor-only exemption.
export function dependencyProfile({
  records,
  before,
  after,
  beforeLock,
  lock,
}) {
  if (
    !records.length ||
    records.some(
      (r) =>
        r.status !== "M" || r.oldMode !== "100644" || r.newMode !== "100644",
    )
  )
    return null;
  const directory = ["tools/help", "frontend"].find((dir) =>
    records.every((r) =>
      [`${dir}/package.json`, `${dir}/package-lock.json`].includes(r.path),
    ),
  );
  if (!directory || !records.some((r) => r.path.endsWith("package-lock.json")))
    return null;
  const fields = ["dependencies", "devDependencies", "optionalDependencies"];
  const metadata = (value) =>
    Object.fromEntries(
      Object.entries(value).filter(([key]) => !fields.includes(key)),
    );
  if (!isDeepStrictEqual(metadata(before), metadata(after))) return null;
  // npm may rewrite transitive bindings without changing a direct manifest entry.
  const native = (packages) =>
    Object.fromEntries(
      Object.entries(packages ?? {}).filter(([key]) =>
        key.includes("node_modules/@tauri-apps/"),
      ),
    );
  if (!isDeepStrictEqual(native(beforeLock.packages), native(lock.packages)))
    return null;
  let changed = false;
  for (const field of fields) {
    const a = before[field] ?? {},
      b = after[field] ?? {};
    if (!isDeepStrictEqual(Object.keys(a).sort(), Object.keys(b).sort()))
      return null;
    for (const name of Object.keys(a)) {
      if (typeof b[name] !== "string") return null;
      if (a[name] !== b[name]) {
        changed = true;
        // Desktop bindings and lifecycle tooling require native acceptance.
        if (
          name.startsWith("@tauri-apps/") ||
          !/^[~^]?\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?$/u.test(b[name])
        )
          return null;
      }
    }
    if (!isDeepStrictEqual(b, lock.packages?.[""]?.[field] ?? {})) return null;
  }
  if (!changed || lock.lockfileVersion !== 3) return null;
  return directory === "frontend" ? "frontend" : "help";
}
export async function resolveScope({
  root = process.cwd(),
  env = process.env,
  event,
  expectedFull = "auto",
  upstreamResult = "success",
} = {}) {
  if (!["auto", "true", "false"].includes(expectedFull))
    throw Error("expected-full must be auto, true or false");
  if (upstreamResult !== "success")
    throw Error(`Upstream scope did not succeed: ${upstreamResult}`);
  const deadline = Date.now() + 30000;
  const run = (args) => git(root, args, deadline);
  let decision = {
    profile: "full",
    source: null,
    base: null,
    head: null,
    files: [],
    reason: "event requires full verification",
    full: true,
  };
  if (env.GITHUB_EVENT_NAME === "pull_request") {
    if (!event) {
      if (!env.GITHUB_EVENT_PATH) throw Error("Missing PR event payload");
      event = JSON.parse(await readFile(env.GITHUB_EVENT_PATH, "utf8"));
    }
    const pr = event?.pull_request;
    if (!pr || !sha(pr.base?.sha) || !sha(pr.head?.sha) || !sha(env.GITHUB_SHA))
      throw Error("Invalid PR source metadata");
    if (
      !/^[^/\s]+\/[^/\s]+$/u.test(env.GITHUB_REPOSITORY ?? "") ||
      event.repository?.full_name !== env.GITHUB_REPOSITORY ||
      pr.base.repo?.full_name !== env.GITHUB_REPOSITORY
    )
      throw Error("PR repository identity mismatch");
    if (!/^[^/\s]+\/[^/\s]+$/u.test(pr.head.repo?.full_name ?? ""))
      throw Error("Invalid PR head repository identity");
    const sameRepository = pr.head.repo.full_name === env.GITHUB_REPOSITORY;
    const number = pr.number ?? event.number;
    if (
      !Number.isSafeInteger(number) ||
      number < 1 ||
      env.GITHUB_REF !== `refs/pull/${number}/merge`
    )
      throw Error("Expected a PR merge ref");
    const source = (await run(["rev-parse", "--verify", "HEAD"])).trim();
    if (source !== env.GITHUB_SHA)
      throw Error("Checked-out source differs from GITHUB_SHA");
    const parents = (await run(["show", "-s", "--format=%P", source]))
      .trim()
      .split(" ");
    if (
      parents.length !== 2 ||
      parents[0] !== pr.base.sha ||
      parents[1] !== pr.head.sha
    )
      throw Error("Tested source is not the exact two-parent PR merge");
    for (const object of parents)
      await run(["cat-file", "-e", `${object}^{commit}`]);
    const records = parseRawDiff(
      await run([
        "diff",
        "--raw",
        "--no-abbrev",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        "-z",
        pr.base.sha,
        source,
        "--",
      ]),
    );
    decision = {
      ...decision,
      source,
      base: pr.base.sha,
      head: pr.head.sha,
      files: records.map((item) => item.path),
      reason: "non-prose or empty delta requires full verification",
    };
    // Validate the complete inventory, including contracts not matched by this delta.
    const contracts = await loadContracts(root);
    const plan = buildAffectedPlan([], { contracts });
    if (
      sameRepository &&
      pr.base.ref === "main" &&
      records.length &&
      records.every(isProseChange)
    ) {
      const affected = buildAffectedPlan(decision.files, { contracts });
      const authority = new Set(
        contracts.flatMap((item) =>
          item.capability
            ? [
                item.capability.guide,
                ...item.capability.invariants.map(
                  (invariant) => invariant.reference,
                ),
              ].map((reference) => reference.split("#")[0])
            : [],
        ),
      );
      const selected =
        affected.owners.length ||
        affected.rust.length ||
        affected.frontend.length ||
        affected.node.length ||
        affected.blockers.length ||
        affected.escalation.length ||
        plan.blockers.length ||
        decision.files.some((file) => authority.has(file));
      decision.full = !!selected;
      decision.reason = selected
        ? "validation owner or authority requires full verification"
        : "unowned prose-only PR delta; product tests not executed";
    }
    if (
      sameRepository &&
      pr.base.ref === "dev" &&
      pr.user?.login === "dependabot[bot]" &&
      pr.user?.type === "Bot"
    ) {
      const directory = ["tools/help", "frontend"].find(
        (dir) =>
          records.length &&
          records.every((r) =>
            [`${dir}/package.json`, `${dir}/package-lock.json`].includes(
              r.path,
            ),
          ),
      );
      if (
        directory &&
        records.every(
          (r) =>
            r.status === "M" &&
            r.oldMode === "100644" &&
            r.newMode === "100644",
        )
      ) {
        const json = async (commit, file) =>
          JSON.parse(await run(["show", `${commit}:${file}`]));
        const profile = dependencyProfile({
          records,
          before: await json(pr.base.sha, `${directory}/package.json`),
          after: await json(source, `${directory}/package.json`),
          beforeLock: await json(pr.base.sha, `${directory}/package-lock.json`),
          lock: await json(source, `${directory}/package-lock.json`),
        });
        if (profile)
          decision = {
            ...decision,
            full: false,
            profile,
            reason: `isolated ${profile} npm dependency acceptance required`,
          };
      }
    }
    if (!sameRepository)
      decision.reason = "fork PR requires full protected provider verification";
  }
  if (!decision.full && decision.profile === "full") decision.profile = "docs";
  if (expectedFull === "false" && decision.full)
    throw Error(
      "Unsafe docs-only hint: independent classification requires full verification",
    );
  if (expectedFull === "true")
    decision = {
      ...decision,
      full: true,
      profile: "full",
      reason: "caller explicitly requires full verification",
    };
  return decision;
}
async function main() {
  const options = {};
  const args = process.argv.slice(2);
  const seen = new Set();
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i];
    if (
      !["--expected-full", "--upstream-result"].includes(key) ||
      i + 1 >= args.length ||
      seen.has(key)
    )
      throw Error(`Invalid argument: ${key}`);
    seen.add(key);
    options[key === "--expected-full" ? "expectedFull" : "upstreamResult"] =
      args[i + 1];
  }
  const decision = await resolveScope(options);
  if (process.env.GITHUB_STEP_SUMMARY)
    await appendFile(
      process.env.GITHUB_STEP_SUMMARY,
      decision.full
        ? "CI scope: full verification required.\n"
        : decision.profile !== "docs"
          ? `CI scope: ${decision.profile} dependency acceptance. Full product/native matrices not executed; selected checks remain required.\n`
          : "CI scope: documentation only. N/A: product tests not executed. Structural policy checks remain required.\n",
    );
  if (process.env.GITHUB_OUTPUT)
    await appendFile(
      process.env.GITHUB_OUTPUT,
      `full=${decision.full}\nprofile=${decision.profile}\n`,
    );
  process.stdout.write(`${JSON.stringify(decision)}\n`);
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href)
  main().catch((error) => {
    console.error(`CI scope rejected: ${error.message}`);
    process.exitCode = 1;
  });
