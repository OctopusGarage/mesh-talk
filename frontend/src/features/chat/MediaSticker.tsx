import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { useFileStatus } from "@/store/fileAvailability";
import { useFileObjectUrl } from "./mediaFile";
import { ReceiveProgress } from "./ReceiveProgress";
import { stickerSummary } from "@/lib/stickerMedia";

/** Artwork belongs to the received manifest, independently of installed packs. */
export function MediaSticker({
  fileConv,
  mime,
  fallback,
  label,
  readOnly = false,
}: {
  fileConv: string;
  mime: string;
  fallback: string;
  label?: string;
  readOnly?: boolean;
}) {
  const { t } = useTranslation();
  const description = stickerSummary({ label }, t("message.sticker"));
  const status = useFileStatus(fileConv || undefined);
  const [readFailed, setReadFailed] = useState(false);
  const [retryKey, setRetryKey] = useState(0);
  const onReadError = useCallback(() => setReadFailed(true), []);
  const url = useFileObjectUrl(
    fileConv,
    !!status?.ready && !status.unavailable,
    mime,
    { onReadError, retryKey },
  );
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  if (url && failedUrl !== url)
    return (
      <img
        src={url}
        alt={description}
        title={label}
        data-testid="message-sticker"
        draggable={false}
        onError={() => setFailedUrl(url)}
        className="h-32 w-32 max-w-full select-none object-contain"
      />
    );
  return (
    <div className="w-32 max-w-full space-y-2 text-center">
      <span
        data-testid="message-sticker-fallback"
        role="img"
        aria-label={description}
        className="inline-flex h-32 w-32 max-w-full items-center justify-center text-6xl"
      >
        {fallback}
      </span>
      {(failedUrl && url) ||
      (readFailed && status?.ready && !status.unavailable) ? (
        <div className="text-xs text-muted-foreground">
          <p role="status">
            {t(url ? "files.imageUnsupported" : "files.unavailable")}
          </p>
          {!readOnly && (
            <button
              type="button"
              onClick={() => {
                setFailedUrl(null);
                setReadFailed(false);
                setRetryKey((key) => key + 1);
              }}
              className="mt-1 rounded-md border px-2 py-1 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t("common.retry")}
            </button>
          )}
        </div>
      ) : status?.ready ? (
        <p role="status" className="text-xs text-muted-foreground">
          {t("common.loading")}
        </p>
      ) : (
        <ReceiveProgress status={status} />
      )}
    </div>
  );
}
