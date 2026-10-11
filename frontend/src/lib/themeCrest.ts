import type { Theme } from "@/lib/theme";

// The emblem for each brand theme — the club/national crest, or Messi's portrait mark. Base
// themes (dark/light/oled) have none (they're the app's own look, represented by a swatch).
export const THEME_CREST: Partial<Record<Theme, string>> = {
  barcelona: "/themes/barcelona-crest.svg",
  argentina: "/themes/argentina-crest.png",
  messi: "/themes/messi-crest.png",
};
