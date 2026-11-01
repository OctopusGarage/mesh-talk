import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

import * as native from "../validation/native-build.mjs";
const suffix = process.platform === "win32" ? ".exe" : "";
const completed = output => ({ code: 0, output, stdout: output, stderr: "", outputTruncated: false });

async function fixture(t) {
  assert.equal(typeof native.buildNative, "function", "source-pinned native build helper exists");
  const root = await mkdtemp(path.join(tmpdir(), "mesh-native-build-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const git = args => execFileSync("git", ["-C", root, ...args], { encoding: "utf8" }).trim();
  await writeFile(path.join(root, ".gitignore"), "target/\nfrontend/dist/\n");
  await writeFile(path.join(root, "source.txt"), "committed input\n");
  git(["init", "--quiet"]);
  git(["add", "."]);
  const commit = () => git(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-am", "fixture"]);
  commit();
  const calls = [];
  const run = async (program, args, options) => {
    calls.push({ program, args, cwd: options.cwd });
    const relative = program === "npm" ? ["frontend/dist/index.html", "frontend/dist/assets/app.js"] : args.includes("--example") ? [`target/release/examples/native-contact-eval${suffix}`] : [`target/release/mesh-talk${suffix}`, `target/release/mesh-talk-node${suffix}`];
    for (const file of relative) {
      await mkdir(path.dirname(path.join(root, file)), { recursive: true });
      await writeFile(path.join(root, file), `built from ${git(["rev-parse", "HEAD"])}: ${file}`);
    }
    return completed(program === "npm" ? "built" : relative.map(file => JSON.stringify({ reason: "compiler-artifact", target: { name: path.basename(file, suffix), kind: [args.includes("--example") ? "example" : "bin"] }, executable: path.join(root, file), fresh: true })).join("\n"));
  };
  const appBinary = path.join(root, `target/release/examples/native-contact-eval${suffix}`);
  const nodeBinary = path.join(root, `target/release/mesh-talk-node${suffix}`);
  // Each temporary repository owns its source pin; the guest's outer SHA belongs
  // to the real checkout and must not leak into independent fixture builds.
  const build = (options = {}) => native.buildNative({
    log: () => {}, root, run,
    env: { ...process.env, EVAL_SOURCE_SHA: git(["rev-parse", "HEAD"]) },
    ...options,
  });
  return { root, git, commit, run, calls, build, appBinary, nodeBinary, verify: () => native.verifyNativeBuild({ root, appBinary, nodeBinary }) };
}

test("native build records clean source and the exact frontend and binaries after real build commands", async t => {
  const f = await fixture(t);
  const manifest = await f.build();
  assert.equal(manifest.sourceSha, f.git(["rev-parse", "HEAD"]));
  assert.equal(manifest.schema, 1);
  assert.equal(f.calls.length, 3);
  assert.deepEqual(f.calls[0], { program: "npm", args: ["run", "build"], cwd: path.join(f.root, "frontend") });
  assert.deepEqual(f.calls[1].args, ["build", "--release", "--locked", "--message-format=json", "--target-dir", "target", "-p", "mesh-talk", "--bin", "mesh-talk", "-p", "mesh-talk-core", "--bin", "mesh-talk-node"]);
  assert.deepEqual(f.calls[2].args, ["build", "--release", "--locked", "--message-format=json", "--target-dir", "target", "-p", "mesh-talk", "--example", "native-contact-eval"]);
  const proof = await f.verify();
  assert.equal(proof.buildVerified, true, proof.error);
  assert.equal(f.git(["status", "--porcelain"]), "", "build manifest remains ignored");
});

test("missing proof and binaries from a previous commit cannot certify current source", async t => {
  const f = await fixture(t);
  assert.equal((await f.verify()).buildVerified, false);
  await f.build();
  await writeFile(path.join(f.root, "source.txt"), "next commit\n");
  f.commit();
  const proof = await f.verify();
  assert.equal(proof.buildVerified, false);
  assert.match(proof.error, /source/i);
});

test("changed binaries, frontend assets and identical-content path overrides fail provenance", async t => {
  const f = await fixture(t);
  for (const relative of [`target/release/examples/native-contact-eval${suffix}`, `target/release/mesh-talk-node${suffix}`, "frontend/dist/assets/app.js", `target/release/mesh-talk${suffix}`]) {
    await f.build();
    await writeFile(path.join(f.root, relative), "stale or modified bytes");
    const proof = await f.verify();
    assert.equal(proof.buildVerified, false, relative);
    assert.match(proof.error, /digest/i);
  }
  await f.build();
  const alternate = path.join(f.root, `target/alternate${suffix}`);
  await writeFile(alternate, await readFile(f.appBinary));
  assert.equal((await native.verifyNativeBuild({ root: f.root, appBinary: alternate, nodeBinary: f.nodeBinary })).buildVerified, false);
  await writeFile(alternate, await readFile(f.nodeBinary));
  assert.equal((await native.verifyNativeBuild({ root: f.root, appBinary: f.appBinary, nodeBinary: alternate })).buildVerified, false);
});

test("dirty input, failed builds and changes during builds never leave valid native proof", async t => {
  const f = await fixture(t);
  await f.build();
  await assert.rejects(f.build({ run: async () => ({ code: 1, output: "compile failed" }) }), /compile failed/);
  assert.equal((await f.verify()).buildVerified, false);
  await f.build();
  await writeFile(path.join(f.root, "source.txt"), "dirty input\n");
  assert.equal((await f.verify()).buildVerified, false);
  await assert.rejects(f.build(), /clean/i);
  f.commit();
  await assert.rejects(f.build({ run: async (...args) => {
    const result = await f.run(...args);
    await writeFile(path.join(f.root, "source.txt"), "changed while compiling\n");
    return result;
  } }), /source.*changed/i);
  assert.equal((await f.verify()).buildVerified, false);
});

test("runner uses shared native proof checks and CI builds through the helper", async () => {
  const runner = await readFile(new URL("./hidden-contacts.mjs", import.meta.url), "utf8");
  const workflow = await readFile(new URL("../../.github/workflows/hidden-contacts-native.yml", import.meta.url), "utf8");
  assert.match(runner, /verifyNativeBuild/);
  assert.match(runner, /report\.buildVerified/);
  assert.match(workflow, /node scripts\/validation\/native-build\.mjs/);
  assert.doesNotMatch(workflow, /run: cargo build/);
});


test("proof is bound to this platform and architecture and cannot be replaced during evaluation", async t => {
  const f = await fixture(t);
  await f.build();
  const initial = await f.verify();
  const manifestFile = native.nativeBuildManifest(f.root);
  const original = JSON.parse(await readFile(manifestFile, "utf8"));
  for (const field of ["platform", "architecture"]) {
    await writeFile(manifestFile, JSON.stringify({ ...original, [field]: "other-host" }));
    const proof = await f.verify();
    assert.equal(proof.buildVerified, false);
    assert.match(proof.error, /platform|architecture/i);
  }
  await writeFile(manifestFile, JSON.stringify({ ...original, builtAt: "replacement manifest" }));
  assert.equal((await f.verify()).buildVerified, true, "replacement otherwise describes the same files");
  const final = await native.verifyNativeBuild({ root: f.root, appBinary: f.appBinary, nodeBinary: f.nodeBinary, expectedManifestDigest: initial.manifestDigest });
  assert.equal(final.buildVerified, false);
  assert.match(final.error, /manifest.*changed/i);
});


test("successful redirected Cargo output cannot stamp stale binaries from the previous commit", async t => {
  const f = await fixture(t);
  await f.build();
  const staleNode = await readFile(f.nodeBinary, "utf8");
  await writeFile(path.join(f.root, "source.txt"), "new source with configured target triple\n");
  f.commit();
  const run = async (program, args, options) => {
    if (program === "npm") return f.run(program, args, options);
    const names = args.includes("--example") ? ["native-contact-eval"] : ["mesh-talk", "mesh-talk-node"];
    const records = [];
    for (const name of names) {
      const executable = path.join(f.root, "target/configured-target/release", ...(args.includes("--example") ? ["examples"] : []), `${name}${suffix}`);
      await mkdir(path.dirname(executable), { recursive: true });
      await writeFile(executable, `current output ${name}`);
      records.push(JSON.stringify({ reason: "compiler-artifact", target: { name, kind: [args.includes("--example") ? "example" : "bin"] }, executable }));
    }
    return completed(records.join("\n"));
  };
  await assert.rejects(f.build({ run }), /Cargo.*executable.*target\/release/i);
  assert.equal(await readFile(f.nodeBinary, "utf8"), staleNode, "old default output exists to expose accidental certification");
  assert.equal((await f.verify()).buildVerified, false);
});

test("successful Cargo exit without every requested executable artifact cannot publish proof", async t => {
  const f = await fixture(t);
  await f.build();
  for (const mode of ["no-records", "missing-node", "wrong-target-kind"]) {
    const run = async (program, args, options) => {
      const result = await f.run(program, args, options);
      if (program !== "cargo") return result;
      if (mode === "no-records") return completed("cargo finished successfully");
      const records = result.output.split("\n").map(line => JSON.parse(line));
      return completed(records.filter(record => mode !== "missing-node" || record.target.name !== "mesh-talk-node").map(record => JSON.stringify(mode === "wrong-target-kind" ? { ...record, target: { ...record.target, kind: ["lib"] } } : record)).join("\n"));
    };
    await assert.rejects(f.build({ run }), /Cargo.*artifact/i);
    assert.equal((await f.verify()).buildVerified, false, mode);
  }
});


test("an explicit source mismatch still rejects production build admission", async t => {
  const f = await fixture(t);
  await assert.rejects(f.build({ env: { ...process.env, EVAL_SOURCE_SHA: "0".repeat(40) } }), /source SHA differs from HEAD/);
  assert.equal(f.calls.length, 0, "no build command runs with a mismatched source pin");
});

test("platform-generated Tauri schemas do not dirty source or invalidate native proof", async t => {
  const f = await fixture(t);
  await writeFile(path.join(f.root, ".gitignore"), await readFile(new URL("../../.gitignore", import.meta.url)));
  f.commit();
  const manifest = await f.build({ run: async (...args) => {
    const result = await f.run(...args);
    if (args[0] === "cargo") {
      const directory = path.join(f.root, "src-tauri/gen/schemas");
      await mkdir(directory, { recursive: true });
      for (const filename of ["linux-schema.json", "windows-schema.json", "capabilities.json", "acl-manifests.json"]) {
        await writeFile(path.join(directory, filename), "generated platform schema\n");
      }
    }
    return result;
  } });
  assert.equal(manifest.sourceSha, f.git(["rev-parse", "HEAD"]));
  assert.equal(native.nativeSourceIdentity(f.root).cleanSource, true);
  assert.equal((await f.verify()).buildVerified, true);
});

test("native artifact proof uses complete stdout rather than the combined human log", async t => {
  const f = await fixture(t);
  await f.build({ run: async (...args) => {
    const result = await f.run(...args);
    return args[0] === "cargo" ? { ...result, stderr: "progress-prefix ", output: `progress-prefix ${result.stdout}` } : result;
  } });
  assert.equal((await f.verify()).buildVerified, true);
  for (const mode of ["missing", "truncated", "stderr-only"]) {
    await assert.rejects(f.build({ run: async (...args) => {
      const result = await f.run(...args);
      if (args[0] !== "cargo") return result;
      if (mode === "missing") { delete result.stdout; return result; }
      if (mode === "truncated") return { ...result, outputTruncated: true };
      return { ...result, stdout: "", stderr: result.stdout };
    } }), /stdout|truncat|artifact/i);
    assert.equal((await f.verify()).buildVerified, false, mode);
  }
});
