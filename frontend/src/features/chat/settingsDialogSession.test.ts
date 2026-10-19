import { expect, it, vi } from "vitest";
import { createSettingsDialogSession } from "./settingsDialogSession";

it("drops queued writes from a previous owner without publishing late state", async () => {
  let finishFirst!: () => void;
  const firstWrite = new Promise<string>((resolve) => {
    finishFirst = () => resolve("saved");
  });
  const session = createSettingsDialogSession(vi.fn());
  session.setOwner("alice");
  const apply = vi.fn();
  const firstStarted = vi.fn();
  const secondWrite = vi.fn(async () => "second");

  const first = session.save(
    () => {
      firstStarted();
      return firstWrite;
    },
    apply,
    vi.fn(),
  );
  const second = session.save(secondWrite, apply, vi.fn());
  await vi.waitFor(() => expect(firstStarted).toHaveBeenCalledOnce());
  session.setOwner("bob");
  finishFirst();
  await Promise.all([first, second]);

  expect(secondWrite).not.toHaveBeenCalled();
  expect(apply).not.toHaveBeenCalled();
});

it("finishes a queued write after closing while withholding its stale UI update", async () => {
  const session = createSettingsDialogSession(vi.fn());
  session.setOwner("alice");
  const write = vi.fn(async () => "saved");
  const apply = vi.fn();
  const pending = session.save(write, apply, vi.fn());

  session.invalidateView();
  await pending;

  expect(write).toHaveBeenCalledOnce();
  expect(apply).not.toHaveBeenCalled();
});

it("does not read settings for a superseded dialog load", async () => {
  const session = createSettingsDialogSession(vi.fn());
  session.setOwner("alice");
  const request = session.beginLoad();
  const read = vi.fn(async () => "old settings");
  const apply = vi.fn();
  const pending = session.load(request, read, apply, vi.fn());

  session.invalidateView();
  await pending;

  expect(read).not.toHaveBeenCalled();
  expect(apply).not.toHaveBeenCalled();
});
