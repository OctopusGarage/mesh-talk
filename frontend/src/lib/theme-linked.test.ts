// @vitest-environment jsdom
import { beforeEach, expect, it, vi } from "vitest";

const packs = vi.hoisted(() => ({
  selected: undefined as import("./pack").ThemePack | undefined,
}));
vi.mock("@/store/packs", () => ({
  installedTheme: (id: string) =>
    packs.selected?.id === id ? packs.selected : undefined,
}));

import {
  appearanceValues,
  savedAppearanceValues,
  applyLinkedAppearance,
  useTheme,
} from "./theme";

beforeEach(() => {
  localStorage.clear();
  packs.selected = undefined;
  useTheme.setState({
    theme: "dark",
    wallpaperEnabled: true,
    packWallpaperIds: {},
  });
});

it("distinguishes fresh defaults from individually saved appearance choices", () => {
  expect(appearanceValues()).toEqual({
    theme: "dark",
    wallpaper_enabled: "on",
    pack_wallpaper_ids: "{}",
  });
  expect(savedAppearanceValues()).toEqual({});

  // Explicitly choosing a default value still counts as a saved choice.
  useTheme.getState().set("dark");
  expect(savedAppearanceValues()).toEqual({ theme: "dark" });
  useTheme.getState().setWallpaperEnabled(true);
  expect(savedAppearanceValues()).toEqual({
    theme: "dark",
    wallpaper_enabled: "on",
  });
  applyLinkedAppearance("pack_wallpaper_ids", '{"nature":"forest-1"}');
  expect(savedAppearanceValues()).toEqual({
    theme: "dark",
    wallpaper_enabled: "on",
    pack_wallpaper_ids: '{"nature":"forest-1"}',
  });
});

it("accepts only valid linked themes and wallpaper choices", () => {
  applyLinkedAppearance("theme", null);
  applyLinkedAppearance("theme", "../../wrong");
  expect(useTheme.getState().theme).toBe("dark");
  applyLinkedAppearance("theme", "light");
  expect(useTheme.getState().theme).toBe("light");
  expect(localStorage.getItem("mesh-talk-theme")).toBe("light");

  applyLinkedAppearance("wallpaper_enabled", "invalid");
  expect(useTheme.getState().wallpaperEnabled).toBe(true);
  applyLinkedAppearance("wallpaper_enabled", "off");
  expect(useTheme.getState().wallpaperEnabled).toBe(false);
  expect(document.documentElement.dataset.wallpaper).toBe("off");
  expect(appearanceValues()).toMatchObject({
    theme: "light",
    wallpaper_enabled: "off",
  });
});

it("filters malformed linked wallpaper IDs before storing them", () => {
  applyLinkedAppearance("pack_wallpaper_ids", "not json");
  applyLinkedAppearance("pack_wallpaper_ids", "[]");
  expect(useTheme.getState().packWallpaperIds).toEqual({});

  applyLinkedAppearance(
    "pack_wallpaper_ids",
    JSON.stringify({
      nature: "forest-1",
      "../wrong": "unsafe",
      football: "../../wrong",
      cats: 12,
    }),
  );
  expect(useTheme.getState().packWallpaperIds).toEqual({ nature: "forest-1" });
  expect(
    JSON.parse(localStorage.getItem("mesh-talk-pack-wallpapers") ?? "null"),
  ).toEqual({
    nature: "forest-1",
  });
  expect(appearanceValues().pack_wallpaper_ids).toBe(
    JSON.stringify({ nature: "forest-1" }),
  );
});

it("preserves a selected account theme while its assets are still loading", () => {
  useTheme.getState().set("source.custom-theme");
  useTheme.getState().refresh(true);
  expect(useTheme.getState().theme).toBe("source.custom-theme");
  expect(savedAppearanceValues().theme).toBe("source.custom-theme");
  useTheme.getState().refresh();
  expect(useTheme.getState().theme).toBe("dark");
});
it("refreshes imported and removed pack CSS without changing the linked selection", () => {
  useTheme.getState().set("source.custom-theme");
  packs.selected = {
    format: 1,
    id: "source.custom-theme",
    kind: "theme",
    version: "1.0.0",
    name: "Source",
    base: "dark",
    colors: { primary: "120 100% 40%" },
  };
  const changed = vi.fn();
  const unsubscribe = useTheme.subscribe(changed);
  useTheme.getState().refresh(true);
  expect(document.documentElement.style.getPropertyValue("--primary")).toBe(
    "120 100% 40%",
  );
  packs.selected = undefined;
  useTheme.getState().refresh(true);
  expect(document.documentElement.style.getPropertyValue("--primary")).toBe("");
  expect(useTheme.getState().theme).toBe("source.custom-theme");
  expect(changed).not.toHaveBeenCalled();
  unsubscribe();
});

it("applies linked appearance tombstones as field defaults", () => {
  applyLinkedAppearance("theme", "source.custom-theme");
  applyLinkedAppearance("wallpaper_enabled", "off");
  applyLinkedAppearance("pack_wallpaper_ids", '{"nature":"forest-1"}');
  applyLinkedAppearance("theme", null);
  applyLinkedAppearance("wallpaper_enabled", null);
  applyLinkedAppearance("pack_wallpaper_ids", null);
  expect(appearanceValues()).toEqual({
    theme: "dark",
    wallpaper_enabled: "on",
    pack_wallpaper_ids: "{}",
  });
});
