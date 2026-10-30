import { GroupControlStatus } from "./GroupControlStatus";
import { useGroupControlOperation } from "./useGroupControlOperation";
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

export function PendingChannelInvite({ channelId }: { channelId: string }) {
  const { t } = useTranslation();
  const acceptChannelInvite = useChat((s) => s.acceptChannelInvite);
  const declineChannelInvite = useChat((s) => s.declineChannelInvite);
  const operation = useGroupControlOperation(channelId);
  const busy = operation.busy;
  const [declineChannelId, setDeclineChannelId] = useState<string | null>(null);
  const isCurrent = () => {
    const active = useChat.getState().active;
    return active?.kind === "channel" && active.id === channelId;
  };

  return (
    <div className="border-t bg-card px-5 py-4" role="status">
      <p className="text-sm text-muted-foreground">
        {t("conversation.pendingInviteHint")}
      </p>
      <div className="mt-3 flex gap-2">
        <Button
          disabled={busy}
          onClick={() => {
            if (busy || !isCurrent()) return;
            void operation.run(acceptChannelInvite);
          }}
        >
          {t("conversation.acceptInvite")}
        </Button>
        <Button
          variant="outline"
          disabled={busy}
          onClick={() => {
            if (isCurrent()) setDeclineChannelId(channelId);
          }}
          data-testid="pending-invite-decline-trigger"
        >
          {t("conversation.declineInvite")}
        </Button>
      </div>
      <GroupControlStatus
        outcome={operation.outcome}
        cancelRequested={operation.cancelRequested}
      />
      <Dialog
        open={declineChannelId === channelId}
        onOpenChange={(open) => {
          setDeclineChannelId(open && isCurrent() ? channelId : null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("conversation.declineInvite")}</DialogTitle>
            <DialogDescription>
              {t("conversation.declineInviteConfirm")}
            </DialogDescription>
          </DialogHeader>
          <GroupControlStatus
            outcome={operation.outcome}
            cancelRequested={operation.cancelRequested}
          />
          <div className="flex justify-end gap-2">
            <Button
              variant="ghost"
              disabled={busy}
              onClick={() => setDeclineChannelId(null)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              variant="destructive"
              disabled={busy}
              data-testid="pending-invite-decline-confirm"
              onClick={() => {
                if (busy || declineChannelId !== channelId || !isCurrent())
                  return;
                void operation.run(declineChannelInvite, () =>
                  setDeclineChannelId(null),
                );
              }}
            >
              {t("conversation.declineInvite")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
