/** Observe IPC transport without replacing Tauri's immutable invoke function. */
export function installNetworkTrace(target) {
  target.nativeNetworkCalls = [];
  target.nativeNetworkPhase = "natural";
  const original = target.fetch;
  const observed = function(input, options) {
    const url = new URL(typeof input === "string" ? input : input.url, target.location.href);
    if (url.hostname !== "ipc.localhost" || url.pathname !== "/network_name") {
      return original.call(target, input, options);
    }
    const entry = { phase: target.nativeNetworkPhase, start: Date.now() };
    target.nativeNetworkCalls.push(entry);
    return original.call(target, input, options).then(response => {
      entry.end = Date.now();
      entry.ok = response.headers.get("Tauri-Response") === "ok";
      return response; // No cloning/consuming the body; the real IPC handles its unchanged response.
    }, error => {
      entry.end = Date.now(); entry.ok = false; throw error;
    });
  };
  target.fetch = observed;
  if (target.fetch !== observed) throw new Error("IPC fetch observer could not be installed");
  return true;
}
