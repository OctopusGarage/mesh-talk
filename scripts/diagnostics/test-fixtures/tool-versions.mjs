import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

// Report and lifecycle fixtures own real commands, receipts and retirement.
// Only unrelated host tool-version startup is replaced with a stable dependency.
export function toolVersionFixture(root, env) {
  const preload = path.join(root, "target/fixture-tool-versions.cjs");
  const probes = path.join(root, "target/fixture-tool-versions.log");
  mkdirSync(path.dirname(preload), { recursive: true });
  writeFileSync(preload, `
const cp = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');
const original = cp.spawnSync;
cp.spawnSync = function(program, args) {
  const name = path.basename(String(program)).replace(/\\.exe$/i, '');
  const direct = ['cargo', 'rustc', 'npm'].includes(name) && args?.length === 1 && args[0] === '--version';
  const npm = path.resolve(String(program)) === path.resolve(process.execPath) && args?.length === 2 && path.basename(args[0]) === 'npm-cli.js' && args[1] === '--version';
  if (!direct && !npm) return original.apply(this, arguments);
  const tool = npm ? 'npm' : name;
  fs.appendFileSync(process.env.MESH_FIXTURE_TOOL_VERSION_PROBES, tool + '\\n');
  const stdout = tool + ' validation-fixture\\n';
  return { status: 0, signal: null, stdout, stderr: '', output: [null, stdout, ''] };
};
require('node:module').syncBuiltinESMExports();
`);
  return {
    probes,
    env: { ...env,
      NODE_OPTIONS: `${env.NODE_OPTIONS || ""} --import=${pathToFileURL(preload).href}`.trim(),
      MESH_FIXTURE_TOOL_VERSION_PROBES: probes },
  };
}
