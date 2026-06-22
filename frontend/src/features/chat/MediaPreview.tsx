import { useState } from "react";
import { X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { isVideo, useFileObjectUrl, withinInlineCap } from "./mediaFile";

/** Inline preview for a received image or small video. Images are clickable to open a
 * full-size lightbox; videos get a playable `<video controls>`. Renders nothing while the
 * bytes are still loading. Bytes are fetched only when under the relevant size cap. */
export function MediaPreview({
  fileConv,
  name,
  size,
}: {
  fileConv: string;
  name: string;
  size: number;
}) {
  const { t } = useTranslation();
  const video = isVideo(name);
  const withinCap = withinInlineCap(name, size);
  const url = useFileObjectUrl(fileConv, withinCap);
  const [lightbox, setLightbox] = useState(false);

  // A large video stays a file card; surface a hint so the user knows to open it externally.
  if (video && !withinCap) {
    return (
      <p className="mb-1.5 text-xs text-muted-foreground">
        {t("files.largeVideo")}
      </p>
    );
  }
  if (!url) return null;

  if (video) {
    return (
      <video
        src={url}
        controls
        preload="metadata"
        data-testid="file-video"
        className="mb-1.5 max-h-80 w-full rounded-lg bg-black"
      />
    );
  }

  return (
    <>
      <button
        type="button"
        onClick={() => setLightbox(true)}
        title={t("files.viewFullSize")}
        aria-label={t("files.viewFullSize")}
        className="mb-1.5 block w-full"
      >
        <img
          src={url}
          alt={name}
          data-testid="file-image"
          className="max-h-80 w-full rounded-lg object-cover"
        />
      </button>
      {lightbox && (
        <div
          role="dialog"
          aria-modal="true"
          aria-label={name}
          data-testid="file-image-lightbox"
          onClick={() => setLightbox(false)}
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-6"
        >
          <button
            type="button"
            aria-label={t("common.close")}
            className="absolute right-4 top-4 rounded-full bg-background/80 p-2 text-foreground hover:bg-background"
          >
            <X className="h-4 w-4" />
          </button>
          <img
            src={url}
            alt={name}
            className="max-h-full max-w-full rounded-lg object-contain"
          />
        </div>
      )}
    </>
  );
}
