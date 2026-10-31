// Actual compiler/coverage qualification; the owning CI installs matching LLVM tools.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

test("ordinary CI uses line tables consistently without changing specialized profiles", () => {
  for (const name of ["ci.yml", "windows-workspace.yml", "e2e-backend.yml"]) {
    const workflow = readFileSync(new URL(`../../.github/workflows/${name}`, import.meta.url), "utf8");
    for (const profile of ["DEV", "TEST"]) assert.match(workflow, new RegExp(`^  CARGO_PROFILE_${profile}_DEBUG: line-tables-only$`, "m"), name);
  }
  for (const name of ["sanitizers.yml", "mutants.yml", "release.yml"]) {
    const workflow = readFileSync(new URL(`../../.github/workflows/${name}`, import.meta.url), "utf8");
    assert.doesNotMatch(workflow, /CARGO_PROFILE_(?:DEV|TEST)_DEBUG/);
  }
});

test("line tables preserve panic locations, runtime checks and full-debug source coverage", { timeout: 120000 }, t => {
  const root = mkdtempSync(path.join(tmpdir(), "mesh-rust-profile-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(path.join(root, "src"));
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname="profile-fixture"\nversion="0.0.0"\nedition="2024"\n[workspace]\n');
  writeFileSync(path.join(root, "src/main.rs"), '#[inline(never)]\nfn diagnostic_probe() { panic!("profile location probe"); }\nfn main() { diagnostic_probe(); }\n');
  writeFileSync(path.join(root, "src/lib.rs"), `
pub fn branch(input: bool) -> u8 {
    if input {
        7
    } else {
        9
    }
}
#[test] fn runtime_checks() {
    assert!(cfg!(debug_assertions));
    assert!(std::panic::catch_unwind(|| { debug_assert!(std::hint::black_box(false)); }).is_err());
    assert!(std::panic::catch_unwind(|| { std::hint::black_box(u8::MAX) + std::hint::black_box(1) }).is_err());
    assert_eq!(branch(true), 7);
}
`);
  const baseEnv = { ...process.env, CARGO_NET_OFFLINE: "true", CARGO_TERM_COLOR: "never", RUST_BACKTRACE: "1" };
  for (const name of Object.keys(baseEnv)) if (name.startsWith("CARGO_LLVM_COV") || name.startsWith("__CARGO_LLVM_COV") || ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "LLVM_PROFILE_FILE", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"].includes(name)) delete baseEnv[name];
  const run = (args, env, status = 0) => {
    const result = spawnSync("cargo", args, { cwd: root, env, encoding: "utf8", timeout: 60000, maxBuffer: 8 * 1024 * 1024 });
    assert.equal(result.status, status, `${args.join(" ")}\n${result.error ?? ""}\n${result.stdout}\n${result.stderr}`);
    return result;
  };
  run(["generate-lockfile", "--offline"], baseEnv);
  const maps = [];
  for (const debug of ["full", "line-tables-only", "none"]) {
    const env = { ...baseEnv, CARGO_PROFILE_DEV_DEBUG: debug, CARGO_PROFILE_TEST_DEBUG: debug, CARGO_TARGET_DIR: path.join(root, `target-${debug}`) };
    const panic = run(["run", "--locked", "--quiet"], env, 101);
    assert.match(panic.stderr, /profile location probe/);
    const stack = panic.stderr.split("stack backtrace:")[1];
    assert.ok(stack, "missing backtrace");
    if (debug === "none") {
      // Negative control: the panic header itself has a location even without symbols.
      assert.doesNotMatch(stack, /src[/\\]main.rs:\d+/, "probe must distinguish stack line tables from panic header locations");
      continue;
    }
    assert.match(stack, /src[/\\]main.rs:\d+/, `${debug} lost the application stack location`);
    const coverageFile = path.join(root, `${debug}.lcov`);
    const result = run(["llvm-cov", "--lib", "--locked", "--lcov", "--output-path", coverageFile], env);
    assert.match(result.stdout, /1 passed; 0 failed/);
    const coverage = readFileSync(coverageFile, "utf8");
    assert.match(coverage, /^FNDA:[1-9]\d*,[^\n]*branch/m);
    const lines = [...coverage.matchAll(/^DA:(\d+),(\d+)/gm)].map(([, line, hits]) => [Number(line), Number(hits) > 0]);
    assert.ok(lines.some(([, hit]) => hit) && lines.some(([, hit]) => !hit), "coverage must distinguish executed and unexecuted lines");
    maps.push(lines);
  }
  assert.deepEqual(maps[1], maps[0], "line tables changed the measured source-line coverage");
});
