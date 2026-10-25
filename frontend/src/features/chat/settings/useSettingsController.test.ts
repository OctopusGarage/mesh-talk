// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { AppSettings } from "@/lib/types";
const mocks = vi.hoisted(() => ({
  get: vi.fn(),
  update: vi.fn(),
  autostartEnabled: vi.fn(),
  setAutostart: vi.fn(),
  setPrivacy: vi.fn(),
  setCallsEnabled: vi.fn(),
  setNotificationsEnabled: vi.fn(),
  setRingtone: vi.fn(),
  auth: { user: { id: "alice" } as { id: string } | null, generation: 0 },
}));
vi.mock("@/lib/api", () => ({ settings: mocks }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@/store/auth", () => ({
  useAuth: (select: (state: typeof mocks.auth) => unknown) =>
    select(mocks.auth),
}));
vi.mock("@/store/settings", () => ({ useSettings: { getState: () => mocks } }));
import {
  useSettingsController,
  type SettingsController,
} from "./useSettingsController";
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
const initial: AppSettings = {
  minimize_to_tray: true,
  notifications: true,
  download_dir: "",
  stay_signed_in: false,
  last_user: null,
  retention_days: 0,
  calls_enabled: false,
  post_office_enabled: false,
  post_office_send_enabled: true,
  post_office_receive_enabled: true,
  physical_lan_only: false,
  bridge_discovery_enabled: false,
  origin_sharing_enabled: false,
  bridge_hub_id: "",
  ringtone: "classic",
  privacy_level: "balanced",
  notification_preview: true,
  auto_lock_seconds: 0,
  lock_when_hidden: false,
  confirm_sensitive_copy: false,
  clipboard_clear_seconds: 0,
};
let root: Root;
let controller: SettingsController;
function Probe({ open }: { open: boolean }) {
  controller = useSettingsController(open);
  return null;
}
async function render(open = true) {
  await act(async () => root.render(createElement(Probe, { open })));
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  mocks.auth = { user: { id: "alice" }, generation: 0 };
  mocks.get.mockResolvedValue({ ...initial });
  mocks.autostartEnabled.mockResolvedValue(false);
  mocks.update.mockResolvedValue(undefined);
  mocks.setAutostart.mockResolvedValue(undefined);
  root = createRoot(document.createElement("div"));
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
it("keeps acknowledged settings until the write and canonical readback finish", async () => {
  await render();
  const write = deferred<void>();
  const readback = deferred<AppSettings>();
  mocks.update.mockReturnValueOnce(write.promise);
  mocks.get.mockReturnValueOnce(readback.promise);
  mocks.setPrivacy.mockClear();
  let saving!: Promise<boolean>;
  await act(async () => {
    saving = controller.update({ privacy_level: "strict" }, "privacy");
  });
  expect(controller.saving).toBe(true);
  expect(controller.settings).toEqual(initial);
  expect(mocks.get).toHaveBeenCalledTimes(1);
  expect(mocks.setPrivacy).not.toHaveBeenCalled();
  await act(async () => write.resolve());
  expect(controller.settings).toEqual(initial);
  expect(controller.savedCategory).toBeNull();
  const canonical: AppSettings = {
    ...initial,
    notifications: false,
    privacy_level: "strict",
    notification_preview: false,
    auto_lock_seconds: 60,
    lock_when_hidden: true,
    confirm_sensitive_copy: true,
    clipboard_clear_seconds: 30,
    calls_enabled: true,
    ringtone: "digital",
    retention_days: 7,
  };
  await act(async () => {
    readback.resolve(canonical);
    await saving;
  });
  expect(controller.settings).toEqual(canonical);
  expect(mocks.setNotificationsEnabled).toHaveBeenLastCalledWith(false);
  expect(controller.savedCategory).toBe("privacy");
  expect(controller.saving).toBe(false);
  expect(mocks.setPrivacy).toHaveBeenCalledWith({
    privacy_level: "strict",
    notification_preview: false,
    auto_lock_seconds: 60,
    lock_when_hidden: true,
    confirm_sensitive_copy: true,
    clipboard_clear_seconds: 30,
  });
  expect(mocks.setCallsEnabled).toHaveBeenCalledWith(true);
  expect(mocks.setRingtone).toHaveBeenCalledWith("digital");
});
it("marks settings unknown after successful write with failed readback and supports refresh", async () => {
  await render();
  mocks.get.mockRejectedValueOnce(new Error("readback unavailable"));
  await act(async () => {
    expect(
      await controller.update({ notifications: false }, "notifications"),
    ).toBe(false);
  });
  expect(controller.settings).toBeNull();
  expect(controller.loadError).toEqual({
    kind: "refresh",
    detail: "readback unavailable",
  });
  expect(controller.errors.notifications?.kind).toBe("refresh");
  expect(controller.savedCategory).toBeNull();
  mocks.get.mockResolvedValueOnce({ ...initial, notifications: false });
  await act(async () => controller.reload());
  expect(controller.loadError).toBeNull();
  expect(controller.settings?.notifications).toBe(false);
});
it("keeps acknowledged settings after rejected write", async () => {
  await render();
  mocks.update.mockRejectedValueOnce(new Error("write unavailable"));
  await act(async () => {
    await controller.update({ retention_days: 30 }, "history");
  });
  expect(controller.settings).toEqual(initial);
  expect(controller.errors.history).toEqual({
    kind: "save",
    detail: "write unavailable",
  });
  expect(mocks.get).toHaveBeenCalledTimes(1);
});
it("does not acknowledge origin sharing when the owner save fails", async () => {
  await render();
  mocks.update.mockRejectedValueOnce(
    new Error("origin preference was not saved"),
  );
  await act(async () => {
    expect(
      await controller.update({ origin_sharing_enabled: true }, "connection"),
    ).toBe(false);
  });
  expect(controller.settings?.origin_sharing_enabled).toBe(false);
  expect(controller.errors.connection?.kind).toBe("save");
});

it("does not apply a prior owner's enabled origin-sharing readback", async () => {
  await render();
  const readback = deferred<AppSettings>();
  mocks.get.mockReturnValueOnce(readback.promise);
  let saving!: Promise<boolean>;
  await act(async () => {
    saving = controller.update({ origin_sharing_enabled: true }, "connection");
  });
  mocks.auth = { user: { id: "bob" }, generation: 1 };
  await render();
  await act(async () => {
    readback.resolve({ ...initial, origin_sharing_enabled: true });
    await saving;
  });
  expect(controller.owner).toBe("bob");
  expect(controller.settings?.origin_sharing_enabled).toBe(false);
  expect(controller.savedCategory).toBeNull();
});
it("does not publish a late settings read after closing", async () => {
  const pending = deferred<AppSettings>();
  mocks.get.mockReturnValueOnce(pending.promise);
  await render();
  await render(false);
  mocks.setPrivacy.mockClear();
  await act(async () =>
    pending.resolve({ ...initial, privacy_level: "strict" }),
  );
  expect(controller.settings).toBeNull();
  expect(mocks.setPrivacy).not.toHaveBeenCalled();
});
it("rejects old settings across owner and generation ABA transitions", async () => {
  const pending = deferred<AppSettings>();
  mocks.get.mockReturnValueOnce(pending.promise);
  await render();
  mocks.auth = { user: { id: "bob" }, generation: 1 };
  await render();
  mocks.auth = { user: { id: "alice" }, generation: 2 };
  await render();
  mocks.setPrivacy.mockClear();
  await act(async () =>
    pending.resolve({ ...initial, download_dir: "/private-old-owner" }),
  );
  expect(controller.settings?.download_dir).toBe("");
  expect(mocks.setPrivacy).not.toHaveBeenCalled();
});
it("does not publish old-generation write readback", async () => {
  await render();
  const readback = deferred<AppSettings>();
  mocks.get.mockReturnValueOnce(readback.promise);
  let saving!: Promise<boolean>;
  await act(async () => {
    saving = controller.update({ download_dir: "/old-owner" }, "files");
  });
  mocks.auth.generation++;
  await render();
  mocks.setPrivacy.mockClear();
  await act(async () => {
    readback.resolve({ ...initial, download_dir: "/old-owner" });
    await saving;
  });
  expect(controller.settings?.download_dir).toBe("");
  expect(controller.savedCategory).toBeNull();
  expect(mocks.setPrivacy).toHaveBeenCalledTimes(1);
});
it("does not mutate retained autostart after closed view readback rejection", async () => {
  await render();
  const pending = deferred<boolean>();
  mocks.autostartEnabled.mockReturnValueOnce(pending.promise);
  let saving!: Promise<boolean>;
  await act(async () => {
    saving = controller.setAutostartEnabled(true);
  });
  expect(controller.autostart).toEqual({ on: false, loaded: true });
  await render(false);
  await act(async () => {
    pending.reject(new Error("old readback failed"));
    await saving;
  });
  expect(controller.autostart).toEqual({ on: false, loaded: true });
  expect(controller.errors.general).toBeUndefined();
});
it("keeps replacement-owner autostart authoritative after stale readback rejection", async () => {
  await render();
  const pending = deferred<boolean>();
  mocks.autostartEnabled.mockReturnValueOnce(pending.promise);
  let saving!: Promise<boolean>;
  await act(async () => {
    saving = controller.setAutostartEnabled(true);
  });
  mocks.auth = { user: { id: "bob" }, generation: 1 };
  mocks.autostartEnabled.mockResolvedValue(true);
  await render();
  await act(async () => {
    pending.reject(new Error("old readback failed"));
    await saving;
  });
  expect(controller.owner).toBe("bob");
  expect(controller.autostart).toEqual({ on: true, loaded: true });
  expect(controller.errors.general).toBeUndefined();
});
