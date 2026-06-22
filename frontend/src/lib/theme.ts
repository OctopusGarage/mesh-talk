import { create } from "zustand";

// "oled" is a pure-black dark variant (true-black backgrounds, easy on OLED panels);
// it carries the `dark` class plus an `oled` class that overrides the dark palette's
// near-black surfaces with #000.
export type Theme = "light" | "dark" | "oled";

const KEY = "mesh-talk-theme";

function read(): Theme {
  if (typeof localStorage === "undefined") return "dark";
  const v = localStorage.getItem(KEY);
  return v === "light" || v === "oled" ? v : "dark";
}

function apply(t: Theme) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  root.classList.toggle("dark", t === "dark" || t === "oled");
  root.classList.toggle("oled", t === "oled");
}

const initial = read();
apply(initial); // run on import so the theme is set before/at first paint

interface ThemeState {
  theme: Theme;
  /** Quick light↔dark toggle (the sidebar icon button). An OLED theme toggles to light. */
  toggle: () => void;
  /** Set an explicit theme (the Settings select). */
  set: (t: Theme) => void;
}

function persist(t: Theme) {
  if (typeof localStorage !== "undefined") localStorage.setItem(KEY, t);
  apply(t);
}

export const useTheme = create<ThemeState>((set, get) => ({
  theme: initial,
  toggle: () => {
    const next: Theme = get().theme === "light" ? "dark" : "light";
    persist(next);
    set({ theme: next });
  },
  set: (next: Theme) => {
    persist(next);
    set({ theme: next });
  },
}));
