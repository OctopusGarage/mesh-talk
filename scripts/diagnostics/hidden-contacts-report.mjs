export const REQUIRED_SCENARIOS = ["hide-cancel", "hide", "restart", "settings-search", "offline-restore", "hidden-inbound", "history-retained", "raw-peer-retained", "local-user-isolation", "narrow-keyboard-layout"];
import { readFile } from "node:fs/promises";
import { join } from "node:path";

export async function validateEvidence(report, root) {
  const errors = validateReport(report);
  for (const name of REQUIRED_SCENARIOS) {
    try {
      const evidence = JSON.parse(await readFile(join(root, `${name}.json`), "utf8"));
      if (evidence.name !== name || !evidence.observations || typeof evidence.observations !== "object") throw new Error("invalid observation JSON");
      const png = await readFile(join(root, `${name}.png`));
      if (png.length <= 100 || !png.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])) || png.toString("ascii", 12, 16) !== "IHDR" || !png.readUInt32BE(16) || !png.readUInt32BE(20) || png.toString("ascii", png.length - 8, png.length - 4) !== "IEND") throw new Error("invalid PNG screenshot");
    } catch (error) {
      errors.push(`Invalid evidence for ${name}: ${error.message}`);
    }
  }
  return errors;
}
export function validateReport(report) {
  const errors = [];
  if (report?.failure || report?.cleanupFailure) errors.push("Native evaluation or process cleanup failed");
  if (report?.schema !== 1 || report?.native !== true || report?.mocked !== false || !["darwin", "linux", "win32"].includes(report?.platform)) errors.push("Expected a versioned native desktop report without mocks");
  for (const name of REQUIRED_SCENARIOS) {
    const result = report?.scenarios?.[name];
    if (result?.passed !== true || !Number.isFinite(result.elapsedMs) || result.elapsedMs < 0 || !Array.isArray(result.evidence) || result.evidence.length !== 2 || result.evidence[0] !== `${name}.json` || result.evidence[1] !== `${name}.png`) errors.push(`Missing successful evidence: ${name}`);
  }
  return errors;
}
