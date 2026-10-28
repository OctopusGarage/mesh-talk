import { useState } from "react";
import { Image as ImageIcon } from "lucide-react";
import { useTranslation } from "react-i18next";
import { isImage } from "@/lib/mediaTypes";
import { messageSticker, messageSummary } from "@/lib/messagePresentation";
import type { ChatMessage } from "@/store/chat";
import { usePacks } from "@/store/packs";
import { useFileStatus } from "@/store/fileAvailability";
import { useSavedDownloads } from "@/lib/savedDownloads";
import { blobMime, useFileObjectUrl, withinInlineCap } from "./mediaFile";

/** Compact content shared by sent quotes and the composer; never reveals protected bytes. */
export function ReplyPreview({
  message,
  authorName,
}: {
  message: ChatMessage;
  authorName?: string;
}) {
  const { t } = useTranslation();
  const packs = usePacks((s) => s.packs);
  const file = message.file;
  const expired =
    message.expiresAtMs != null && message.expiresAtMs <= Date.now();
  const image =
    !!file &&
    isImage(file.name) &&
    !message.protection &&
    !message.recalled &&
    !expired &&
    !messageSticker(message, packs);
  const fileConv = image ? file.fileConv : "";
  const status = useFileStatus(fileConv || undefined);
  const savedPath = useSavedDownloads()[fileConv];
  const available = !!savedPath || !!status?.ready;
  const url = useFileObjectUrl(
    fileConv,
    image &&
      !message.pending &&
      available &&
      withinInlineCap(file.name, file.size),
    image ? blobMime(file.name, file.mime) : undefined,
  );
  const [failedUrl, setFailedUrl] = useState<string | null>(null);
  const summary = messageSummary(message, t, packs) || t("composer.message");
  const author = authorName ? (
    <span className="max-w-32 shrink-0 truncate font-medium">
      {authorName}:
    </span>
  ) : null;
  if (!image)
    return (
      <span className="flex min-w-0 items-center gap-1.5">
        {author}
        <span className="block min-w-0 truncate">{summary}</span>
      </span>
    );
  return (
    <span className="flex min-w-0 items-center gap-2">
      {author}
      <span className="flex h-12 w-16 shrink-0 items-center justify-center overflow-hidden rounded bg-foreground/5">
        {url && url !== failedUrl ? (
          <img
            src={url}
            alt={file.name}
            onError={() => setFailedUrl(url)}
            className="h-full w-full object-contain"
            draggable={false}
          />
        ) : (
          <ImageIcon aria-hidden="true" className="h-5 w-5 opacity-70" />
        )}
      </span>
      {(!authorName || !url || url === failedUrl) && (
        <span className="min-w-0 truncate">
          {!available ? t("files.unavailable") : t("message.image")}
        </span>
      )}
    </span>
  );
}
