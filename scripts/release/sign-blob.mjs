/** Bounded retries for transient keyless signing failures; verification stays mandatory. */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { renameSync, rmSync, statSync } from "node:fs";
import { setTimeout } from "node:timers/promises";
import { pathToFileURL } from "node:url";

function retryable(error) {
  const details = `${error.message}\n${error.stderr || ""}`;
  return /createLogEntryConflict|\b(?:HTTP[ /]|status(?: code)?[ :=]+|\]\[)(?:429|5\d\d)\b|\b(?:ETIMEDOUT|ECONNRESET|EAI_AGAIN)\b|context deadline exceeded|TLS handshake timeout/i.test(
    details,
  );
}

export async function signBlob({
  artifact,
  bundle,
  run = execFileSync,
  sleep = setTimeout,
  log = console.warn,
}) {
  assert.ok(
    artifact && bundle && artifact !== bundle,
    "distinct artifact and bundle paths required",
  );
  assert.ok(statSync(artifact).size > 0, "nonempty signing artifact required");
  rmSync(bundle, { force: true });
  for (let attempt = 0; attempt < 3; attempt++) {
    const temporary = `${bundle}.attempt-${randomUUID()}`;
    try {
      // A fresh process generates a new ephemeral key after a Rekor conflict.
      // A conflict alone is never success: require a complete new bundle.
      run("cosign", ["sign-blob", "--yes", "--bundle", temporary, artifact], {
        timeout: 180000,
        stdio: ["ignore", "pipe", "pipe"],
      });
      assert.ok(
        statSync(temporary).size > 0,
        "signing produced an empty bundle",
      );
      renameSync(temporary, bundle);
      return;
    } catch (error) {
      if (attempt === 2 || !retryable(error)) throw error;
      const delay = [15000, 45000][attempt];
      log(
        `Transient keyless signing failure; starting attempt ${attempt + 2}/3 in ${delay / 1000}s`,
      );
      await sleep(delay);
    } finally {
      rmSync(temporary, { force: true });
    }
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    await signBlob({ artifact: process.argv[2], bundle: process.argv[3] });
  } catch (error) {
    console.error(`Artifact signing failed: ${error.message}`);
    process.exitCode = 1;
  }
}
