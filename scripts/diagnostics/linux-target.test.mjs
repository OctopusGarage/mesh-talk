import assert from "node:assert/strict";
import test from "node:test";
import { linuxTarget, dockerInvocation, remoteInvocation } from "../pr-platform/linux-target.mjs";

test("local Linux keeps its existing Docker mount and executable", () => {
  const target = linuxTarget({}, "1234-abcd", "/local/evidence");
  assert.equal(target.directory, "/local/evidence");
  assert.equal(target.host, undefined);
  assert.deepEqual(dockerInvocation(target, ["image", "inspect", "example:tag"]), {
    program: "docker", args: ["image", "inspect", "example:tag"],
  });
});

test("remote Linux uses its own evidence path and defaults to native amd64", () => {
  const target = linuxTarget({
    MESH_PR_LINUX_HOST: "mac2015",
    MESH_PR_LINUX_DOCKER: "/Applications/Docker.app/Contents/Resources/bin/docker",
  }, "1234-abcd", "/local/evidence");
  assert.equal(target.directory, "/tmp/mesh-talk-pr-preflight/1234-abcd");
  assert.equal(target.platform, "linux/amd64");
  const command = dockerInvocation(target, ["run", "--platform", target.platform, "image", "bash", "-c", "echo 'literal'; $(false)"]);
  assert.equal(command.program, "ssh");
  assert.deepEqual(command.args.slice(0, 5), ["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "mac2015"]);
  assert.match(command.args[5], /'\/Applications\/Docker.app\/Contents\/Resources\/bin\/docker'/);
  // The complete remote command must preserve every argument through the SSH shell.
  assert.match(command.args[5], /'echo '\\''literal'\\''; \$\(false\)'$/);
});

test("remote configuration rejects unsafe hosts, traversal and unsupported platforms", () => {
  for (const env of [
    { MESH_PR_LINUX_HOST: "-option" },
    { MESH_PR_LINUX_HOST: "host;echo" },
    { MESH_PR_LINUX_HOST: "mac2015", MESH_PR_LINUX_ROOT: "/tmp/../escape" },
    { MESH_PR_LINUX_HOST: "mac2015", MESH_PR_LINUX_ROOT: "/" },
    { MESH_PR_LINUX_HOST: "mac2015", MESH_PR_LINUX_DOCKER: "docker;false" },
    { MESH_PR_LINUX_PLATFORM: "linux/amd64;false" },
  ]) assert.throws(() => linuxTarget(env, "1234-abcd", "/local/evidence"));
  assert.throws(() => linuxTarget({ MESH_PR_LINUX_HOST: "mac2015" }, "../escape", "/local/evidence"));
});

test("remote argv survives quotes and shell metacharacters without execution", { skip: process.platform === "win32" }, async () => {
  const { spawnSync } = await import("node:child_process");
  const values = ["space here", "quote'here", "$(printf injected)", "semi;colon", "line\nbreak"];
  const command = remoteInvocation("mac2015", process.execPath, ["-e", "console.log(JSON.stringify(process.argv.slice(1)))", ...values]);
  const result = spawnSync("/bin/sh", ["-c", command.args.at(-1)], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), values);
});
