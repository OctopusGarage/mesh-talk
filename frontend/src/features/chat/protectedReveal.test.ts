import { afterEach, expect, it, vi } from "vitest";
import { createProtectedReveal } from "./protectedReveal";
afterEach(() => vi.useRealTimers());
function setup(authenticate = false) {
  vi.useFakeTimers();
  let focused = true;
  let current = true;
  const changed = vi.fn();
  const reveal = vi.fn(async () => ({
    text: "private",
    visible_for_ms: 30000,
  }));
  const controller = createProtectedReveal({
    nativeAuthentication: authenticate,
    reveal,
    changed,
    foreground: () => focused,
    current: () => current,
  });
  return {
    controller,
    changed,
    reveal,
    focus: (value: boolean) => {
      focused = value;
    },
    retire: () => {
      current = false;
    },
  };
}
it("expires after thirty seconds and authenticates again on each reveal", async () => {
  const s = setup(true);
  await s.controller.reveal();
  expect(s.changed).toHaveBeenLastCalledWith(
    expect.objectContaining({ text: "private", remaining: 30 }),
  );
  vi.advanceTimersByTime(30000);
  expect(s.changed).toHaveBeenLastCalledWith(
    expect.objectContaining({ text: null }),
  );
  await s.controller.reveal();
  expect(s.reveal).toHaveBeenCalledTimes(2);
  s.controller.hide();
});
it("rejects late replies after cancellation", async () => {
  const s = setup();
  let resolve!: (value: { text: string; visible_for_ms: number }) => void;
  s.reveal.mockImplementationOnce(
    () =>
      new Promise((yes) => {
        resolve = yes;
      }),
  );
  const request = s.controller.reveal();
  s.controller.hide();
  resolve({ text: "late", visible_for_ms: 30000 });
  await request;
  expect(s.changed.mock.calls.some(([state]) => state.text === "late")).toBe(
    false,
  );
});
it("waits for foreground after the native authentication prompt and hides on subsequent blur", async () => {
  const s = setup(true);
  const request = s.controller.reveal();
  s.focus(false);
  s.controller.blur();
  await request;
  expect(s.changed).toHaveBeenLastCalledWith(
    expect.objectContaining({ text: null }),
  );
  s.focus(true);
  s.controller.focus();
  expect(s.changed).toHaveBeenLastCalledWith(
    expect.objectContaining({ text: "private" }),
  );
  s.focus(false);
  s.controller.blur();
  s.focus(true);
  s.controller.focus();
  expect(s.changed).toHaveBeenLastCalledWith(
    expect.objectContaining({ text: null }),
  );
});
it("rejects a reply after owner or conversation retirement", async () => {
  const s = setup(true);
  const request = s.controller.reveal();
  s.retire();
  await request;
  expect(s.changed.mock.calls.some(([state]) => state.text !== null)).toBe(
    false,
  );
});
it("starts a full visible lifetime when focus returns after native authentication", async () => {
  const s = setup(true);
  s.focus(false);
  await s.controller.reveal();
  vi.advanceTimersByTime(5000);
  s.focus(true);
  s.controller.focus();
  expect(s.changed).toHaveBeenLastCalledWith(
    expect.objectContaining({ text: "private", remaining: 30 }),
  );
  s.controller.hide();
});
it("does not extend visibility when the wall clock rolls back", async () => {
  const s = setup();
  await s.controller.reveal();
  vi.setSystemTime(Date.now() - 60000);
  vi.advanceTimersByTime(30000);
  expect(s.changed).toHaveBeenLastCalledWith(
    expect.objectContaining({ text: null }),
  );
});
it("hides on focus after a suspended device advances wall time", async () => {
  const s = setup();
  await s.controller.reveal();
  vi.setSystemTime(Date.now() + 60000);
  s.controller.focus();
  expect(s.changed).toHaveBeenLastCalledWith(
    expect.objectContaining({ text: null }),
  );
});
it("invalidates inline password verification on blur and rejects its late response", async () => {
  const s = setup(false);
  let resolve!: (value: { text: string; visible_for_ms: number }) => void;
  s.reveal.mockImplementationOnce(
    () =>
      new Promise((yes) => {
        resolve = yes;
      }),
  );
  const pending = s.controller.reveal();
  s.focus(false);
  s.controller.blur();
  resolve({ text: "password result", visible_for_ms: 30000 });
  await pending;
  s.focus(true);
  s.controller.focus();
  expect(s.changed.mock.calls.some(([state]) => state.text !== null)).toBe(
    false,
  );
});

it("disposes local image URLs on hide and late reveal responses", async () => {
  vi.useFakeTimers();
  const dispose = vi.fn();
  let resolve!: (result: { text: string; visible_for_ms: number }) => void;
  const reveal = vi
    .fn()
    .mockResolvedValueOnce({ text: "blob:shown", visible_for_ms: 30000 })
    .mockImplementationOnce(
      () =>
        new Promise((yes) => {
          resolve = yes;
        }),
    );
  const controller = createProtectedReveal({
    reveal,
    nativeAuthentication: false,
    current: () => true,
    foreground: () => true,
    changed: () => {},
    disposeBody: dispose,
  });
  await controller.reveal();
  controller.hide();
  expect(dispose).toHaveBeenCalledWith("blob:shown");
  const pending = controller.reveal();
  controller.hide();
  resolve({ text: "blob:late", visible_for_ms: 30000 });
  await pending;
  expect(dispose).toHaveBeenCalledWith("blob:late");
});
