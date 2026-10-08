import { readdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const root = resolve("coverage");
const parts = resolve(root, "e2e-parts");
const unitLcov = await readFile(resolve(root, "lcov.info"), "utf8");
const coverable = new Map();
let current;
for (const row of unitLcov.split("\n")) {
  if (row.startsWith("SF:")) {
    current = row.slice(3);
    coverable.set(current, new Set());
  } else if (row.startsWith("DA:") && current) {
    coverable.get(current).add(Number(row.slice(3).split(",", 1)[0]));
  }
}

const hit = new Map([...coverable.keys()].map((path) => [path, new Set()]));
let reports = 0;
for (const name of await readdir(parts)) {
  if (!name.endsWith(".json")) continue;
  reports++;
  const files = JSON.parse(await readFile(resolve(parts, name), "utf8"));
  for (const [url, lines] of Object.entries(files)) {
    const match = url.match(/\/src\/([^?]+\.tsx?)(?:\?|$)/);
    if (!match) continue;
    const path = `src/${match[1]}`;
    if (!coverable.has(path)) continue;
    for (const line of lines) hit.get(path).add(line);
  }
}
if (reports === 0)
  throw new Error("No browser coverage reports were collected");

const output = [];
for (const [path, lines] of coverable) {
  const measured = new Set([...lines, ...hit.get(path)]);
  output.push("TN:browser", `SF:${path}`);
  for (const line of [...measured].sort((a, b) => a - b)) {
    output.push(`DA:${line},${hit.get(path).has(line) ? 1 : 0}`);
  }
  output.push(
    `LF:${measured.size}`,
    `LH:${hit.get(path).size}`,
    "end_of_record",
  );
}
await writeFile(resolve(root, "e2e.lcov"), `${output.join("\n")}\n`);
const covered = [...hit.values()].reduce((sum, lines) => sum + lines.size, 0);
const total = [...coverable].reduce(
  (sum, [path, lines]) => sum + new Set([...lines, ...hit.get(path)]).size,
  0,
);
console.log(
  `Browser coverage: ${reports} cases, ${covered}/${total} coverable lines`,
);
