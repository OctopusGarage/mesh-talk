import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { useChat } from "@/store/chat";
import { captureRuntimeOwner } from "@/store/ownership";

/** A personal deletion; group membership and other members' copies are untouched. */
export function DeleteChatDialog({
  channelId,
  name,
  onClose,
}: {
  channelId: string;
  name: string;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const live = useRef(true);
  const scope = useRef(captureRuntimeOwner());
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const ready = useChat((s) => s.ready);
  async function remove() {
    if (busy) return;
    const lease = scope.current;
    if (!lease.current()) return;
    setBusy(true);
    setFailed(false);
    const ok = await useChat.getState().deleteLeftChannel(channelId);
    if (!live.current || !lease.current()) return;
    setBusy(false);
    if (ok) onClose();
    else setFailed(true);
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent
        data-testid="delete-chat-dialog"
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          document.getElementById("delete-chat-cancel")?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>{t("conversationDeletion.title")}</DialogTitle>
          <DialogDescription>
            {t("conversationDeletion.description")}
          </DialogDescription>
        </DialogHeader>
        <p className="break-words text-sm font-medium">{name}</p>
        {failed && (
          <p role="alert" className="text-sm text-destructive">
            {t("conversationDeletion.error")}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button id="delete-chat-cancel" variant="outline" onClick={onClose}>
            {t(busy ? "common.close" : "common.cancel")}
          </Button>
          <Button
            variant="destructive"
            disabled={busy || !ready}
            onClick={() => void remove()}
          >
            {t(busy ? "common.loading" : "conversationDeletion.delete")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
