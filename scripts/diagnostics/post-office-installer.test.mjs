import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "../..");

function fixture(t) {
  const dir = mkdtempSync(path.join(tmpdir(), "relay-installer-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const home = path.join(dir, "service home");
  const tools = path.join(dir, "tools");
  mkdirSync(home);
  mkdirSync(tools);
  writeFileSync(path.join(tools, "plutil"), "#!/bin/sh\nexit 0\n", { mode: 0o755 });
  const binary = path.join(dir, "staged-node");
  writeFileSync(binary, `#!/bin/sh
if [ "$1" = federation ]; then
  [ "$2" = --file ] && [ "$4" = list ] || exit 10
  [ "$(head -n 1 "$3")" = mesh-talk-federation-v1 ] || exit 11
fi
`, { mode: 0o755 });
  const manifest = path.join(dir, "public pins.conf");
  writeFileSync(manifest, "mesh-talk-federation-v1\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@127.0.0.1:47474\n");
  const installer = path.join(dir, "installer.sh");
  writeFileSync(installer, readFileSync(path.join(root, "scripts/install-post-office-macos.sh"), "utf8").replace("home_dir=$HOME", "home_dir=$MESH_TALK_INSTALLER_TEST_HOME"));
  const run = (...extras) => spawnSync("bash", [installer, "prepare", "--binary", binary, "--name", "relay", "--port", "47475", ...extras], {
    cwd: dir, env: { ...process.env, MESH_TALK_INSTALLER_TEST_HOME: home, PATH: `${tools}:${process.env.PATH}` }, encoding: "utf8",
  });
  return { dir, home, manifest, run };
}

test("macOS prepare persists an authoritative config and keeps existing private identity and password", { skip: process.platform === "win32" }, (t) => {
  const f = fixture(t);
  const data = path.join(f.home, ".local/share/mesh-talk-post-office");
  mkdirSync(data, { recursive: true });
  writeFileSync(path.join(data, "password"), "keep-password");
  writeFileSync(path.join(data, "identity.keystore"), "keep-identity");
  const result = f.run("--federation-config", path.basename(f.manifest));
  assert.equal(result.status, 0, result.stderr);
  const plist = path.join(f.home, ".local/state/mesh-talk-post-office/LaunchDaemon.plist");
  const args = JSON.parse(spawnSync("python3", ["-c", "import json,plistlib,sys; print(json.dumps(plistlib.load(open(sys.argv[1], 'rb'))['ProgramArguments']))", plist], { encoding: "utf8" }).stdout);
  assert.ok(args.includes("--bridge"));
  const configPath = args[args.indexOf("--federation-config") + 1];
  assert.ok(path.isAbsolute(configPath));
  assert.equal(configPath, realpathSync(path.join(data, "federation.conf")));
  assert.equal(readFileSync(configPath, "utf8"), readFileSync(f.manifest, "utf8"));
  rmSync(f.manifest);
  assert.ok(existsSync(configPath), "service config survives staging cleanup");
  assert.equal(readFileSync(path.join(data, "password"), "utf8"), "keep-password");
  assert.equal(readFileSync(path.join(data, "identity.keystore"), "utf8"), "keep-identity");
});

test("macOS config errors and conflicting inline pins fail before installation mutation", { skip: process.platform === "win32" }, (t) => {
  for (const kind of ["invalid", "missing", "conflict"]) {
    const f = fixture(t);
    if (kind === "invalid") writeFileSync(f.manifest, "unsupported-version\n");
    if (kind === "missing") rmSync(f.manifest);
    const extra = kind === "conflict" ? ["--federation-peer", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@127.0.0.1:47474"] : [];
    const result = f.run("--federation-config", f.manifest, ...extra);
    assert.notEqual(result.status, 0, kind);
    assert.ok(!existsSync(path.join(f.home, ".local")), `${kind} mutated installation`);
  }
});

test("Windows parses the installer and rejects missing config before creating service state", { skip: process.platform !== "win32" }, (t) => {
  const dir = mkdtempSync(path.join(tmpdir(), "relay-installer-windows-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const installer = path.join(root, "scripts/install-post-office-windows.ps1");
  const target = path.join(dir, "not-installed");
  const quote = value => "'" + value.replaceAll("'", "''") + "'";
  const command = `
$ErrorActionPreference = 'Stop'
$tokens = $null; $errors = $null
[System.Management.Automation.Language.Parser]::ParseFile(${quote(installer)}, [ref]$tokens, [ref]$errors) | Out-Null
if ($errors.Count -gt 0) { throw ($errors | Out-String) }
try {
  & ${quote(installer)} -StagedBinary ${quote(process.execPath)} -Root ${quote(target)} -FederationConfig ${quote(path.join(dir, "missing.conf"))}
  throw 'Missing manifest was accepted'
} catch {
  if ($_.Exception.Message -notlike 'Federation config must be an existing public manifest:*') { throw }
}
exit 0
`;
  const result = spawnSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", command], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.ok(!existsSync(target), "rejected configuration must not create service state");
});
