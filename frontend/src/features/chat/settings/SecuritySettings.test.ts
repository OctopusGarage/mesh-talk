// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import i18n from "@/lib/i18n";
import type { LocalAgentAccess } from "@/lib/api/localAgentAccess";
import type { SettingsController } from "./useSettingsController";
const mocks = vi.hoisted(() => ({
  get: vi.fn(),
  set: vi.fn(),
  update: vi.fn(),
  manage: vi.fn(),
}));
vi.mock("@/lib/api/localAgentAccess", () => ({ localAgentAccess: mocks }));
vi.mock("../HiddenContactsDialog", () => ({
  HiddenContactsDialog: () => null,
}));
import { SecuritySettings } from "./SecuritySettings";
let host: HTMLDivElement;
let root: Root;
let controller: SettingsController;
let access: LocalAgentAccess;
beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.resetAllMocks();
  await i18n.changeLanguage("en");
  access = {
    enabled: false,
    running: false,
    lifetime: "one_hour",
    expires_at_ms: null,
    expired: false,
    recent_activity: [],
    activity_error: null,
    destinations: [],
    available_destinations: [],
    instructions_path: "",
    instructions: "",
    error: null,
  };
  mocks.get.mockImplementation(async () => access);
  controller = {
    owner: "alice",
    identity: "alice:1",
    saving: false,
    settings: {
      privacy_level: "balanced",
      stay_signed_in: true,
      notification_preview: true,
      auto_lock_seconds: 0,
      clipboard_clear_seconds: 0,
    },
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
    root.render(
      createElement(SecuritySettings, {
        controller,
        onNotifications: vi.fn(),
        onDraftChange: vi.fn(),
        onAgentSettings: mocks.manage,
      }),
    ),
  );
}
function defer<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}
it("shows acknowledged saved login and protections while a preset is only a draft", async () => {
  await render();
  const preset = host.querySelector<HTMLSelectElement>(
    "#setting-privacy-level",
  )!;
  await act(async () => {
    preset.value = "strict";
    preset.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(host.textContent).toContain(i18n.t("settings.privacyEffectiveState"));
  expect(host.textContent).toContain(i18n.t("settings.savedLoginEnabled"));
  expect(
    host.querySelector<HTMLSelectElement>("#setting-auto-lock")!.value,
  ).toBe("0");
  expect(mocks.update).not.toHaveBeenCalled();
  await act(async () =>
    host.querySelector<HTMLButtonElement>("#setting-stay-signed-in")!.click(),
  );
  expect(mocks.update).toHaveBeenCalledWith(
    { stay_signed_in: false },
    "privacy",
  );
  expect(host.textContent).toContain(i18n.t("settings.savedLoginEnabled"));
  controller = {
    ...controller,
    settings: { ...controller.settings!, stay_signed_in: false },
  };
  await render();
  expect(host.textContent).toContain(i18n.t("settings.savedLoginDisabled"));
});
it.each([
  [false, false, "agentAccess.off"],
  [true, true, "agentAccess.running"],
  [true, false, "agentAccess.unavailable"],
])(
  "reports actual agent state enabled=%s running=%s and navigates without mutation",
  async (enabled, running, status) => {
    access = {
      ...access,
      enabled,
      running,
      destinations: [{ kind: "channel", id: "group" }],
    };
    await render();
    expect(mocks.get).toHaveBeenCalledWith("alice");
    const summary = host.querySelector('[data-testid="privacy-agent-status"]')!;
    expect(summary.textContent).toContain(i18n.t(status));
    expect(summary.textContent).toContain(
      i18n.t("agentAccess.grantedConversations", { count: 1 }),
    );
    await act(async () =>
      summary
        .querySelector<HTMLButtonElement>(
          '[data-testid="manage-privacy-agent-access"]',
        )!
        .click(),
    );
    expect(mocks.manage).toHaveBeenCalledOnce();
    expect(mocks.set).not.toHaveBeenCalled();
    expect(mocks.update).not.toHaveBeenCalled();
  },
);
it("shows loading, a recoverable error and retries only the status read", async () => {
  const pending = defer<LocalAgentAccess>();
  mocks.get.mockReturnValueOnce(pending.promise);
  await render();
  expect(
    host.querySelector('[data-testid="privacy-agent-status"]')!.textContent,
  ).toContain(i18n.t("common.loading"));
  await act(async () => pending.reject(new Error("Node unavailable")));
  expect(host.querySelector('[role="alert"]')!.textContent).toContain(
    "Node unavailable",
  );
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="retry-privacy-agent-status"]',
      )!
      .click(),
  );
  expect(mocks.get).toHaveBeenCalledTimes(2);
  expect(host.querySelector('[role="alert"]')).toBeNull();
  expect(mocks.set).not.toHaveBeenCalled();
});
it("reports unavailable service details and refreshes without enabling it", async () => {
  access = { ...access, enabled: true, error: "Listener stopped" };
  await render();
  expect(host.querySelector('[role="alert"]')!.textContent).toContain(
    "Listener stopped",
  );
  access = { ...access, enabled: false, error: null };
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="retry-privacy-agent-status"]',
      )!
      .click(),
  );
  expect(
    host.querySelector('[data-testid="privacy-agent-status"]')!.textContent,
  ).toContain(i18n.t("agentAccess.off"));
  expect(mocks.set).not.toHaveBeenCalled();
});
it.each(["success", "failure"])(
  "ignores stale status %s after same-owner session replacement",
  async (outcome) => {
    const pending = defer<LocalAgentAccess>();
    mocks.get.mockReturnValueOnce(pending.promise);
    await render();
    controller = { ...controller, identity: "alice:2" };
    await render();
    await act(async () =>
      outcome === "success"
        ? pending.resolve({ ...access, enabled: true, running: true })
        : pending.reject(new Error("Retired session")),
    );
    expect(
      host.querySelector('[data-testid="privacy-agent-status"]')!.textContent,
    ).toContain(i18n.t("agentAccess.off"));
    expect(host.textContent).not.toContain("Retired session");
    expect(host.textContent).not.toContain(i18n.t("agentAccess.running"));
  },
);
it("does not query private state without acknowledged settings", async () => {
  controller = { ...controller, settings: null };
  await render();
  expect(mocks.get).not.toHaveBeenCalled();
});

it("does not query private state without an owner", async () => {
  controller = { ...controller, owner: null, identity: null };
  await render();
  expect(mocks.get).not.toHaveBeenCalled();
  expect(
    host.querySelector<HTMLButtonElement>(
      '[data-testid="manage-privacy-agent-access"]',
    )!.disabled,
  ).toBe(true);
});

it("discards a delayed retry when the owner changes", async () => {
  mocks.get.mockRejectedValueOnce(new Error("Initial failure"));
  await render();
  const pending = defer<LocalAgentAccess>();
  mocks.get.mockReturnValueOnce(pending.promise);
  await act(async () =>
    host
      .querySelector<HTMLButtonElement>(
        '[data-testid="retry-privacy-agent-status"]',
      )!
      .click(),
  );
  controller = { ...controller, owner: "bob", identity: "bob:2" };
  await render();
  await act(async () =>
    pending.resolve({
      ...access,
      enabled: true,
      running: true,
      destinations: [{ kind: "account", id: "private" }],
    }),
  );
  expect(mocks.get).toHaveBeenLastCalledWith("bob");
  expect(
    host.querySelector('[data-testid="privacy-agent-status"]')!.textContent,
  ).toContain(i18n.t("agentAccess.grantedConversations", { count: 0 }));
  expect(host.textContent).not.toContain(i18n.t("agentAccess.running"));
});

it("discards a delayed failure after the privacy page unmounts", async () => {
  const pending = defer<LocalAgentAccess>();
  mocks.get.mockReturnValueOnce(pending.promise);
  await render();
  await act(async () => root.render(null));
  await act(async () => pending.reject(new Error("Retired view")));
  expect(host.textContent).toBe("");
});

it("reports expired authorization distinctly without renewing it", async () => {
  access = { ...access, enabled: true, expired: true, expires_at_ms: 1 };
  await render();
  expect(
    host.querySelector('[data-testid="privacy-agent-status"]')!.textContent,
  ).toContain(i18n.t("agentAccess.expired"));
  expect(mocks.set).not.toHaveBeenCalled();
});

it("refreshes the privacy expiration read-only and cleans up on unmount", async () => {
  vi.useFakeTimers();
  try {
    access = {
      ...access,
      enabled: true,
      running: true,
      expires_at_ms: Date.now() + 1000,
    };
    await render();
    access = { ...access, running: false, expired: true };
    await act(async () => vi.advanceTimersByTimeAsync(1001));
    expect(
      host.querySelector('[data-testid="privacy-agent-status"]')!.textContent,
    ).toContain(i18n.t("agentAccess.expired"));
    expect(mocks.get).toHaveBeenCalledTimes(2);
    expect(mocks.set).not.toHaveBeenCalled();
    await act(async () => root.render(null));
    await act(async () => vi.advanceTimersByTimeAsync(10000));
    expect(mocks.get).toHaveBeenCalledTimes(2);
  } finally {
    vi.useRealTimers();
  }
});

it("keeps lock and clipboard editors separate from related permission links", async () => {
  await render();
  const section = (key: string) =>
    [...host.querySelectorAll("section")].find(
      (node) =>
        node.querySelector("h3")?.textContent ===
        i18n.t(`settings.sections.${key}`),
    )!;
  expect(
    section("lockAndLogin").querySelector("#setting-auto-lock"),
  ).not.toBeNull();
  expect(
    section("lockAndLogin").querySelector("#setting-stay-signed-in"),
  ).not.toBeNull();
  expect(
    section("clipboard").querySelector("#setting-confirm-copy"),
  ).not.toBeNull();
  expect(
    section("clipboard").querySelector("#setting-clipboard-clear"),
  ).not.toBeNull();
  expect(
    section("relatedPermissions").querySelector('select,[role="switch"]'),
  ).toBeNull();
  expect(host.querySelectorAll("#setting-stay-signed-in")).toHaveLength(1);
});
