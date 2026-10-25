import { beforeEach, expect, it, vi } from "vitest";
const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
import { recovery } from "./recovery";
import { googleDrive } from "./googleDrive";
let requests: { command: string; args: Record<string, unknown> }[];
beforeEach(() => {
  requests = [];
  native.invoke.mockReset().mockImplementation(async (command, args) => {
    // Tauri transports JSON: undefined optional keys must disappear on the wire.
    requests.push({ command, args: JSON.parse(JSON.stringify(args ?? {})) });
    return command === "google_drive_resume";
  });
});
it("uses the confirmed native key for both destinations while preserving explicit-key callers", async () => {
  await recovery.setup();
  await recovery.confirmKey("saved-key");
  await recovery.setDestination("local");
  await recovery.create("/tmp/local.mtbackup");
  await googleDrive.create("drive-view");
  expect(requests).toEqual([
    { command: "backup_setup", args: {} },
    { command: "confirm_backup_key", args: { recoveryKey: "saved-key" } },
    { command: "set_backup_destination", args: { destination: "local" } },
    { command: "create_backup", args: { destination: "/tmp/local.mtbackup" } },
    {
      command: "google_drive_create_backup",
      args: { connectionId: "drive-view" },
    },
  ]);
  await recovery.create("/tmp/legacy.mtbackup", "explicit-key");
  await googleDrive.create("legacy-view", "explicit-key");
  expect(requests.slice(-2)).toEqual([
    {
      command: "create_backup",
      args: {
        destination: "/tmp/legacy.mtbackup",
        recoveryKey: "explicit-key",
      },
    },
    {
      command: "google_drive_create_backup",
      args: { connectionId: "legacy-view", recoveryKey: "explicit-key" },
    },
  ]);
});
it("separates resuming a remembered grant, retiring a view and explicitly forgetting authorization", async () => {
  expect(await googleDrive.resume("current-view")).toBe(true);
  await googleDrive.disconnect("current-view");
  await googleDrive.forget("new-view");
  expect(requests).toEqual([
    { command: "google_drive_resume", args: { connectionId: "current-view" } },
    {
      command: "google_drive_disconnect",
      args: { connectionId: "current-view" },
    },
    { command: "google_drive_forget", args: { connectionId: "new-view" } },
  ]);
});
it("propagates native setup and remembered-grant failures to their UI retry owner", async () => {
  const failure = new Error("native persistence unavailable");
  native.invoke.mockRejectedValue(failure);
  await expect(recovery.confirmKey("key")).rejects.toBe(failure);
  await expect(recovery.setDestination("drive")).rejects.toBe(failure);
  await expect(googleDrive.resume("view")).rejects.toBe(failure);
  await expect(googleDrive.forget("view")).rejects.toBe(failure);
});
