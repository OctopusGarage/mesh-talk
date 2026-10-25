/** Optional delta assets. Canonical updater bytes are never replaced. */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  cpSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const native = fileURLToPath(new URL("delta_assets.py", import.meta.url));
const MAX_TARGET = 256 * 1024 * 1024;
const identity = (tag) => {
  const m =
    /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-nightly\.([1-9]\d*)\.([1-9]\d*))?$/.exec(
      tag,
    );
  return m ? m.slice(1).map((v) => (v === undefined ? null : BigInt(v))) : null;
};
function compare(a, b) {
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return a[i] > b[i] ? 1 : -1;
  }
  return 0;
}
export function eligibleBases(releases, tag, limit = 2) {
  const selected = identity(tag);
  assert.ok(selected, "invalid delta release identity");
  return releases
    .filter((r) => {
      const v = identity(r.tag_name);
      return (
        v &&
        r.draft === false &&
        Boolean(v[3]) === Boolean(selected[3]) &&
        r.prerelease === Boolean(v[3]) &&
        compare(v, selected) < 0
      );
    })
    .sort((a, b) => -compare(identity(a.tag_name), identity(b.tag_name)))
    .filter(
      (r, i, all) =>
        all.findIndex((other) => other.tag_name === r.tag_name) === i,
    )
    .slice(0, limit);
}
export function worthwhile(full, patch, overhead) {
  const total = BigInt(patch) + BigInt(overhead);
  return total + 1048576n <= BigInt(full) && total * 4n <= BigInt(full) * 3n;
}
export function deltaNames(full, old) {
  assert.match(
    full,
    /^mesh-talk-update_v[\d.a-z-]+_(?:macos|windows)_(?:arm64|x86_64)\.(?:app\.tar\.gz|nsis\.exe)$/,
  );
  const stem = full
    .replace("mesh-talk-update_", "")
    .replace(/\.(app\.tar\.gz|nsis\.exe)$/, "");
  return {
    target: full.replace("mesh-talk-update_", "mesh-talk-update-raw_"),
    catalog: full + ".delta.json",
    patch: `mesh-talk-patch_${stem}_from-${old}.zst`,
  };
}
const hash = (file) =>
  createHash("sha256").update(readFileSync(file)).digest("hex");
const artifact = (file) => ({
  name: file.split(/[\\/]/).at(-1),
  size: statSync(file).size,
  sha256: hash(file),
});
const run = (program, args, options = {}) =>
  execFileSync(program, args, {
    timeout: 180000,
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  });
const python = () => (process.platform === "win32" ? "python" : "python3");

async function download(url, limit, token = false) {
  const response = await fetch(url, {
    signal: AbortSignal.timeout(120000),
    headers:
      token && process.env.GH_TOKEN
        ? {
            Authorization: `Bearer ${process.env.GH_TOKEN}`,
            Accept: "application/vnd.github+json",
          }
        : {},
  });
  assert.ok(response.ok, `delta source HTTP ${response.status}`);
  assert.ok(
    !response.headers.get("content-length") ||
      Number(response.headers.get("content-length")) <= limit,
    "delta download exceeds limit",
  );
  const chunks = [];
  let size = 0;
  for await (const chunk of response.body) {
    size += chunk.length;
    if (size > limit) {
      throw new Error("delta download exceeds limit");
    }
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}
async function priorAssets(repository, release, full, work) {
  const name = full.replace(/_v[^_]+_/, `_${release.tag_name}_`);
  const result = [];
  for (const suffix of ["", ".sig"]) {
    const matches = release.assets.filter((a) => a.name === name + suffix);
    assert.equal(matches.length, 1, "missing or ambiguous prior updater asset");
    const asset = matches[0];
    const limit = suffix ? 16384 : MAX_TARGET;
    assert.ok(
      Number.isSafeInteger(asset.size) &&
        asset.size > 0 &&
        asset.size <= limit &&
        /^sha256:[a-f0-9]{64}$/.test(asset.digest),
      "prior updater digest/size missing",
    );
    const url = `https://github.com/${repository}/releases/download/${release.tag_name}/${name}${suffix}`;
    assert.equal(
      asset.browser_download_url,
      url,
      "prior updater URL contradicts immutable release",
    );
    const data = await download(url, limit);
    assert.equal(data.length, asset.size, "prior updater size mismatch");
    assert.equal(
      `sha256:${createHash("sha256").update(data).digest("hex")}`,
      asset.digest,
      "prior updater digest mismatch",
    );
    const path = join(work, name + suffix);
    writeFileSync(path, data);
    result.push(path);
  }
  return result;
}

export function rawWindowsTarget({ canonical, destination, target, work }) {
  const release = resolve("target", target, "release");
  const bundle = join(release, "bundle");
  const binary = join(release, "mesh-talk.exe");
  const savedBundle = join(work, "saved-bundle");
  const savedBinary = join(work, "saved-mesh-talk.exe");
  cpSync(bundle, savedBundle, { recursive: true });
  copyFileSync(binary, savedBinary);
  try {
    // Multi-bundle builds can leave the MSI marker in the build executable.
    const executable = readFileSync(binary);
    const markers = ["UNK", "MSI", "NSS"].map((s) =>
      Buffer.from(`__TAURI_BUNDLE_TYPE_VAR_${s}`),
    );
    const offsets = markers.flatMap((marker) => {
      const at = executable.indexOf(marker);
      return at < 0 ? [] : [{ at, marker }];
    });
    assert.equal(offsets.length, 1, "ambiguous Tauri bundle marker");
    markers[2].copy(executable, offsets[0].at);
    writeFileSync(binary, executable);
    run(
      process.execPath,
      [
        resolve("frontend/node_modules/@tauri-apps/cli/tauri.js"),
        "bundle",
        "--target",
        target,
        "--bundles",
        "nsis",
        "--config",
        JSON.stringify({
          bundle: {
            createUpdaterArtifacts: false,
            windows: { nsis: { compression: "none" } },
          },
        }),
      ],
      { cwd: resolve("src-tauri") },
    );
    const files = readdirSync(join(bundle, "nsis")).filter((f) =>
      f.endsWith(".exe"),
    );
    assert.equal(files.length, 1, "expected one raw NSIS installer");
    copyFileSync(join(bundle, "nsis", files[0]), destination);
    run(python(), [native, "equivalent", canonical, destination, "windows"]);
  } finally {
    copyFileSync(savedBinary, binary);
    rmSync(bundle, { recursive: true, force: true });
    renameSync(savedBundle, bundle);
  }
}

export async function produceDelta({
  output,
  tag,
  target,
  full,
  sign,
  helper,
  repository = process.env.GITHUB_REPOSITORY,
}) {
  if (target.includes("linux")) return [];
  assert.match(
    repository || "",
    /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/,
    "delta release repository required",
  );
  const os = target.includes("windows") ? "windows" : "macos";
  const arch = target.startsWith("aarch64") ? "aarch64" : "x86_64";
  const temporary = mkdtempSync(join(tmpdir(), "mesh-delta-"));
  const outputRoot = resolve(output);
  const canonical = join(outputRoot, full);
  const publicKey = join(temporary, "updater.pub");
  writeFileSync(publicKey, process.env.MESH_TALK_UPDATER_PUBLIC_KEY || "");
  try {
    const releases = JSON.parse(
      await download(
        `https://api.github.com/repos/${repository}/releases?per_page=100`,
        4 * 1024 * 1024,
        true,
      ),
    );
    assert.ok(
      Array.isArray(releases) && releases.length <= 100,
      "invalid bounded release list",
    );
    const candidates = eligibleBases(releases, tag, 100)
      .filter(
        (r) =>
          Array.isArray(r.assets) &&
          r.assets.some(
            (a) => a.name === full.replace(/_v[^_]+_/, `_${r.tag_name}_`),
          ),
      )
      .slice(0, 8);
    const authenticated = [];
    for (const release of candidates) {
      const [payload, signature] = await priorAssets(
        repository,
        release,
        full,
        temporary,
      );
      try {
        run(helper, [
          "verify",
          payload,
          signature,
          publicKey,
          release.tag_name.slice(1),
        ]);
      } catch (error) {
        if (error.status === 3) {
          rmSync(payload);
          rmSync(signature);
          console.info(
            `Skipping authenticated legacy delta base without signed version: ${release.tag_name}`,
          );
          continue;
        }
        throw error;
      }
      authenticated.push({ release, payload });
      if (authenticated.length === 2) break;
    }
    if (!authenticated.length) return [];
    const names = deltaNames(full, candidates[0].tag_name);
    const raw = join(temporary, names.target);
    if (os === "macos") run(python(), [native, "mac-target", canonical, raw]);
    else
      rawWindowsTarget({
        canonical,
        destination: raw,
        target,
        work: temporary,
      });
    assert.ok(
      statSync(raw).size > 0 && statSync(raw).size <= MAX_TARGET,
      "raw updater exceeds bounds",
    );
    sign(raw, tag.slice(1));
    run(helper, ["verify", raw, raw + ".sig", publicKey, tag.slice(1)]);
    const catalog = {
      schema: 1,
      role: "mesh-talk-delta",
      algorithm: "zstd-prefix-v1",
      tag,
      release_kind: tag.includes("-nightly.") ? "nightly" : "stable",
      os,
      arch,
      fallback: artifact(canonical),
      target: artifact(raw),
      bases: [],
    };
    for (const { release, payload } of authenticated) {
      const base = join(temporary, release.tag_name + ".base");
      run(python(), [native, "extract-base", payload, os, base]);
      assert.ok(statSync(base).size <= 128 * 1024 * 1024, "base exceeds limit");
      const patch = join(temporary, deltaNames(full, release.tag_name).patch);
      run("zstd", [
        "--patch-from=" + base,
        raw,
        "-19",
        "--long=27",
        "--no-progress",
        "-f",
        "-o",
        patch,
      ]);
      run(helper, [
        "reconstruct",
        base,
        patch,
        join(temporary, release.tag_name + ".roundtrip"),
        String(catalog.target.size),
        catalog.target.sha256,
      ]);
      catalog.bases.push({
        tag: release.tag_name,
        size: statSync(base).size,
        sha256: hash(base),
        patch: artifact(patch),
      });
    }
    // Sign the exact final catalog and measure real signature lengths. Filtering
    // can only shrink the catalog; repeat until every retained offer qualifies.
    const path = join(temporary, names.catalog);
    while (catalog.bases.length) {
      writeFileSync(path, JSON.stringify(catalog));
      assert.ok(statSync(path).size <= 65536, "catalog exceeds limit");
      sign(path, tag.slice(1));
      const overhead =
        statSync(path).size +
        statSync(path + ".sig").size +
        statSync(raw + ".sig").size;
      const retained = catalog.bases.filter((base) =>
        worthwhile(catalog.fallback.size, base.patch.size, overhead),
      );
      if (retained.length === catalog.bases.length) break;
      catalog.bases = retained;
    }
    if (!catalog.bases.length) return [];
    run(helper, ["verify", path, path + ".sig", publicKey, tag.slice(1)]);
    const emitted = [
      names.target,
      names.target + ".sig",
      names.catalog,
      names.catalog + ".sig",
      ...catalog.bases.map((b) => b.patch.name),
    ];
    for (const name of emitted)
      copyFileSync(join(temporary, name), join(outputRoot, name));
    return emitted;
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}
