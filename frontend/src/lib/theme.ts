import { create } from "zustand";

// Themes come in two kinds:
//  • base modes of the default "Ink & Signal" look — light / dark / oled (a true-black dark)
//  • football-brand PALETTES — argentina / barcelona / messi — each a complete, cohesive
//    look applied via a `data-palette` attribute on top of a dark base (so any token a
//    palette doesn't override falls back to the dark scale, never the light one).
export type Theme =
  "light" | "dark" | "oled" | "argentina" | "barcelona" | "messi";

/** Brand palettes (driven by `html[data-palette=…]`); the rest are base modes. */
const PALETTES = new Set<Theme>(["argentina", "barcelona", "messi"]);

/** Brand palettes that build on the LIGHT base (airy white/blue) — Argentina (Albiceleste)
 *  and Messi. Barcelona (blaugrana) stays dark. */
const LIGHT_PALETTES = new Set<Theme>(["argentina", "messi"]);

/** Every selectable theme, in display order. */
export const ALL_THEMES: Theme[] = [
  "dark",
  "light",
  "oled",
  "argentina",
  "barcelona",
  "messi",
];

const KEY = "mesh-talk-theme";
const WALLPAPER_KEY = "mesh-talk-wallpaper";
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
  // Light brand themes (Argentina, Messi) build on the light base; oled + every dark brand
  // builds on the dark base.
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
apply(initial, false); // before first paint — no animation
applyWallpaper(initialWallpaper);

interface ThemeState {
  theme: Theme;
  wallpaperEnabled: boolean;
  /** Quick light↔dark toggle (the sidebar icon button); from any brand/oled it lands on dark. */
  toggle: () => void;
  /** Set an explicit theme (the Settings picker). */
  set: (t: Theme) => void;
  setWallpaperEnabled: (enabled: boolean) => void;
}

function persist(t: Theme, animate: boolean) {
  if (typeof localStorage !== "undefined") localStorage.setItem(KEY, t);
  apply(t, animate);
}

export const useTheme = create<ThemeState>((set, get) => ({
  theme: initial,
  wallpaperEnabled: initialWallpaper,
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
}));
