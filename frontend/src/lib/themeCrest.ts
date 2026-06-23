import type { Theme } from "@/lib/theme";
import barcelonaCrest from "@/assets/themes/barcelona-crest.svg";
import argentinaCrest from "@/assets/themes/argentina-crest.png";
import messiCrest from "@/assets/themes/messi-crest.png";

// The emblem for each brand theme — the club/national crest, or Messi's portrait mark. Base
// themes (dark/light/oled) have none (they're the app's own look, represented by a swatch).
export const THEME_CREST: Partial<Record<Theme, string>> = {
  barcelona: barcelonaCrest,
  argentina: argentinaCrest,
  messi: messiCrest,
};
