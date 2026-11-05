// Reclaim only unused SDK payloads on disposable GitHub-hosted Ubuntu images.
import { lstat, readFile, rm, statfs } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const unusedImageDirectories = [
  "/usr/share/dotnet",
  "/usr/local/lib/android",
  "/opt/ghc",
];

export async function assertHostedUbuntu({
  root = "/",
  env = process.env,
  platform = process.platform,
} = {}) {
  if (
    platform !== "linux" ||
    env.GITHUB_ACTIONS !== "true" ||
    env.RUNNER_ENVIRONMENT !== "github-hosted" ||
    env.RUNNER_OS !== "Linux"
  ) {
    throw new Error(
      "Preparation requires a disposable GitHub-hosted Linux runner",
    );
  }
  if (!(await lstat(resolve(root))).isDirectory()) {
    throw new Error("Preparation root must be a real directory, not a symlink");
  }
  const release = await readFile(join(root, "etc/os-release"), "utf8");
  if (!/^ID=(?:ubuntu|"ubuntu"|'ubuntu')[ \t]*$/m.test(release)) {
    throw new Error("Preparation requires Ubuntu");
  }
}

async function candidate(root, imagePath) {
  let path = root;
  for (const component of imagePath.slice(1).split("/")) {
    path = join(path, component);
    try {
      const info = await lstat(path);
      if (!info.isDirectory()) {
        return {
          path: imagePath,
          reason: "symlink or non-directory component",
        };
      }
    } catch (error) {
      if (error.code === "ENOENT") return { path: imagePath, reason: "absent" };
      throw error;
    }
  }
  return null;
}

async function availableBytes(root) {
  const usage = await statfs(root);
  return usage.bavail * usage.bsize;
}

export async function prepareRunnerStorage({
  root = "/",
  env = process.env,
  platform = process.platform,
} = {}) {
  await assertHostedUbuntu({ root, env, platform });
  const availableBytesBefore = await availableBytes(root);
  // Validate the complete allowlist before any deletion. Never traverse an
  // image path whose parent has been redirected outside the runner image.
  const candidates = await Promise.all(
    unusedImageDirectories.map(async (path) => ({
      path,
      skip: await candidate(root, path),
    })),
  );
  const removed = [];
  const skipped = [];
  for (const { path, skip } of candidates) {
    const unsafe = skip ?? (await candidate(root, path));
    if (unsafe) {
      skipped.push(unsafe);
      continue;
    }
    // Recursive rm unlinks nested symlinks without following their targets.
    await rm(join(root, path.slice(1)), { recursive: true });
    removed.push(path);
  }
  return {
    availableBytesBefore,
    availableBytesAfter: await availableBytes(root),
    removed,
    skipped,
  };
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  if (process.argv.length !== 3 || process.argv[2] !== "--prepare") {
    throw new Error("Usage: sudo node scripts/ci/runner-storage.mjs --prepare");
  }
  if (process.getuid?.() !== 0)
    throw new Error("Runner storage preparation requires sudo");
  console.log(JSON.stringify(await prepareRunnerStorage(), null, 2));
}
