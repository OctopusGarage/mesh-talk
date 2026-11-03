import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  perlProbe,
  prepareWindowsNativeDependencies,
  selectNativePerl,
} from "../ci/windows-native-deps.mjs";

const native = "C:\\Strawberry\\perl\\bin\\perl.exe";
const msys = "C:\\Program Files\\Git\\usr\\bin\\perl.exe";
const success = { status: 0, stdout: "mesh-talk-native-perl-ok\n", stderr: "" };

test("Git/MSYS Perl ahead of native Perl cannot be exported to the MSVC build", () => {
  const calls = [];
  const env = {
    Path: "C:\\Program Files\\Git\\usr\\bin;C:\\Strawberry\\perl\\bin",
  };
  const perl = selectNativePerl({
    env,
    exists: () => true,
    run: (program, args, options) => {
      calls.push(program);
      assert.deepEqual(args, ["-e", perlProbe]);
      assert.equal(options.shell, false);
      assert.equal(options.env, env);
      return program === msys
        ? { status: 2, stderr: "Can't locate Locale/Maketext/Simple.pm" }
        : success;
    },
  });
  assert.equal(perl, native);
  assert.deepEqual(calls, [msys, native]);
  assert.match(perlProbe, /use IPC::Cmd; use Locale::Maketext::Simple/u);
  assert.match(perlProbe, /\$\^O eq "MSWin32"/u);
});

test("explicit native Perl is validated and exported without changing vendoring or PATH", () => {
  const env = {
    GITHUB_ENV: "runner-env",
    OPENSSL_SRC_PERL: "D:\\Native Tools\\perl.exe",
    PATH: "unchanged",
  };
  const before = { ...env };
  const writes = [];
  assert.equal(
    prepareWindowsNativeDependencies({
      platform: "win32",
      env,
      exists: () => true,
      run: (program) => {
        assert.equal(program, env.OPENSSL_SRC_PERL);
        return success;
      },
      append: (...args) => writes.push(args),
    }),
    env.OPENSSL_SRC_PERL,
  );
  assert.deepEqual(writes, [
    [
      "runner-env",
      `OPENSSL_SRC_PERL=${env.OPENSSL_SRC_PERL}\nPERL=${env.OPENSSL_SRC_PERL}\n`,
      "utf8",
    ],
  ]);
  assert.deepEqual(env, before);
});

test("missing, broken, nonnative and timed-out interpreters fail before environment publication", () => {
  for (const result of [
    { status: 2, stderr: "Can't locate Locale/Maketext/Simple.pm" },
    { status: 0, stdout: "msys" },
    { status: null, error: Error("timed out") },
  ]) {
    let written = false;
    assert.throws(
      () =>
        prepareWindowsNativeDependencies({
          platform: "win32",
          env: { GITHUB_ENV: "runner-env", OPENSSL_SRC_PERL: msys },
          exists: () => true,
          run: () => result,
          append: () => {
            written = true;
          },
        }),
      /Native Windows Perl/u,
    );
    assert.equal(written, false);
  }
  assert.throws(
    () => selectNativePerl({ env: {}, exists: () => false }),
    /Native Windows Perl/u,
  );
  for (const explicit of ["perl", "C:\\perl.exe\nEXTRA=value"]) {
    assert.throws(
      () => selectNativePerl({ env: { OPENSSL_SRC_PERL: explicit } }),
      /absolute Windows/u,
    );
  }
  assert.throws(
    () => prepareWindowsNativeDependencies({ platform: "linux" }),
    /requires Windows/u,
  );
  assert.throws(
    () => prepareWindowsNativeDependencies({ platform: "win32", env: {} }),
    /GITHUB_ENV/u,
  );
});

test("the runner image native installation is checked even when Bash omits it from PATH", () => {
  const found = selectNativePerl({
    env: { PATH: "C:\\Program Files\\Git\\usr\\bin" },
    exists: (filename) => filename === native,
    run: () => success,
  });
  assert.equal(found, native);
});

test(
  "the actual Windows runner provides a complete native Perl installation",
  {
    skip:
      process.platform === "win32"
        ? false
        : "Requires the native Windows interpreter",
  },
  () => {
    assert.match(selectNativePerl(), /[\\/]perl\.exe$/iu);
  },
);

test("Windows producer validates native Perl before every Cargo build boundary", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/windows-workspace.yml", import.meta.url),
    "utf8",
  );
  const producer = workflow.split("\n  tests:")[0];
  const bootstrap = producer.indexOf("node scripts/ci/windows-native-deps.mjs");
  assert.ok(
    bootstrap >= 0,
    "vendored OpenSSL must not inherit bare Git/MSYS perl",
  );
  for (const boundary of [
    "scripts/ci/backend-coverage.test.mjs",
    "cargo clippy",
    "cargo build",
    "cargo test",
    "cargo llvm-cov show-env",
  ]) {
    assert.ok(bootstrap < producer.indexOf(boundary), boundary);
  }
  assert.doesNotMatch(
    producer,
    /OPENSSL_NO_VENDOR:\s*['"]?1|continue-on-error:\s*true/u,
  );
});
