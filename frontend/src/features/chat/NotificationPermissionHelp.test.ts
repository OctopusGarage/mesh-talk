// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import { useNotificationPermission } from "@/lib/notify";
import { NotificationPermissionHelp } from "./NotificationPermissionHelp";
const mocks = vi.hoisted(() => ({
  check: vi.fn(),
  request: vi.fn(),
  openSettings: vi.fn(),
}));
vi.mock("@/lib/api/notifications", () => ({ notifications: mocks }));
let root: Root;
let host: HTMLDivElement;
const denied = {
  status: "denied" as const,
  platform: "macos" as const,
  can_open_settings: true,
};
beforeEach(async () => {
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
  vi.resetAllMocks();
  await i18n.changeLanguage("en");
  useNotificationPermission.setState({
    permission: denied,
    checking: false,
    requesting: false,
  });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});
async function render() {
  await act(async () => root.render(createElement(NotificationPermissionHelp)));
}
async function click(text: string) {
  const button = [...host.querySelectorAll("button")].find(
    (item) => item.textContent === text,
  );
  expect(button, text).toBeDefined();
  await act(async () => button!.click());
}
it("explains denied authorization inline and opens settings only after clicking", async () => {
  mocks.openSettings.mockResolvedValue(undefined);
  await render();
  expect(host.textContent).toContain(
    "Notifications are blocked in system settings",
  );
  expect(mocks.openSettings).not.toHaveBeenCalled();
  await click("Open system settings");
  expect(mocks.openSettings).toHaveBeenCalledOnce();
  expect(host.textContent).toContain("System Settings");
});
it("keeps manual recovery visible if the settings link fails", async () => {
  mocks.openSettings.mockRejectedValue(new Error("unavailable"));
  await render();
  await click("Open system settings");
  expect(host.querySelector('[role="alert"]')?.textContent).toContain(
    "Couldn’t open system settings",
  );
  expect(host.textContent).toContain("Notifications → Mesh-Talk");
});
it("rechecks and replaces the blocked state after a grant", async () => {
  mocks.check.mockResolvedValue({ ...denied, status: "granted" });
  await render();
  await click("Check again");
  expect(host.textContent).toContain("Allowed by your system");
  expect(host.textContent).not.toContain("Notifications are blocked");
});
it("offers an explicit request for not-yet-decided authorization", async () => {
  useNotificationPermission.setState({
    permission: { ...denied, status: "prompt" },
  });
  mocks.check.mockResolvedValue({ ...denied, status: "prompt" });
  mocks.request.mockResolvedValue(denied);
  await render();
  expect(mocks.request).not.toHaveBeenCalled();
  await click("Allow notifications");
  expect(mocks.request).toHaveBeenCalledOnce();
  expect(host.textContent).toContain("Notifications are blocked");
});
it("shows honest manual guidance when a desktop cannot verify permission", async () => {
  useNotificationPermission.setState({
    permission: {
      ...denied,
      status: "unknown",
      platform: "linux",
      can_open_settings: false,
    },
  });
  await render();
  expect(host.textContent).toContain("System permission couldn’t be verified");
  expect(host.textContent).toContain("desktop’s notification settings");
  expect(
    [...host.querySelectorAll("button")].some(
      (item) => item.textContent === "Open system settings",
    ),
  ).toBe(false);
});
