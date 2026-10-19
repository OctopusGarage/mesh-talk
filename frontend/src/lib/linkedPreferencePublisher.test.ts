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
