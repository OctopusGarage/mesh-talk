import { useEffect, useRef, useState } from "react";
import { Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { presetToAvatarDataUrl } from "@/lib/avatarImage";
import {
  CLUB_AVATARS,
  PLAYER_AVATARS,
  NBA_TEAM_AVATARS,
  CITY_AVATARS,
  NBA_PLAYER_AVATARS,
  SPORTS_AVATARS,
  FAMOUS_AVATARS,
  type AvatarPackName,
} from "@/lib/avatarPacks";
import cityCredits from "@/assets/avatars/cities/credits.json";

export type AvatarGalleryCategory = "personal" | "group";

const EAGER_THUMBNAILS = 24;
const EMPTY_THUMBNAIL =
  "data:image/gif;base64,R0lGODlhAQABAAD/ACwAAAAAAQABAAACADs=";

interface Tab {
  id: AvatarPackName;
  label: string;
  presets: { label: string; url: string }[];
  /** "cover" fills the square (photos); "contain" keeps the logo shape */
  fit: "cover" | "contain";
}

const PERSONAL_TABS = (t: (k: string) => string): Tab[] => [
  {
    id: "players",
    label: t("avatar.tabFootballStars"),
    presets: PLAYER_AVATARS,
    fit: "cover",
  },
  {
    id: "nba-players",
    label: t("avatar.tabNbaStars"),
    presets: NBA_PLAYER_AVATARS,
    fit: "cover",
  },
  {
    id: "sports",
    label: t("avatar.tabSportsStars"),
    presets: SPORTS_AVATARS,
    fit: "cover",
  },
  {
    id: "famous",
    label: t("avatar.tabFamousPeople"),
    presets: FAMOUS_AVATARS,
    fit: "cover",
  },
];

const GROUP_TABS = (t: (k: string) => string): Tab[] => [
  {
    id: "cities",
    label: t("avatar.tabCities"),
    presets: CITY_AVATARS,
    fit: "contain",
  },
  {
    id: "clubs",
    label: t("avatar.tabFootballClubs"),
    presets: CLUB_AVATARS,
    fit: "contain",
  },
  {
    id: "nba-teams",
    label: t("avatar.tabNbaTeams"),
    presets: NBA_TEAM_AVATARS,
    fit: "contain",
  },
];

/**
 * A grid of built-in preset avatars with tab navigation. Two categories:
 * - "personal": Football | NBA | Sports Stars | Famous People (cover fit)
 * - "group":    City paintings | Football Clubs | NBA Teams (contain fit)
 *
 * Clicking a preset normalizes it to the same 256×256 JPEG an upload produces and
 * hands it back via `onPick`, so the caller stores it exactly like a custom photo.
 */
export function AvatarGallery({
  category,
  open,
  onPick,
  onClose,
}: {
  category: AvatarGalleryCategory;
  open: boolean;
  onPick: (dataUrl: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<AvatarPackName>(
    category === "personal" ? "players" : "cities",
  );
  const [busy, setBusy] = useState<string | null>(null);
  const [visibleUrls, setVisibleUrls] = useState<Set<string>>(() => new Set());
  const gridRef = useRef<HTMLDivElement>(null);

  const tabs = category === "personal" ? PERSONAL_TABS(t) : GROUP_TABS(t);

  // Sync active tab when category changes externally (e.g. dialog re-opened)
  const defaultTab = category === "personal" ? "players" : "cities";
  if (!tabs.find((t) => t.id === activeTab)) {
    setActiveTab(defaultTab);
  }

  const currentTab = tabs.find((t) => t.id === activeTab) ?? tabs[0];
  const presets = currentTab.presets;

  useEffect(() => {
    if (!open || !gridRef.current) return;
    const grid = gridRef.current;
    const deferred = grid.querySelectorAll<HTMLElement>(
      "[data-avatar-deferred]",
    );
    if (deferred.length === 0) return;

    if (!("IntersectionObserver" in window)) {
      setVisibleUrls((previous) => {
        const next = new Set(previous);
        for (const item of deferred) {
          if (item.dataset.avatarUrl) next.add(item.dataset.avatarUrl);
        }
        return next;
      });
      return;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        const newlyVisible = entries
          .filter((entry) => entry.isIntersecting)
          .map((entry) => (entry.target as HTMLElement).dataset.avatarUrl)
          .filter((url): url is string => !!url);
        if (newlyVisible.length === 0) return;
        setVisibleUrls((previous) => {
          const next = new Set(previous);
          for (const url of newlyVisible) next.add(url);
          return next;
        });
        for (const entry of entries) {
          if (entry.isIntersecting) observer.unobserve(entry.target);
        }
      },
      { root: grid, rootMargin: "120px 0px" },
    );
    deferred.forEach((item) => observer.observe(item));
    return () => observer.disconnect();
  }, [activeTab, open, visibleUrls]);

  const choose = async (url: string) => {
    setBusy(url);
    try {
      onPick(await presetToAvatarDataUrl(url, currentTab.fit));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        if (!o) onClose();
      }}
    >
      <DialogContent className="gap-0 overflow-hidden p-0 sm:max-w-[740px]">
        <DialogHeader className="border-b px-5 pb-5 pt-6 pr-14 sm:px-7 sm:pr-16">
          <DialogTitle className="text-xl leading-7">
            {t("avatar.galleryTitle")}
          </DialogTitle>
          <DialogDescription>{t("avatar.galleryHint")}</DialogDescription>
        </DialogHeader>

        <div className="min-h-0 sm:grid sm:grid-cols-[176px_minmax(0,1fr)]">
          <nav
            aria-label={t("avatar.galleryTitle")}
            className="flex gap-1 overflow-x-auto border-b bg-muted/40 p-2.5 sm:flex-col sm:gap-1.5 sm:overflow-x-visible sm:border-b-0 sm:border-r sm:p-4"
          >
            {tabs.map((tab) => (
              <button
                key={tab.id}
                type="button"
                aria-pressed={activeTab === tab.id}
                onClick={() => setActiveTab(tab.id)}
                className={`shrink-0 rounded-lg border-l-2 px-3 py-2.5 text-left text-sm font-medium whitespace-nowrap outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-popover sm:w-full ${
                  activeTab === tab.id
                    ? "border-primary bg-popover text-foreground shadow-sm"
                    : "border-transparent text-muted-foreground hover:bg-popover/70 hover:text-foreground"
                }`}
              >
                {tab.label}
              </button>
            ))}
          </nav>

          <div className="min-w-0">
            <div
              ref={gridRef}
              data-testid="avatar-gallery"
              className="grid max-h-[min(54vh,480px)] grid-cols-3 content-start gap-2 overflow-y-auto p-3 sm:max-h-[min(60vh,520px)] sm:grid-cols-4 sm:gap-3 sm:p-5"
            >
              {presets.map((p, index) => (
                <button
                  key={p.url}
                  data-avatar-url={p.url}
                  data-avatar-deferred={
                    index >= EAGER_THUMBNAILS && !visibleUrls.has(p.url)
                      ? "true"
                      : undefined
                  }
                  type="button"
                  aria-busy={busy === p.url}
                  onClick={() => void choose(p.url)}
                  disabled={busy !== null}
                  title={p.label}
                  className="flex min-w-0 flex-col items-center gap-2 rounded-xl border border-transparent bg-muted/45 p-2.5 text-center outline-none transition-colors hover:border-border hover:bg-accent/70 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-popover disabled:opacity-50"
                >
                  <div className="relative h-16 w-16 shrink-0 rounded-[28%] ring-1 ring-border/70 sm:h-[76px] sm:w-[76px]">
                    <img
                      src={
                        index < EAGER_THUMBNAILS || visibleUrls.has(p.url)
                          ? p.url
                          : EMPTY_THUMBNAIL
                      }
                      alt=""
                      loading="lazy"
                      decoding="async"
                      className="h-full w-full rounded-[28%] bg-secondary object-cover"
                      style={
                        currentTab.fit === "contain"
                          ? { objectFit: "contain" }
                          : undefined
                      }
                    />
                    {busy === p.url && (
                      <span className="absolute inset-0 flex items-center justify-center rounded-[28%] bg-black/40">
                        <Loader2 className="h-5 w-5 animate-spin text-white" />
                      </span>
                    )}
                  </div>
                  <span className="line-clamp-2 min-h-8 w-full text-xs leading-4 text-foreground/85">
                    {p.label}
                  </span>
                </button>
              ))}
            </div>
            {activeTab === "cities" && (
              <details className="border-t px-4 py-3 text-xs text-muted-foreground sm:px-5">
                <summary className="cursor-pointer font-medium text-foreground">
                  {t("avatar.cityCredits")}
                </summary>
                <p className="mt-2">{t("avatar.cityCreditsIntro")}</p>
                <ul className="mt-2 max-h-32 space-y-1 overflow-y-auto">
                  {cityCredits.map((credit) => (
                    <li key={credit.file}>
                      {credit.city}: {credit.artist} · {credit.license} ·{" "}
                      <a
                        href={credit.source}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="underline underline-offset-2"
                      >
                        {t("avatar.sourcePhoto")}
                      </a>
                      {credit.licenseUrl && (
                        <>
                          {" · "}
                          <a
                            href={credit.licenseUrl}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="underline underline-offset-2"
                          >
                            {t("avatar.license")}
                          </a>
                        </>
                      )}
                    </li>
                  ))}
                </ul>
              </details>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
