import { spawnSync } from "node:child_process";

// Inspect the candidate index, including force-added and previously tracked files.
// .gitignore is the single owner of the implementation-artifact policy.
const result = spawnSync(
  "git",
  [
    "ls-files",
    "--cached",
    "--ignored",
    "--exclude-per-directory=.gitignore",
    "-z",
  ],
  { encoding: "utf8" },
);
if (result.error || result.status !== 0) {
  console.error(
    "Cannot check repository hygiene:",
    result.error?.message ?? result.stderr.trim(),
  );
  process.exitCode = 1;
} else {
  const files = result.stdout.split("\0").filter(Boolean);
  if (files.length) {
    console.error(
      "Ignored implementation artifacts must be removed from the Git index:",
    );
    for (const file of files) console.error(`  ${JSON.stringify(file)}`);
    console.error(
      "Keep temporary work under tmp/ and put lasting decisions in the maintained owning guide.",
    );
    process.exitCode = 1;
  }
}
