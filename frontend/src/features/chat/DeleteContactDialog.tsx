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
  const [clearHistory, setClearHistory] = useState(false);
  const live = useRef(true);
  const scope = useRef(captureRuntimeOwner());
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);
  const ready = useChat((s) => s.ready);
  const save = async () => {
    const lease = scope.current;
    if (!lease.current()) return;
    setBusy(true);
    setFailed(false);
    const ok = await useChat.getState().deleteContact(account, clearHistory);
    if (!live.current || !lease.current()) return;
    setBusy(false);
    if (ok) onClose();
    else setFailed(true);
  };
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
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
        <label className="flex items-start gap-3 text-sm">
          <input
            data-testid="delete-contact-history"
            type="checkbox"
            checked={clearHistory}
            disabled={busy}
            onChange={(event) => setClearHistory(event.target.checked)}
            className="mt-1 accent-primary"
          />
          <span>
            {t("contactDeletion.clearHistory")}
            <span className="mt-1 block text-xs text-muted-foreground">
              {t("contactDeletion.historyHint")}
            </span>
          </span>
        </label>
        {failed && (
          <p role="alert" className="text-sm text-destructive">
            {t("contactDeletion.error")}
          </p>
        )}
        <div className="flex flex-wrap justify-end gap-2">
          <Button
            id="delete-contact-cancel"
            data-testid="delete-contact-cancel"
            variant="outline"
            onClick={onClose}
          >
            {t(busy ? "common.close" : "common.cancel")}
          </Button>
          <Button
            data-testid="delete-contact-confirm"
            variant="destructive"
            disabled={busy || !ready}
            onClick={() => void save()}
          >
            {t(busy ? "common.loading" : "contactDeletion.delete")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
