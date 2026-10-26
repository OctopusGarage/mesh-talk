import { usePacks } from "@/store/packs";
import { messageSticker, messageSummary } from "@/lib/messagePresentation";
import { useTranslation } from "react-i18next";
import { StickerThumb } from "../StickerThumb";
export function FavoriteMessageText({
  text,
  sticker,
  fallback,
}: {
  text: string;
  sticker?: string | null;
  fallback: string;
}) {
  const packs = usePacks((s) => s.packs);
  const { t } = useTranslation();
  const item = messageSticker({ text, sticker }, packs);
  const description = messageSummary({ text, sticker }, t, packs);
  if (item?.url)
    return (
      <StickerThumb src={item.url} alt={description} fallback={item.fallback} />
    );
  return (
    <p className="max-h-64 overflow-y-auto whitespace-pre-wrap break-words text-sm [overflow-wrap:anywhere]">
      {sticker ? description : text || fallback}
    </p>
  );
}
