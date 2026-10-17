import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

// The controller is a POSIX shell script; the Windows guest runs its Node plan.
test("separate PR preflights cannot erase each other's or full-health browser artifacts", { skip: process.platform === "win32" }, () => {
  const root = mkdtempSync(path.join(tmpdir(), "mesh-preflight-output-"));
  try {
    for (const directory of ["scripts", "scripts/diagnostics", "frontend", "bin"])
      mkdirSync(path.join(root, directory), { recursive: true });
    copyFileSync(new URL("../pr-preflight.sh", import.meta.url), path.join(root, "scripts/pr-preflight.sh"));
    const executable = (name, body) => writeFileSync(path.join(root, "bin", name), body, { mode: 0o755 });
    const shellQuote = value => `'${value.replaceAll("'", "'\\''")}'`;
    const node = shellQuote(process.execPath);
    writeFileSync(path.join(root, "fake-node.mjs"), `
      import { readFileSync } from 'node:fs';
      const args = process.argv.slice(2);
      if (args[0] === '--input-type=module') {
        const source = readFileSync(0, 'utf8');
        console.log(source.includes('flags.uiSpecs.join') ? 'e2e/settings-recovery.spec.ts' : 'origin/main');
      } else if (args.includes('--plan')) console.log('{}');
    `);
    writeFileSync(path.join(root, "capture.mjs"), `
      import { appendFileSync } from 'node:fs';
      appendFileSync(process.env.PREFLIGHT_TRACE, JSON.stringify({ args: process.argv.slice(2), html: process.env.PLAYWRIGHT_HTML_OUTPUT_DIR }) + '\\n');
    `);
    executable("node", `#!/bin/sh\nexec ${node} ${shellQuote(path.join(root, "fake-node.mjs"))} "$@"\n`);
    executable("npx", `#!/bin/sh\nexec ${node} ${shellQuote(path.join(root, "capture.mjs"))} "$@"\n`);
    for (const tool of ["npm", "typos"]) executable(tool, "#!/bin/sh\nexit 0\n");
    executable("git", `#!/bin/sh
      case "$1" in
        status) ;;
        rev-parse|merge-base) echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ;;
        diff) echo frontend/src/App.tsx ;;
      esac
    `);
    const trace = path.join(root, "trace.jsonl");
    for (let run = 0; run < 2; run++) {
      const result = spawnSync("bash", [path.join(root, "scripts/pr-preflight.sh"), "e2e/settings-recovery.spec.ts"], {
        cwd: root,
        env: { ...process.env, PATH: `${path.join(root, "bin")}:${process.env.PATH}`, MESH_PR_KEEP_AWAKE: "1", PREFLIGHT_TRACE: trace },
        encoding: "utf8",
        timeout: 15_000,
      });
      assert.equal(result.status, 0, result.stdout + result.stderr);
    }
    const commands = readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse);
    assert.equal(commands.length, 2);
    const outputs = commands.map(({ args, html }) => {
      const position = args.indexOf("--output");
      assert(position >= 0, "PR preflight must isolate its artifacts from full-health's default test-results");
      const directory = path.resolve(root, "frontend", args[position + 1]);
      assert(directory.startsWith(path.join(root, "target") + path.sep));
      assert(args.includes("e2e/settings-recovery.spec.ts"));
      assert.equal(path.dirname(html), path.dirname(directory));
      assert.notEqual(html, directory);
      return directory;
    });
    assert.notEqual(outputs[0], outputs[1], "each invocation needs its own output directory");
  } finally { rmSync(root, { recursive: true, force: true }); }
});
