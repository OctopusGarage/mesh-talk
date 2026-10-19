/** Optional SSH Docker target. Source/test selectors never become shell code. */
export function linuxTarget(env, runId, localDirectory) {
  const host = env.MESH_PR_LINUX_HOST || undefined;
  if (host && !/^[\w][\w.@-]*$/u.test(host)) throw Error("Unsafe Linux SSH host; use a configured SSH alias");
  const platform = env.MESH_PR_LINUX_PLATFORM || (host ? "linux/amd64" : undefined);
  if (platform && !["linux/amd64", "linux/arm64"].includes(platform)) throw Error("Linux platform must be linux/amd64 or linux/arm64");
  if (!host) return { host, directory: localDirectory, docker: "docker", platform };
  const root = (env.MESH_PR_LINUX_ROOT || "/tmp/mesh-talk-pr-preflight").replace(/\/$/u, "");
  if (!/^\/[\w./-]+$/u.test(root) || root.split("/").some(part => part === ".." || part === ".")) throw Error("Linux remote root must be a safe absolute path");
  if (!/^[a-zA-Z0-9-]+$/u.test(runId)) throw Error("Invalid Linux run ID");
  const docker = env.MESH_PR_LINUX_DOCKER || "/Applications/Docker.app/Contents/Resources/bin/docker";
  if (docker !== "docker" && !/^\/[\w./ -]+$/u.test(docker)) throw Error("Linux Docker executable must be docker or an absolute path");
  return { host, directory: `${root}/${runId}`, docker, platform };
}

const quote = value => `'${String(value).replaceAll("'", "'\\''")}'`;
export function remoteInvocation(host, program, args) {
  return { program: "ssh", args: ["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", host, [program, ...args].map(quote).join(" ")] };
}
export function dockerInvocation(target, args) {
  if (!target.host) return { program: "docker", args };
  // GUI-installed Docker and its credential helpers are absent from SSH's PATH.
  const directory = target.docker.includes("/") ? target.docker.slice(0, target.docker.lastIndexOf("/")) : "/usr/local/bin";
  return remoteInvocation(target.host, "env", [`PATH=${directory}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`, target.docker, ...args]);
}
