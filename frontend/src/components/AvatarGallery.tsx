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
import { avatarPack, type AvatarPackName } from "@/lib/avatarPacks";

/**
 * A grid of built-in preset avatars (a "pack"). Clicking one normalizes it to the same
 * 256×256 JPEG an upload produces and hands it back via `onPick`, so the caller stores it
 * exactly like a custom photo. Club logos use `contain` (keep their shape); player photos
 * use `cover` (fill the square).
 */
export function AvatarGallery({
  pack,
  open,
  onPick,
  onClose,
}: {
  pack: AvatarPackName;
  open: boolean;
  onPick: (dataUrl: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const presets = avatarPack(pack);
  const fit = pack === "clubs" ? "contain" : "cover";
  const [busy, setBusy] = useState<string | null>(null);

  const choose = async (url: string) => {
    setBusy(url);
    try {
      onPick(await presetToAvatarDataUrl(url, fit));
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
        <div
          data-testid="avatar-gallery"
          className="grid max-h-[58vh] grid-cols-4 gap-3 overflow-y-auto p-1 sm:grid-cols-5"
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
                  className="h-14 w-14 rounded-[28%] bg-secondary object-contain"
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
