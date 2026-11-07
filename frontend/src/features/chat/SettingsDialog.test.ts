// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
const { loaded, requests, controller } = vi.hoisted(() => ({
  loaded: [] as string[],
  requests: [] as string[],
  controller: {
    identity: "owner:1",
    settings: {},
    saving: false,
    savedCategory: null,
    loadError: null,
    errors: {},
    reload: () => {},
    clearError: () => {},
  },
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("./settings/useSettingsController", () => ({
  useSettingsController: () => controller,
}));
vi.mock("./settings/GeneralSettings", () => {
  loaded.push("GeneralSettings");
  return {
    GeneralSettings: () =>
      createElement("input", {
        "data-testid": "GeneralSettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/AppearanceSettings", () => {
  loaded.push("AppearanceSettings");
  return {
    AppearanceSettings: () =>
      createElement("input", {
        "data-testid": "AppearanceSettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/ScreenshotShortcutSettings", () => {
  loaded.push("ScreenshotShortcutSettings");
  return {
    ScreenshotShortcutSettings: () =>
      createElement("input", {
        "data-testid": "ScreenshotShortcutSettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/NotificationSettings", () => {
  loaded.push("NotificationSettings");
  return {
    NotificationSettings: () =>
      createElement("input", {
        "data-testid": "NotificationSettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/NetworkSettings", () => {
  loaded.push("NetworkSettings");
  return {
    NetworkSettings: () =>
      createElement("input", {
        "data-testid": "NetworkSettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/SecuritySettings", () => {
  loaded.push("SecuritySettings");
  return {
    SecuritySettings: () =>
      createElement("input", {
        "data-testid": "SecuritySettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/LocalAgentSettings", () => {
  loaded.push("LocalAgentSettings");
  return {
    LocalAgentSettings: () =>
      createElement("input", {
        "data-testid": "LocalAgentSettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/FileSettings", () => {
  loaded.push("FileSettings");
  return {
    FileSettings: () =>
      createElement("input", {
        "data-testid": "FileSettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/HistorySettings", () => {
  loaded.push("HistorySettings");
  return {
    HistorySettings: () =>
      createElement("input", {
        "data-testid": "HistorySettings",
        id: "setting-launch-at-login",
      }),
  };
});
vi.mock("./settings/loadSettingsPage", async (importOriginal) => {
  const original =
    await importOriginal<typeof import("./settings/loadSettingsPage")>();
  return {
    loadSettingsPage: (
      category: Parameters<typeof original.loadSettingsPage>[0],
    ) => {
      requests.push(category);
      return original.loadSettingsPage(category);
    },
  };
});
import { SettingsDialog } from "./SettingsDialog";
it("loads only the settings category a user opens", async () => {
  expect(loaded).toEqual([]);
  (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  Element.prototype.scrollIntoView = vi.fn();
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  try {
    await act(async () => {
      root.render(
        createElement<NonNullable<Parameters<typeof SettingsDialog>[0]>>(
          SettingsDialog,
          { open: false },
        ),
      );
    });
    expect(loaded).toEqual([]);
    await act(async () => {
      root.render(
        createElement<NonNullable<Parameters<typeof SettingsDialog>[0]>>(
          SettingsDialog,
          { open: true },
        ),
      );
    });
    await vi.waitFor(() =>
      expect(
        document.querySelector('[data-testid="GeneralSettings"]'),
      ).not.toBeNull(),
    );
    expect(loaded).toEqual(["GeneralSettings"]);
    await act(async () => {
      (
        document.querySelector(
          '[data-testid="settings-nav-appearance"]',
        ) as HTMLButtonElement
      ).click();
    });
    await vi.waitFor(() =>
      expect(
        document.querySelector('[data-testid="AppearanceSettings"]'),
      ).not.toBeNull(),
    );
    expect(loaded).toEqual(["GeneralSettings", "AppearanceSettings"]);
    expect(
      document.querySelector('[data-testid="GeneralSettings"]'),
    ).toBeNull();
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
});

it("opens and reopens directly at the requested category without importing the previous page", async () => {
  (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
  Element.prototype.scrollIntoView = vi.fn();
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const render = async (open: boolean, category: "history" | "privacy") => {
    await act(async () =>
      root.render(
        createElement<NonNullable<Parameters<typeof SettingsDialog>[0]>>(
          SettingsDialog,
          { open, initialTarget: { category } },
        ),
      ),
    );
  };
  try {
    requests.length = 0;
    await render(true, "history");
    expect(requests).toEqual(["history"]);
    await render(false, "history");
    requests.length = 0;
    await render(true, "privacy");
    expect(requests).toEqual(["privacy"]);
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
});
