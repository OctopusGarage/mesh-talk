/** Durable Node test lifecycle evidence; Node's test runner retains acceptance. */
export default async function* progress(events) {
  let records = 0;
  const selected = new Set(["test:enqueue", "test:dequeue", "test:start", "test:pass", "test:fail"]);
  for await (const event of events) {
    if (!selected.has(event.type)) continue;
    if (++records > 100000) throw Error("Node test progress record limit exceeded");
    const data = event.data ?? {};
    yield JSON.stringify({ time: Date.now(), sourceSha: process.env.GITHUB_SHA ?? null, event: event.type,
      name: String(data.name ?? "").slice(0, 2048), file: String(data.file ?? "").slice(0, 2048),
      nesting: data.nesting ?? null }) + "\n";
  }
}
