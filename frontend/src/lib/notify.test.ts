import { describe, it, expect, vi } from "vitest";

const { isPermissionGranted, requestPermission, sendNotification } = vi.hoisted(
  () => ({
    isPermissionGranted: vi.fn(),
    requestPermission: vi.fn(),
    sendNotification: vi.fn(),
  }),
);
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted,
  requestPermission,
  sendNotification,
}));

import { ensureNotificationPermission, shouldNotify } from "./notify";

it("shares a pending notification permission request across startup calls", async () => {
  let resolveCheck!: (result: boolean) => void;
  const pendingCheck = new Promise<boolean>((resolve) => {
    resolveCheck = resolve;
  });
  isPermissionGranted.mockReturnValue(pendingCheck);
  requestPermission.mockResolvedValue("granted");

  const first = ensureNotificationPermission();
  await vi.waitFor(() => expect(isPermissionGranted).toHaveBeenCalledOnce());
  const second = ensureNotificationPermission();
  await new Promise((resolve) => setTimeout(resolve, 0));
  const checksBeforeResolution = isPermissionGranted.mock.calls.length;
  resolveCheck(false);
  await Promise.all([first, second]);

  expect(checksBeforeResolution).toBe(1);
  expect(requestPermission).toHaveBeenCalledTimes(1);
  await ensureNotificationPermission();
  expect(isPermissionGranted).toHaveBeenCalledTimes(1);
});

it("waits for an active permission dialog before deciding whether a new event may notify", async () => {
  vi.resetModules();
  isPermissionGranted.mockReset().mockResolvedValue(false);
  sendNotification.mockReset();
  let finishRequest!: (result: string) => void;
  requestPermission.mockReset().mockImplementation(
    () =>
      new Promise<string>((resolve) => {
        finishRequest = resolve;
      }),
  );
  const { ensureNotificationPermission: ensure, notifyInbound } =
    await import("./notify");
  const first = ensure();
  await vi.waitFor(() => expect(requestPermission).toHaveBeenCalledOnce());
  let secondFinished = false;
  const second = notifyInbound("New message", "Hello", false).then(() => {
    secondFinished = true;
  });
  await new Promise((resolve) => setTimeout(resolve, 0));
  const finishedBeforeDecision = secondFinished;
  finishRequest("granted");
  await Promise.all([first, second]);

  expect(finishedBeforeDecision).toBe(false);
  expect(requestPermission).toHaveBeenCalledTimes(1);
  expect(sendNotification).toHaveBeenCalledWith({
    title: "New message",
    body: "Hello",
  });
});

it("uses existing OS permission without opening a new dialog", async () => {
  vi.resetModules();
  isPermissionGranted.mockReset().mockResolvedValue(true);
  requestPermission.mockReset();
  const { ensureNotificationPermission: ensure } = await import("./notify");

  await ensure();

  expect(isPermissionGranted).toHaveBeenCalledOnce();
  expect(requestPermission).not.toHaveBeenCalled();
});

it("uses a useful fallback body for an empty inbound notification", async () => {
  vi.resetModules();
  isPermissionGranted.mockReset().mockResolvedValue(true);
  sendNotification.mockReset();
  const { notifyInbound } = await import("./notify");

  await notifyInbound("Alice", "", false);

  expect(sendNotification).toHaveBeenCalledWith({
    title: "Alice",
    body: "New message",
  });
});

describe("shouldNotify", () => {
  it("suppresses when focused on the active conversation", () => {
    expect(
      shouldNotify({ windowFocused: true, isActiveConversation: true }),
    ).toBe(false);
  });
  it("notifies when the window is unfocused", () => {
    expect(
      shouldNotify({ windowFocused: false, isActiveConversation: true }),
    ).toBe(true);
  });
  it("notifies when focused but a different conversation is open", () => {
    expect(
      shouldNotify({ windowFocused: true, isActiveConversation: false }),
    ).toBe(true);
  });
});
