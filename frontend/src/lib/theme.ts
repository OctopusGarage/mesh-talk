import { create } from "zustand";
import {
  DEFAULT_NATURE_WALLPAPER,
  getNatureWallpaper,
} from "@/lib/natureWallpapers";
import {
  DEFAULT_CAT_ACRYLIC_WALLPAPER,
  getCatAcrylicWallpaper,
} from "@/lib/catAcrylicWallpapers";

// Base modes use the default Ink & Signal look. Football and nature themes apply
// their own palette via `data-palette`; nature also selects a separate wallpaper.
export type Theme =
  | "light"
  | "dark"
  | "oled"
  | "argentina"
  | "barcelona"
  | "messi"
  | "nature"
  | "cat-acrylic";

/** Personal palettes (driven by `html[data-palette=…]`); the rest are base modes. */
const PALETTES = new Set<Theme>([
  "argentina",
  "barcelona",
  "messi",
  "nature",
  "cat-acrylic",
]);

/** Light palettes build on light defaults; Barcelona remains dark. */
const LIGHT_PALETTES = new Set<Theme>([
  "argentina",
  "messi",
  "nature",
  "cat-acrylic",
]);

/** Every selectable theme, in display order. */
export const ALL_THEMES: Theme[] = [
  "dark",
  "light",
  "oled",
  "argentina",
  "barcelona",
  "messi",
  "nature",
  "cat-acrylic",
];

const KEY = "mesh-talk-theme";
const WALLPAPER_KEY = "mesh-talk-wallpaper";
const NATURE_WALLPAPER_KEY = "mesh-talk-nature-wallpaper";
const CAT_ACRYLIC_WALLPAPER_KEY = "mesh-talk-cat-acrylic-wallpaper";
let themeTransitionTimer: number | undefined;

function readWallpaper(): boolean {
  if (typeof localStorage === "undefined") return true;
  return localStorage.getItem(WALLPAPER_KEY) !== "off";
}

function applyWallpaper(enabled: boolean) {
  if (typeof document !== "undefined") {
    document.documentElement.dataset.wallpaper = enabled ? "on" : "off";
  }
}

function read(): Theme {
  if (typeof localStorage === "undefined") return "dark";
  const v = localStorage.getItem(KEY) as Theme | null;
  return v && ALL_THEMES.includes(v) ? v : "dark";
}

function readNatureWallpaper(): string {
  if (typeof localStorage === "undefined") return DEFAULT_NATURE_WALLPAPER.id;
  return getNatureWallpaper(
    localStorage.getItem(NATURE_WALLPAPER_KEY) ?? DEFAULT_NATURE_WALLPAPER.id,
  ).id;
}

function applyNatureWallpaper(id: string) {
  if (typeof document === "undefined") return;
  const url = getNatureWallpaper(id).url;
  document.documentElement.style.setProperty(
    "--nature-wallpaper-url",
    `url("${url}")`,
  );
}

function readCatAcrylicWallpaper(): string {
  if (typeof localStorage === "undefined")
    return DEFAULT_CAT_ACRYLIC_WALLPAPER.id;
  return getCatAcrylicWallpaper(
    localStorage.getItem(CAT_ACRYLIC_WALLPAPER_KEY) ??
      DEFAULT_CAT_ACRYLIC_WALLPAPER.id,
  ).id;
}

function applyCatAcrylicWallpaper(id: string) {
  if (typeof document === "undefined") return;
  const url = getCatAcrylicWallpaper(id).url;
  document.documentElement.style.setProperty(
    "--cat-acrylic-wallpaper-url",
    `url("${url}")`,
  );
}

function apply(t: Theme, animate: boolean) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;

  // Only structural surfaces soften a theme switch. Clear the prior timer so a rapid
  // second switch retargets from the current colors and cannot end the new transition.
  if (themeTransitionTimer !== undefined) {
    window.clearTimeout(themeTransitionTimer);
    themeTransitionTimer = undefined;
  }
  root.classList.remove("theme-transitioning");
  const reduceMotion =
    typeof window !== "undefined" &&
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
  if (animate && !reduceMotion) root.classList.add("theme-transitioning");

  const isPalette = PALETTES.has(t);
  // Light palettes build on light defaults; OLED and Barcelona build on dark defaults.
  const darkBase =
    t === "dark" || t === "oled" || (isPalette && !LIGHT_PALETTES.has(t));
  root.classList.toggle("dark", darkBase);
  root.classList.toggle("oled", t === "oled");
  if (isPalette) root.setAttribute("data-palette", t);
  else root.removeAttribute("data-palette");

  if (animate && !reduceMotion) {
    themeTransitionTimer = window.setTimeout(() => {
      root.classList.remove("theme-transitioning");
      themeTransitionTimer = undefined;
    }, 180);
  }
}

const initial = read();
const initialWallpaper = readWallpaper();
const initialNatureWallpaper = readNatureWallpaper();
const initialCatAcrylicWallpaper = readCatAcrylicWallpaper();
apply(initial, false); // before first paint — no animation
applyWallpaper(initialWallpaper);
applyNatureWallpaper(initialNatureWallpaper);
applyCatAcrylicWallpaper(initialCatAcrylicWallpaper);

interface ThemeState {
  theme: Theme;
  wallpaperEnabled: boolean;
  natureWallpaperId: string;
  catAcrylicWallpaperId: string;
  /** Quick light↔dark toggle (the sidebar icon button); from any brand/oled it lands on dark. */
  toggle: () => void;
  /** Set an explicit theme (the Settings picker). */
  set: (t: Theme) => void;
  setWallpaperEnabled: (enabled: boolean) => void;
  setNatureWallpaper: (id: string) => void;
  setCatAcrylicWallpaper: (id: string) => void;
}

function persist(t: Theme, animate: boolean) {
  if (typeof localStorage !== "undefined") localStorage.setItem(KEY, t);
  apply(t, animate);
}

export const useTheme = create<ThemeState>((set, get) => ({
  theme: initial,
  wallpaperEnabled: initialWallpaper,
  natureWallpaperId: initialNatureWallpaper,
  catAcrylicWallpaperId: initialCatAcrylicWallpaper,
  toggle: () => {
    const next: Theme = get().theme === "light" ? "dark" : "light";
    persist(next, true);
    set({ theme: next });
  },
  set: (next: Theme) => {
    persist(next, true);
    set({ theme: next });
  },
  setWallpaperEnabled: (enabled: boolean) => {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(WALLPAPER_KEY, enabled ? "on" : "off");
    }
    applyWallpaper(enabled);
    set({ wallpaperEnabled: enabled });
  },
  setNatureWallpaper: (id: string) => {
    const wallpaper = getNatureWallpaper(id);
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(NATURE_WALLPAPER_KEY, wallpaper.id);
    }
    applyNatureWallpaper(wallpaper.id);
    persist("nature", true);
    set({ theme: "nature", natureWallpaperId: wallpaper.id });
  },
  setCatAcrylicWallpaper: (id: string) => {
    const wallpaper = getCatAcrylicWallpaper(id);
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(CAT_ACRYLIC_WALLPAPER_KEY, wallpaper.id);
    }
    applyCatAcrylicWallpaper(wallpaper.id);
    persist("cat-acrylic", true);
    set({ theme: "cat-acrylic", catAcrylicWallpaperId: wallpaper.id });
  },
}));
