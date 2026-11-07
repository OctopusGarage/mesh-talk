import { rm } from "node:fs/promises";

// A copied executable can remain locked after verified process retirement.
// Bound filesystem cleanup only; persistent locks still reject the fixture.
export function removeCliFixture(path) {
  return rm(path, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
}
