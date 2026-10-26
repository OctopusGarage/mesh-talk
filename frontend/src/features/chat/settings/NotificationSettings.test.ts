// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { useNotificationPermission } from "@/lib/notify";
import { NotificationSettings } from "./NotificationSettings";
import type { SettingsController } from "./useSettingsController";

const mocks = vi.hoisted(() => ({
  check: vi.fn(),
  request: vi.fn(),
  update: vi.fn(),
  preview: vi.fn(),
}));
vi.mock("@/lib/api/notifications", () => ({ notifications: mocks }));
vi.mock("@/lib/ringtones", () => ({
  asRingtoneId: (id: string) => id,
  previewRingtone: mocks.preview,
  RINGTONE_IDS: ["classic", "digital"],
}));
const prompt = { status: "prompt", platform: "macos", can_open_settings: true };
let root: Root;
let host: HTMLDivElement;
let controller: SettingsController;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  await i18n.changeLanguage("en");
  mocks.check.mockResolvedValue(prompt);
  mocks.request.mockResolvedValue({ ...prompt, status: "denied" });
  mocks.update.mockResolvedValue(true);
  useNotificationPermission.setState({
    permission: null,
    checking: false,
    requesting: false,
  });
  controller = {
    identity: "alice:1",
    settings: {
      notifications: false,
      notification_preview: true,
      calls_enabled: false,
      ringtone: "classic",
    },
    saving: false,
    update: mocks.update,
  } as unknown as SettingsController;
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});
async function render() {
  await act(async () =>
    root.render(createElement(NotificationSettings, { controller })),
  );
}
async function toggle(id = "setting-notifications") {
  await act(async () =>
    host.querySelector<HTMLButtonElement>(`#${id}`)!.click(),
  );
}
function defer<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

it("requests OS permission only after the notification preference save is acknowledged", async () => {
  const write = defer<boolean>();
  mocks.update.mockReturnValue(write.promise);
  await render();
  expect(mocks.check).not.toHaveBeenCalled();
  await toggle();
  expect(mocks.update).toHaveBeenCalledWith(
    { notifications: true },
    "notifications",
  );
  expect(mocks.request).not.toHaveBeenCalled();
  await act(async () => {
    write.resolve(true);
    await write.promise;
  });
  expect(mocks.request).toHaveBeenCalledOnce();
  expect(
    host.querySelector("#setting-notifications")?.getAttribute("aria-checked"),
  ).toBe("false");
  controller = {
    ...controller,
    settings: { ...controller.settings!, notifications: true },
  };
  await render();
  expect(host.textContent).toContain("Allow notifications on this device");
});
it("keeps the existing preference and never prompts after a rejected save", async () => {
  mocks.update.mockResolvedValue(false);
  await render();
  await toggle();
  expect(mocks.request).not.toHaveBeenCalled();
  expect(mocks.check).not.toHaveBeenCalled();
  expect(
    host.querySelector("#setting-notifications")?.getAttribute("aria-checked"),
  ).toBe("false");
});
it("does not prompt after a delayed save finishes in a retired settings view", async () => {
  const write = defer<boolean>();
  mocks.update.mockReturnValue(write.promise);
  await render();
  await toggle();
  await act(async () => root.render(null));
  await act(async () => {
    write.resolve(true);
    await write.promise;
  });
  expect(mocks.request).not.toHaveBeenCalled();
  expect(mocks.check).not.toHaveBeenCalled();
});
it("does not carry a delayed enable across account replacement", async () => {
  const write = defer<boolean>();
  mocks.update.mockReturnValue(write.promise);
  await render();
  await toggle();
  controller = { ...controller, identity: "bob:2" };
  await render();
  await act(async () => {
    write.resolve(true);
    await write.promise;
  });
  expect(mocks.request).not.toHaveBeenCalled();
});
it("turns off the saved preference without asking the system or changing previews", async () => {
  controller.settings!.notifications = true;
  mocks.check.mockResolvedValue({ ...prompt, status: "granted" });
  await render();
  await toggle();
  expect(mocks.update).toHaveBeenCalledWith(
    { notifications: false },
    "notifications",
  );
  expect(mocks.request).not.toHaveBeenCalled();
  expect(
    host
      .querySelector("#setting-notification-preview")
      ?.getAttribute("aria-checked"),
  ).toBe("true");
});
it("retains disabled previews and saves preview changes as the custom privacy preference", async () => {
  await render();
  expect(
    host.querySelector<HTMLButtonElement>("#setting-notification-preview")!
      .disabled,
  ).toBe(true);
  controller.settings!.notifications = true;
  await render();
  await toggle("setting-notification-preview");
  expect(mocks.update).toHaveBeenCalledWith(
    { privacy_level: "custom", notification_preview: false },
    "notifications",
  );
});
it("keeps notification edits disabled during saves and system authorization", async () => {
  controller.saving = true;
  await render();
  expect(
    host.querySelector<HTMLButtonElement>("#setting-notifications")!.disabled,
  ).toBe(true);
  controller.saving = false;
  useNotificationPermission.setState({ requesting: true });
  await render();
  expect(
    host.querySelector<HTMLButtonElement>("#setting-notifications")!.disabled,
  ).toBe(true);
});
it("preserves experimental call and ringtone controls in the notification page", async () => {
  controller.settings!.calls_enabled = true;
  await render();
  await toggle("setting-calls");
  expect(mocks.update).toHaveBeenCalledWith(
    { calls_enabled: false },
    "notifications",
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>('button[aria-label="Preview"]')!
      .click(),
  );
  expect(mocks.preview).toHaveBeenCalledWith("classic");
  const select = host.querySelector<HTMLSelectElement>("#setting-ringtone")!;
  await act(async () => {
    select.value = "digital";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(mocks.update).toHaveBeenCalledWith(
    { ringtone: "digital" },
    "notifications",
  );
  expect(mocks.preview).toHaveBeenLastCalledWith("digital");
});
it("renders no controls until account settings are known", async () => {
  controller.settings = null;
  await render();
  expect(host.textContent).toBe("");
  expect(mocks.check).not.toHaveBeenCalled();
});
