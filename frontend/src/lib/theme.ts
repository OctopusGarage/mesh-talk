import { create } from "zustand";
import { installedTheme } from "@/store/packs";

// Base modes use the default Ink & Signal look. Football and nature themes apply
// their own palette via `data-palette`; nature also selects a separate wallpaper.
export type Theme = string;

/** Personal palettes (driven by `html[data-palette=…]`); the rest are base modes. */
/** Every selectable theme, in display order. */
export const ALL_THEMES: Theme[] = ["dark", "light", "oled"];

const KEY = "mesh-talk-theme";
const WALLPAPER_KEY = "mesh-talk-wallpaper";
const PACK_WALLPAPER_KEY = "mesh-talk-pack-wallpaper";
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
  return v && /^[a-z0-9][a-z0-9._-]{2,79}$/.test(v) ? v : "dark";
}

function readPackWallpaper(): string {
  return typeof localStorage === "undefined"
    ? ""
    : (localStorage.getItem(PACK_WALLPAPER_KEY) ?? "");
}

let activeTokens: string[] = [];

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

  for (const token of activeTokens) root.style.removeProperty(`--${token}`);
  activeTokens = [];
  root.style.removeProperty("--pack-wallpaper-url");
  const pack = installedTheme(t);
  const darkBase =
    t === "dark" ||
    t === "oled" ||
    pack?.base === "dark" ||
    (!pack && !ALL_THEMES.includes(t));
  root.classList.toggle("dark", darkBase);
  root.classList.toggle("oled", t === "oled");
  if (pack) {
    for (const [token, value] of Object.entries(pack.colors)) {
      root.style.setProperty(`--${token}`, value);
      activeTokens.push(token);
    }
    const selected = pack.wallpapers?.find(
      (item) => item.id === readPackWallpaper(),
    );
    const wallpaper =
      selected?.url ?? pack.wallpapers?.[0]?.url ?? pack.wallpaper;
    if (wallpaper)
      root.style.setProperty("--pack-wallpaper-url", `url("${wallpaper}")`);
    root.setAttribute("data-pack-theme", "");
  } else root.removeAttribute("data-pack-theme");
  if (pack) root.setAttribute("data-palette", t);
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
apply(initial, false); // before first paint — no animation
applyWallpaper(initialWallpaper);

interface ThemeState {
  theme: Theme;
  wallpaperEnabled: boolean;
  packWallpaperId: string;
  /** Quick light↔dark toggle (the sidebar icon button); from any brand/oled it lands on dark. */
  toggle: () => void;
  /** Set an explicit theme (the Settings picker). */
  set: (t: Theme) => void;
  setWallpaperEnabled: (enabled: boolean) => void;
  setPackWallpaper: (id: string) => void;
  refresh: () => void;
}

function persist(t: Theme, animate: boolean) {
  if (typeof localStorage !== "undefined") localStorage.setItem(KEY, t);
  apply(t, animate);
}

export const useTheme = create<ThemeState>((set, get) => ({
  theme: initial,
  wallpaperEnabled: initialWallpaper,
  packWallpaperId: readPackWallpaper(),
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
  setPackWallpaper: (id: string) => {
    const pack = installedTheme(get().theme);
    const wallpaper = pack?.wallpapers?.find((item) => item.id === id);
    if (!wallpaper) return;
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(PACK_WALLPAPER_KEY, wallpaper.id);
    }
    apply(get().theme, true);
    set({ packWallpaperId: wallpaper.id });
  },
  refresh: () => {
    const current = get().theme;
    if (!ALL_THEMES.includes(current) && !installedTheme(current)) {
      persist("dark", false);
      set({ theme: "dark" });
    } else apply(current, false);
  },
}));
