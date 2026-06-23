import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { ALL_THEMES, useTheme, type Theme } from "@/lib/theme";
import { THEME_CREST } from "@/lib/themeCrest";

// Representative preview swatches per theme: [surface, crest accent, secondary accent].
// These mirror the palette tokens in index.css so a card previews the real look.
const SWATCH: Record<Theme, [string, string, string]> = {
  dark: ["hsl(220 24% 7%)", "hsl(172 66% 50%)", "hsl(220 18% 16%)"],
  light: ["hsl(210 20% 98%)", "hsl(174 72% 33%)", "hsl(215 20% 86%)"],
  oled: ["hsl(0 0% 0%)", "hsl(172 66% 50%)", "hsl(220 14% 16%)"],
  argentina: ["hsl(205 55% 95%)", "hsl(202 84% 46%)", "hsl(38 90% 42%)"],
  barcelona: ["hsl(224 46% 9%)", "hsl(344 72% 52%)", "hsl(45 88% 58%)"],
  messi: ["hsl(208 46% 95%)", "hsl(214 82% 46%)", "hsl(38 88% 42%)"],
};

/** A gallery of theme cards, each previewing its palette; click to apply (with a crossfade). */
export function ThemePicker() {
  const { t } = useTranslation();
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.set);

  return (
    <div data-testid="theme-picker" className="grid grid-cols-3 gap-2">
      {ALL_THEMES.map((id) => {
        const [bg, a1, a2] = SWATCH[id];
        const crest = THEME_CREST[id];
        const active = theme === id;
        return (
          <button
            key={id}
            type="button"
            data-testid={`theme-${id}`}
            aria-pressed={active}
            onClick={() => setTheme(id)}
            className={cn(
              "group flex flex-col gap-2 rounded-xl border p-1.5 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
              active
                ? "border-signal ring-2 ring-signal/35"
                : "border-border hover:border-muted-foreground/40",
            )}
          >
            <span
              className="flex h-12 items-center justify-center overflow-hidden rounded-lg ring-1 ring-inset ring-white/5"
              style={{ background: bg }}
            >
              {crest ? (
                // Brand themes lead with their crest/emblem — that's the identity.
                <img
                  src={crest}
                  alt=""
                  className="h-9 w-9 object-contain drop-shadow"
                />
              ) : (
                // Base themes show the palette as two swatch dots.
                <span className="flex items-center gap-1.5">
                  <span
                    className="h-4 w-4 rounded-full shadow-sm ring-1 ring-white/20"
                    style={{ background: a1 }}
                  />
                  <span
                    className="h-3 w-3 rounded-full ring-1 ring-white/15"
                    style={{ background: a2 }}
                  />
                </span>
              )}
            </span>
            <span className="truncate px-1 pb-0.5 text-center text-xs font-medium">
              {t(`settings.theme_${id}`)}
            </span>
          </button>
        );
      })}
    </div>
  );
}
