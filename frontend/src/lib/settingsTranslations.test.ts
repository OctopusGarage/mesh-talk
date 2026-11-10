import { createInstance } from "i18next";
import { describe, expect, it } from "vitest";

const locales = import.meta.glob<Record<string, unknown>>("../locales/*.json", {
  eager: true,
  import: "default",
});
const categoryKeys = [
  "general",
  "appearance",
  "notifications",
  "shortcuts",
  "connection",
  "privacy",
  "integrations",
  "files",
  "history",
];

describe("settings translations", () => {
  for (const [path, locale] of Object.entries(locales)) {
    it(`provides all settings categories in ${path}`, async () => {
      const i18n = createInstance();
      await i18n.init({
        lng: "test",
        fallbackLng: false,
        resources: { test: { translation: locale } },
      });
      const categories = i18n.t("settings.categories", { returnObjects: true });
      expect(Object.keys(categories).sort()).toEqual([...categoryKeys].sort());
      for (const key of categoryKeys) {
        expect(i18n.exists(`settings.categories.${key}`)).toBe(true);
        expect(i18n.t(`settings.categories.${key}`).trim()).not.toBe("");
      }
    });
    it(`provides every notification recovery state in ${path}`, async () => {
      const i18n = createInstance();
      await i18n.init({
        lng: "test",
        fallbackLng: false,
        resources: { test: { translation: locale } },
      });
      for (const key of [
        "denied",
        "granted",
        "prompt",
        "limited",
        "unknown",
        "checking",
        "settingsHint",
        "messageHint",
        "allowedHint",
        "openSettings",
        "checkAgain",
        "allow",
        "requesting",
        "returnHint",
        "openFailed",
        "reminder",
        "dismiss",
        "instructions.macos",
        "instructions.windows",
        "instructions.linux",
        "instructions.other",
      ]) {
        expect(i18n.exists(`notificationPermission.${key}`), key).toBe(true);
        expect(i18n.t(`notificationPermission.${key}`).trim(), key).not.toBe(
          "",
        );
      }
    });
    it(`renders saved relay details and local history consequences in ${path}`, async () => {
      const i18n = createInstance();
      await i18n.init({
        lng: "test",
        fallbackLng: false,
        resources: { test: { translation: locale } },
        interpolation: { escapeValue: false },
      });
      for (const key of [
        "discoveryScope",
        "discoveryScopeLocal",
        "discoveryScopeRouted",
        "discoveryScopeLocalDesc",
        "discoveryScopeRoutedDesc",
        "addDeviceAddress",
      ]) {
        expect(i18n.exists(`settings.${key}`), key).toBe(true);
        expect(i18n.t(`settings.${key}`).trim(), key).not.toBe("");
      }
      const cases = [
        ["settings.bridgeHubRemote", { name: "Garden relay" }, "Garden relay"],
        ["settings.bridgeHubUnavailable", { id: "a1b2c3" }, "a1b2c3"],
        ["settings.retentionConfirmDesc", { count: 37 }, "37"],
        [
          "settings.refreshFailed",
          { error: "Readback unavailable" },
          "Readback unavailable",
        ],
        [
          "files.receiveLoadFailed",
          { error: "Policy unavailable" },
          "Policy unavailable",
        ],
        [
          "recovery.statusUnavailable",
          { error: "Backup unavailable" },
          "Backup unavailable",
        ],
      ] as const;
      for (const [key, values, expected] of cases) {
        expect(i18n.exists(key), key).toBe(true);
        const rendered = i18n.t(key, values);
        expect(rendered, key).toContain(expected);
        expect(rendered, key).not.toMatch(/[{}]/);
      }
    });
  }
});
