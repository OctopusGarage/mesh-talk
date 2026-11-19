import { useCallback, useLayoutEffect, useRef, useState } from "react";
import { captureChatOwnership } from "@/store/chat";
import type { OutgoingMediaPreview } from "@/store/outgoingMediaPreviews";
import { useMotionOK } from "@/lib/motion";
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
  localPreview,
  externalStatus = false,
}: {
  fileConv: string;
  mime: string;
  fallback: string;
  label?: string;
  readOnly?: boolean;
  localPreview?: OutgoingMediaPreview;
  externalStatus?: boolean;
}) {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const description = stickerSummary({ label }, t("message.sticker"));
  const status = useFileStatus(fileConv || undefined);
  const [readFailed, setReadFailed] = useState<string | null>(null);
  const [retryKey, setRetryKey] = useState(0);
  const onReadError = useCallback(() => setReadFailed(fileConv), [fileConv]);
  const url = useFileObjectUrl(
    fileConv,
    !!status?.ready && !status.unavailable,
    mime,
    { onReadError, retryKey, reuse: true },
  );
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  const [decodedUrl, setDecodedUrl] = useState<string | null>(null);
  const source = useRef<{ fileConv: string; url: string | null } | null>(null);
  useLayoutEffect(() => {
    source.current = { fileConv, url };
    return () => {
      source.current = null;
    };
  }, [fileConv, url]);
  const lease = captureChatOwnership();
  const currentImage = (image: HTMLImageElement) =>
    lease.current() &&
    source.current?.fileConv === fileConv &&
    source.current.url === url &&
    image.getAttribute("src") === url;
  const [failedPreview, setFailedPreview] = useState<string | null>(null);
  const preview =
    !readOnly && localPreview?.current() && failedPreview !== localPreview.url
      ? localPreview
      : undefined;
  const durable = !!url && failedUrl !== url;
  const decoded = durable && decodedUrl === url;
  return (
    <div className="w-32 max-w-full space-y-2 text-center">
      <div className="relative h-32 w-32 max-w-full">
        {preview && !decoded ? (
          <img
            data-testid="outgoing-media-preview"
            key={preview.url}
            src={preview.url}
            onError={() => {
              if (preview.current() && lease.current())
                setFailedPreview(preview.url);
            }}
            alt={description}
            title={label}
            draggable={false}
            className="absolute inset-0 h-32 w-32 max-w-full select-none object-contain"
          />
        ) : !preview ? (
          <span
            data-testid="message-sticker-fallback"
            aria-hidden={decoded}
            style={{
              opacity: decoded ? 0 : 1,
              transition: `opacity ${motionOK ? 180 : 80}ms cubic-bezier(0.22, 1, 0.36, 1)`,
            }}
            role="img"
            aria-label={description}
            className="inline-flex h-32 w-32 max-w-full items-center justify-center text-6xl"
          >
            {fallback}
          </span>
        ) : null}
        {durable && (
          <img
            key={url}
            src={url}
            alt={description}
            title={label}
            aria-hidden={!decoded}
            data-testid="message-sticker"
            draggable={false}
            onLoad={(event) => {
              if (
                !currentImage(event.currentTarget) ||
                !event.currentTarget.naturalWidth ||
                !event.currentTarget.naturalHeight
              )
                return;
              setDecodedUrl(url);
              if (preview?.current()) preview.release();
            }}
            onError={(event) => {
              if (currentImage(event.currentTarget)) setFailedUrl(url);
            }}
            className="absolute inset-0 h-32 w-32 max-w-full select-none object-contain"
            style={{
              opacity: decoded ? 1 : 0,
            }}
          />
        )}
      </div>
      {!decoded &&
      ((!!url && failedUrl === url) ||
        (readFailed === fileConv && status?.ready && !status.unavailable)) ? (
        <div className="text-xs text-muted-foreground">
          <p role="status">
            {t(url ? "files.imageUnsupported" : "files.unavailable")}
          </p>
          {!readOnly && (
            <button
              type="button"
              onClick={() => {
                setFailedUrl(null);
                setReadFailed(null);
                setRetryKey((key) => key + 1);
              }}
              className="mt-1 rounded-md border px-2 py-1 hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t("common.retry")}
            </button>
          )}
        </div>
      ) : decoded || preview || externalStatus ? null : status?.ready ? (
        <p role="status" className="text-xs text-muted-foreground">
          {t("common.loading")}
        </p>
      ) : (
        <ReceiveProgress status={status} />
      )}
    </div>
  );
}
