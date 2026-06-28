import { useState } from "react";
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
  NBA_PLAYER_AVATARS,
  type AvatarPackName,
} from "@/lib/avatarPacks";

export type AvatarGalleryCategory = "personal" | "group";

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
];

const GROUP_TABS = (t: (k: string) => string): Tab[] => [
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
 * - "personal": Football Stars | NBA Stars  (cover fit — photos fill the square)
 * - "group":    Football Clubs | NBA Teams  (contain fit — logos keep their shape)
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
    category === "personal" ? "players" : "clubs",
  );
  const [busy, setBusy] = useState<string | null>(null);

  const tabs = category === "personal" ? PERSONAL_TABS(t) : GROUP_TABS(t);

  // Sync active tab when category changes externally (e.g. dialog re-opened)
  const defaultTab = category === "personal" ? "players" : "clubs";
  if (!tabs.find((t) => t.id === activeTab)) {
    setActiveTab(defaultTab);
  }

  const currentTab = tabs.find((t) => t.id === activeTab) ?? tabs[0];
  const presets = currentTab.presets;

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
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("avatar.galleryTitle")}</DialogTitle>
          <DialogDescription>{t("avatar.galleryHint")}</DialogDescription>
        </DialogHeader>

        {/* Tab bar */}
        <div className="flex gap-1 rounded-lg border bg-secondary/50 p-1">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              type="button"
              onClick={() => setActiveTab(tab.id)}
              className={`flex-1 rounded-md px-3 py-1.5 text-sm font-medium transition-all ${
                activeTab === tab.id
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground"
              }`}
            >
              {tab.label}
            </button>
          ))}
        </div>

        {/* Preset grid */}
        <div
          data-testid="avatar-gallery"
          className="grid max-h-[48vh] grid-cols-4 gap-3 overflow-y-auto p-1 sm:grid-cols-5"
        >
          {presets.map((p) => (
            <button
              key={p.url}
              type="button"
              onClick={() => void choose(p.url)}
              disabled={busy !== null}
              title={p.label}
              className="flex flex-col items-center gap-1 rounded-lg p-1.5 hover:bg-accent disabled:opacity-50"
            >
              <div className="relative h-14 w-14">
                <img
                  src={p.url}
                  alt={p.label}
                  className="h-14 w-14 rounded-[28%] bg-secondary object-cover"
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
              <span className="w-full truncate text-center text-[10px] text-muted-foreground">
                {p.label}
              </span>
            </button>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
