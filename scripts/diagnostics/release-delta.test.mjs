import assert from "node:assert/strict";
import test from "node:test";
import { eligibleBases, worthwhile, deltaNames } from "../release/delta.mjs";

test("delta bases are bounded, published, strictly older and same release kind", () => {
  const release = (tag_name, extra = {}) => ({
    tag_name,
    draft: false,
    prerelease: tag_name.includes("nightly"),
    ...extra,
  });
  const releases = [
    release("v0.4.5-nightly.41.1"),
    release("v0.4.5-nightly.43.1"),
    release("v0.4.5"),
    release("v0.4.5-nightly.40.2"),
    release("v0.4.5-nightly.39.1"),
    release("v0.4.5-nightly.42.1", { draft: true }),
  ];
  assert.deepEqual(
    eligibleBases(releases, "v0.4.5-nightly.42.2").map((r) => r.tag_name),
    ["v0.4.5-nightly.41.1", "v0.4.5-nightly.40.2"],
  );
  assert.deepEqual(
    eligibleBases(
      [release("v0.4.4"), release("v0.4.3"), release("v0.4.5")],
      "v0.4.5",
    ).map((r) => r.tag_name),
    ["v0.4.4", "v0.4.3"],
  );
});

test("savings include the complete signed metadata overhead at both boundaries", () => {
  assert.equal(worthwhile(8 * 1024 * 1024, 6 * 1024 * 1024 - 100, 100), true);
  assert.equal(worthwhile(8 * 1024 * 1024, 6 * 1024 * 1024 - 99, 100), false);
  assert.equal(worthwhile(2 * 1024 * 1024, 1024 * 1024 - 99, 100), false);
});

test("delta names remain distinct from canonical updater names", () => {
  assert.deepEqual(
    deltaNames("mesh-talk-update_v0.4.5_macos_arm64.app.tar.gz", "v0.4.4"),
    {
      target: "mesh-talk-update-raw_v0.4.5_macos_arm64.app.tar.gz",
      catalog: "mesh-talk-update_v0.4.5_macos_arm64.app.tar.gz.delta.json",
      patch: "mesh-talk-patch_v0.4.5_macos_arm64_from-v0.4.4.zst",
    },
  );
});

test(
  "producer signs a complete worthwhile set and round-trips through the production Rust decoder",
  { skip: !process.env.TEST_DELTA_HELPER },
  async () => {
    const { execFileSync } = await import("node:child_process");
    const { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } =
      await import("node:fs");
    const { tmpdir } = await import("node:os");
    const { join, resolve } = await import("node:path");
    const { createHash } = await import("node:crypto");
    const { produceDelta } = await import("../release/delta.mjs");
    const root = mkdtempSync(join(tmpdir(), "mesh-delta-production-"));
    const originalFetch = globalThis.fetch;
    const originalPublic = process.env.MESH_TALK_UPDATER_PUBLIC_KEY;
    try {
      const cli = resolve("frontend/node_modules/@tauri-apps/cli/tauri.js");
      const key = join(root, "fixture.key");
      const runCli = (args) =>
        execFileSync(process.execPath, [cli, ...args], {
          stdio: ["ignore", "pipe", "pipe"],
        });
      runCli(["signer", "generate", "--ci", "-p", "", "-w", key]);
      process.env.MESH_TALK_UPDATER_PUBLIC_KEY = readFileSync(
        key + ".pub",
        "utf8",
      );
      const sign = (file, version) =>
        execFileSync(
          process.execPath,
          [cli, "signer", "sign", "--app-version", version, file],
          {
            env: {
              ...process.env,
              TAURI_SIGNING_PRIVATE_KEY: readFileSync(key, "utf8"),
              TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "",
            },
            stdio: ["ignore", "pipe", "pipe"],
          },
        );
      const base = Buffer.alloc(2 * 1024 * 1024);
      let state = 71;
      for (let i = 0; i < base.length; i++) {
        state ^= state << 13;
        state ^= state >>> 17;
        state ^= state << 5;
        base[i] = state & 255;
      }
      const oldExe = join(root, "old.exe");
      const newExe = join(root, "new.exe");
      writeFileSync(oldExe, base);
      const next = Buffer.from(base);
      next.write("new build", 12345);
      writeFileSync(newExe, next);
      const output = join(root, "release");
      mkdirSync(output);
      const oldName = "mesh-talk-update_v0.4.4_macos_arm64.app.tar.gz";
      const full = "mesh-talk-update_v0.4.5_macos_arm64.app.tar.gz";
      const oldFile = join(root, oldName);
      const canonical = join(output, full);
      execFileSync(process.platform === "win32" ? "python" : "python3", [
        "-c",
        "import io,sys,tarfile; from pathlib import Path\nfor source,dest in zip(sys.argv[1::2],sys.argv[2::2]):\n data=Path(source).read_bytes()\n with tarfile.open(dest,'w:gz') as t:\n  m=tarfile.TarInfo('mesh-talk.app/Contents/MacOS/mesh-talk');m.size=len(data);m.mode=0o755;t.addfile(m,io.BytesIO(data))",
        oldExe,
        oldFile,
        newExe,
        canonical,
      ]);
      sign(oldFile, "0.4.4");
      sign(canonical, "0.4.5");
      const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
      const before = sha(readFileSync(canonical));
      const assets = [oldName, oldName + ".sig"].map((name) => {
        const bytes = readFileSync(join(root, name));
        return {
          name,
          size: bytes.length,
          digest: "sha256:" + sha(bytes),
          browser_download_url: `https://github.com/example/project/releases/download/v0.4.4/${name}`,
        };
      });
      let corrupt = false;
      // GitHub returns full asset metadata and release notes. A legitimate
      // bounded 100-release history can exceed the former 4 MiB budget.
      let history = JSON.stringify([
        { tag_name: "v0.4.4", draft: false, prerelease: false, assets },
        ...Array.from({ length: 99 }, (_, index) => ({
          tag_name: `v0.0.${index}`,
          draft: false,
          prerelease: false,
          assets: [],
          body: "Release notes ".repeat(3500),
        })),
      ]);
      assert.ok(Buffer.byteLength(history) > 4 * 1024 * 1024);
      globalThis.fetch = async (url) => {
        if (
          url ===
          "https://api.github.com/repos/example/project/releases?per_page=100"
        )
          return new Response(history);
        const asset = assets.find((a) => a.browser_download_url === url);
        assert.ok(asset, "only exact immutable release URLs may be fetched");
        return new Response(
          corrupt
            ? Buffer.from("tampered")
            : readFileSync(join(root, asset.name)),
        );
      };
      const args = {
        output,
        tag: "v0.4.5",
        target: "aarch64-apple-darwin",
        full,
        sign,
        helper: process.env.TEST_DELTA_HELPER,
        repository: "example/project",
      };
      const emitted = await produceDelta(args);
      assert.equal(emitted.length, 5);
      assert.equal(sha(readFileSync(canonical)), before);
      const catalog = JSON.parse(
        readFileSync(join(output, full + ".delta.json")),
      );
      assert.equal(catalog.bases[0].sha256, sha(base));
      assert.ok(
        worthwhile(
          catalog.fallback.size,
          catalog.bases[0].patch.size,
          readFileSync(join(output, full + ".delta.json")).length +
            readFileSync(join(output, full + ".delta.json.sig")).length +
            readFileSync(join(output, catalog.target.name + ".sig")).length,
        ),
      );
      corrupt = true;
      await assert.rejects(
        produceDelta(args),
        /prior updater (size|digest) mismatch/,
      );
      history = "x".repeat(16 * 1024 * 1024 + 1);
      await assert.rejects(produceDelta(args), /delta download exceeds limit/);
    } finally {
      globalThis.fetch = originalFetch;
      if (originalPublic === undefined)
        delete process.env.MESH_TALK_UPDATER_PUBLIC_KEY;
      else process.env.MESH_TALK_UPDATER_PUBLIC_KEY = originalPublic;
      rmSync(root, { recursive: true, force: true });
    }
  },
);


test("Windows delta bundling restores canonical outputs when system temp is on another volume", async () => {
  const fs = await import("node:fs");
  const child = await import("node:child_process");
  const { syncBuiltinESMExports } = await import("node:module");
  const { tmpdir } = await import("node:os");
  const { join, resolve } = await import("node:path");
  const { rawWindowsTarget } = await import("../release/delta.mjs");
  const root = fs.mkdtempSync(join(tmpdir(), "mesh-delta-volumes-"));
  const cwd = process.cwd();
  const originalRename = fs.default.renameSync;
  const originalExec = child.default.execFileSync;
  try {
    process.chdir(root);
    const release = resolve("target/x86_64-pc-windows-msvc/release");
    const bundle = join(release, "bundle");
    const work = join(root, "external-volume");
    fs.mkdirSync(join(bundle, "nsis"), { recursive: true });
    fs.mkdirSync(work);
    fs.writeFileSync(join(bundle, "nsis/canonical.exe"), "canonical installer");
    fs.writeFileSync(join(release, "mesh-talk.exe"), "__TAURI_BUNDLE_TYPE_VAR_MSI");
    fs.default.renameSync = (source, destination) => {
      if (source.startsWith(work)) throw Object.assign(new Error("cross-device rename"), { code: "EXDEV" });
      return originalRename(source, destination);
    };
    let fail = false;
    child.default.execFileSync = (program, args) => {
      if (args.includes("bundle")) {
        fs.writeFileSync(join(bundle, "nsis/canonical.exe"), "raw installer");
      } else if (fail) {
        throw new Error("native equivalence rejected");
      }
      return Buffer.alloc(0);
    };
    syncBuiltinESMExports();
    const args = { canonical: join(root, "canonical.exe"), destination: join(work, "raw.exe"), target: "x86_64-pc-windows-msvc", work };
    rawWindowsTarget(args);
    assert.equal(fs.readFileSync(args.destination, "utf8"), "raw installer");
    assert.equal(fs.readFileSync(join(bundle, "nsis/canonical.exe"), "utf8"), "canonical installer");
    assert.equal(fs.readFileSync(join(release, "mesh-talk.exe"), "utf8"), "__TAURI_BUNDLE_TYPE_VAR_MSI");
    fail = true;
    assert.throws(() => rawWindowsTarget(args), /native equivalence rejected/);
    assert.equal(fs.readFileSync(join(bundle, "nsis/canonical.exe"), "utf8"), "canonical installer");
    assert.equal(fs.readFileSync(join(release, "mesh-talk.exe"), "utf8"), "__TAURI_BUNDLE_TYPE_VAR_MSI");
    assert.deepEqual(fs.readdirSync(release).sort(), ["bundle", "mesh-talk.exe"]);
  } finally {
    fs.default.renameSync = originalRename;
    child.default.execFileSync = originalExec;
    syncBuiltinESMExports();
    process.chdir(cwd);
    fs.rmSync(root, { recursive: true, force: true });
  }
});
