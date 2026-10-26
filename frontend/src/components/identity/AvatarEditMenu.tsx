import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { Camera, ImagePlus, LayoutGrid, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { captureRuntimeOwner } from "@/store/ownership";
import { pickImageFile } from "@/lib/avatarImage";
import { useAvatar, useAvatars } from "@/store/avatars";
import { cn } from "@/lib/utils";
import { AvatarCropDialog } from "@/features/chat/AvatarCropDialog";
import type { AvatarGalleryCategory } from "@/components/AvatarGallery";

const AvatarGallery = lazy(() =>
  import("@/components/AvatarGallery").then(({ AvatarGallery }) => ({
    default: AvatarGallery,
  })),
);

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
  category,
}: {
  id: string;
  children: React.ReactNode;
  className?: string;
  ariaLabel: string;
  /** Gallery category offered alongside "upload" (personal for people, group for channels). */
  category?: AvatarGalleryCategory;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [cropFile, setCropFile] = useState<File | null>(null);
  const [gallery, setGallery] = useState(false);
  const [saveFailed, setSaveFailed] = useState(false);
  const [failedChoice, setFailedChoice] = useState<{
    dataUrl: string | null;
  } | null>(null);
  const attempt = useRef(0);
  const viewLifetime = useRef({ active: true });
  const currentId = useRef(id);
  currentId.current = id;
  useEffect(() => {
    const lifetime = { active: true };
    viewLifetime.current = lifetime;
    setSaveFailed(false);
    setFailedChoice(null);
    setCropFile(null);
    setGallery(false);
    setOpen(false);
    return () => {
      lifetime.active = false;
    };
  }, [id]);
  const hasPhoto = useAvatar(id) !== undefined;
  const setAvatar = useAvatars((s) => s.setAvatar);

  const save = async (dataUrl: string | null) => {
    const request = ++attempt.current;
    const lifetime = viewLifetime.current;
    const lease = captureRuntimeOwner();
    const target = id;
    setSaveFailed(false);
    setFailedChoice(null);
    setOpen(false);
    try {
      await setAvatar(target, dataUrl);
    } catch {
      if (
        request !== attempt.current ||
        !lifetime.active ||
        !lease.current() ||
        currentId.current !== target
      )
        return;
      setFailedChoice({ dataUrl });
      setSaveFailed(true);
      setOpen(true);
    }
  };
  const choose = async () => {
    const request = ++attempt.current;
    const lifetime = viewLifetime.current;
    const lease = captureRuntimeOwner();
    const target = id;
    setSaveFailed(false);
    setFailedChoice(null);
    setOpen(false);
    try {
      const file = await pickImageFile();
      if (
        request === attempt.current &&
        lifetime.active &&
        lease.current() &&
        currentId.current === target &&
        file
      )
        setCropFile(file);
    } catch {
      if (
        request === attempt.current &&
        lifetime.active &&
        lease.current() &&
        currentId.current === target
      ) {
        setSaveFailed(true);
        setOpen(true);
      }
    }
  };
  const remove = () => {
    void save(null);
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
          {category && (
            <button
              type="button"
              onClick={() => {
                attempt.current++;
                setSaveFailed(false);
                setFailedChoice(null);
                setOpen(false);
                setGallery(true);
              }}
              className="flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left text-sm hover:bg-accent"
            >
              <LayoutGrid className="h-4 w-4 text-muted-foreground" />
              {t("avatar.gallery")}
            </button>
          )}
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
          {saveFailed && (
            <div className="border-t border-border px-2.5 py-2">
              <p role="alert" className="text-xs text-destructive">
                {t("avatar.saveFailed")}
              </p>
              {failedChoice && (
                <button
                  type="button"
                  className="mt-2 text-xs font-medium underline"
                  onClick={() => void save(failedChoice.dataUrl)}
                >
                  {t("privacy.retry")}
                </button>
              )}
            </div>
          )}
        </PopoverContent>
      </Popover>
      <AvatarCropDialog
        file={cropFile}
        onConfirm={(dataUrl) => {
          setCropFile(null);
          void save(dataUrl);
        }}
        onCancel={() => setCropFile(null)}
      />
      {category && (open || gallery) && (
        <Suspense fallback={null}>
          <AvatarGallery
            category={category}
            open={gallery}
            onPick={(dataUrl) => {
              setGallery(false);
              void save(dataUrl);
            }}
            onClose={() => setGallery(false)}
          />
        </Suspense>
      )}
    </>
  );
}
