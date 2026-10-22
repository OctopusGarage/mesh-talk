import { expect, it, vi } from "vitest";
import { createLinkedPreferencePublisher } from "./linkedPreferencePublisher";

it("keeps the latest local edit while an earlier write is in flight", async () => {
  let complete!: (revision: number) => void;
  const write = vi
    .fn()
    .mockImplementationOnce(
      () =>
        new Promise<{ revision: number }>((resolve) => {
          complete = (revision) => resolve({ revision });
        }),
    )
    .mockResolvedValue({ revision: 8 });
  const publisher = createLinkedPreferencePublisher(write, vi.fn());
  publisher.setEnabled(true);
  publisher.publish("theme", "light");
  publisher.publish("theme", "dark");
  expect(write).toHaveBeenCalledTimes(1);
  complete(7);
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(2));
  expect(write).toHaveBeenLastCalledWith("theme", "dark");
  await vi.waitFor(() => expect(publisher.shouldApply("theme", 8)).toBe(false));
  publisher.stop();
});

it("retains failed edits for the next reconciliation and ignores work after stop", async () => {
  const write = vi
    .fn()
    .mockRejectedValueOnce(new Error("offline"))
    .mockResolvedValue({ revision: 4 });
  const onError = vi.fn();
  const publisher = createLinkedPreferencePublisher(write, onError);
  publisher.publish("favorite:a", null);
  publisher.setEnabled(true);
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(1));
  await vi.waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
  await Promise.resolve();
  expect(publisher.shouldApply("favorite:a", 3)).toBe(false);
  publisher.flush();
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(2));
  publisher.stop();
  publisher.publish("favorite:b", "new");
  expect(write).toHaveBeenCalledTimes(2);
});

it("applies an equal-revision preference when its author wins the core tie break", () => {
  const publisher = createLinkedPreferencePublisher(vi.fn(), vi.fn());
  publisher.markApplied("theme", 7, "a");
  expect(publisher.shouldApply("theme", 7, "b")).toBe(true);
  expect(publisher.shouldApply("theme", 7, "a")).toBe(false);
  publisher.markApplied("theme", 7, "b");
  expect(publisher.shouldApply("theme", 7, "a")).toBe(false);
  publisher.stop();
});

it("bounds concurrent preference writes and does not spin on failed keys", async () => {
  const releases: Array<(record: { revision: number }) => void> = [];
  const write = vi.fn(
    () =>
      new Promise<{ revision: number }>((resolve) => releases.push(resolve)),
  );
  const publisher = createLinkedPreferencePublisher(write, vi.fn());
  publisher.setEnabled(true);
  for (let i = 0; i < 12; i++) publisher.publish(`favorite:${i}`, `${i}`);
  expect(write).toHaveBeenCalledTimes(4);
  releases.splice(0).forEach((resolve) => resolve({ revision: 1 }));
  await vi.waitFor(() => expect(write).toHaveBeenCalledTimes(8));
  publisher.stop();
  releases.splice(0).forEach((resolve) => resolve({ revision: 2 }));
  const failure = vi.fn().mockRejectedValue(new Error("disk unavailable"));
  const retry = createLinkedPreferencePublisher(failure, vi.fn());
  retry.setEnabled(true);
  retry.publish("theme", "light");
  await vi.waitFor(() => expect(failure).toHaveBeenCalledTimes(1));
  await Promise.resolve();
  await Promise.resolve();
  expect(failure).toHaveBeenCalledTimes(1);
  retry.stop();
});
