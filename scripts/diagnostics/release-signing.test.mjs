import assert from "node:assert/strict";
import test from "node:test";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Import dynamically so a missing owner is an explicit regression failure.
const sign = async (options) =>
  (await import("../release/sign-blob.mjs")).signBlob(options);
function fixture() {
  const root = mkdtempSync(join(tmpdir(), "mesh-signing-"));
  const artifact = join(root, "installer with spaces.zip");
  const bundle = artifact + ".bundle";
  writeFileSync(artifact, "immutable archive");
  return { root, artifact, bundle };
}
const transient = () =>
  Object.assign(new Error("signing failed"), {
    stderr: Buffer.from(
      '[POST /api/v1/log/entries][409] createLogEntryConflict {"code":409}',
    ),
  });

test("a Rekor conflict starts a fresh signing process and promotes only its complete bundle", async () => {
  const f = fixture();
  const paths = [],
    delays = [];
  try {
    writeFileSync(f.bundle, "stale bundle");
    await sign({
      ...f,
      sleep: async (ms) => delays.push(ms),
      log: () => {},
      run: (program, args) => {
        assert.equal(program, "cosign");
        assert.deepEqual(args.slice(0, 3), ["sign-blob", "--yes", "--bundle"]);
        assert.equal(args.at(-1), f.artifact);
        paths.push(args[3]);
        if (paths.length === 1) {
          writeFileSync(args[3], "partial");
          throw transient();
        }
        writeFileSync(args[3], "complete verified-later bundle");
      },
    });
    assert.equal(paths.length, 2);
    assert.notEqual(paths[0], paths[1]);
    assert.equal(
      readFileSync(f.bundle, "utf8"),
      "complete verified-later bundle",
    );
    assert.equal(readFileSync(f.artifact, "utf8"), "immutable archive");
    assert.deepEqual(delays, [15000]);
    assert.deepEqual(readdirSync(f.root).sort(), [
      "installer with spaces.zip",
      "installer with spaces.zip.bundle",
    ]);
  } finally {
    rmSync(f.root, { recursive: true, force: true });
  }
});

for (const message of [
  "HTTP 429 Too Many Requests",
  "HTTP 503 Service Unavailable",
  "context deadline exceeded",
  "read ECONNRESET",
]) {
  test(`retries transient service failure: ${message}`, async () => {
    const f = fixture();
    let attempts = 0;
    try {
      await sign({
        ...f,
        sleep: async () => {},
        log: () => {},
        run: (_program, args) => {
          if (++attempts === 1) throw new Error(message);
          writeFileSync(args[3], "complete");
        },
      });
      assert.equal(attempts, 2);
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });
}

test("persistent transient failure stops after three attempts without leaving a publishable bundle", async () => {
  const f = fixture();
  let attempts = 0;
  const delays = [];
  try {
    await assert.rejects(
      sign({
        ...f,
        sleep: async (ms) => delays.push(ms),
        log: () => {},
        run: (_program, args) => {
          attempts++;
          writeFileSync(args[3], "partial");
          throw transient();
        },
      }),
      /signing failed/,
    );
    assert.equal(attempts, 3);
    assert.deepEqual(delays, [15000, 45000]);
    assert.equal(existsSync(f.bundle), false);
    assert.deepEqual(readdirSync(f.root), ["installer with spaces.zip"]);
  } finally {
    rmSync(f.root, { recursive: true, force: true });
  }
});

for (const message of [
  "HTTP 403 permission denied",
  "invalid identity token",
  "certificate verification failed",
  "command not found",
]) {
  test(`permanent signing error fails immediately: ${message}`, async () => {
    const f = fixture();
    let attempts = 0;
    try {
      await assert.rejects(
        sign({
          ...f,
          sleep: async () => assert.fail("must not wait"),
          run: () => {
            attempts++;
            throw new Error(message);
          },
        }),
        new RegExp(message),
      );
      assert.equal(attempts, 1);
      assert.equal(existsSync(f.bundle), false);
    } finally {
      rmSync(f.root, { recursive: true, force: true });
    }
  });
}

test("a successful process without a nonempty bundle is still a signing failure", async () => {
  const f = fixture();
  try {
    await assert.rejects(sign({ ...f, run: () => {} }), /bundle/);
    assert.equal(existsSync(f.bundle), false);
  } finally {
    rmSync(f.root, { recursive: true, force: true });
  }
});
