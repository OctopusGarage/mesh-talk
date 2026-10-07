import { execFileSync } from "node:child_process";
import test from "node:test";

test("release asset verifier accepts valid fixtures and rejects incomplete or corrupt releases", () => {
  execFileSync(process.platform === "win32" ? "python" : "python3", ["-B", "scripts/release/test_verify_assets.py"], {
    cwd: new URL("../../", import.meta.url),
    stdio: "pipe",
  });
});
