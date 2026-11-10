import path from "node:path";

/** Durable Node test lifecycle evidence; Node's test runner retains acceptance. */
export default async function* progress(events) {
  let records = 0;
  const files = new Set();
  const selected = new Set(["test:enqueue", "test:dequeue", "test:start", "test:pass", "test:fail", "test:summary", "test:complete"]);
  for await (const event of events) {
    if (!selected.has(event.type)) continue;
    const data = event.data ?? {};
    const file = data.file && path.resolve(data.file);
    const fileEvent = file && data.name && path.resolve(data.name) === file && !data.entryFile;
    if (event.type === "test:enqueue" && fileEvent) {
      if (files.size >= 1024) throw Error("Node test file selection limit exceeded");
      files.add(file);
    }
    // File-task completion is diagnostic, never process/job retirement authority.
    // Complete events bypass ordered result publication. Admit only exact file
    // tasks already selected by Node, never similarly named child test cases.
    if (event.type === "test:complete" && (!fileEvent || !files.has(file))) continue;
    if (++records > 100000) throw Error("Node test progress record limit exceeded");
    yield JSON.stringify({ time: Date.now(), sourceSha: process.env.GITHUB_SHA ?? null, event: event.type,
      name: String(data.name ?? "").slice(0, 2048), file: String(data.file ?? "").slice(0, 2048),
      nesting: data.nesting ?? null, ...(event.type === "test:summary" ? { success: data.success === true } : {}),
      ...(event.type === "test:complete" ? { passed: data.details?.passed === true } : {}) }) + "\n";
  }
}
