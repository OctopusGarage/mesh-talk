import { afterEach, expect, it, vi } from "vitest";
import { readWithDeadline } from "./readDeadline";
afterEach(() => vi.useRealTimers());
it("expires an unresolved read and allows a later healthy snapshot", async () => {
  vi.useFakeTimers();
  let finish!: (value: string) => void;
  const pending = readWithDeadline(
    () =>
      new Promise<string>((resolve) => {
        finish = resolve;
      }),
    100,
  );
  const rejection = expect(pending).rejects.toThrow("timed out");
  await vi.advanceTimersByTimeAsync(100);
  await rejection;
  expect(await readWithDeadline(async () => "fresh")).toBe("fresh");
  finish("obsolete");
});
it("bounds abandoned native requests while preserving successful read capacity", async () => {
  const releases: Array<() => void> = [];
  const pending = Array.from({ length: 64 }, () =>
    readWithDeadline(
      () => new Promise<void>((resolve) => releases.push(resolve)),
    ),
  );
  const rejected = vi.fn(async () => "never");
  await expect(readWithDeadline(rejected)).rejects.toThrow("capacity");
  expect(rejected).not.toHaveBeenCalled();
  releases.forEach((resolve) => resolve());
  await Promise.all(pending);
  expect(await readWithDeadline(async () => "recovered")).toBe("recovered");
});
