import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

function positiveInteger(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function collection(pages, key) {
  assert.ok(Array.isArray(pages), "invalid paginated Actions response");
  return pages.flatMap((page) => {
    assert.ok(Array.isArray(page?.[key]), `invalid Actions ${key} response`);
    return page[key];
  });
}

function successful(item) {
  return item?.status === "completed" && item.conclusion === "success";
}

function readActions(endpoint) {
  // Capture stderr as well: API failures must never echo credentials or responses.
  try {
    return JSON.parse(execFileSync("gh", ["api", "--paginate", "--slurp", endpoint], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }));
  } catch {
    throw new Error("Unable to read verified release build evidence from GitHub Actions");
  }
}

export async function resolveVerifiedBuild({ repo, sha, tag, currentRunId, read = readActions }) {
  assert.match(repo ?? "", /^[\w.-]+\/[\w.-]+$/);
  assert.match(sha ?? "", /^[a-f\d]{40}$/);
  assert.match(tag ?? "", /^v\d+\.\d+\.\d+$/);
  if (currentRunId !== undefined) assert.ok(positiveInteger(Number(currentRunId)), "invalid current run ID");
  const prefix = `repos/${repo}/actions`;
  const runs = collection(await read(`${prefix}/workflows/release.yml/runs?head_sha=${sha}&per_page=100`), "workflow_runs")
    .filter((run) => String(run.id) !== String(currentRunId));
  for (const run of runs) {
    assert.ok(positiveInteger(run.id) && positiveInteger(run.run_attempt), "invalid build run identity");
    assert.equal(run.repository?.full_name, repo, "build repository changed");
    assert.equal(run.head_repository?.full_name, repo, "build source repository changed");
    assert.equal(run.head_sha, sha, "build source changed");
    assert.ok(typeof run.head_branch === "string" && run.head_branch.length > 0, "missing build ref");
    assert.equal(run.path, ".github/workflows/release.yml", "build workflow changed");
    assert.ok(typeof run.run_started_at === "string" && Number.isFinite(Date.parse(run.run_started_at)), "missing build attempt time");
  }
  // Branch dry runs use the same source but never verify a tagged draft.
  // A rerun of an older tagged workflow may be the newest build execution.
  const candidates = runs.filter((run) => run.head_branch === tag);
  candidates.sort((a, b) => Date.parse(b.run_started_at) - Date.parse(a.run_started_at) || b.id - a.id);
  for (const run of candidates) {
    const jobs = collection(await read(`${prefix}/runs/${run.id}/attempts/${run.run_attempt}/jobs?per_page=100`), "jobs");
    const builds = jobs.filter((job) => /^build-release(?:$| \()/.test(job.name));
    if (builds.length && builds.every((job) => job.status === "completed" && job.conclusion === "skipped")) continue;
    assert.ok(builds.length > 0, "latest run has no build jobs");
    assert.ok(successful(run) && builds.every(successful), "latest build has not succeeded");
    const verification = jobs.filter((job) => job.name === "verify-release");
    assert.equal(verification.length, 1, "missing or ambiguous release verification job");
    assert.ok(successful(verification[0]), "release verification has not succeeded");
    const steps = verification[0].steps?.filter((step) => step.name === "Verify actual draft release assets");
    assert.ok(steps?.length === 1 && successful(steps[0]), "actual draft release verification has not succeeded");
    const artifacts = collection(await read(`${prefix}/runs/${run.id}/artifacts?per_page=100`), "artifacts")
      .filter((artifact) => artifact.name === "verified-release-evidence");
    assert.equal(artifacts.length, 1, "missing or ambiguous verified release evidence");
    assert.ok(positiveInteger(artifacts[0].id) && artifacts[0].expired === false && positiveInteger(artifacts[0].size_in_bytes), "verified release evidence is expired or empty");
    return run.id;
  }
  throw new Error("No eligible verified release build found");
}

async function main() {
  const runId = await resolveVerifiedBuild({
    repo: process.env.SOURCE_REPO,
    sha: process.env.SOURCE_SHA,
    tag: process.env.RELEASE_TAG,
    currentRunId: process.env.GITHUB_RUN_ID,
  });
  assert.ok(process.env.GITHUB_OUTPUT, "missing GITHUB_OUTPUT");
  appendFileSync(process.env.GITHUB_OUTPUT, `run_id=${runId}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    await main();
  } catch {
    console.error("Could not resolve a successful exact-source verified release build");
    process.exitCode = 1;
  }
}
