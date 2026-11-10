import assert from "node:assert/strict";

// Fixture-only slicing of named profile tables; this is not a TOML parser.
// Keep unrelated profiles and overrides byte-for-byte when accelerating a test.
function profileRange(config, name) {
  assert.equal(typeof config, "string");
  assert.match(name, /^[a-z][a-z0-9_-]*$/iu);
  const headers = [...config.matchAll(new RegExp(`^\\[profile\\.${name}\\][ \\t]*(?:#[^\\r\\n]*)?\\r?\\n`, "gm"))];
  assert.equal(headers.length, 1, `expected one Nextest profile: ${name}`);
  const start = headers[0].index + headers[0][0].length;
  const next = /^\[/mu.exec(config.slice(start));
  return { start, end: next ? start + next.index : config.length };
}
export function nextestProfileSection(config, name) {
  const { start, end } = profileRange(config, name);
  return config.slice(start, end);
}
export function replaceNextestProfileText(config, name, before, after) {
  assert.ok(typeof before === "string" && before.length > 0);
  assert.equal(typeof after, "string");
  const { start, end } = profileRange(config, name);
  const section = config.slice(start, end);
  assert.equal(section.split(before).length, 2, `expected one replacement in Nextest profile: ${name}`);
  return config.slice(0, start) + section.replace(before, after) + config.slice(end);
}
