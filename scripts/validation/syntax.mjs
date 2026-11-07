/** Cheap parsing of changed worktree source files; CI validates the committed candidate. */
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

function command(binary, args, root, input) {
  const result = spawnSync(binary, args, { cwd: root, encoding: "utf8", input });
  if (result.error) throw new Error(`${binary}: ${result.error.message}; install the required local tool before checking these files`);
  if (result.status !== 0) throw new Error(`${binary} rejected syntax:\n${result.stderr || result.stdout || `exit ${result.status}`}`);
  return result.stdout;
}
const list = value => value.split("\0").filter(Boolean);
export function syntaxKind(file) {
  if (/\.([mc]?js)$/u.test(file)) return "node";
  if (/\.sh$/u.test(file) || /^hooks\/[^/]+$/u.test(file)) return "bash";
  if (/\.py$/u.test(file)) return "python";
  if (/^\.github\/workflows\/[^/]+\.ya?ml$/u.test(file)) return "workflow";
  if (/^\.github\/actions\/.+\/action\.ya?ml$/u.test(file)) return "action";
  return undefined;
}
export function checkSyntax({ root = process.cwd(), files, staged = false, base } = {}) {
  const git = args => command("git", args, root);
  const eligible = new Set([
    ...list(git(["ls-files", "-z"])),
    // An explicit affected plan also owns intended new, nonignored sources.
    ...(files ? list(git(["ls-files", "--others", "--exclude-standard", "-z"])) : []),
  ]);
  const selected = files ?? (staged
    ? list(git(["diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z"]))
    : [...list(git(["diff", "--name-only", "--diff-filter=ACMR", "-z", ...(base ? [`${base}...HEAD`] : ["HEAD"])])),
      ...list(git(["diff", "HEAD", "--name-only", "--diff-filter=ACMR", "-z"]))]);
  const checked = [];
  for (const file of [...new Set(selected)].sort()) {
    const kind = syntaxKind(file);
    if (!kind || !eligible.has(file) || !existsSync(path.join(root, file))) continue;
    if (kind === "node") command(process.execPath, ["--check", "--", file], root);
    if (kind === "bash") command("bash", ["-n", "--", file], root);
    if (kind === "python") command(process.platform === "win32" ? "python" : "python3", ["-c", "import ast,sys; ast.parse(sys.stdin.buffer.read(), filename=sys.argv[1])", file], root, readFileSync(path.join(root, file)));
    if (kind === "workflow") command("actionlint", [file], root);
    // Local action metadata is parsed through its workflow callers, not as a
    // standalone workflow. Keep their input/output contracts checked together.
    if (kind === "action") command("actionlint", [], root);
    checked.push(file);
  }
  return checked;
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    const args = process.argv.slice(2);
    let options;
    if (args[0] === "--files") options = { files: args.slice(1) };
    else if (args.length === 1 && args[0] === "--staged") options = { staged: true };
    else if (args.length === 2 && args[0] === "--base") options = { base: args[1] };
    else if (args.length === 0) options = {};
    else throw new Error("Usage: syntax.mjs [--staged | --base REF | --files PATH ...]");
    const checked = checkSyntax(options);
    console.log(`Syntax checked ${checked.length} changed worktree source files.`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
