import { execFileSync, spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

function command(bin, args, options = {}) {
  return execFileSync(bin, args, { encoding: "utf8", ...options }).trim();
}

export function isIntegrated(cwd, base, branch) {
  const git = (...args) => spawnSync("git", args, { cwd, encoding: "utf8" });
  const ancestor = git("merge-base", "--is-ancestor", branch, base);
  if (ancestor.error) throw ancestor.error;
  if (ancestor.status === 0) return true;
  if (ancestor.status !== 1) throw new Error(ancestor.stderr || "git merge-base failed");

  // A merge commit can contain changes that git cherry does not inspect.
  const merges = git("rev-list", "--merges", `${base}..${branch}`);
  if (merges.status !== 0) throw new Error(merges.stderr || "git rev-list failed");
  if (merges.stdout.trim()) return false;

  const patches = git("cherry", base, branch);
  if (patches.status !== 0) throw new Error(patches.stderr || "git cherry failed");
  return patches.stdout.trim().split("\n").every(line => !line || line.startsWith("- "));
}

export function isEligible(branch, defaultBranch, openRefs) {
  return branch.name !== defaultBranch && branch.name !== "dev" &&
    !branch.protected && !openRefs.has(branch.name);
}

function apiPages(path) {
  return JSON.parse(command("gh", ["api", "--paginate", "--slurp", path])).flat();
}

function main() {
  const repo = process.env.GITHUB_REPOSITORY;
  if (!repo || !/^[\w.-]+\/[\w.-]+$/.test(repo)) {
    throw new Error("GITHUB_REPOSITORY must be owner/repo");
  }
  const cwd = process.cwd();
  const dryRun = process.env.DRY_RUN !== "false";
  const defaultBranch = JSON.parse(command("gh", ["api", `repos/${repo}`])).default_branch;
  const branches = apiPages(`repos/${repo}/branches?per_page=100`);
  const pulls = apiPages(`repos/${repo}/pulls?state=open&per_page=100`);
  const openRefs = new Set();
  for (const pr of pulls) {
    if (pr.head.repo?.full_name === repo) openRefs.add(pr.head.ref);
    if (pr.base.repo?.full_name === repo) openRefs.add(pr.base.ref);
  }

  let selected = 0;
  for (const branch of branches) {
    if (!isEligible(branch, defaultBranch, openRefs)) continue;
    const remoteRef = `refs/remotes/origin/${branch.name}`;
    const localSha = spawnSync("git", ["rev-parse", "--verify", remoteRef], { cwd, encoding: "utf8" });
    if (localSha.status !== 0 || localSha.stdout.trim() !== branch.commit.sha) {
      console.log(`Skip ${branch.name}: fetched ref differs from GitHub`);
      continue;
    }
    if (!isIntegrated(cwd, `refs/remotes/origin/${defaultBranch}`, remoteRef)) continue;
    selected++;
    if (dryRun) {
      console.log(`Would delete ${branch.name} (${branch.commit.sha})`);
      continue;
    }
    // The lease fails if anyone updates the branch after our fetch.
    command("git", ["push", `--force-with-lease=refs/heads/${branch.name}:${branch.commit.sha}`,
      "origin", `:refs/heads/${branch.name}`], { cwd, stdio: "pipe" });
    console.log(`Deleted ${branch.name} (${branch.commit.sha})`);
  }
  console.log(`${dryRun ? "Would delete" : "Deleted"} ${selected} integrated branch(es)`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
