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
import { usePacks } from "@/store/packs";
import { PackManager } from "@/components/PackManager";
import type { AvatarPack } from "@/lib/pack";

export type AvatarGalleryCategory = "personal" | "group";

/**
 * A grid of installed avatar libraries with tab navigation for people or channels.
 *
 * Clicking an image normalizes it to the same 256×256 JPEG an upload produces and
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
  const [activeTab, setActiveTab] = useState<string>("");
  const [busy, setBusy] = useState<string | null>(null);

  const tabs = usePacks((state) => state.packs).filter(
    (pack): pack is AvatarPack =>
      pack.kind === "avatar" && pack.category === category,
  );
  const currentTab = tabs.find((tab) => tab.id === activeTab) ?? tabs[0];
  const presets = currentTab?.avatars ?? [];

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
                aria-pressed={currentTab?.id === tab.id}
                onClick={() => setActiveTab(tab.id)}
                className={`shrink-0 rounded-lg px-3 py-2.5 text-left text-sm font-medium whitespace-nowrap outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-popover sm:w-full ${
                  currentTab?.id === tab.id
                    ? "bg-popover font-semibold text-foreground shadow-sm"
                    : "text-muted-foreground hover:bg-popover/70 hover:text-foreground"
                }`}
              >
                {tab.name}
              </button>
            ))}
          </nav>

          <div className="min-w-0">
            <div
              data-testid="avatar-gallery"
              className="grid max-h-[min(54vh,480px)] grid-cols-3 content-start gap-2 overflow-y-auto p-3 sm:max-h-[min(60vh,520px)] sm:grid-cols-4 sm:gap-3 sm:p-5"
            >
              {presets.length === 0 && (
                <p className="col-span-full p-4 text-center text-sm text-muted-foreground">
                  {t("packs.noAvatars")}
                </p>
              )}
              {presets.map((p) => (
                <button
                  key={p.url}
                  type="button"
                  aria-busy={busy === p.url}
                  onClick={() => void choose(p.url)}
                  disabled={busy !== null}
                  title={p.label}
                  className="flex min-w-0 flex-col items-center gap-2 rounded-xl border border-transparent bg-muted/45 p-2.5 text-center outline-none transition-colors hover:border-border hover:bg-accent/70 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-popover disabled:opacity-50"
                >
                  <div className="relative h-16 w-16 shrink-0 rounded-[28%] ring-1 ring-border/70 sm:h-[76px] sm:w-[76px]">
                    <img
                      src={p.url}
                      alt=""
                      loading="lazy"
                      decoding="async"
                      className="h-full w-full rounded-[28%] bg-secondary object-cover"
                      style={
                        currentTab?.fit === "contain"
                          ? { objectFit: "contain" }
                          : undefined
                      }
                    />
                    {busy === p.url && (
                      <span className="absolute inset-0 flex items-center justify-center rounded-[28%] bg-black/40">
                        <Loader2 className="h-5 w-5 animate-spin text-white motion-reduce:hidden" />
                        <span className="hidden max-w-full rounded bg-popover px-1 py-0.5 text-center text-[10px] leading-tight font-semibold text-popover-foreground motion-reduce:block">
                          {t("common.loading")}
                        </span>
                      </span>
                    )}
                  </div>
                  <span className="line-clamp-2 min-h-8 w-full text-xs leading-4 text-foreground/85">
                    {p.label}
                  </span>
                </button>
              ))}
            </div>
          </div>
        </div>
        <div className="max-h-52 overflow-y-auto border-t px-5 py-3 sm:px-7">
          <PackManager kind="avatar" category={category} />
        </div>
      </DialogContent>
    </Dialog>
  );
}
