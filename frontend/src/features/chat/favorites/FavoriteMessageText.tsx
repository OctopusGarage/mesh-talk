import { usePacks } from "@/store/packs";
import { stickerById } from "@/lib/stickerPacks";
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
  const item = sticker ? stickerById(sticker, packs) : undefined;
  if (item)
    return (
      <StickerThumb src={item.url} alt={item.label ?? text} fallback={text} />
    );
  return (
    <p className="max-h-64 overflow-y-auto whitespace-pre-wrap break-words text-sm [overflow-wrap:anywhere]">
      {text || fallback}
    </p>
  );
}
