import { useState } from "react";
import { Eraser } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { useChat } from "@/store/chat";

/**
 * Clears the active conversation's local history after a confirm. Clearing is local-only
 * (it erases this device's stored plaintext); it does not delete anything on the other
 * party's device. Lives inside the conversation-history dialog; `onCleared` lets that
 * parent dialog close itself once the history is gone.
 */
export function ClearHistoryButton({
  withLabel,
  onCleared,
}: {
  /** Render a labelled text button (in the history dialog) rather than a bare icon. */
  withLabel?: boolean;
  onCleared?: () => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const clearConversation = useChat((s) => s.clearConversation);

  const confirm = async () => {
    setBusy(true);
    await clearConversation();
    setBusy(false);
    setOpen(false);
    onCleared?.();
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        {withLabel ? (
          <Button
            variant="ghost"
            size="sm"
            data-testid="clear-history"
            className="gap-1.5 text-destructive hover:text-destructive"
          >
            <Eraser className="h-4 w-4" />
            {t("conversation.clearHistory")}
          </Button>
        ) : (
          <Button
            variant="ghost"
            size="icon"
            data-testid="clear-history"
            title={t("conversation.clearHistory")}
            aria-label={t("conversation.clearHistory")}
          >
            <Eraser className="h-4 w-4" />
          </Button>
        )}
      </DialogTrigger>
      <DialogContent className="max-w-sm" data-testid="clear-history-dialog">
        <DialogHeader>
          <DialogTitle>{t("conversation.clearHistory")}</DialogTitle>
          <DialogDescription>
            {t("conversation.clearHistoryConfirm")}
          </DialogDescription>
        </DialogHeader>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setOpen(false)}>
            {t("common.cancel")}
          </Button>
          <Button
            variant="destructive"
            data-testid="clear-history-confirm"
            disabled={busy}
            onClick={() => void confirm()}
          >
            {t("conversation.clearHistory")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
