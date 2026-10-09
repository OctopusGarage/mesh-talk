import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { ALL_THEMES, useTheme, type Theme } from "@/lib/theme";
import { THEME_CREST } from "@/lib/themeCrest";
import { getNatureWallpaper, NATURE_WALLPAPERS } from "@/lib/natureWallpapers";

// Representative preview swatches per theme: [surface, crest accent, secondary accent].
// These mirror the palette tokens in index.css so a card previews the real look.
const SWATCH: Record<Theme, [string, string, string]> = {
  dark: ["hsl(185 20% 8%)", "hsl(163 42% 61%)", "hsl(178 17% 13%)"],
  light: ["hsl(156 12% 97%)", "hsl(163 49% 33%)", "hsl(155 12% 95%)"],
  oled: ["hsl(0 0% 0%)", "hsl(163 42% 61%)", "hsl(220 12% 5%)"],
  argentina: ["hsl(205 55% 95%)", "hsl(202 84% 46%)", "hsl(38 90% 42%)"],
  barcelona: ["hsl(224 46% 9%)", "hsl(344 72% 52%)", "hsl(45 88% 58%)"],
  messi: ["hsl(208 46% 95%)", "hsl(214 82% 46%)", "hsl(38 88% 42%)"],
  nature: ["hsl(44 34% 96%)", "hsl(154 42% 31%)", "hsl(148 20% 87%)"],
};

/** A gallery of theme cards, each previewing its palette; click to apply (with a crossfade). */
export function ThemePicker() {
  const { t } = useTranslation();
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.set);
  const natureWallpaperId = useTheme((s) => s.natureWallpaperId);
  const setNatureWallpaper = useTheme((s) => s.setNatureWallpaper);
  const natureWallpaper = getNatureWallpaper(natureWallpaperId);

  const cards = (themes: Theme[]) =>
    themes.map((id) => {
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
            "group flex flex-col gap-2 rounded-lg border p-1.5 text-left",
            active
              ? "border-signal bg-signal/5"
              : "border-border hover:border-muted-foreground/40",
          )}
        >
          <span
            className="flex h-12 items-center justify-center overflow-hidden rounded-lg ring-1 ring-inset ring-white/5"
            style={{ background: bg }}
          >
            {id === "nature" ? (
              <img
                src={natureWallpaper.url}
                alt=""
                className="h-full w-full object-cover"
              />
            ) : crest ? (
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
                  className="h-4 w-4 rounded-full ring-1 ring-white/20"
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
    });

  return (
    <div data-testid="theme-picker" className="space-y-4">
      <div className="grid grid-cols-3 gap-2">
        {cards(ALL_THEMES.slice(0, 3))}
      </div>
      <div>
        <p className="mb-2 text-xs font-medium text-muted-foreground">
          {t("redesign.personalThemes")}
        </p>
        <div className="grid grid-cols-2 gap-2 lg:grid-cols-4">
          {cards(ALL_THEMES.slice(3))}
        </div>
        {theme === "nature" && (
          <div className="mt-3" data-testid="nature-wallpaper-picker">
            <p className="mb-2 text-xs font-medium text-muted-foreground">
              {t("settings.natureWallpapers")}
            </p>
            <div className="grid max-h-64 grid-cols-3 gap-2 overflow-y-auto rounded-lg border border-border bg-card p-2 sm:grid-cols-4">
              {NATURE_WALLPAPERS.map((wallpaper) => (
                <button
                  key={wallpaper.id}
                  type="button"
                  data-testid={`nature-wallpaper-${wallpaper.id}`}
                  aria-label={wallpaper.title}
                  aria-pressed={natureWallpaperId === wallpaper.id}
                  title={wallpaper.title}
                  onClick={() => setNatureWallpaper(wallpaper.id)}
                  className={cn(
                    "overflow-hidden rounded-md border text-left transition-colors hover:border-signal focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    natureWallpaperId === wallpaper.id
                      ? "border-signal ring-2 ring-signal"
                      : "border-border",
                  )}
                >
                  <img
                    src={wallpaper.url}
                    alt=""
                    loading="lazy"
                    className="aspect-[3/2] w-full object-cover"
                  />
                  <span className="block truncate px-1.5 py-1 text-[11px]">
                    {wallpaper.title}
                  </span>
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
