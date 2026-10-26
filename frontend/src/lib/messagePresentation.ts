import type { CustomizationPack } from "./pack";
import {
  cleanStickerLabel,
  decodeStickerMedia,
  stickerSummary,
} from "./stickerMedia";
import { stickerById } from "./stickerPacks";
import { attachmentLabel } from "./directoryAttachment";

/** A presentation input shared by live messages, history and saved excerpts. */
interface MessageContent {
  text?: string;
  sticker?: string | null;
  protection?: unknown;
  recalled?: boolean;
  expiresAtMs?: number | null;
  file?: { name: string; mime: string; size: number; media: boolean } | null;
}

export function messageSticker(
  message: MessageContent,
  packs: CustomizationPack[] = [],
  now = Date.now(),
) {
  if (
    message.protection ||
    message.recalled ||
    (message.expiresAtMs != null && message.expiresAtMs <= now)
  )
    return null;
  const media = message.file ? decodeStickerMedia(message.file) : null;
  const id = media?.id ?? message.sticker;
  if (!id) return null;
  const installed = stickerById(id, packs);
  return {
    id,
    label: media?.label ?? (cleanStickerLabel(installed?.label) || undefined),
    fallback: media?.fallback ?? installed?.emoji ?? message.text ?? "",
    url: media ? undefined : installed?.url,
    media: !!media,
  };
}

/** Resolve content once; every compact surface uses the same semantic description. */
export function messageSummary(
  message: MessageContent,
  t: (key: string) => string,
  packs: CustomizationPack[] = [],
  now = Date.now(),
): string {
  if (message.recalled) return t("message.recalledSummary");
  if (message.expiresAtMs != null && message.expiresAtMs <= now)
    return t("messageFavorites.contentUnavailable");
  if (message.protection) return t("protectedMessage.label");
  const sticker = messageSticker(message, packs, now);
  if (sticker) return stickerSummary(sticker, t("message.sticker"));
  return (
    message.text ||
    (message.file
      ? attachmentLabel(
          message.file.name,
          message.file.mime,
          message.file,
          t("message.sticker"),
        )
      : "")
  );
}
