import { useState } from "react";
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
import { usePresenceFor } from "@/store/presence";
import { captureRuntimeOwner } from "@/store/ownership";

export function DeleteContactDialog({
  account,
  name,
  onClose,
}: {
  account: string;
  name: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const online = usePresenceFor(account).online;
  const ready = useChat((s) => s.ready);
  const save = async () => {
    const lease = captureRuntimeOwner();
    setBusy(true);
    setFailed(false);
    const ok = await useChat.getState().deleteContact(account);
    setBusy(false);
    if (!lease.current()) return;
    if (ok) onClose();
    else setFailed(true);
  };
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent
        data-testid="delete-contact-dialog"
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          document.getElementById("delete-contact-cancel")?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>{t("contactDeletion.title")}</DialogTitle>
          <DialogDescription>
            {t("contactDeletion.description")}
          </DialogDescription>
        </DialogHeader>
        <p className="break-all text-sm font-medium">{name}</p>
        <p className="break-all font-mono text-xs text-muted-foreground">
          {account}
        </p>
        {(failed || online) && (
          <p role="alert" className="text-sm text-destructive">
            {t(online ? "contactDeletion.online" : "contactDeletion.error")}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          <Button
            id="delete-contact-cancel"
            data-testid="delete-contact-cancel"
            variant="outline"
            disabled={busy}
            onClick={onClose}
          >
            {t("common.cancel")}
          </Button>
          <Button
            data-testid="delete-contact-confirm"
            variant="destructive"
            disabled={busy || online || !ready}
            onClick={() => void save()}
          >
            {t(busy ? "common.loading" : "contactDeletion.delete")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
