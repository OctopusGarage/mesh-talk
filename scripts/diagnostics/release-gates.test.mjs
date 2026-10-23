import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");

test("native release builds use the Tauri CLI installed from the npm lockfile", () => {
  assert.doesNotMatch(workflow, /cargo install tauri-cli/);
  assert.match(workflow, /npm ci/);
  assert.match(workflow, /node \.\.\/frontend\/node_modules\/@tauri-apps\/cli\/tauri\.js build --target/);
});

test("release verification waits for every platform and checks exact-source signed artifacts", () => {
  assert.match(workflow, /^  verify-release:\n/m);
  const verify = workflow.split("  verify-release:\n")[1]?.split(/^  publish-release:/m)[0];
  assert.match(verify, /needs: \[preflight, build-release\]/);
  assert.match(verify, /actions\/download-artifact@/);
  assert.match(verify, /gh release download/);
  assert.match(verify, /scripts\/release\/verify-release\.sh/);
  assert.match(verify, /\$\{\{ github\.sha \}\}/);
});

test("publication is opt-in, tag-only and requires verified release evidence", () => {
  const publish = workflow.split("  publish-release:\n")[1];
  assert.ok(publish);
  assert.match(publish, /needs: \[preflight, resolve-publication\]/);
  assert.match(publish, /inputs\.publish_release/);
  assert.match(publish, /startsWith\(github\.ref, 'refs\/tags\/'\)/);
  assert.match(publish, /scripts\/release\/check-source-checks\.mjs/);
  assert.match(publish, /scripts\/release\/release-state\.mjs publish/);
  assert.match(publish, /name: verified-release-evidence/);
  assert.match(publish, /run-id: \$\{\{ needs\.resolve-publication\.outputs\.run_id \}\}/);
  assert.match(publish, /github-token: \$\{\{ secrets\.GITHUB_TOKEN \}\}/);
  assert.match(publish, /release-state\.mjs verify/);
  assert.match(publish, /verify-release\.sh draft-assets/);
});

test("publication resolves a verified build and skips compilation", () => {
  for (const job of ["build-release", "verify-release"]) {
    const body = workflow.split(`  ${job}:\n`)[1]?.split(/^  [a-z-]+:/m)[0];
    assert.match(body, /if: .*inputs\.publish_release != true/);
  }
  const resolve = workflow.split("  resolve-publication:\n")[1]?.split(/^  publish-release:/m)[0];
  assert.ok(resolve);
  assert.match(resolve, /scripts\/release\/verified-build\.mjs/);
  assert.match(resolve, /actions: read/);
});

test("post-office images use verified Linux archives and publish before the release", () => {
  const build = workflow.split("  build-release:\n")[1]?.split(/^  verify-release:/m)[0];
  const publish = workflow.split("  publish-release:\n")[1];
  assert.match(build, /name: Build headless post-office node\n\s+run: cargo build/);
  assert.match(build, /scripts\/release\/package-post-office\.sh/);
  assert.match(build, /subject-path: "release_zips\/\*\.zip"/);
  assert.match(publish, /packages: write/);
  assert.match(publish, /name: verified-release-evidence/);
  assert.match(publish, /scripts\/release\/prepare-post-office-container\.sh container-input verification\.txt/);
  const artifactName = build.match(/name: (mesh-talk-.*\$\{\{ matrix\.target \}\})/)?.[1];
  const linuxPattern = publish.match(/pattern: (.+)\n\s+merge-multiple: true\n\s+path: container-input/)?.[1];
  assert.ok(artifactName && linuxPattern, "publication must select the uploaded Linux artifacts");
  const selected = new RegExp(`^${linuxPattern.replaceAll("*", ".*")}$`);
  for (const target of ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]) {
    const uploaded = artifactName.replace("${{ matrix.os }}", "linux").replace("${{ matrix.target }}", target);
    assert.ok(selected.test(uploaded), `container input must include ${uploaded}`);
  }
  assert.equal(selected.test(artifactName.replace("${{ matrix.os }}", "windows").replace("${{ matrix.target }}", "x86_64-pc-windows-msvc")), false);
  assert.match(publish, /platforms: linux\/amd64,linux\/arm64/);
  assert.match(publish, /org\.opencontainers\.image\.source=/);
  assert.ok(publish.indexOf("Publish verified multi-platform post-office image") < publish.indexOf("Publish only the verified draft"));
});

test("release preflight prevents overwriting published versions before building", () => {
  assert.match(workflow, /^  preflight:\n/m);
  assert.match(workflow, /scripts\/release\/preflight\.mjs/);
  assert.match(workflow.split("  build-release:\n")[1], /^    needs: preflight/m);
  assert.match(workflow, /concurrency:/);
});
