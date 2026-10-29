// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SettingsController } from "./useSettingsController";

const mocks = vi.hoisted(() => ({ update: vi.fn() }));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (s: { peers: [] }) => unknown) => select({ peers: [] }),
}));
vi.mock("@/store/privacy", () => ({
  usePrivacy: (select: (s: object) => unknown) =>
    select({
      owner: "alice",
      loaded: true,
      loading: false,
      busy: false,
      snapshot: { invisible: false },
    }),
}));
vi.mock("../PrivacySettings", () => ({ PrivacySettings: () => null }));
vi.mock("../OfflineConnectDialog", () => ({
  OfflineConnectDialog: ({ open }: { open: boolean }) =>
    open ? createElement("div", { "data-testid": "manual-connect" }) : null,
}));
import { NetworkSettings } from "./NetworkSettings";
let host: HTMLDivElement;
let root: Root;
let controller: SettingsController;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.update.mockReset();
  host = document.createElement("div");
  root = createRoot(host);
  controller = {
    owner: "alice",
    saving: false,
    update: mocks.update,
    settings: {
      physical_lan_only: false,
      bridge_discovery_enabled: true,
      origin_sharing_enabled: false,
      post_office_enabled: true,
      post_office_send_enabled: true,
      post_office_receive_enabled: true,
      bridge_hub_id: "",
    },
  } as unknown as SettingsController;
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
async function render() {
  await act(async () =>
    root.render(createElement(NetworkSettings, { controller })),
  );
}
function scope() {
  return host.querySelector<HTMLSelectElement>("#setting-discovery-scope")!;
}
it.each([true, false])(
  "projects saved physical_lan_only=%s without rewriting preferences",
  async (physical) => {
    controller.settings!.physical_lan_only = physical;
    await render();
    expect(scope()).not.toBeNull();
    expect(scope().value).toBe(physical ? "local" : "routed");
    expect(
      host.querySelector('label[for="setting-discovery-scope"]')?.textContent,
    ).toBe("settings.discoveryScope");
    expect(scope().getAttribute("aria-describedby")).toBe(
      "setting-discovery-scope-help",
    );
    const bridge = host.querySelector("#setting-bridge-discovery")!;
    expect(bridge.getAttribute("aria-checked")).toBe("true");
    expect(bridge.hasAttribute("disabled")).toBe(physical);
    expect(mocks.update).not.toHaveBeenCalled();
  },
);
it("writes only the existing scope field and waits for canonical readback", async () => {
  await render();
  expect(scope()).not.toBeNull();
  await act(async () => {
    scope().value = "local";
    scope().dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(mocks.update).toHaveBeenCalledExactlyOnceWith(
    { physical_lan_only: true },
    "connection",
  );
  expect(scope().value).toBe("routed");
});
it("restores routed discovery without enabling directory discovery", async () => {
  controller.settings!.physical_lan_only = true;
  controller.settings!.bridge_discovery_enabled = false;
  await render();
  expect(scope()).not.toBeNull();
  await act(async () => {
    scope().value = "routed";
    scope().dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(mocks.update).toHaveBeenCalledExactlyOnceWith(
    { physical_lan_only: false },
    "connection",
  );
});
it("disables scope changes while saving and hides controls without a snapshot", async () => {
  controller.saving = true;
  await render();
  expect(scope()).not.toBeNull();
  expect(scope().disabled).toBe(true);
  controller.settings = null;
  await render();
  expect(scope()).toBeNull();
});
it("opens the existing manual address flow", async () => {
  await render();
  const button = [...host.querySelectorAll("button")].find(
    (el) => el.textContent === "settings.addDeviceAddress",
  );
  expect(button).toBeDefined();
  await act(async () => button!.click());
  expect(host.querySelector('[data-testid="manual-connect"]')).not.toBeNull();
});

it("shows relay availability without requiring federation configuration", async () => {
  await render();
  expect(
    host.querySelector('[data-testid="post-office-status"]')?.textContent,
  ).toBe("settings.postOfficeStatusUnavailable");
  controller.settings!.post_office_enabled = false;
  await render();
  expect(
    host.querySelector('[data-testid="post-office-status"]')?.textContent,
  ).toBe("settings.postOfficeStatusDisabled");
});

it("keeps origin sharing off until the owner explicitly enables it", async () => {
  await render();
  const sharing = host.querySelector<HTMLButtonElement>(
    "#setting-origin-sharing",
  );
  expect(sharing).not.toBeNull();
  expect(sharing!.getAttribute("aria-checked")).toBe("false");
  await act(async () => sharing!.click());
  expect(mocks.update).toHaveBeenCalledExactlyOnceWith(
    { origin_sharing_enabled: true },
    "connection",
  );
  expect(sharing!.getAttribute("aria-checked")).toBe("false");
});

it("prevents enabling origin sharing without directory discovery", async () => {
  controller.settings!.bridge_discovery_enabled = false;
  await render();
  const sharing = host.querySelector<HTMLButtonElement>(
    "#setting-origin-sharing",
  );
  expect(sharing).not.toBeNull();
  expect(sharing!.disabled).toBe(true);
});

it("allows withdrawing origin sharing when discovery dependencies are off", async () => {
  controller.settings!.bridge_discovery_enabled = false;
  controller.settings!.post_office_enabled = false;
  controller.settings!.physical_lan_only = true;
  controller.settings!.origin_sharing_enabled = true;
  await render();
  const sharing = host.querySelector<HTMLButtonElement>(
    "#setting-origin-sharing",
  );
  expect(sharing).not.toBeNull();
  expect(sharing!.disabled).toBe(false);
  await act(async () => sharing!.click());
  expect(mocks.update).toHaveBeenCalledExactlyOnceWith(
    { origin_sharing_enabled: false },
    "connection",
  );
});

it("groups discovery, network visibility and delivery without duplicating controls", async () => {
  await render();
  const section = (key: string) =>
    [...host.querySelectorAll("section")].find(
      (node) =>
        node.querySelector("h3")?.textContent === `settings.sections.${key}`,
    )!;
  expect(
    section("deviceDiscovery").querySelector("#setting-discovery-scope"),
  ).not.toBeNull();
  expect(
    section("crossNetwork").querySelector("#setting-bridge-discovery"),
  ).not.toBeNull();
  expect(
    section("crossNetwork").querySelector("#setting-origin-sharing"),
  ).not.toBeNull();
  expect(
    section("offlineDelivery").querySelector("#setting-post-office"),
  ).not.toBeNull();
  expect(
    section("offlineDelivery").querySelector("#setting-post-office-send"),
  ).not.toBeNull();
  expect(host.querySelectorAll("#setting-post-office")).toHaveLength(1);
});
