import { Bookmark, ListChecks, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import type { ChatMessage } from "@/store/conversationModel";
import { MAX_FAVORITE_MESSAGES } from "./selection";
import { FavoriteEditor } from "./FavoriteEditor";

export function CollectionToolbar({
  selected,
  onCancel,
  onCollect,
}: {
  selected: ChatMessage[];
  onCancel: () => void;
  onCollect: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div
      data-testid="favorite-selection-toolbar"
      className="flex flex-wrap items-center gap-2 border-b bg-card px-4 py-2"
    >
      <ListChecks className="h-4 w-4 shrink-0 text-signal" aria-hidden="true" />
      <span role="status" className="flex-1 text-sm">
        {t("messageFavorites.selected", {
          count: selected.length,
          max: MAX_FAVORITE_MESSAGES,
        })}
      </span>
      <Button size="sm" variant="ghost" onClick={onCancel}>
        <X className="mr-1 h-4 w-4" aria-hidden="true" />
        {t("common.cancel")}
      </Button>
      <Button size="sm" onClick={onCollect} disabled={selected.length === 0}>
        <Bookmark className="mr-1 h-4 w-4" aria-hidden="true" />
        {t("messageFavorites.collect")}
      </Button>
    </div>
  );
}
export { FavoriteEditor };
