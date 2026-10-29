import { expect, it } from "vitest";
import {
  SETTINGS_CATEGORIES,
  SETTINGS_GROUPS,
  settingsCategoryForControl,
  retentionTightens,
  networkControlState,
} from "./model";

it("provides one unique destination for every approved settings task", () => {
  expect(SETTINGS_CATEGORIES).toEqual([
    "general",
    "appearance",
    "notifications",
    "shortcuts",
    "privacy",
    "files",
    "history",
    "connection",
    "integrations",
  ]);
  expect(new Set(SETTINGS_CATEGORIES).size).toBe(SETTINGS_CATEGORIES.length);
  expect(SETTINGS_GROUPS.map((group) => group.id)).toEqual([
    "experience",
    "privacyData",
    "connectionExtensions",
  ]);
  expect(settingsCategoryForControl("setting-language")).toBe("appearance");
  expect(settingsCategoryForControl("setting-screenshot-capture")).toBe(
    "shortcuts",
  );
  expect(settingsCategoryForControl("setting-local-agent-access")).toBe(
    "integrations",
  );
});
it.each([
  [0, 7, true],
  [30, 7, true],
  [7, 30, false],
  [7, 0, false],
  [0, 0, false],
  [30, 30, false],
])("retention %i -> %i needs confirmation: %s", (previous, next, expected) => {
  expect(retentionTightens(previous as number, next as number)).toBe(expected);
});
it("keeps relay directions separate from public discovery and explains overriding policy", () => {
  const input = {
    loaded: true,
    saving: false,
    relayEnabled: true,
    physicalOnly: false,
    invisible: false as boolean | null,
    send: true,
    receive: true,
  };
  expect(
    networkControlState({ ...input, invisible: true }).directory.reason,
  ).toBe("invisible");
  expect(
    networkControlState({ ...input, invisible: null }).directory.reason,
  ).toBe("privacyUnknown");
  expect(
    networkControlState({ ...input, physicalOnly: true }).directory.reason,
  ).toBe("physicalOnly");
  expect(
    networkControlState({ ...input, relayEnabled: false, physicalOnly: true })
      .directory.reason,
  ).toBe("relayOff");
  expect(
    networkControlState({ ...input, invisible: true }).relay.disabled,
  ).toBe(false);
  expect(networkControlState({ ...input, send: false }).partialReceive).toBe(
    true,
  );
  expect(
    networkControlState({ ...input, relayEnabled: false, send: false })
      .partialReceive,
  ).toBe(false);
});

it("leaves unknown control destinations to the supplied category", () => {
  expect(settingsCategoryForControl("unknown-control")).toBeUndefined();
  expect(settingsCategoryForControl("constructor")).toBeUndefined();
  expect(settingsCategoryForControl()).toBeUndefined();
});
