import { useState } from "react";
import { Camera, ImagePlus, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { pickImageFile } from "@/lib/avatarImage";
import { useAvatar, useAvatars } from "@/store/avatars";
import { cn } from "@/lib/utils";
import { AvatarCropDialog } from "@/features/chat/AvatarCropDialog";

/**
 * AvatarEditMenu — wraps an identity glyph (its `children`) in a button that opens a small
 * popover to change or remove the custom avatar for `id`. Reused for the user's own photo
 * (Sidebar header) and per-contact photos (VerifyContactDialog). The pick → canvas-resize
 * → setAvatar flow lives in `pickAndResizeAvatar`; clearing reverts to the glyph.
 */
export function AvatarEditMenu({
  id,
  children,
  className,
  ariaLabel,
}: {
  id: string;
  children: React.ReactNode;
  className?: string;
  ariaLabel: string;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [cropFile, setCropFile] = useState<File | null>(null);
  const hasPhoto = useAvatar(id) !== undefined;
  const setAvatar = useAvatars((s) => s.setAvatar);

  const choose = async () => {
    setOpen(false);
    const file = await pickImageFile();
    if (file) setCropFile(file);
  };
  const remove = () => {
    setOpen(false);
    void setAvatar(id, null);
  };

  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label={ariaLabel}
            className={cn(
              "group/avatar relative rounded-[28%] outline-none focus-visible:ring-2 focus-visible:ring-ring",
              className,
            )}
          >
            {children}
            {/* Hover hint that this avatar is editable. */}
            <span
              aria-hidden
              className="absolute inset-0 flex items-center justify-center rounded-[28%] bg-black/45 opacity-0 transition-opacity group-hover/avatar:opacity-100"
            >
              <Camera className="h-4 w-4 text-white" />
            </span>
          </button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-48 p-1">
          <button
            type="button"
            onClick={() => void choose()}
            className="flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left text-sm hover:bg-accent"
          >
            <ImagePlus className="h-4 w-4 text-muted-foreground" />
            {hasPhoto ? t("avatar.change") : t("avatar.set")}
          </button>
          {hasPhoto && (
            <button
              type="button"
              onClick={remove}
              className="flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left text-sm text-destructive hover:bg-accent"
            >
              <Trash2 className="h-4 w-4" />
              {t("avatar.remove")}
            </button>
          )}
        </PopoverContent>
      </Popover>
      <AvatarCropDialog
        file={cropFile}
        onConfirm={(dataUrl) => {
          setCropFile(null);
          void setAvatar(id, dataUrl);
        }}
        onCancel={() => setCropFile(null)}
      />
    </>
  );
}
