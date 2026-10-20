import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  existsSync,
  realpathSync,
  rmSync,
} from "node:fs";
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
  writeFileSync(path.join(tools, "plutil"), "#!/bin/sh\nexit 0\n", {
    mode: 0o755,
  });
  const signingLog = path.join(dir, "signing.log");
  writeFileSync(signingLog, "");
  writeFileSync(
    path.join(tools, "codesign"),
    `#!/usr/bin/env python3
import os, pathlib, re, sys
args = sys.argv[1:]
binary = pathlib.Path(args[-1])
body = binary.read_text()
with open(os.environ['MESH_TALK_SIGNING_LOG'], 'a') as output:
    output.write(repr(args) + '\\n')
identifier = re.search(r'^# signing-id: (.+)$', body, re.M)
if '--display' in args:
    if not identifier:
        sys.exit(1)
    print('Identifier=' + identifier[1], file=sys.stderr)
    print('Authority=Test Publisher' if '# publisher-signed' in body else 'Signature=adhoc', file=sys.stderr)
elif '--sign' in args:
    if '# signing-fails' in body:
        sys.exit(2)
    identifier = args[args.index('--identifier') + 1]
    body = re.sub(r'^# signing-id: .+\\n?', '', body, flags=re.M)
    binary.write_text(body + '\\n# signing-id: ' + identifier + '\\n')
elif '--verify' in args:
    sys.exit(0 if identifier and '# verification-fails' not in body else 3)
else:
    sys.exit(4)
`,
    { mode: 0o755 },
  );
  const binary = path.join(dir, "staged-node");
  writeFileSync(
    binary,
    `#!/bin/sh
if [ "$1" = federation ]; then
  [ "$2" = --file ] && [ "$4" = list ] || exit 10
  [ "$(head -n 1 "$3")" = mesh-talk-federation-v1 ] || exit 11
fi
`,
    { mode: 0o755 },
  );
  const manifest = path.join(dir, "public pins.conf");
  writeFileSync(
    manifest,
    "mesh-talk-federation-v1\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@127.0.0.1:47474\n",
  );
  const installer = path.join(dir, "installer.sh");
  writeFileSync(
    installer,
    readFileSync(
      path.join(root, "scripts/install-post-office-macos.sh"),
      "utf8",
    ).replace("home_dir=$HOME", "home_dir=$MESH_TALK_INSTALLER_TEST_HOME"),
  );
  const run = (...extras) =>
    spawnSync(
      "bash",
      [
        installer,
        "prepare",
        "--binary",
        binary,
        "--name",
        "relay",
        "--port",
        "47479",
        ...extras,
      ],
      {
        cwd: dir,
        env: {
          ...process.env,
          MESH_TALK_INSTALLER_TEST_HOME: home,
          MESH_TALK_SIGNING_LOG: signingLog,
          PATH: `${tools}:${process.env.PATH}`,
        },
        encoding: "utf8",
      },
    );
  return { dir, home, manifest, run, binary, signingLog };
}

test(
  "macOS updates retain the installed signing identifier without modifying the source binary",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    const installed = path.join(f.home, ".local/bin/mesh-talk-node");
    mkdirSync(path.dirname(installed), { recursive: true });
    writeFileSync(
      installed,
      "#!/bin/sh\n# signing-id: existing-relay-identity\n",
      { mode: 0o755 },
    );
    const source =
      readFileSync(f.binary, "utf8") +
      "\n# signing-id: changed-linker-identity\n";
    writeFileSync(f.binary, source, { mode: 0o755 });
    const result = f.run();
    assert.equal(result.status, 0, result.stderr);
    assert.match(
      readFileSync(installed, "utf8"),
      /# signing-id: existing-relay-identity/,
    );
    assert.equal(readFileSync(f.binary, "utf8"), source);
    assert.match(readFileSync(f.signingLog, "utf8"), /--verify/);
  },
);

test(
  "macOS first installs use a stable signing identifier",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    const result = f.run();
    assert.equal(result.status, 0, result.stderr);
    assert.match(
      readFileSync(path.join(f.home, ".local/bin/mesh-talk-node"), "utf8"),
      /# signing-id: com\.octopusgarage\.mesh-talk\.node/,
    );
  },
);

test(
  "macOS prepare preserves publisher signatures",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    const source =
      readFileSync(f.binary, "utf8") +
      "\n# signing-id: publisher-relay\n# publisher-signed\n";
    writeFileSync(f.binary, source, { mode: 0o755 });
    const result = f.run();
    assert.equal(result.status, 0, result.stderr);
    assert.equal(
      readFileSync(path.join(f.home, ".local/bin/mesh-talk-node"), "utf8"),
      source,
    );
    assert.doesNotMatch(readFileSync(f.signingLog, "utf8"), /--sign/);
  },
);

test(
  "macOS signing and verification failures leave the installed executable intact",
  { skip: process.platform === "win32" },
  (t) => {
    for (const failure of ["signing-fails", "verification-fails"]) {
      const f = fixture(t);
      const installed = path.join(f.home, ".local/bin/mesh-talk-node");
      mkdirSync(path.dirname(installed), { recursive: true });
      const previous = "#!/bin/sh\n# signing-id: existing-relay-identity\n";
      writeFileSync(installed, previous, { mode: 0o755 });
      writeFileSync(
        f.binary,
        readFileSync(f.binary, "utf8") + `\n# ${failure}\n`,
        { mode: 0o755 },
      );
      const result = f.run();
      assert.notEqual(result.status, 0, failure);
      assert.equal(readFileSync(installed, "utf8"), previous);
    }
  },
);

test(
  "macOS prepare persists an authoritative config and keeps existing private identity and password",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    const data = path.join(f.home, ".local/share/mesh-talk-post-office");
    mkdirSync(data, { recursive: true });
    writeFileSync(path.join(data, "password"), "keep-password");
    writeFileSync(path.join(data, "identity.keystore"), "keep-identity");
    const result = f.run("--federation-config", path.basename(f.manifest));
    assert.equal(result.status, 0, result.stderr);
    const plist = path.join(
      f.home,
      ".local/state/mesh-talk-post-office/LaunchDaemon.plist",
    );
    const args = JSON.parse(
      spawnSync(
        "python3",
        [
          "-c",
          "import json,plistlib,sys; print(json.dumps(plistlib.load(open(sys.argv[1], 'rb'))['ProgramArguments']))",
          plist,
        ],
        { encoding: "utf8" },
      ).stdout,
    );
    assert.ok(args.includes("--bridge"));
    const configPath = args[args.indexOf("--federation-config") + 1];
    assert.ok(path.isAbsolute(configPath));
    assert.equal(configPath, realpathSync(path.join(data, "federation.conf")));
    assert.equal(
      readFileSync(configPath, "utf8"),
      readFileSync(f.manifest, "utf8"),
    );
    rmSync(f.manifest);
    assert.ok(
      existsSync(configPath),
      "service config survives staging cleanup",
    );
    assert.equal(
      readFileSync(path.join(data, "password"), "utf8"),
      "keep-password",
    );
    assert.equal(
      readFileSync(path.join(data, "identity.keystore"), "utf8"),
      "keep-identity",
    );
  },
);

test(
  "macOS config errors and conflicting inline pins fail before installation mutation",
  { skip: process.platform === "win32" },
  (t) => {
    for (const kind of ["invalid", "missing", "conflict"]) {
      const f = fixture(t);
      if (kind === "invalid")
        writeFileSync(f.manifest, "unsupported-version\n");
      if (kind === "missing") rmSync(f.manifest);
      const extra =
        kind === "conflict"
          ? [
              "--federation-peer",
              "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@127.0.0.1:47474",
            ]
          : [];
      const result = f.run("--federation-config", f.manifest, ...extra);
      assert.notEqual(result.status, 0, kind);
      assert.ok(
        !existsSync(path.join(f.home, ".local")),
        `${kind} mutated installation`,
      );
    }
  },
);

test(
  "Windows parses the installer and rejects missing config before creating service state",
  { skip: process.platform !== "win32" },
  (t) => {
    const dir = mkdtempSync(path.join(tmpdir(), "relay-installer-windows-"));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    const installer = path.join(
      root,
      "scripts/install-post-office-windows.ps1",
    );
    const target = path.join(dir, "not-installed");
    const quote = (value) => "'" + value.replaceAll("'", "''") + "'";
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
    const result = spawnSync(
      "powershell",
      ["-NoProfile", "-NonInteractive", "-Command", command],
      { encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr || result.stdout);
    assert.ok(
      !existsSync(target),
      "rejected configuration must not create service state",
    );
  },
);

test(
  "macOS service uses the standard post-office and shared LAN discovery ports",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    const result = f.run();
    assert.equal(result.status, 0, result.stderr);
    const plist = path.join(
      f.home,
      ".local/state/mesh-talk-post-office/LaunchDaemon.plist",
    );
    const parsed = spawnSync(
      "python3",
      [
        "-c",
        "import json,plistlib,sys; print(json.dumps(plistlib.load(open(sys.argv[1], 'rb'))['ProgramArguments']))",
        plist,
      ],
      { encoding: "utf8" },
    );
    assert.equal(parsed.status, 0, parsed.stderr);
    const args = JSON.parse(parsed.stdout);
    assert.equal(args[args.indexOf("--discovery-port") + 1], "47478");
    assert.ok(args.includes("--lan-announce-port"));
    assert.equal(args[args.indexOf("--lan-announce-port") + 1], "47474");
  },
);

test(
  "macOS rejects nonstandard service discovery ports before changing private state",
  { skip: process.platform === "win32" },
  (t) => {
    for (const extra of [
      ["--discovery-port", "47479"],
      ["--lan-announce-port", "47480"],
    ]) {
      const f = fixture(t);
      const result = f.run(...extra);
      assert.notEqual(result.status, 0, extra.join(" "));
      assert.match(
        result.stderr,
        /Managed post offices require UDP 47478 and LAN UDP 47474/,
      );
      assert.ok(!existsSync(path.join(f.home, ".local")));
    }
  },
);

test(
  "Windows rejects nonstandard discovery ports before creating service state",
  { skip: process.platform !== "win32" },
  (t) => {
    const dir = mkdtempSync(path.join(tmpdir(), "relay-standard-ports-"));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    const installer = path.join(
      root,
      "scripts/install-post-office-windows.ps1",
    );
    const target = path.join(dir, "not-installed");
    const quote = (value) => "'" + value.replaceAll("'", "''") + "'";
    const command = `
$ErrorActionPreference = 'Stop'
foreach ($ports in @(@(47479,47474), @(47478,47480))) {
  try {
    & ${quote(installer)} -StagedBinary ${quote(process.execPath)} -Root ${quote(target)} -DiscoveryPort $ports[0] -LanAnnouncePort $ports[1]
    throw 'Nonstandard discovery ports were accepted'
  } catch {
    if ($_.Exception.Message -ne 'Managed post offices require UDP 47478 and LAN UDP 47474') { throw }
  }
}
exit 0
`;
    const result = spawnSync(
      "powershell",
      ["-NoProfile", "-NonInteractive", "-Command", command],
      { encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr || result.stdout);
    assert.ok(!existsSync(target));
  },
);

test(
  "macOS activation waits for launchd to remove the old service before bootstrapping",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    assert.equal(f.run().status, 0);
    const installer = path.join(f.dir, "installer.sh");
    const installed = path.join(f.dir, "installed.plist");
    writeFileSync(
      installer,
      readFileSync(installer, "utf8").replace(
        "installed_plist=/Library/LaunchDaemons/$label.plist",
        `installed_plist='${installed}'`,
      ),
    );
    const tools = path.join(f.dir, "tools");
    writeFileSync(
      path.join(tools, "sudo"),
      `#!/usr/bin/env python3
import shutil, subprocess, sys
args=sys.argv[1:]
if args==['-v']: sys.exit(0)
if args[0]=='install':
    shutil.copyfile(args[-2],args[-1])
    sys.exit(0)
sys.exit(subprocess.call(args))
`,
      { mode: 0o755 },
    );
    writeFileSync(
      path.join(tools, "launchctl"),
      `#!/usr/bin/env python3
import json, os, pathlib, sys
path=pathlib.Path(os.environ['MESH_TALK_LAUNCHD_STATE'])
state=json.loads(path.read_text()) if path.exists() else {'pending':0,'running':True,'enabled':False}
args=sys.argv[1:]
code=0
if args[0]=='bootout':
    if args[1].startswith('gui/'): sys.exit(113)
    state.update(pending=2,running=False)
elif args[0]=='print':
    if state['pending']:
        state['pending']-=1
        print('state = terminating')
    elif state['running']: print('state = running\\npid = 123')
    else: code=113
elif args[0]=='enable': state['enabled']=True
elif args[0]=='bootstrap':
    if state['pending'] or not state['enabled']:
        print('Bootstrap failed: 37: Operation already in progress',file=sys.stderr)
        code=37
    else: state['running']=True
path.write_text(json.dumps(state))
sys.exit(code)
`,
      { mode: 0o755 },
    );
    const result = spawnSync("bash", [installer, "activate"], {
      encoding: "utf8",
      env: {
        ...process.env,
        MESH_TALK_INSTALLER_TEST_HOME: f.home,
        MESH_TALK_LAUNCHD_STATE: path.join(f.dir, "launchd.json"),
        PATH: `${tools}:${process.env.PATH}`,
      },
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /state = running/);
    assert.ok(existsSync(installed));
  },
);

test(
  "macOS activation rejects a legacy staged discovery port before requesting sudo",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    assert.equal(f.run().status, 0);
    const plist = path.join(
      f.home,
      ".local/state/mesh-talk-post-office/LaunchDaemon.plist",
    );
    const edited = spawnSync("python3", [
      "-c",
      "import plistlib,sys; p=sys.argv[1]; d=plistlib.load(open(p,'rb')); a=d['ProgramArguments']; a[a.index('--discovery-port')+1]='47479'; plistlib.dump(d,open(p,'wb'))",
      plist,
    ]);
    assert.equal(edited.status, 0);
    const tools = path.join(f.dir, "tools");
    const sudoLog = path.join(f.dir, "sudo-called");
    writeFileSync(
      path.join(tools, "sudo"),
      '#!/bin/sh\ntouch "$MESH_TALK_SUDO_LOG"\nexit 97\n',
      { mode: 0o755 },
    );
    const result = spawnSync(
      "bash",
      [path.join(f.dir, "installer.sh"), "activate"],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          MESH_TALK_INSTALLER_TEST_HOME: f.home,
          MESH_TALK_SUDO_LOG: sudoLog,
          PATH: `${tools}:${process.env.PATH}`,
        },
      },
    );
    assert.notEqual(result.status, 0);
    assert.match(
      result.stderr,
      /Managed post offices require UDP 47478 and LAN UDP 47474/,
    );
    assert.ok(!existsSync(sudoLog));
  },
);

test(
  "macOS service defaults to TCP 47479 without a per-host port argument",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    const result = spawnSync(
      "bash",
      [
        path.join(f.dir, "installer.sh"),
        "prepare",
        "--binary",
        f.binary,
        "--name",
        "relay",
      ],
      {
        encoding: "utf8",
        env: {
          ...process.env,
          MESH_TALK_INSTALLER_TEST_HOME: f.home,
          MESH_TALK_SIGNING_LOG: f.signingLog,
          PATH: `${path.join(f.dir, "tools")}:${process.env.PATH}`,
        },
      },
    );
    assert.equal(result.status, 0, result.stderr);
    const plist = path.join(
      f.home,
      ".local/state/mesh-talk-post-office/LaunchDaemon.plist",
    );
    const parsed = spawnSync(
      "python3",
      [
        "-c",
        "import json,plistlib,sys; print(json.dumps(plistlib.load(open(sys.argv[1], 'rb'))['ProgramArguments']))",
        plist,
      ],
      { encoding: "utf8" },
    );
    assert.equal(parsed.status, 0, parsed.stderr);
    const args = JSON.parse(parsed.stdout);
    assert.equal(args[args.indexOf("--port") + 1], "47479");
  },
);

test(
  "macOS rejects nonstandard managed TCP ports before changing private state",
  { skip: process.platform === "win32" },
  (t) => {
    const f = fixture(t);
    const result = f.run("--port", "47476");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Managed post offices require TCP 47479/);
    assert.ok(!existsSync(path.join(f.home, ".local")));
  },
);
