import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import { ALL_THEMES, selectedPackWallpaper, useTheme } from "@/lib/theme";
import { usePacks } from "@/store/packs";
import { useThemePreview } from "@/lib/usePack";
import { PackManager } from "@/components/PackManager";

const BASE_SWATCH: Record<string, [string, string, string]> = {
  dark: ["hsl(176 13% 9%)", "hsl(163 42% 61%)", "hsl(176 11% 14%)"],
  light: ["hsl(44 19% 96%)", "hsl(163 43% 30%)", "hsl(43 17% 92%)"],
  oled: ["hsl(0 0% 0%)", "hsl(163 42% 61%)", "hsl(220 12% 5%)"],
};

export function ThemePicker() {
  const { t } = useTranslation();
  const selection = useRef(0);
  const currentScope = usePacks((s) => s.scope);
  useEffect(
    () => () => {
      selection.current++;
    },
    [],
  );
  const [failedSelection, setFailedSelection] = useState<{
    id: string;
    scope: ReturnType<typeof usePacks.getState>["scope"];
  }>();
  const selectTheme = async (id: string) => {
    setFailedSelection(undefined);
    const request = ++selection.current;
    const scope = usePacks.getState().scope;
    if (ALL_THEMES.includes(id)) {
      setTheme(id);
      return;
    }
    try {
      const prepared = await usePacks.getState().prepareTheme(id);
      if (!prepared) throw new Error("Theme preparation superseded");
      if (selection.current === request && usePacks.getState().scope === scope)
        setTheme(id);
    } catch {
      if (selection.current === request && usePacks.getState().scope === scope)
        setFailedSelection({ id, scope });
    }
  };
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.set);
  const wallpaperIds = useTheme((s) => s.packWallpaperIds);
  const setWallpaper = useTheme((s) => s.setPackWallpaper);
  const packs = usePacks((s) => s.packs).filter(
    (pack) => pack.kind === "theme",
  );
  const hydrated = usePacks((s) => s.selectedTheme);
  const current = hydrated?.kind === "theme" ? hydrated : undefined;
  const selectedWallpaperId = current
    ? selectedPackWallpaper(current, wallpaperIds)?.id
    : undefined;

  const cards = (
    items: { id: string; name: string; swatch: [string, string, string] }[],
  ) =>
    items.map((item) => (
      <ThemePreviewCard
        key={item.id}
        {...item}
        wallpaperId={wallpaperIds[item.id]}
        selected={theme === item.id}
        select={() => void selectTheme(item.id)}
      />
    ));

  return (
    <div data-testid="theme-picker" className="space-y-4">
      {failedSelection && failedSelection.scope === currentScope && (
        <p role="alert" className="text-sm">
          {t("common.loadFailed")}{" "}
          <button
            type="button"
            className="underline"
            onClick={() => void selectTheme(failedSelection.id)}
          >
            {t("common.retry")}
          </button>
        </p>
      )}
      <div className="grid grid-cols-3 gap-2">
        {cards(
          ALL_THEMES.map((id) => ({
            id,
            name: t(`settings.theme_${id}`),
            swatch: BASE_SWATCH[id],
          })),
        )}
      </div>
      <div>
        <p className="mb-2 text-xs font-medium text-muted-foreground">
          {t("redesign.personalThemes")}
        </p>
        {packs.length ? (
          <div className="grid grid-cols-2 gap-2 lg:grid-cols-4">
            {cards(
              packs.map((pack) => ({
                id: pack.id,
                name: pack.name,
                swatch: [
                  `hsl(${pack.colors.background ?? "185 20% 8%"})`,
                  `hsl(${pack.colors.signal ?? pack.colors.primary ?? "163 42% 61%"})`,
                  `hsl(${pack.colors["shell-rail"] ?? pack.colors.secondary ?? "178 17% 13%"})`,
                ] as [string, string, string],
              })),
            )}
          </div>
        ) : (
          <p className="rounded-lg border border-dashed border-border p-3 text-xs text-muted-foreground">
            {t("packs.noThemes")}
          </p>
        )}
        {current?.wallpapers && (
          <div
            className="mt-3"
            data-testid={
              current.id === "nature"
                ? "nature-wallpaper-picker"
                : current.id === "cat-acrylic"
                  ? "cat-acrylic-wallpaper-picker"
                  : "pack-wallpaper-picker"
            }
          >
            <p className="mb-2 text-xs font-medium text-muted-foreground">
              {t(
                current.id === "nature"
                  ? "settings.natureWallpapers"
                  : current.id === "cat-acrylic"
                    ? "settings.catAcrylicWallpapers"
                    : "settings.wallpaper",
              )}
            </p>
            <div className="grid max-h-64 grid-cols-3 gap-2 overflow-y-auto rounded-lg border border-border bg-card p-2 sm:grid-cols-4">
              {current.wallpapers.map((wallpaper) => (
                <button
                  key={wallpaper.id}
                  type="button"
                  data-testid={`${current.id}-wallpaper-${wallpaper.id}`}
                  aria-label={wallpaper.title}
                  aria-pressed={selectedWallpaperId === wallpaper.id}
                  onClick={() => setWallpaper(wallpaper.id)}
                  className={cn(
                    "overflow-hidden rounded-md border text-left focus-visible:ring-2 focus-visible:ring-ring",
                    selectedWallpaperId === wallpaper.id
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
      <PackManager kind="theme" />
    </div>
  );
}

/** Cards hydrate only while intersecting the visible settings panel. */
function ThemePreviewCard({
  id,
  name,
  swatch,
  wallpaperId,
  selected,
  select,
}: {
  id: string;
  name: string;
  swatch: [string, string, string];
  wallpaperId?: string;
  selected: boolean;
  select: () => void;
}) {
  const { t } = useTranslation();
  const target = useRef<HTMLButtonElement>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    if (!target.current) return;
    if (typeof IntersectionObserver === "undefined") {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver((entries) =>
      setVisible(entries.some((entry) => entry.isIntersecting)),
    );
    observer.observe(target.current);
    return () => observer.disconnect();
  }, []);
  const preview = useThemePreview(id, wallpaperId, visible);
  return (
    <div className="min-w-0">
      <button
        ref={target}
        type="button"
        data-testid={`theme-${id}`}
        aria-pressed={selected}
        onClick={select}
        className={cn(
          "group flex min-w-0 flex-col gap-2 w-full rounded-lg border p-1.5 text-left",
          selected
            ? "border-signal bg-signal/5"
            : "border-border hover:border-muted-foreground/40",
        )}
      >
        <span
          data-testid={`theme-preview-${id}`}
          data-preview-signal={swatch[1]}
          data-preview-rail={swatch[2]}
          className="flex h-12 items-center justify-center overflow-hidden rounded-lg ring-1 ring-inset ring-white/5"
          style={{ background: swatch[0] }}
        >
          {preview.value ? (
            <img
              src={preview.value}
              alt=""
              className="h-full w-full object-cover"
            />
          ) : (
            <span className="flex items-center gap-1.5">
              <span
                className="h-4 w-4 rounded-full ring-1 ring-white/20"
                style={{ background: swatch[1] }}
              />
              <span
                className="h-3 w-3 rounded-full ring-1 ring-white/15"
                style={{ background: swatch[2] }}
              />
            </span>
          )}
        </span>
        <span className="truncate px-1 pb-0.5 text-center text-xs font-medium">
          {name}
        </span>
      </button>
      {preview.error && (
        <p role="alert" className="text-xs">
          {t("common.loadFailed")}{" "}
          <button type="button" onClick={preview.retry} className="underline">
            {t("common.retry")}
          </button>
        </p>
      )}
    </div>
  );
}
