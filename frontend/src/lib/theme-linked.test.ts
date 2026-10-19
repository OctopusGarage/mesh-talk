// @vitest-environment jsdom
import { beforeEach, expect, it, vi } from "vitest";

vi.mock("@/store/packs", () => ({ installedTheme: () => undefined }));

import {
  appearanceValues,
  savedAppearanceValues,
  applyLinkedAppearance,
  useTheme,
} from "./theme";

beforeEach(() => {
  localStorage.clear();
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
