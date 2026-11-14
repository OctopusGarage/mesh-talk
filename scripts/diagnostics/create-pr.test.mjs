import assert from "node:assert/strict";
import test from "node:test";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

test(
  "PR creation is fast by default, supports full preparation and preserves argument boundaries",
  { skip: process.platform === "win32" },
  (t) => {
    const entry = resolve("scripts/create-pr.sh");
    assert.ok(
      existsSync(entry),
      "PR creation needs a preflight-enforcing entry point",
    );
    const root = mkdtempSync(join(tmpdir(), "mesh-create-pr-"));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    mkdirSync(join(root, "scripts"));
    mkdirSync(join(root, "scripts/pr-platform"));
    mkdirSync(join(root, "scripts/validation"));
    mkdirSync(join(root, "bin"));
    assert.equal(spawnSync("git", ["init", "-q"], { cwd: root }).status, 0);
    writeFileSync(join(root, ".gitignore"), "*\n");
    assert.equal(
      spawnSync(
        "git",
        [
          "-c",
          "user.name=Preflight Test",
          "-c",
          "user.email=preflight@example.test",
          "commit",
          "--allow-empty",
          "-qm",
          "fixture",
        ],
        { cwd: root },
      ).status,
      0,
    );
    for (const branch of ["main", "release"])
      assert.equal(
        spawnSync(
          "git",
          ["update-ref", `refs/remotes/origin/${branch}`, "HEAD"],
          { cwd: root },
        ).status,
        0,
      );
    copyFileSync(entry, join(root, "scripts/create-pr.sh"));
    copyFileSync(
      resolve("scripts/validation/syntax.mjs"),
      join(root, "scripts/validation/syntax.mjs"),
    );
    for (const file of [
      "scripts/patch-coverage.mjs",
      "scripts/validation/stage.mjs",
      "scripts/validation/stage-inputs.mjs",
    ])
      copyFileSync(resolve(file), join(root, file));
    writeFileSync(
      join(root, "scripts/validation/changed.mjs"),
      `
import { checkSyntax } from './syntax.mjs';
const args = process.argv.slice(2);
const explicit = args.slice(3, -1);
if (JSON.stringify(explicit) !== JSON.stringify([]) && JSON.stringify(explicit) !== JSON.stringify(['--owner','core-transport','--owner','pr-preparation','--reason',process.env.OWNER_REASON])) throw Error('invalid explicit owner arguments');
(await import('node:fs')).writeFileSync('cheap-args.json',JSON.stringify(args));
if (args[0] !== '--cheap' || args[1] !== '--base' || !['origin/main', 'origin/release'].includes(args[2]) || args.at(-1) !== '--json') throw Error('invalid cheap-check arguments');
if (!process.env.PLAN_BLOCKED) checkSyntax({ base: args[2] });
console.log(JSON.stringify({blockers: process.env.PLAN_BLOCKED ? ['fixture ownership blocker'] : []}));
process.exit(process.env.PLAN_BLOCKED ? 2 : 0);
`,
    );
    copyFileSync(
      resolve("scripts/pr-platform/plan.mjs"),
      join(root, "scripts/pr-platform/plan.mjs"),
    );
    copyFileSync(
      resolve("scripts/pr-platform/rust-packages.mjs"),
      join(root, "scripts/pr-platform/rust-packages.mjs"),
    );
    const trace = join(root, "trace.jsonl");
    const recorder = `#!${process.execPath}\nimport('node:fs').then(({appendFileSync}) => {appendFileSync(process.env.TRACE, JSON.stringify({kind:process.argv[1].endsWith('gh')?'gh':'prepare',args:process.argv.slice(2)})+'\\n');process.exit(process.argv[1].endsWith('gh')?0:Number(process.env.PREFLIGHT_STATUS ?? 0));});\n`;
    for (const file of [
      "scripts/validation/standards.mjs",
      "scripts/check-repository-hygiene.mjs",
    ])
      writeFileSync(
        join(root, file),
        "process.exit(Number(process.env.CHEAP_STATUS ?? 0));\n",
      );
    writeFileSync(join(root, "scripts/prepare-pr.sh"), recorder, {
      mode: 0o755,
    });
    writeFileSync(join(root, "bin/gh"), recorder, { mode: 0o755 });
    const args = [
      "--core-filter",
      "relay_",
      "e2e/ui-visual-regression.spec.ts",
      "--",
      "--draft",
      "--title",
      "literal $(touch unexpected)",
    ];
    const run = (status, options = args, extraEnv = {}, full = true) =>
      spawnSync(
        "bash",
        [
          join(root, "scripts/create-pr.sh"),
          ...(full ? ["--full-preflight"] : []),
          ...options,
        ],
        {
          cwd: root,
          encoding: "utf8",
          env: {
            ...process.env,
            TRACE: trace,
            PREFLIGHT_STATUS: String(status),
            PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`,
            ...extraEnv,
          },
        },
      );
    const fast = run(7, args, {}, false);
    assert.equal(fast.status, 0, fast.stderr);
    assert.deepEqual(
      readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse),
      [
        {
          kind: "gh",
          args: ["pr", "create", "--base", "main", ...args.slice(4)],
        },
      ],
      "default publication never starts full preparation",
    );
    writeFileSync(trace, "");
    for (const full of [false, true]) {
      const blocked = run(0, args, { PLAN_BLOCKED: "1" }, full);
      assert.notEqual(blocked.status, 0);
      assert.match(blocked.stderr, /PR readiness is blocked/);
      assert.deepEqual(
        JSON.parse(
          readFileSync(join(root, "target/validation/pr-plan.json"), "utf8"),
        ).blockers,
        ["fixture ownership blocker"],
      );
      assert.equal(
        readFileSync(trace, "utf8"),
        "",
        "plan blockers prevent preparation and publication",
      );
    }
    const ownerReason =
      "Reviewed historical main-to-dev consumers $(touch unexpected)";
    const ownerArgs = [
      "--owner",
      "core-transport",
      "--owner",
      "pr-preparation",
      "--reason",
      ownerReason,
      ...args,
    ];
    for (const full of [false, true]) {
      writeFileSync(trace, "");
      const explicit = run(0, ownerArgs, { OWNER_REASON: ownerReason }, full);
      assert.equal(explicit.status, 0, explicit.stderr);
      assert.deepEqual(
        JSON.parse(readFileSync(join(root, "cheap-args.json"), "utf8")),
        [
          "--cheap",
          "--base",
          "origin/main",
          "--owner",
          "core-transport",
          "--owner",
          "pr-preparation",
          "--reason",
          ownerReason,
          "--json",
        ],
      );
      const records = readFileSync(trace, "utf8")
        .trim()
        .split("\n")
        .map(JSON.parse);
      assert.deepEqual(
        records,
        full
          ? [
              { kind: "prepare", args: args.slice(0, 3) },
              {
                kind: "gh",
                args: ["pr", "create", "--base", "main", ...args.slice(4)],
              },
            ]
          : [
              {
                kind: "gh",
                args: ["pr", "create", "--base", "main", ...args.slice(4)],
              },
            ],
      );
      writeFileSync(trace, "");
      const blocked = run(
        0,
        ownerArgs,
        { OWNER_REASON: ownerReason, PLAN_BLOCKED: "1" },
        full,
      );
      assert.notEqual(blocked.status, 0);
      assert.equal(
        readFileSync(trace, "utf8"),
        "",
        "explicit owners do not bypass affected-plan failure",
      );
    }
    for (const malformed of [
      ["--owner"],
      ["--reason"],
      ["--owner", ""],
      ["--owner", "core-transport"],
      ["--reason", ""],
      ["--owner", "--reason"],
    ]) {
      writeFileSync(trace, "");
      for (const full of [false, true]) {
        assert.notEqual(run(0, [...malformed, ...args], {}, full).status, 0);
        assert.equal(
          readFileSync(trace, "utf8"),
          "",
          "invalid ownership arguments cannot prepare or publish",
        );
      }
    }
    assert.equal(run(0, args, { CHEAP_STATUS: "9" }, false).status, 9);
    assert.equal(
      readFileSync(trace, "utf8"),
      "",
      "cheap checks fail before publication",
    );
    writeFileSync(join(root, "dirty"), "uncommitted\n");
    assert.equal(
      spawnSync("git", ["add", "-f", "dirty"], { cwd: root }).status,
      0,
    );
    assert.notEqual(run(0, args, {}, false).status, 0);
    assert.equal(
      readFileSync(trace, "utf8"),
      "",
      "dirty candidates cannot publish",
    );
    assert.equal(
      spawnSync("git", ["rm", "--cached", "dirty"], { cwd: root }).status,
      0,
    );
    const failed = run(7);
    assert.equal(failed.status, 7, failed.stderr);
    assert.deepEqual(
      readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse),
      [{ kind: "prepare", args: args.slice(0, 3) }],
    );
    writeFileSync(trace, "");
    const passed = run(0);
    assert.equal(passed.status, 0, passed.stderr);
    assert.deepEqual(
      readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse),
      [
        { kind: "prepare", args: args.slice(0, 3) },
        {
          kind: "gh",
          args: ["pr", "create", "--base", "main", ...args.slice(4)],
        },
      ],
    );
    assert.equal(existsSync(join(root, "unexpected")), false);

    for (const override of [
      ["--head", "other"],
      ["--head=other"],
      ["-Hother"],
      ["--repo", "other/repo"],
      ["--repo=other/repo"],
      ["-Rother/repo"],
      ["--base", "release"],
      ["--base=release"],
      ["-Brelease"],
    ]) {
      writeFileSync(trace, "");
      assert.notEqual(
        run(0, [...args, ...override]).status,
        0,
        "cannot publish another head or repository",
      );
      assert.equal(
        readFileSync(trace, "utf8"),
        "",
        "reject candidate overrides before expensive preflight",
      );
    }
    assert.notEqual(run(0, args, { GH_REPO: "other/repo" }).status, 0);
    assert.equal(
      readFileSync(trace, "utf8"),
      "",
      "environment overrides cannot select an unverified repository",
    );
    assert.notEqual(run(0, ["--base", "HEAD", ...args]).status, 0);
    assert.equal(
      readFileSync(trace, "utf8"),
      "",
      "a local revision cannot masquerade as the PR target branch",
    );
    const releaseArgs = ["--base", "origin/release", ...args];
    assert.equal(run(0, releaseArgs).status, 0);
    const releaseTrace = readFileSync(trace, "utf8")
      .trim()
      .split("\n")
      .map(JSON.parse);
    assert.deepEqual(releaseTrace[0].args, releaseArgs.slice(0, 5));
    assert.deepEqual(releaseTrace[1].args, [
      "pr",
      "create",
      "--base",
      "release",
      ...args.slice(4),
    ]);

    writeFileSync(trace, "");
    const docsOnly = run(0, ["--", "--draft"], {}, false);
    assert.equal(docsOnly.status, 0, docsOnly.stderr);
    assert.deepEqual(
      readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse),
      [{ kind: "gh", args: ["pr", "create", "--base", "main", "--draft"] }],
    );

    mkdirSync(join(root, "frontend/src"), { recursive: true });
    writeFileSync(
      join(root, "frontend/src/committed.ts"),
      "export const changed = true;\n",
    );
    assert.equal(
      spawnSync("git", ["add", "-f", "frontend/src/committed.ts"], {
        cwd: root,
      }).status,
      0,
    );
    assert.equal(
      spawnSync(
        "git",
        [
          "-c",
          "user.name=Test",
          "-c",
          "user.email=preflight@example.test",
          "commit",
          "-qm",
          "frontend source without coverage",
        ],
        { cwd: root },
      ).status,
      0,
    );
    writeFileSync(
      join(root, "bin/npm"),
      '#!/bin/bash\nif [ "$1" = --version ]; then echo fixture-npm; else touch npm-tests-started; exit 99; fi\n',
      { mode: 0o755 },
    );
    for (const full of [false, true]) {
      writeFileSync(trace, "");
      const missing = run(0, args, {}, full);
      assert.notEqual(missing.status, 0);
      assert.match(
        missing.stderr,
        /Current frontend coverage evidence is unavailable/,
      );
      assert.match(missing.stderr, /Generate it with:/);
      assert.equal(
        existsSync(join(root, "npm-tests-started")),
        false,
        "coverage verification must never start npm tests",
      );
      const recorded = readFileSync(trace, "utf8")
        .trim()
        .split("\n")
        .filter(Boolean)
        .map(JSON.parse);
      assert.deepEqual(
        recorded,
        full ? [{ kind: "prepare", args: args.slice(0, 3) }] : [],
        "missing coverage prevents publication",
      );
    }
    assert.equal(
      spawnSync("git", ["rm", "frontend/src/committed.ts"], { cwd: root })
        .status,
      0,
    );
    assert.equal(
      spawnSync(
        "git",
        [
          "-c",
          "user.name=Test",
          "-c",
          "user.email=preflight@example.test",
          "commit",
          "-qm",
          "remove frontend fixture",
        ],
        { cwd: root },
      ).status,
      0,
    );

    writeFileSync(trace, "");
    writeFileSync(
      join(root, "scripts/prepare-pr.sh"),
      "#!/usr/bin/env bash\ngit -c user.name=Test -c user.email=preflight@example.test commit --allow-empty -qm changed\n",
      { mode: 0o755 },
    );
    const changed = run(0);
    assert.notEqual(
      changed.status,
      0,
      "cannot publish a revision changed during verification",
    );
    assert.equal(
      readFileSync(trace, "utf8"),
      "",
      "gh must not run for an unverified revision",
    );
    writeFileSync(
      join(root, "scripts/validation/standards.mjs"),
      "import {execFileSync} from 'node:child_process'; execFileSync('git', ['-c', 'user.name=Test', '-c', 'user.email=preflight@example.test', 'commit', '--allow-empty', '-qm', 'changed-during-cheap-check']);\n",
    );
    assert.notEqual(run(0, args, {}, false).status, 0);
    assert.equal(
      readFileSync(trace, "utf8"),
      "",
      "cheap-check changes cannot publish either",
    );
    assert.notEqual(
      run(0, ["--draft"], {}, false).status,
      0,
      "separator remains mandatory",
    );
    writeFileSync(join(root, "scripts/broken.mjs"), "export const = ;\n");
    assert.equal(
      spawnSync("git", ["add", "-f", "scripts/broken.mjs"], { cwd: root })
        .status,
      0,
    );
    assert.equal(
      spawnSync(
        "git",
        [
          "-c",
          "user.name=Test",
          "-c",
          "user.email=preflight@example.test",
          "commit",
          "-qm",
          "invalid syntax fixture",
        ],
        { cwd: root },
      ).status,
      0,
    );
    for (const full of [false, true]) {
      const invalid = run(0, args, {}, full);
      assert.notEqual(invalid.status, 0);
      assert.match(invalid.stderr, /rejected syntax/);
      assert.equal(
        readFileSync(trace, "utf8"),
        "",
        "invalid committed scripts cannot reach preparation or publication",
      );
    }
  },
);
