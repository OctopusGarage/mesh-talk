import { execFileSync } from "node:child_process";
import { openSync, readSync, closeSync } from "node:fs";

// Archives retain Git's own commit stamp; an environment variable alone is not proof.
export function sourceIdentity({ cwd = process.cwd(), env = process.env } = {}) {
  let actual;
  if (env.MESH_PR_ARCHIVE) {
    if (!env.EVAL_SOURCE_SHA) throw new Error("Archived evidence requires a pinned source SHA");
    const header = Buffer.alloc(1024);
    const fd = openSync(env.MESH_PR_ARCHIVE, "r");
    let length;
    try {
      length = readSync(fd, header, 0, header.length, 0);
    } finally {
      closeSync(fd);
    }
    actual = execFileSync("git", ["get-tar-commit-id"], {
      input: header.subarray(0, length), encoding: "utf8", stdio: ["pipe", "pipe", "pipe"],
    }).trim();
  } else {
    actual = execFileSync("git", ["rev-parse", "HEAD"], {
      cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"],
    }).trim();
  }
  if (!/^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(actual)) throw new Error("Invalid source commit identity");
  if (env.EVAL_SOURCE_SHA !== undefined && env.EVAL_SOURCE_SHA !== actual)
    throw new Error("Evidence source must match the actual source commit");
  return actual;
}
