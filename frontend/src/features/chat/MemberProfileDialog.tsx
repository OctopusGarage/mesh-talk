import { MessageCircle } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { IdentityCrest } from "@/components/identity";
import { useChat } from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import type { MemberProfileTarget } from "./memberProfileDetails";
import { memberProfileDevices } from "./memberProfileDetails";

export function MemberProfileDialog({
  target,
  onOpenChange,
  onMessage,
}: {
  target: MemberProfileTarget | null;
  onOpenChange: (open: boolean) => void;
  onMessage?: (accountId: string, name: string) => void;
}) {
  if (!target) return null;
  return (
    <MemberProfileContent
      target={target}
      onOpenChange={onOpenChange}
      onMessage={onMessage}
    />
  );
}

function MemberProfileContent({
  target,
  onOpenChange,
  onMessage,
}: {
  target: MemberProfileTarget;
  onOpenChange: (open: boolean) => void;
  onMessage?: (accountId: string, name: string) => void;
}) {
  const { t } = useTranslation();
  const peers = useChat((s) => s.peers);
  const presence = usePresenceFor(target.accountId ?? undefined);
  const devices = memberProfileDevices(target, peers);
  const name = target.name || t("common.unnamed");
  const accountId = target.accountId;

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent className="max-w-sm" data-testid="member-profile-dialog">
        <DialogHeader>
          <DialogTitle>{t("memberProfile.title")}</DialogTitle>
          <DialogDescription>
            {t("memberProfile.description")}
          </DialogDescription>
        </DialogHeader>
        <div className="rounded-xl border bg-muted/30 p-4">
          <IdentityCrest
            id={target.accountId || target.deviceId || name}
            name={name}
            hideId
            wrapName
            status={
              target.isSelf
                ? "online"
                : accountId
                  ? presenceStatus(presence)
                  : undefined
            }
            variant="large"
          />
          <p className="mt-2 pl-[68px] text-xs text-muted-foreground">
            {target.isSelf
              ? t("presence.online")
              : accountId
                ? presenceLabel(presence, t)
                : t("memberProfile.presenceUnavailable")}
          </p>
        </div>

        <div className="space-y-3">
          {accountId && (
            <div>
              <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                {t("memberProfile.accountId")}
              </div>
              <code
                data-testid="member-profile-account-id"
                className="mt-1 block select-text break-all rounded-md bg-muted px-2.5 py-2 text-xs"
              >
                {accountId}
              </code>
            </div>
          )}
          <div>
            <div className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
              {t("memberProfile.devices")}
            </div>
            {devices.length === 0 ? (
              <p className="mt-1 text-sm text-muted-foreground">
                {t("memberProfile.noDevice")}
              </p>
            ) : (
              <div className="mt-1 space-y-2">
                {devices.map((device) => (
                  <div
                    key={device.id}
                    className="min-w-0 rounded-lg border px-3 py-2.5"
                  >
                    <div className="flex flex-wrap items-baseline justify-between gap-1">
                      <span className="text-xs font-medium">
                        {t("memberProfile.deviceId")}
                      </span>
                      {device.postOffice && (
                        <span className="text-[11px] text-muted-foreground">
                          {t("memberProfile.postOffice")}
                        </span>
                      )}
                    </div>
                    <code className="mt-1 block select-text break-all text-xs">
                      {device.id}
                    </code>
                    <div className="mt-2 text-[11px] font-medium text-muted-foreground">
                      {t("memberProfile.networkEndpoint")}
                    </div>
                    {device.endpoint ? (
                      <code className="block select-text text-xs [overflow-wrap:anywhere]">
                        {device.endpoint}
                      </code>
                    ) : (
                      <span className="block text-xs text-muted-foreground">
                        {t("memberProfile.unavailable")}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
        {devices.some((device) => device.endpoint) && (
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("memberProfile.endpointHint")}
          </p>
        )}
        {accountId && onMessage && !target.isSelf && (
          <Button
            data-testid="member-profile-message"
            className="w-full gap-2"
            onClick={() => {
              onOpenChange(false);
              onMessage(accountId, name);
            }}
          >
            <MessageCircle className="h-4 w-4" />
            {t("members.openDm")}
          </Button>
        )}
      </DialogContent>
    </Dialog>
  );
}
