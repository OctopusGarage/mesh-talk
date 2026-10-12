import { it, expect, vi } from "vitest";

const { isPermissionGranted, requestPermission } = vi.hoisted(() => ({
  isPermissionGranted: vi.fn(),
  requestPermission: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted,
  requestPermission,
}));

import { ensureNotificationPermission } from "./notify";

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

it("uses existing OS permission without opening a new dialog", async () => {
  vi.resetModules();
  isPermissionGranted.mockReset().mockResolvedValue(true);
  requestPermission.mockReset();
  const { ensureNotificationPermission: ensure } = await import("./notify");

  await ensure();

  expect(isPermissionGranted).toHaveBeenCalledOnce();
  expect(requestPermission).not.toHaveBeenCalled();
});
